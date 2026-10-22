//! SWE-bench Verified 适配：sweb.eval 容器内跑 agent，官方 eval_script 判分。
//!
//! 判分链：agent 在 /testbed 改代码 → git diff 取证 → 容器内执行官方 eval_script
//! （内嵌 test_patch 与 START/END 测试段）→ 本地 swev-judge.py（swebench 4.1 官方解析器）裁决。
//!
//! D-20 证据链加固（2026-10-20，可靠性修复，不改判分语义）：
//! - eval 桥接 timeout 击杀时在 eval.log 末尾写显式标记行 `EVAL_TIMEOUT_KILLED <秒数>`；
//! - 判分时计算 eval.log sha256，随结果行落 eval_sha256 字段（判分复核可逐字节对账）。

use serde::Deserialize;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::judge;
use crate::tb;

#[derive(Debug, Deserialize)]
pub struct SwevTask {
    pub instance_id: String,
    pub repo: String,
    pub problem_statement: String,
    pub image: String,
    pub eval_script: String,
    /// fix2 对齐后不再注入提示（审计 D4），字段保留只为数据包反序列化完整。
    #[serde(default)]
    #[allow(dead_code)]
    pub hints_text: String,
}

impl SwevTask {
    /// fix2 对齐（设计/harness-对齐审计.md D4/§6.A.5）：不再注入 hints_text——
    /// 三个标准 harness 都只给 problem_statement，官方 hints 是面向解题者的补丁提示，
    /// 注入属"反向虚高"（与榜单不可比）。字段保留（判分器全字段来源不动）。
    pub fn prompt(&self) -> String {
        format!(
            "Repository: {}\n\nIssue description:\n{}",
            self.repo, self.problem_statement
        )
    }
}

pub fn load_swev_tasks(root: &Path, names: &[String]) -> Result<Vec<SwevTask>, String> {
    let mut out = Vec::new();
    for name in names {
        let path = root.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .map_err(|err| format!("读 {} 失败: {err}", path.display()))?;
        let task: SwevTask = serde_json::from_str(&text)
            .map_err(|err| format!("{} 不是 JSON: {err}", path.display()))?;
        out.push(task);
    }
    Ok(out)
}

