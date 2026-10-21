//! 干净容器判分（POC-1）——判分搬出 agent 容器，落在**全新 eval 容器**里。
//!
//! 为什么要搬：agent 容器被注入了出网代理环境变量（`swev.rs` 的 proxy_env 分支），
//! 判分脚本里的 `pip install .` 与 P2P 断言都会看到这些变量，于是同一份补丁
//! 在 agent 容器里判 false、搬进干净容器判 true（受控实验见 设计/判分复核-差异清单.md）。
//! 另一个洞：原地判分会把 agent 留在容器里的未跟踪文件与装过的依赖一起带进判分，
//! 而 agent.diff 只取 `git diff`——两条线判的不是同一个状态。
//!
//! 三个坑（照抄 tools/cb-judge.sh 的实证教训，改动前先读那份脚本）：
//!   1. eval 镜像的 /testbed 是合成 SWE-bench 提交，与上游 blob 不同——
//!      `git apply`（含 --3way）报 "does not match index" 拒收，**必须** GNU `patch -p1 --fuzz=3`；
//!   2. 判分链上 `| tee` 会吞掉 judge 的退出码——judge 崩了上层看到 0；
//!      故本模块不用管道，直接取子进程退出码；
//!   3. 「补丁应用失败」与「判分链错误」必须分得开——本模块用不同 note 前缀
//!      （`APPLY_FAILED:` / `JUDGE_ERROR:`）与不同的 resolved 取值区分。
//!
//! 容器宿主：干净判分要求容器能出网（eval_script 第 13 行 `pip install .`），
//! 而 anolis 宿主出网靠代理环境变量——一旦注入代理就又污染回去了。
//! 故干净判分**只落在本机 docker**（本机容器走 docker NAT 直连），不随 --host 走。

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::swev::SwevTask;
use crate::tb;

/// 判分器（与 swev.rs 同源路径；工具链在 venv 里，不进仓库）。
/// fix4-observability：改为 pub(crate)，obs 模块对脚本/venv 采集指纹时同源引用，防两份常量漂移。
pub(crate) const JUDGE_PY: &str = "/home/pan-ding/workspace/agent黑灯工厂/tools/swev-judge.py";
pub(crate) const JUDGE_PY_BIN: &str = "/home/pan-ding/swev-venv/bin/python";
/// eval 脚本超时（官方 run_evaluation --timeout 默认 1800s）。
const EVAL_TIMEOUT_SEC: u32 = 1800;
/// judge 调用退避重试：首次失败后再试 2 次（供给面抖动：urllib3 Traceback 等）。
const JUDGE_MAX_ATTEMPTS: u32 = 3;
const JUDGE_BACKOFF_SEC: [u64; 2] = [5, 15];

/// 判分结论：resolved（None=判分链出错，Some(false)=判过但没修好）+ 说明 + eval 日志指纹。
pub struct Verdict {
    pub resolved: Option<bool>,
    pub note: String,
    pub eval_sha256: Option<String>,
    /// fix4-observability：eval 是否被超时击杀（超时假阴性的显式判别位）。
    pub eval_timeout: bool,
    /// fix4-observability：执行判分的容器名（R8 容器级取证，与 judge_mode_actual 互证）。
    pub judge_container: Option<String>,
    /// fix4-observability：判分子进程结构化详情（见 [`JudgeRun`]）。
    pub judge_run: JudgeRun,
}

/// fix4-observability：判分子进程（swev-judge.py）的结构化详情。
/// 全部字段进 harness.judge 事件；None = 判分链未走通到对应环节。
#[derive(Debug, Clone, Default)]
pub struct JudgeRun {
    pub resolved: Option<bool>,
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
}

