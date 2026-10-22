//! SWE-bench Verified 适配：sweb.eval 容器内跑 agent，官方 eval_script 判分。
//!
//! 判分链：agent 在 /testbed 改代码 → git diff 取证 → 容器内执行官方 eval_script
//! （内嵌 test_patch 与 START/END 测试段）→ 本地 swev-judge.py（swebench 4.1 官方解析器）裁决。

use serde::Deserialize;
use std::path::Path;
use std::process::Command;

// fix4-obs-fix1：观测侧常量与指纹函数同源引用（判分脚本/venv 路径两份常量防漂移）。
use crate::obs;

use crate::tb;

/// fix4-observability：harness.judge 事件的全字段载体（红队④的实际分派位在此钉死）。
/// judge_mode_actual 与 run.started.judge_mode_declared 不等即判分无效。
#[derive(Debug, Clone, Default)]
pub struct JudgeDetail {
    pub resolved: Option<bool>,
    pub note: String,
    pub eval_sha256: Option<String>,
    pub eval_timeout: bool,
    pub judge_exit_code: Option<i32>,
    pub judge_stderr: Option<String>,
    pub judge_raw: Option<String>,
    pub log_parsed: Option<bool>,
    pub tests_collected: Option<u32>,
    pub env_yml_synthesized: Option<bool>,
    pub f2p_pass: Option<u32>,
    pub f2p_total: Option<u32>,
    pub p2p_pass: Option<u32>,
    pub p2p_total: Option<u32>,
    pub swebench_version: Option<String>,
    pub judge_container: Option<String>,
    pub judge_mode_actual: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct SwevTask {
    pub instance_id: String,
    pub repo: String,
    pub problem_statement: String,
    pub image: String,
    pub eval_script: String,
    #[serde(default)]
    pub hints_text: String,
}

impl SwevTask {
    pub fn prompt(&self) -> String {
        let mut text = format!(
            "Repository: {}\n\nIssue description:\n{}",
            self.repo, self.problem_statement
        );
        let hints = self.hints_text.trim();
        if !hints.is_empty() {
            text.push_str("\n\nHints:\n");
            text.push_str(hints);
        }
        text
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

/// 起容器。sweb.eval 镜像自带 /testbed 与全部测试资产，无需 build。
pub fn setup(host: &str, task: &SwevTask, profile: &str) -> Result<tb::TbEnv, String> {
    let container = format!(
        "afbench-swev-{}-{profile}",
        task.instance_id.replace("__", "-")
    );
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
    // 资源护栏（2026-10-21，常量与校准注释见 tb 模块）：agent 容器 6g/512。
    // 基建修复不改判分语义：上限远高于实测峰值（agent 期 30-90MiB）；
    // 本代际判分在该容器内原地执行，判分负载同受此封顶。
    let mem = tb::SWEV_AGENT_CONTAINER_MEM;
    let pids = tb::SWEV_AGENT_CONTAINER_PIDS;
    let script = format!(
        "docker run -d --name {container} \
         --memory {mem} --pids-limit {pids} \
         {proxy_env} \
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

/// 判分：取证 diff → 写入 eval 脚本 → 容器内执行 → 本地官方判分。
/// `task_json_path` 是 swev-prep 产出的原始任务包（判分器全字段来源）。
/// fix4-obs-fix1：返回结构化 [`JudgeDetail`]（观测事件全字段载体；
/// 判分容器=agent 容器，judge_mode_actual="in-place"——fix1 代际原地判分口径，
/// fix3 的干净判分容器路径不移植）。
pub fn verify(
    host: &str,
    env: &tb::TbEnv,
    task: &SwevTask,
    work_dir: &Path,
    task_json_path: &Path,
) -> JudgeDetail {
    let mut detail = JudgeDetail {
        judge_mode_actual: "in-place",
        judge_container: Some(env.container.clone()),
        ..JudgeDetail::default()
    };
    if let Err(err) = capture_diff(host, env, work_dir) {
        detail.note = err;
        return detail;
    }
    let diff_path = work_dir.join("agent.diff");
    let eval_path = work_dir.join("eval.sh");
    if let Err(err) = std::fs::write(&eval_path, &task.eval_script) {
        detail.note = format!("写 eval.sh 失败: {err}");
        return detail;
    }
    // eval 脚本经 stdin 喂给容器 bash，避免 docker cp 往返。
    // stderr 必须并入日志：set -x 轨迹与 START/END 标记走 stderr，官方判分需要。
    // host == "local" 时免 ssh 一跳，其余语义一致（timeout 1200 不变）。
    let run_cmd = format!("docker exec -i {} bash -s 2>&1", env.container);
    let log_path = work_dir.join("eval.log");
    let bridge = format!("timeout 1200 {run_cmd}");
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
    // fix4-obs-fix1 裁决：eval 超时（timeout 击杀 exit 124）只记观测位
    // detail.eval_timeout，不在日志落标记行（fix2 D-20 的 EVAL_TIMEOUT_KILLED
    // 标记会改判分器读到的日志字节，fix1 口径日志零改动）。
    if status.as_ref().map(|st| st.code() == Some(124)).unwrap_or(false) {
        detail.eval_timeout = true;
    }
    if let Err(err) = status {
        detail.note = format!("eval 执行桥接失败: {err}");
        return detail;
    }
    let judge = Command::new(obs::JUDGE_PY_BIN)
        .arg(obs::JUDGE_PY)
        .arg(task_json_path)
        .arg(&log_path)
        .arg(&diff_path)
        .output();
    // fix4-obs-fix1：判分日志指纹（哈希判分器实际读到的字节；obs 侧字段）。
    detail.eval_sha256 = obs::sha256_file(&log_path).ok();
    match judge {
        Ok(output) => {
            detail.judge_exit_code = output.status.code();
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                // judge_raw：stdout 原文 ≤4096 字节（按字符边界截断，防多字节 panic）。
                let raw_owned = text.to_string();
                detail.judge_raw = Some(if raw_owned.len() > 4096 {
                    let mut end = 4096;
                    while !raw_owned.is_char_boundary(end) {
                        end -= 1;
                    }
                    raw_owned[..end].to_string()
                } else {
                    raw_owned.clone()
                });
                match serde_json::from_str::<serde_json::Value>(text.trim()) {
                    Ok(row) => {
                        // fix1 口径：结果行只取 resolved；其余字段为 obs 事件载体
                        // （f2p/p2p 由判分器输出的 success/failure 计数映射而来）。
                        detail.resolved = row.get("resolved").and_then(serde_json::Value::as_bool);
                        detail.log_parsed = row.get("log_parsed").and_then(serde_json::Value::as_bool);
                        detail.env_yml_synthesized = row
                            .get("env_yml_synthesized")
                            .and_then(serde_json::Value::as_bool);
                        detail.swebench_version = row
                            .get("swebench_version")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                        detail.tests_collected = row
                            .get("tests_collected")
                            .and_then(serde_json::Value::as_u64)
                            .map(|v| v as u32);
                        let f2p_pass = row.get("f2p_success").and_then(serde_json::Value::as_u64);
                        let f2p_fail = row.get("f2p_failure").and_then(serde_json::Value::as_u64);
                        let p2p_pass = row.get("p2p_success").and_then(serde_json::Value::as_u64);
                        let p2p_fail = row.get("p2p_failure").and_then(serde_json::Value::as_u64);
                        if let Some(p) = f2p_pass {
                            detail.f2p_pass = Some(p as u32);
                        }
                        detail.f2p_total = match (f2p_pass, f2p_fail) {
                            (Some(p), Some(f)) => Some((p + f) as u32),
                            (None, Some(f)) => Some(f as u32),
                            (Some(p), None) => Some(p as u32),
                            (None, None) => None,
                        };
                        if let Some(p) = p2p_pass {
                            detail.p2p_pass = Some(p as u32);
                        }
                        detail.p2p_total = match (p2p_pass, p2p_fail) {
                            (Some(p), Some(f)) => Some((p + f) as u32),
                            (None, Some(f)) => Some(f as u32),
                            (Some(p), None) => Some(p as u32),
                            (None, None) => None,
                        };
                        detail.note = format!("judge: {}", text.trim());
                    }
                    Err(err) => {
                        detail.note = format!(
                            "judge 输出不是 JSON: {err}: {}",
                            &text[..text.len().min(200)]
                        );
                    }
                }
            } else {
                // judge_stderr 事件字段取 stderr 尾部 2KB（决定性错误多在尾部）；
                // note 保持 fix1 口径的头部 300 字符（结果行 verifier_note 语义不变）。
                let stderr_head: String = String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(300)
                    .collect();
                let stderr_raw = &output.stderr;
                detail.judge_stderr = Some(if stderr_raw.len() > 2048 {
                    String::from_utf8_lossy(&stderr_raw[stderr_raw.len() - 2048..]).into_owned()
                } else {
                    String::from_utf8_lossy(stderr_raw).into_owned()
                });
                detail.note = format!("judge 退出 {}: {}", output.status, stderr_head);
            }
        }
        Err(err) => {
            detail.note = format!("judge 启动失败: {err}");
        }
    }
    detail
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