/// D-37 产线实例前缀（2026-10-22，14:4x 金丝雀×align2 撞车立项）：
/// 容器名旧形态 `afbench-swev-<task>-<profile>` 不含产线/out 标识，而 setup 先
/// `docker rm -f` 再 run——两条线同题并发时后者的 setup 会杀掉前者的 agent
/// （今日实证；fix2 A/B 时代已有手工硬排除先例，D-22 技术债）。
/// 新形态 `afbench-<instance>-swev-<task>-<profile>`：instance 取 --out 目录的
/// 短哈希（稳定、无需新参数、不同 out 天然不同名），跨线同题并发互不 rm。
/// 引用面调查结论（全工作区+tools/）：无任何精确名消费者——tools/ 零处引用
/// `afbench`（factory-watch/keepalive/sched 只数 `docker images`；img-gc 按
/// 镜像 ancestor 过滤；escrow/cb 线用各自命名），文档引用均为通配/占位/历史
/// 记录，故默认带前缀、不设兼容开关（测量语义零改动：只动容器命名）。
///
/// 哈希口径：--out 路径字符串的 sha256 前 8 位十六进制。用原字符串而非
/// canonicalize——无 I/O、无失败分支；同一臂的 --out 由调度器固定（factory-sched
/// 恒传绝对路径），跨重启稳定；相对/绝对两种写法哈希不同也只会多出一个实例名，
/// 不会退化成同名互踩（失败模式单向安全）。
pub fn instance_id(out: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(out.to_string_lossy().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 容器名生成（与 [`instance_id`] 合起来即 D-37 命名制）。task_id 的 `__`
/// 按旧口径替换为 `-`（swebench instance_id 惯例），其余段均取 [a-z0-9-]，
/// 合成名落在 docker 合法名字符集 `[a-zA-Z0-9][a-zA-Z0-9_.-]*` 内。
pub fn container_name(instance: &str, task_id: &str, profile: &str) -> String {
    format!(
        "afbench-{instance}-swev-{}-{profile}",
        task_id.replace("__", "-")
    )
}

/// 起容器。sweb.eval 镜像自带 /testbed 与全部测试资产，无需 build。
/// `instance` 为产线实例前缀（见 [`instance_id`]），由调用方按 --out 计算一次。
pub fn setup(
    host: &str,
    task: &SwevTask,
    profile: &str,
    instance: &str,
) -> Result<tb::TbEnv, String> {
    let container = container_name(instance, &task.instance_id, profile);
    let _ = tb_ssh(host, &format!("docker rm -f {container}"), 30);
    // 出网代理按宿主区分：anolis 容器经宿主 squid（172.17.0.1:8123）；
    // 本机（local）容器走 docker NAT 直连（本机无 8123 监听），语义同为"容器可出网"。
    let proxy_env = if host == "local" {
        ""
    } else {
        "-e HTTPS_PROXY=http://172.17.0.1:8123 \
         -e HTTP_PROXY=http://172.17.0.1:8123 \
         -e NO_PROXY=localhost,127.0.0.1,100.64.0.0/10"
    };
    // fix2 环境卫生（mini-swe-agent env 块同源）：分页器挂死与进度条/着色噪音
    // 是 SWE 容器高频暗坑，容器级环境变量一次性根治。测量语义变更，计入 fix2。
    // 资源护栏（2026-10-21，常量与校准注释见 tb 模块）：agent 容器 6g/512。
    // 基建修复不改判分语义：上限远高于实测峰值（agent 期 30-90MiB）。
    let mem = tb::SWEV_AGENT_CONTAINER_MEM;
    let pids = tb::SWEV_AGENT_CONTAINER_PIDS;
    let script = format!(
        "docker run -d --name {container} \
         --memory {mem} --pids-limit {pids} \
         {proxy_env} \
         -e PAGER=cat -e MANPAGER=cat -e LESS=-R \
         -e PIP_PROGRESS_BAR=off -e TQDM_DISABLE=1 -e NO_COLOR=1 \
         {} sleep infinity",
        task.image
    );
    match tb_ssh(host, &script, 120) {
        Ok((true, _)) => Ok(tb::TbEnv {
            container,
            task: task.instance_id.clone(),
            host: host.into(),
            workdir: "/testbed".into(),
        }),
        Ok((false, log)) => Err(format!(
            "起容器失败: {}",
            log.chars().take(400).collect::<String>()
        )),
        Err(err) => Err(err),
    }
}

/// 取证 agent diff（完成度门禁与判分共用）：写 work_dir/agent.diff 并返回文本。
pub fn capture_diff(host: &str, env: &tb::TbEnv, work_dir: &Path) -> Result<String, String> {
    let diff_cmd = format!(
        "docker exec {} bash -lc 'cd /testbed && git -c core.fileMode=false diff' ",
        env.container
    );
    let agent_diff = match tb_ssh(host, &diff_cmd, 60) {
        Ok((_, text)) => text,
        Err(err) => return Err(format!("取 diff 失败: {err}")),
    };
    let _ = std::fs::create_dir_all(work_dir);
    let diff_path = work_dir.join("agent.diff");
    if std::fs::write(&diff_path, &agent_diff).is_err() {
        return Err("写 agent.diff 失败".into());
    }
    Ok(agent_diff)
}

/// diff 是否碰了测试文件（评分 harness 拥有测试，模型改测试=越权，S0 实证缺陷）。
/// 返回被碰的文件列表（空=干净）。
pub fn diff_touches_tests(diff: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(path) = rest.split_whitespace().nth(1) {
                let p = path.trim_start_matches("b/").to_lowercase();
                let is_test = p.contains("test")
                    || p.ends_with("conftest.py")
                    || p.ends_with("tox.ini")
                    || p.ends_with("pytest.ini")
                    || p.ends_with("setup.cfg");
                if is_test {
                    hits.push(path.trim_start_matches("b/").to_string());
                }
            }
        }
    }
    hits
}