/// 干净容器判分：agent.diff → 全新 eval 容器 → 官方 eval_script → 本地官方判分器。
///
/// - `agent_diff_path`：agent 容器里 `git diff` 取到的补丁（工作区已有文件）。
/// - `out_dir`：判分产物目录（eval.sh / apply.log / eval.log）。
/// - `task_json_path`：swev-prep 产出的原始任务包（判分器全字段来源）。
pub fn verify_clean(
    task: &SwevTask,
    agent_diff_path: &Path,
    out_dir: &Path,
    task_json_path: &Path,
) -> Verdict {
    if let Err(err) = std::fs::create_dir_all(out_dir) {
        return Verdict {
            resolved: None,
            note: format!("建判分目录失败: {err}"),
            eval_sha256: None,
            eval_timeout: false,
            judge_container: None,
            judge_run: JudgeRun::default(),
        };
    }
    let diff = std::fs::read_to_string(agent_diff_path).unwrap_or_default();
    if let Err(err) = std::fs::write(out_dir.join("eval.sh"), &task.eval_script) {
        return Verdict {
            resolved: None,
            note: format!("写 eval.sh 失败: {err}"),
            eval_sha256: None,
            eval_timeout: false,
            judge_container: None,
            judge_run: JudgeRun::default(),
        };
    }
    // 判分器要拿补丁做 FAIL_TO_PASS 归因：补丁文件在哪就用哪个。
    let judge_diff = if out_dir.join("agent.diff").is_file() {
        out_dir.join("agent.diff")
    } else {
        agent_diff_path.to_path_buf()
    };
    // 容器名带 pid 与纳秒戳：并发重判不撞名，且事后可溯源到哪次判分。
    let container = format!(
        "afbench-judge-{}-{}",
        sanitize(&task.instance_id),
        uniq_suffix()
    );
    let _ = docker_quiet(&["rm", "-f", &container]);    // 干净容器：**不注入任何代理环境变量**（这一条就是 POC-1 的全部意义）。
    // 资源护栏（2026-10-21，常量与校准注释见 tb 模块）：判分容器 4g/256——
    // 判分跑完整测试套件（pip install . + pytest），给足但封顶；串行判分口径下
    // 全厂同时只有个位数判分容器，14 臂 agent 6g + 判分 4g 仍在 125G 预算内。
    let started = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            &container,
            "--memory",
            tb::SWEV_EVAL_CONTAINER_MEM,
            "--pids-limit",
            tb::SWEV_EVAL_CONTAINER_PIDS,
            &task.image,
            "sleep",
            "infinity",
        ])
        .stdin(Stdio::null())
        .output();
    match started {
        Err(err) => {
            return Verdict {
                resolved: None,
                note: format!("起干净判分容器失败: {err}"),
                eval_sha256: None,
                eval_timeout: false,
                judge_container: None,
                judge_run: JudgeRun::default(),
            }
        }
        Ok(out) if !out.status.success() => {
            return Verdict {
                resolved: None,
                note: format!(
                    "起干净判分容器失败: {}",
                    String::from_utf8_lossy(&out.stderr)
                        .chars()
                        .take(300)
                        .collect::<String>()
                ),
                eval_sha256: None,
                eval_timeout: false,
                judge_container: None,
                judge_run: JudgeRun::default(),
            }
        }
        Ok(_) => {}
    }
    // 判分结束（含中途失败）必清容器：判分容器是本机 docker 资源，泄漏会拖垮后续批次。
    let mut verdict = judge_in_container(&container, &diff, &judge_diff, out_dir, task_json_path);
    let _ = docker_quiet(&["rm", "-f", &container]);
    // fix4-observability：判分容器名与判分子进程详情随 Verdict 上浮（事件流取证位）。
    verdict.judge_container = Some(container);
    verdict
}