/// 判分（原地模式）：取证 diff → 写入 eval 脚本 → **agent 容器内**执行 → 本地官方判分。
/// `task_json_path` 是 swev-prep 产出的原始任务包（判分器全字段来源）。
/// 返回 (resolved, note, eval_sha256)——eval_sha256 是判分日志的字节指纹
/// （D-20 项 2：判分可追溯；桥接/判分早期失败时为 None）。
///
/// POC-1：原地模式是**对照组**，默认判分走 `verify_clean`（干净容器）。
/// 原地模式保留是因为判分语义变更需要对照口径，不能直接把旧路删掉。
pub fn verify(
    host: &str,
    env: &tb::TbEnv,
    task: &SwevTask,
    work_dir: &Path,
    task_json_path: &Path,
) -> (Option<bool>, String, Option<String>) {
    let _agent_diff = match capture_diff(host, env, work_dir) {
        Ok(d) => d,
        Err(err) => return (None, err, None),
    };
    let diff_path = work_dir.join("agent.diff");
    let eval_path = work_dir.join("eval.sh");
    if std::fs::write(&eval_path, &task.eval_script).is_err() {
        return (None, "写 eval.sh 失败".into(), None);
    }
    // eval 脚本经 stdin 喂给容器 bash，避免 docker cp 往返。
    // stderr 必须并入日志：set -x 轨迹与 START/END 标记走 stderr，官方判分需要。
    // host == "local" 时免 ssh 一跳，其余语义一致。
    // fix2 对齐（审计 D9/§6.A.8）：eval 超时 1200 → 1800s（官方 run_evaluation --timeout 默认）。
    const EVAL_TIMEOUT_SEC: u32 = 1800;
    let run_cmd = format!("docker exec -i {} bash -s 2>&1", env.container);
    let log_path = work_dir.join("eval.log");
    let bridge = format!("timeout {EVAL_TIMEOUT_SEC} {run_cmd}");
    let mut bridge_cmd = if host == "local" {
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&bridge);
        cmd
    } else {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o").arg("BatchMode=yes").arg(host).arg(&bridge);
        cmd
    };
    let status = bridge_cmd
        .stdin(std::process::Stdio::from(
            std::fs::File::open(&eval_path).expect("eval.sh 刚写过"),
        ))
        .stdout(std::process::Stdio::from(
            std::fs::File::create(&log_path).expect("eval.log 可建"),
        ))
        .stderr(std::process::Stdio::null())
        .status();
    let mut eval_timed_out = false;
    match status {
        Err(err) => return (None, format!("eval 执行桥接失败: {err}"), None),
        Ok(st) => {
            if st.code() == Some(124) {
                // D-20 项 4：timeout 击杀显式化——在日志末尾落标记行，
                // 不再留无标记截断日志（判分复核据此与"截断/失败日志"灰区区分）。
                eval_timed_out = true;
                let marker = format!("\nEVAL_TIMEOUT_KILLED {EVAL_TIMEOUT_SEC}\n");
                if let Ok(mut log) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                    use std::io::Write as _;
                    let _ = log.write_all(marker.as_bytes());
                }
            }
        }
    }
    // D-20 项 2：判分时计算日志指纹（含超时标记行——哈希的是判分器实际读到的字节）。
    let eval_sha256 = judge::sha256_hex(&log_path);
    // POC-3（仅 af 内半边）：判分器调用失败退避重试 2 次——供给面抖动
    // （urllib3 Traceback / 连接重置）不再直接判成 run 失败。
    let (resolved, judge_note) = judge::run_judge(task_json_path, &log_path, &diff_path);
    let timeout_note = if eval_timed_out {
        format!("eval 超时击杀（已落标记 {EVAL_TIMEOUT_SEC}s）；")
    } else {
        String::new()
    };
    (resolved, format!("{timeout_note}{judge_note}"), eval_sha256)
}