/// 干净容器内的判分链：应用补丁 → 跑 eval_script → 官方判分器。
fn judge_in_container(
    container: &str,
    diff: &str,
    judge_diff: &Path,
    out_dir: &Path,
    task_json_path: &Path,
) -> Verdict {
    let run = JudgeRun::default();
    // --- 1) 应用 agent 补丁 ---
    // 空补丁：没有可应用的东西（gate=no_patch 的题），跳过应用直接判（判分器自会判 false）。
    let apply_log = out_dir.join("apply.log");
    let apply_err = out_dir.join("apply.err");
    if !diff.trim().is_empty() {
        let out_handle = std::fs::File::create(&apply_log).ok();
        let err_handle = std::fs::File::create(&apply_err).ok();
        let mut child = match Command::new("docker")
            .args([
                "exec",
                "-i",
                container,
                "bash",
                "-c",
                // --batch：补丁对不上时 GNU patch 会交互提问（File to patch?），
                // 非交互桥接下会挂死——--batch 让它跳过提问、照常返回非 0。
                "cd /testbed && patch -p1 --fuzz=3 --no-backup-if-mismatch --batch",
            ])
            .stdin(Stdio::piped())
            .stdout(out_handle.map_or_else(Stdio::null, Stdio::from))
            .stderr(err_handle.map_or_else(Stdio::null, Stdio::from))
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                return Verdict {
                    resolved: None,
                    note: format!("APPLY_FAILED: 应用补丁的桥接启动失败: {err}"),
                    eval_sha256: None,
                    eval_timeout: false,
                    judge_container: None,
                    judge_run: run,
                }
            }
        };
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write as _;
            let _ = stdin.write_all(diff.as_bytes());
        }
        // 关闭 stdin 后等子进程（否则 patch 会等 EOF）。
        drop(child.stdin.take());
        let status = child.wait();
        let ok = matches!(&status, Ok(st) if st.success());
        if !ok {
            // 坑 3：应用失败 ≠ 判分链错误——note 前缀区分，resolved=None。
            let detail = std::fs::read_to_string(&apply_err).unwrap_or_default();
            let tail: String = detail
                .chars()
                .rev()
                .take(400)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return Verdict {
                resolved: None,
                note: format!(
                    "APPLY_FAILED: 补丁应用失败（exit={:?}）: {tail}",
                    status.err()
                ),
                eval_sha256: None,
                eval_timeout: false,
                judge_container: None,
                judge_run: run,
            };
        }
    }
    // --- 2) 跑官方 eval_script ---
    let log_path = out_dir.join("eval.log");
    let eval_path = out_dir.join("eval.sh");
    let bridge = format!("timeout {EVAL_TIMEOUT_SEC} docker exec -i {container} bash -s 2>&1");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&bridge);
    let status = cmd
        .stdin(Stdio::from(
            std::fs::File::open(&eval_path).expect("eval.sh 刚写过"),
        ))
        .stdout(Stdio::from(
            std::fs::File::create(&log_path).expect("eval.log 可建"),
        ))
        .stderr(Stdio::null())
        .status();
    let mut timed_out = false;
    match status {
        Err(err) => {
            return Verdict {
                resolved: None,
                note: format!("eval 执行桥接失败: {err}"),
                eval_sha256: None,
                eval_timeout: false,
                judge_container: None,
                judge_run: run,
            }
        }
        Ok(st) if st.code() == Some(124) => {
            // D-20 项 4 同款：超时击杀显式标记，不留无标记截断日志。
            timed_out = true;
            let marker = format!("\nEVAL_TIMEOUT_KILLED {EVAL_TIMEOUT_SEC}\n");
            if let Ok(mut log) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                use std::io::Write as _;
                let _ = log.write_all(marker.as_bytes());
            }
        }
        Ok(_) => {}
    }
    let eval_sha256 = sha256_hex(&log_path);
    // --- 3) 官方判分器（带退避重试） ---
    let (judge_run, note) = run_judge(task_json_path, &log_path, judge_diff);
    let prefix = if timed_out {
        format!("eval 超时击杀（已落标记 {EVAL_TIMEOUT_SEC}s）；")
    } else {
        String::new()
    };
    Verdict {
        resolved: judge_run.resolved,
        note: format!("{prefix}{note}"),
        eval_sha256,
        eval_timeout: timed_out,
        judge_container: None,
        judge_run,
    }
}

/// 官方判分器调用（退避重试：非 0 退出 / 输出非 JSON 都算失败）。
/// fix4-observability：返回 (JudgeRun 结构化详情, note)。重试耗尽仍失败 → resolved=None
/// 且 note 带 "JUDGE_ERROR: " 前缀；stderr 保留末 2KB 原文（崩溃栈在尾部，红队③）。
pub fn run_judge(
    task_json_path: &Path,
    log_path: &Path,
    diff_path: &Path,
) -> (JudgeRun, String) {
    let mut last_err = String::new();
    let mut last_run = JudgeRun::default();
    for attempt in 1..=JUDGE_MAX_ATTEMPTS {
        let out = Command::new(JUDGE_PY_BIN)
            .arg(JUDGE_PY)
            .arg(task_json_path)
            .arg(log_path)
            .arg(diff_path)
            .output();
        let mut run = JudgeRun::default();
        match out {
            Err(err) => {
                last_err = format!("judge 启动失败: {err}");
            }
            Ok(output) => {
                run.judge_exit_code = output.status.code();
                // 判分 stderr 末 2KB 原文（判分器把决定性错误写 stderr，如 swev-judge.py 的
                // env-yml 合成提示与崩溃栈）；取尾部不用头部。头部 300 字符另存进失败 note。
                let stderr_raw = output.stderr;
                let stderr_head: String = String::from_utf8_lossy(&stderr_raw)
                    .chars()
                    .take(300)
                    .collect();
                let stderr_tail = if stderr_raw.len() > 2048 {
                    String::from_utf8_lossy(&stderr_raw[stderr_raw.len() - 2048..]).into_owned()
                } else {
                    String::from_utf8_lossy(&stderr_raw).into_owned()
                };
                run.judge_stderr = if stderr_tail.is_empty() {
                    None
                } else {
                    Some(stderr_tail)
                };
                let text = String::from_utf8_lossy(&output.stdout).into_owned();
                // 判分 stdout JSON 原文 ≤4096 字节（schema 演进期未预见字段的兜底证据）。
                run.judge_raw = Some(if text.len() > 4096 {
                    text[..4096].to_string()
                } else {
                    text.clone()
                });
                if output.status.success() {
                    match serde_json::from_str::<serde_json::Value>(text.trim()) {
                        Ok(row) => {
                            run.resolved = row.get("resolved").and_then(Value::as_bool);
                            run.log_parsed = row.get("log_parsed").and_then(Value::as_bool);
                            run.env_yml_synthesized =
                                row.get("env_yml_synthesized").and_then(Value::as_bool);
                            run.swebench_version =
                                row.get("swebench_version").and_then(Value::as_str).map(str::to_string);
                            let f2p_pass = row.get("f2p_success").and_then(Value::as_u64);
                            let f2p_fail = row.get("f2p_failure").and_then(Value::as_u64);
                            let p2p_pass = row.get("p2p_success").and_then(Value::as_u64);
                            let p2p_fail = row.get("p2p_failure").and_then(Value::as_u64);
                            if let Some(p) = f2p_pass {
                                run.f2p_pass = Some(u32::try_from(p).unwrap_or(u32::MAX));
                            }
                            run.f2p_total = match (f2p_pass, f2p_fail) {
                                (Some(p), Some(f)) => {
                                    Some(u32::try_from(p + f).unwrap_or(u32::MAX))
                                }
                                (None, Some(f)) => Some(u32::try_from(f).unwrap_or(u32::MAX)),
                                (Some(p), None) => Some(u32::try_from(p).unwrap_or(u32::MAX)),
                                (None, None) => None,
                            };
                            run.p2p_pass = p2p_pass.map(|p| u32::try_from(p).unwrap_or(u32::MAX));
                            run.p2p_total = match (p2p_pass, p2p_fail) {
                                (Some(p), Some(f)) => {
                                    Some(u32::try_from(p + f).unwrap_or(u32::MAX))
                                }
                                (None, Some(f)) => Some(u32::try_from(f).unwrap_or(u32::MAX)),
                                (Some(p), None) => Some(u32::try_from(p).unwrap_or(u32::MAX)),
                                (None, None) => None,
                            };
                            run.tests_collected = match (run.f2p_total, run.p2p_total) {
                                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                                (Some(a), None) => Some(a),
                                (None, Some(b)) => Some(b),
                                (None, None) => None,
                            };
                            // D-20 项 3 消费端：log_parsed=false 如实透传（截断日志与真失败不同貌）。
                            let parsed_note = if run.log_parsed == Some(false) {
                                "[log_parsed=false] ".to_string()
                            } else {
                                String::new()
                            };
                            last_run = run;
                            return (
                                last_run.clone(),
                                format!("{parsed_note}judge: {}", text.trim()),
                            );
                        }
                        Err(err) => {
                            last_err = format!(
                                "judge 输出不是 JSON: {err}: {}",
                                &text[..text.len().min(200)]
                            );
                        }
                    }
                } else {
                    last_err = format!("judge 退出 {}: {stderr_head}", output.status);
                }
            }
        }
        last_run = run;
        if attempt < JUDGE_MAX_ATTEMPTS {
            let wait = JUDGE_BACKOFF_SEC[(attempt - 1) as usize];
            eprintln!(
                "[judge] 判分器第 {attempt}/{JUDGE_MAX_ATTEMPTS} 次失败（{last_err}），{wait}s 后重试"
            );
            std::thread::sleep(Duration::from_secs(wait));
        }
    }
    last_run.resolved = None;
    (last_run, format!("JUDGE_ERROR: {last_err}"))
}

/// eval.log 的 sha256 十六进制串；读不到文件时 None（D-20 项 2）。
pub fn sha256_hex(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&data);
    Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// docker 命令（忽略输出，只看退出码）。
fn docker_quiet(args: &[&str]) -> Result<bool, String> {
    Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|st| st.success())
        .map_err(|err| format!("docker 调用失败: {err}"))
}

/// 容器名可用的字符：只留 [a-z0-9._-]。
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 唯一后缀：pid + 纳秒（并发重判不撞名）。
fn uniq_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

/// 判分产物目录：run 目录下的 rejudge-clean 子目录（不动原判分证据）。
pub fn rejudge_out_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("rejudge-clean")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_docker_safe_chars() {
        assert_eq!(sanitize("psf__requests-5414"), "psf__requests-5414");
        assert_eq!(sanitize("a/b c:d"), "a_b_c_d");
    }

    #[test]
    fn rejudge_out_dir_is_under_run_dir() {
        let root = std::env::temp_dir().join(format!("af-judge-test-{}", std::process::id()));
        let run = root.join("swev-x-a1");
        assert_eq!(rejudge_out_dir(&run), run.join("rejudge-clean"));
    }
}