/// 判分（干净容器模式，POC-1 默认）：取证 diff → **全新 eval 容器**（无代理环境变量）
/// 应用补丁 → 跑官方 eval_script → 本地官方判分。
///
/// 与原地模式的差异（这是「判分搬干净容器」的语义变更）：
/// - 判分容器不是 agent 容器：agent 装的依赖、留的未跟踪文件都不进判分；
/// - 判分容器不带代理环境变量：eval_script 的 `pip install .` 与 P2P 断言
///   看到的是默认网络环境，不再被出网代理劫持。
///
/// 干净判分只落本机 docker（anolis 容器出网依赖代理，注入即污染），
/// 因此本函数的容器操作不经 --host 桥接。
pub fn verify_clean(
    host: &str,
    env: &tb::TbEnv,
    task: &SwevTask,
    work_dir: &Path,
    task_json_path: &Path,
) -> (Option<bool>, String, Option<String>) {
    // 取证 diff 仍在 agent 容器里取（改动事实来自 agent 的工作区），
    // 但**判分**搬进干净容器。
    if let Err(err) = capture_diff(host, env, work_dir) {
        return (None, err, None);
    }
    let diff_path = work_dir.join("agent.diff");
    let verdict = judge::verify_clean(task, &diff_path, work_dir, task_json_path);
    (verdict.resolved, verdict.note, verdict.eval_sha256)
}

fn tb_ssh(host: &str, script: &str, timeout_sec: u32) -> Result<(bool, String), String> {
    let quoted = format!(
        "timeout {timeout_sec} bash -c '{}'",
        script.replace('\'', "'\\''")
    );
    let output = if host == "local" {
        Command::new("bash").arg("-c").arg(&quoted).output()
    } else {
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(host)
            .arg(&quoted)
            .output()
    }
    .map_err(|err| format!("命令桥接启动失败: {err}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    Ok((output.status.success(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_stable_per_out_dir() {
        // 同一 --out 稳定（跨重启/跨任务同哈希）；不同 --out 必然不同（防撞的根）。
        let a1 = instance_id(Path::new("/tmp/bench-fix4-canary"));
        let a2 = instance_id(Path::new("/tmp/bench-fix4-canary"));
        let b = instance_id(Path::new("/tmp/bench-fix4-align2"));
        assert_eq!(a1, a2, "同一 out 目录必须给出稳定 instance");
        assert_ne!(a1, b, "不同 out 目录必须给出不同 instance");
        // 8 位小写十六进制（sha256 前 4 字节）。
        assert_eq!(a1.len(), 8);
        assert!(a1
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)));
    }

    #[test]
    fn container_name_shape_and_docker_charset() {
        let name = container_name("1a2b3c4d", "pydata__xarray-4094", "mavis-m31f");
        assert_eq!(name, "afbench-1a2b3c4d-swev-pydata-xarray-4094-mavis-m31f");
        // docker 合法名字符集 [a-zA-Z0-9][a-zA-Z0-9_.-]*（首字符 'a' 满足）。
        assert!(name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')));
    }

    #[test]
    fn cross_line_same_task_never_shares_name() {
        // D-37 根治点：同题同 profile、不同产线（instance 不同）→ 容器名不同，
        // 后者的 `docker rm -f` 物理上不可能命中前者的容器。
        let task = "pydata__xarray-4094";
        let left = container_name(
            &instance_id(Path::new("/tmp/bench-canary")),
            task,
            "mavis-m31f",
        );
        let right = container_name(
            &instance_id(Path::new("/tmp/bench-align2")),
            task,
            "mavis-m31f",
        );
        assert_ne!(left, right);
        assert!(left.starts_with("afbench-") && left.contains("-swev-"));
        assert!(right.starts_with("afbench-") && right.contains("-swev-"));
    }

    #[test]
    fn task_id_double_underscore_sanitized_like_before() {
        // 与旧口径一致：instance_id 的 `__` 替换为 `-`，不改判分/取证任何语义。
        let name = container_name("deadbeef", "astropy__astropy-12907", "glm-cp");
        assert_eq!(name, "afbench-deadbeef-swev-astropy-astropy-12907-glm-cp");
    }
}
