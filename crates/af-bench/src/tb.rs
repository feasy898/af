//! Terminal-Bench 4.0 适配：任务容器在 --host 指定的 docker 宿主上跑
//! （anolis=经 ssh 桥接，local=本机直执行），bash 工具统一经 docker exec 桥接。
//!
//! 生命周期：rsync 任务目录 → build 环境镜像 → 起容器 → harness Run（bash 工具）→
//! 提取 /app → build 验证器镜像 → 挂载跑 test.sh → 读 reward.txt → 清容器。
//! 判分口径：reward.txt=1 为 pass；验证器缺 reward 记 infra（不占分母，50-方案口径）。

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct TbTask {
    pub task_id: String,
    pub prompt: String,
}

pub fn load_tb_tasks(root: &Path, names: &[String]) -> Result<Vec<TbTask>, String> {
    let mut out = Vec::new();
    for name in names {
        let dir = root.join(name);
        let prompt_path = dir.join("instruction.md");
        let prompt = std::fs::read_to_string(&prompt_path)
            .map_err(|err| format!("读 {} 失败: {err}", prompt_path.display()))?;
        if !dir.join("environment").join("Dockerfile").is_file() {
            return Err(format!("{name} 缺 environment/Dockerfile"));
        }
        if !dir.join("tests").join("test.sh").is_file() {
            return Err(format!("{name} 缺 tests/test.sh"));
        }
        out.push(TbTask {
            task_id: name.clone(),
            prompt,
        });
    }
    Ok(out)
}

/// 执行一条宿主命令，返回 (status, stdout+stderr 合并文本)。
/// host == "local" 时本机直接执行（本机 docker 直执行面，免 ssh 一跳，
/// 2026-10-20 新增，测量语义不变：只是换了 docker 宿主）；其余经 ssh <host>。
/// 两种路径都用 timeout 包裹，超时语义一致。
fn ssh(host: &str, script: &str, timeout_sec: u32) -> Result<(bool, String), String> {
    let quoted = format!("timeout {timeout_sec} bash -c {}", shell_quote(script));
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

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// bash 工具命令的容器内桥接脚本（`docker exec … bash -lc <这段>` 的实参）。
/// v8-g3（移植自 588382d v6-p01，同型）：前缀 `set -o pipefail; `——管道返回值
/// 改取最右非零段，令 `__AF_RC=$?`（→ 输出末行 `exit=N` → `bash_result` 的
/// is_error 判别）反映真实失败。不加 errexit（不提前中断顺序命令）、不动
/// `cd {workdir} &&` 前缀与分号串联语义；非管道命令的 rc 完全不变。前缀写在
/// profile 脚本之后（-l 已先于 -c 串执行），容器内 profile 无法重置它。
fn bridge_inner(workdir: &str, command: &str) -> String {
    format!("set -o pipefail; cd {} && {}", workdir, command)
}

/// 从命令输出末尾剥离 `__AF_RC=<n>` 标记行，返回 (正文, 退出码)。
/// 标记行缺失 = 外层 timeout 杀掉了整个桥接（含 echo），调用方按超时处理。
/// 只删标记行本身，其后内容（stderr 合并流）原样保留。
fn split_rc(text: &str) -> (String, Option<i32>) {
    if let Some(pos) = text.rfind("__AF_RC=") {
        let line_start = text[..pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line_end = text[pos..]
            .find('\n')
            .map(|p| pos + p)
            .unwrap_or(text.len());
        let line = text[line_start..line_end].trim();
        if let Some(rc) = line
            .strip_prefix("__AF_RC=")
            .and_then(|v| v.trim().parse::<i32>().ok())
        {
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..line_start]);
            out.push_str(&text[line_end..]);
            return (out, Some(rc));
        }
    }
    (text.to_string(), None)
}

pub struct TbEnv {
    pub container: String,
    pub task: String,
    pub host: String,
    pub workdir: String,
    /// S083 外置文件序号（容器内文件名去重用）。pub(crate)：swev.rs 同 crate 组装。
    pub(crate) out_seq: AtomicU64,
}

/// fix4-align2-tier0（第 0 档 S083：大工具输出外置）——超过 [`CAP`] 的完整输出
/// 写回容器内 `/tmp/.af-outputs/`，上下文只进「预览 + 路径」，模型按需分段回读。
/// 参考：Copilot CLI 默认阈值 20KiB、原输出落临时文件只回路径+预览
/// （harness策略调研/最终报告-harness改进策略总表.md §S083）。
/// 单文件写回上限：超大输出走 base64 分块 ssh 写回的成本失控前先放弃（退回有损截断）。
const LARGE_OUTPUT_SAVE_CAP_BYTES: usize = 512 * 1024;
/// base64 分块大小：72_000 字节（3 的倍数，保证分块 base64 可拼接解码）→ 96_000 字符，
/// 远低于单条 ssh argv 的 MAX_ARG_STRLEN（128KiB）限制。
const LARGE_OUTPUT_CHUNK_BYTES: usize = 72_000;
/// 预览保留：头部 2000 字节 + 尾部 600 字节（尾部必须含末行 exit=N，rc 解析依赖它）。
const LARGE_OUTPUT_PREVIEW_HEAD: usize = 2_000;
const LARGE_OUTPUT_PREVIEW_TAIL: usize = 600;

/// bash 工具单条命令的默认超时（秒）——历史口径，缺省不变。
pub const DEFAULT_BASH_TIMEOUT_SEC: u32 = 180;
/// bash 工具单条命令的超时上限（秒）——硬上限，超出即被裁到此处。
pub const MAX_BASH_TIMEOUT_SEC: u32 = 600;

// ---- 容器资源护栏（2026-10-21 运营者指令：既要吃满资源，又不能把机器跑死）----
// 事故背景：anolis（16C/125G）10-21 因"测评容器无内存上限 + 无 swap"耗尽假死，
// 运营者硬重启两次。此后 af-bench 在**每个** docker 宿主创建的**每个**容器都带
// --memory/--pids-limit。口径：上限是天花板不是预留——实测单容器 agent 期
// 30-90MiB、判分期峰值 ~889MiB（本地臂 423 点采样），正常负载远低于此；
// 上限只为单容器失控（内存泄漏 / fork 炸弹）时不拖垮整机。
// 校准=按 anolis 125G×调度器并发度（factory-sched HOST_LIMITS anolis=14 臂齐发）：
//   agent 容器 6g×14=84G + 判分容器 4G（判分串行，安静判分教训）
//   + 宿主系统与在役 ollama 30B（~18G）后仍留 ~19G 余量；
//   TB 任务/判分容器 2g×14=28G（TB 为轻量挑战，实测峰值 <1G）。
// 基建修复，不改判分语义：上限远高于实测峰值，正常题不受影响；结果代号不变。
/// swev agent 容器（sweb.eval 镜像，agent 工作+原地判分都在这一个容器里）。
pub const SWEV_AGENT_CONTAINER_MEM: &str = "6g";
pub const SWEV_AGENT_CONTAINER_PIDS: &str = "512";
/// swev 干净判分容器（judge::verify_clean 的全新 eval 容器，跑补丁+官方 eval_script）。
pub const SWEV_EVAL_CONTAINER_MEM: &str = "4g";
pub const SWEV_EVAL_CONTAINER_PIDS: &str = "256";
/// TB 路径全部容器统一口径（任务环境/验证器/清卷辅助容器，均为轻量负载）。
pub const TB_CONTAINER_MEM: &str = "2g";
pub const TB_CONTAINER_PIDS: &str = "256";

impl TbEnv {
    /// 建环境与容器。任务目录须已在 anolis:/data/bench/tb/<task>。
    pub fn setup(host: &str, task: &TbTask, profile: &str) -> Result<Self, String> {
        let task = &task.task_id;
        let image = format!("afbench-tbenv-{task}");
        let container = format!("afbench-{task}-{profile}");
        let remote_dir = format!("/data/bench/tb/{task}");
        let (ok, log) = ssh(
            host,
            &format!(
                "docker build -q --network host \
                 --build-arg HTTPS_PROXY=http://127.0.0.1:8123 \
                 --build-arg HTTP_PROXY=http://127.0.0.1:8123 \
                 --build-arg NO_PROXY=localhost,127.0.0.1,100.64.0.0/10 \
                  \
                  \
                 -t {image} {remote_dir}/environment"
            ),
            1800,
        )?;
        if !ok {
            return Err(format!("build {image} 失败: {}", tail(&log, 800)));
        }
        let _ = ssh(host, &format!("docker rm -f {container}"), 30);
        // fix2 环境卫生（mini-swe-agent env 块同源）：分页器挂死与进度条/着色噪音
        // 是 SWE 容器高频暗坑，容器级环境变量一次性根治。测量语义变更，计入 fix2。
        let (ok, log) = ssh(
            host,
            &format!(
                "docker run -d --name {container} \
                 --memory {TB_CONTAINER_MEM} --pids-limit {TB_CONTAINER_PIDS} \
                 -e HTTPS_PROXY=http://172.17.0.1:8123 \
                 -e HTTP_PROXY=http://172.17.0.1:8123 \
                 -e NO_PROXY=localhost,127.0.0.1,100.64.0.0/10 \
                 -e PAGER=cat -e MANPAGER=cat -e LESS=-R \
                 -e PIP_PROGRESS_BAR=off -e TQDM_DISABLE=1 -e NO_COLOR=1 \
                 {image} sleep infinity"
            ),
            60,
        )?;
        if !ok {
            return Err(format!("起容器 {container} 失败: {}", tail(&log, 400)));
        }
        Ok(Self {
            container,
            task: task.clone(),
            host: host.into(),
            workdir: "/app".into(),
            out_seq: AtomicU64::new(0),
        })
    }

    /// 任务工作目录（TB=/app，SWE-V=/testbed）——bash 工具每条命令都先 cd 到这里。
    pub fn workdir(&self) -> &str {
        &self.workdir
    }

    /// bash 工具体：容器内执行，fix2 带内恢复语义（不改判分链）：
    ///   · exit=N 从输出首行移到**末行**——首行留给命令自身输出，完成判定
    ///     「工具输出首行=信号词」才有因果可讲；分隔改真实换行（原字面 `\n` 疵点一并修）；
    ///   · 超时（外层 timeout 杀桥接，无 __AF_RC 标记）→ mini-swe-agent timeout_template
    ///     同源教学反馈，运行继续而非死文案；
    ///   · 桥接失败 → 声明"基建瞬断、可重试"，同样不死；
    ///   · 普通非零退出不教学（失败是常态信号，逐条指点只会加噪）；
    ///   · 截断附「换窄命令」指引（mini observation_template 同源）；
    ///   · fix4-align2-tier0（第 0 档 S083）：超 CAP 的输出改为**外置**——完整输出
    ///     写回容器内 /tmp/.af-outputs/（base64 分块），上下文只进「预览+路径+回读指引」；
    ///     写回失败退回有损截断，行为不劣化于旧口径。
    ///
    /// POC-2：超时不再写死 180s——由工具入参 `timeout_ms` 决定（缺省 180s、上限 600s）。
    /// 改不动的是"被杀"这件事本身，所以恢复文案里**不再教**模型用 `timeout <secs>`
    /// 包长任务（任何 secs>180 的写法都会被 harness 自己的硬上限砍掉，教了就是骗模型）：
    /// 改成教"重定向到文件再分段读"，并把 180s/600s 两个真实数字写进文案。
    ///
    /// v8-g3（移植自 588382d v6-p01，exit 语义修正）：桥接内层加 `set -o pipefail`——
    /// 此前 `python -m pytest … 2>&1 | tail -20` 这类管道用法即便首段真实失败，
    /// `__AF_RC=$?` 也只取末端 tail 的 0，exit=N 谎报成功。pipefail 只改「管道
    /// 返回值=最右非零」，非管道命令 rc 不变、无 errexit 不提前中断——纯反馈面、
    /// 零新增刹车（is_error 通道、指纹刹车、止损阈值与判分链全部维持 v8 语义）。
    pub fn exec(&self, command: &str, timeout_sec: u32) -> String {
        let timeout_sec = timeout_sec.clamp(1, MAX_BASH_TIMEOUT_SEC);
        let script = format!(
            "docker exec {} bash -lc {}; echo __AF_RC=$?",
            self.container,
            shell_quote(&bridge_inner(&self.workdir, command))
        );
        let (mut text, rc) = match ssh(&self.host, &script, timeout_sec) {
            Ok((_, raw)) => split_rc(&raw),
            Err(err) => {
                return format!(
                    "[harness] The execution bridge failed before the command could produce output (transient infrastructure error: {}). This is not a task failure — retry the command.\nexit=127",
                    err
                );
            }
        };
        if rc.is_none() {
            text.push_str(&format!(
                "\n[harness] The command was killed after exceeding the {timeout_sec}s timeout (no exit code was reported). Partial output (if any) is above. Note: the harness enforces a hard cap of {MAX_BASH_TIMEOUT_SEC}s per command, so wrapping the command in `timeout <secs>` cannot extend it. To run long jobs, redirect the output to a file and read it back in parts: `cmd > /tmp/out.txt 2>&1; tail -40 /tmp/out.txt` — then poll the file with a short command. You can also ask for a longer timeout on the next call with the `timeout_ms` parameter (default {DEFAULT_BASH_TIMEOUT_SEC}000 ms, max {MAX_BASH_TIMEOUT_SEC}000 ms). Also use non-interactive flags (-y / -n / DEBIAN_FRONTEND=noninteractive) and avoid commands that never exit on their own (servers, watch loops, pagers).",
            ));
        }
        const CAP: usize = 12_000;
        // 末行 exit=N 先落定再截断：预览的末行必须是真实退出码，
        // bash_result 的 split_exit_line（rfind "\nexit="）依赖这一点。
        text.push_str(&format!(
            "\nexit={}",
            rc.map(|n| n.to_string())
                .unwrap_or_else(|| "124".to_string())
        ));
        if text.len() > CAP {
            // fix4-align2-tier0（第 0 档 S083）：大输出外置——完整输出写回容器内文件，
            // 上下文只进「预览 + 路径 + 分段回读指引」。写回失败（桥接瞬断/超上限）
            // 退回原有损截断，绝不因外置失败放大成 run 失败。
            match self.externalize_output(&text) {
                Ok(path) => text = large_output_preview(&text, &path),
                Err(_) => text = lossy_truncate(&text, CAP),
            }
        }
        text
    }

    /// S083：把完整工具输出写回容器内 `/tmp/.af-outputs/`，返回文件路径。
    /// base64 分块写回（每块独立 ssh 往返），任意字节安全、无引号/转义风险；
    /// 任一块失败即整体失败（调用方退回有损截断）。
    fn externalize_output(&self, full: &str) -> Result<String, String> {
        if full.len() > LARGE_OUTPUT_SAVE_CAP_BYTES {
            return Err(format!("output {} bytes exceeds save cap", full.len()));
        }
        let seq = self.out_seq.fetch_add(1, Ordering::Relaxed);
        let stem = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = format!("/tmp/.af-outputs/{}-{}-{}.txt", self.task, stem, seq);
        for (index, chunk) in full.as_bytes().chunks(LARGE_OUTPUT_CHUNK_BYTES).enumerate() {
            let op = if index == 0 { ">" } else { ">>" };
            let script = format!(
                "docker exec {} bash -c {}",
                self.container,
                shell_quote(&format!(
                    "mkdir -p /tmp/.af-outputs && echo {} | base64 -d {} {}",
                    b64_encode(chunk),
                    op,
                    path
                ))
            );
            let (ok, log) = ssh(&self.host, &script, 60)?;
            if !ok {
                return Err(tail(&log, 200));
            }
        }
        Ok(path)
    }

    /// 判分：/app 提取 → 验证器镜像 → 挂载跑 test.sh → reward.txt。
    /// 返回 (reward, verifier_log_tail)。reward: Some(true)=pass Some(false)=fail None=infra。
    pub fn verify(&self) -> (Option<bool>, String) {
        let vol = format!("/data/bench/vol/{}-{}", self.task, self.container);
        // 旧卷里有 root 所有的验证器产物，pan-ding 删不动——用容器内的 root 清。
        let _ = ssh(
            &self.host,
            &format!(
                "docker run --rm --memory {TB_CONTAINER_MEM} --pids-limit {TB_CONTAINER_PIDS} -v {vol}:/vol --entrypoint sh python:3.12-slim -c 'rm -rf /vol/* /vol/.[!.]*; chmod 777 /vol' 2>/dev/null || rm -rf {vol}"
            ),
            120,
        );
        match ssh(
            &self.host,
            &format!(
                "mkdir -p {vol}/app {vol}/logs && docker cp {}:/app/. {vol}/app/",
                self.container
            ),
            300,
        ) {
            Ok((true, _)) => {}
            Ok((false, log)) => return (None, format!("提取 /app 失败: {}", tail(&log, 300))),
            Err(err) => return (None, format!("提取 /app 失败: {err}")),
        }
        let verifier = format!("afbench-tbver-{}", self.task);
        let remote_dir = format!("/data/bench/tb/{}", self.task);
        match ssh(
            &self.host,
            &format!(
                "docker build -q --network host \
                 --build-arg HTTPS_PROXY=http://127.0.0.1:8123 \
                 --build-arg HTTP_PROXY=http://127.0.0.1:8123 \
                 --build-arg NO_PROXY=localhost,127.0.0.1,100.64.0.0/10 \
                  \
                  \
                 -t {verifier} {remote_dir}/tests"
            ),
            1800,
        ) {
            Ok((true, _)) => {}
            Ok((false, log)) => return (None, format!("build 验证器失败: {}", tail(&log, 600))),
            Err(err) => return (None, format!("build 验证器失败: {err}")),
        }
        let run_log = match ssh(
            &self.host,
            &format!(
                "docker run --rm --memory {TB_CONTAINER_MEM} --pids-limit {TB_CONTAINER_PIDS} -v {vol}/app:/app -v {vol}/logs:/logs {verifier} bash /tests/test.sh"
            ),
            1200,
        ) {
            Ok((_, text)) => text,
            Err(err) => return (None, format!("验证器执行失败: {err}")),
        };
        let reward_cmd = format!("cat {vol}/logs/verifier/reward.txt 2>/dev/null || echo missing");
        match ssh(&self.host, &reward_cmd, 30) {
            Ok((_, text)) => {
                let reward = text.trim();
                match reward {
                    "1" => (Some(true), tail(&run_log, 600)),
                    "0" => (Some(false), tail(&run_log, 600)),
                    _ => (
                        None,
                        format!("无 reward.txt；验证器输出: {}", tail(&run_log, 400)),
                    ),
                }
            }
            Err(err) => (None, format!("读 reward 失败: {err}")),
        }
    }

    pub fn cleanup(&self) {
        let _ = ssh(&self.host, &format!("docker rm -f {}", self.container), 60);
    }
}

fn tail(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        text.to_string()
    } else {
        text[text.len() - cap..].to_string()
    }
}

/// UTF-8 安全的前缀截取（原实现 `&text[..keep]` 在多字节边界会 panic——顺手修正）。
fn safe_prefix(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// UTF-8 安全的后缀截取（末行 exit=N 必须保留在结果里）。
fn safe_suffix(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut start = text.len() - cap;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// S083 预览：头 2000 字节 + 外置文件路径与分段回读指引 + 尾 600 字节（含末行 exit=N）。
/// 纯函数，单测覆盖格式契约（路径出现、exit 行在末、字节数如实）。
fn large_output_preview(full: &str, path: &str) -> String {
    format!(
        "{}\n...[harness] Output truncated for context economy. The full {}-byte output was saved by the harness to `{}` inside this container. Read it back in segments when needed, e.g. `sed -n '1,200p' {}` or `grep -n PATTERN {}`. The final line below is the command's real exit status.\n{}",
        safe_prefix(full, LARGE_OUTPUT_PREVIEW_HEAD),
        full.len(),
        path,
        path,
        path,
        safe_suffix(full, LARGE_OUTPUT_PREVIEW_TAIL)
    )
}

/// 有损截断退路（S083 外置失败时）：头尾各留 cap/2，中段省略量如实标注。
fn lossy_truncate(text: &str, cap: usize) -> String {
    let keep = cap / 2;
    let head = safe_prefix(text, keep);
    let tail_part = safe_suffix(text, keep);
    format!(
        "{}\n...[output truncated: {} bytes elided. Re-run with a narrower command: pipe through head/tail/sed -n, grep for specific patterns, or redirect output to a file and read it in parts.]\n{}",
        head,
        text.len() - head.len() - tail_part.len(),
        tail_part
    )
}

/// 手写 base64（RFC 4648 标准字母表，带 padding）。不引入新依赖：
/// 分块大小是 3 的倍数时各块独立编码可拼接解码（LARGE_OUTPUT_CHUNK_BYTES 已保证）。
fn b64_encode(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TBL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TBL[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// bash 工具声明（ToolDecl JSON）。`workdir` 写进描述，与 exec 的 cd 前缀一致，
/// 避免"工具描述说 /app、实际在 /testbed"的目录矛盾（S0 零补丁缺陷嫌疑之一）。
/// fix2：描述补防错条款（mcode 式"把失败模式写进描述"）——新 shell 语义、
/// 非交互要求、超时行为、exit 行在末尾（首行留给命令输出，配合提交式完成判定）。
/// POC-2：入参加 `timeout_ms`（缺省 180s、上限 600s）——长测试套件可以显式要更久，
/// 不用再靠"命令内 timeout <secs>"去撞 harness 自己的硬上限。
/// fix4-align-r2：描述补离线声明（GLM 机制：工具描述随每次请求重发，是抗遗忘的
/// 最强通道；被墙代理会谎报 exit=0+空响应体，须把这一反馈形态如实写明）。
pub fn bash_tool_decl(workdir: &str) -> Value {
    json!({
        "name": "bash",
        "description": format!("Execute a bash command in the task environment (a Linux container). Every command runs in a fresh non-interactive shell with the working directory set to {workdir}; shell state (cd, exported variables) does not persist between commands. Interactive commands (editors, prompts) and commands that never exit are killed by a timeout — use non-interactive flags. The tool returns the command's output followed by a final `exit=<code>` line; a non-zero exit is reported to you as an error. Use it to inspect, modify, build and test. Each call is killed after {DEFAULT_BASH_TIMEOUT_SEC}s by default; pass timeout_ms (max {MAX_BASH_TIMEOUT_SEC}000) for long-running commands such as a full test suite — the harness cap is hard, so `timeout <secs>` inside the command cannot extend it; for very long jobs, redirect output to a file and read it back in parts. This container has NO usable internet access: pip/curl/wget/apt and git remote operations fail, hang, or return empty responses — sometimes with a misleading exit=0 and an empty body. Never retry a failed or empty network command; work with what is already in the container."),
        "parameters": {
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The bash command to execute."},
                "timeout_ms": {
                    "type": "integer",
                    "description": format!("Timeout for this command in milliseconds. Default {DEFAULT_BASH_TIMEOUT_SEC}000, maximum {MAX_BASH_TIMEOUT_SEC}000. Use a larger value for long test suites; the command is killed when the timeout expires.")
                }
            },
            "required": ["command"]
        },
        "effect": "external_once"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fix4-align2-tier0（第 0 档 S083）大输出外置 ----

    #[test]
    fn b64_encode_matches_known_vectors() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn b64_chunked_encoding_is_concatenable() {
        // 分块（3 的倍数）独立编码 = 整体编码：externalize_output 的分块写回依赖此性质。
        let data: Vec<u8> = (0..=255u8).cycle().take(72_000 * 2 + 3).collect();
        let whole = b64_encode(&data);
        let chunked: String = data
            .chunks(LARGE_OUTPUT_CHUNK_BYTES)
            .map(b64_encode)
            .collect();
        assert_eq!(whole, chunked);
    }

    #[test]
    fn large_output_preview_keeps_head_tail_and_exit_line() {
        let mut full = "HEAD".repeat(600); // 2400 B > 预览头 2000
        full.push('\n');
        full.push_str(&"m".repeat(20_000)); // 中段必然被省略
        full.push_str("\nexit=0");
        let preview = large_output_preview(&full, "/tmp/.af-outputs/x-1-0.txt");
        assert!(preview.starts_with(&safe_prefix(&full, LARGE_OUTPUT_PREVIEW_HEAD)));
        assert!(preview.contains("/tmp/.af-outputs/x-1-0.txt"));
        assert!(preview.contains(&format!("{}-byte output", full.len()))); // 全量字节数如实
        assert!(preview.ends_with("\nexit=0")); // rc 解析锚点在末行
        assert!(preview.len() < full.len());
    }

    #[test]
    fn safe_slices_respect_char_boundaries() {
        // 多字节字符（3 字节 '中'）跨界时不 panic 且不产生无效 UTF-8。
        let text = "中".repeat(10_000); // 30_000 B
        let p = safe_prefix(&text, 12_345);
        assert!(p.len() <= 12_345);
        let s = safe_suffix(&text, 7_777);
        assert!(s.len() <= 7_777);
        assert!(text.ends_with(s) && text.starts_with(p));
    }

    #[test]
    fn lossy_truncate_reports_elided_bytes() {
        let text = format!("{}\nexit=1", "x".repeat(30_000));
        let out = lossy_truncate(&text, 12_000);
        assert!(out.contains("bytes elided"));
        assert!(out.ends_with("\nexit=1"));
        assert!(out.len() < text.len());
    }

    /// 资源护栏常量必须与运营者校准口径一致（agent 6g/512、判分 4g/256、TB 2g/256）：
    /// 防后人误改把护栏改没或改歪（2026-10-21 anolis 无上限容器耗尽事故的固化）。
    /// 校准推导见常量定义处注释（125G×14 臂 + 判分串行 + 宿主/ollama 余量）。
    #[test]
    fn container_guardrail_constants_match_calibration() {
        assert_eq!(SWEV_AGENT_CONTAINER_MEM, "6g");
        assert_eq!(SWEV_AGENT_CONTAINER_PIDS, "512");
        assert_eq!(SWEV_EVAL_CONTAINER_MEM, "4g");
        assert_eq!(SWEV_EVAL_CONTAINER_PIDS, "256");
        assert_eq!(TB_CONTAINER_MEM, "2g");
        assert_eq!(TB_CONTAINER_PIDS, "256");
    }

    /// v8-g3：桥接脚本必须带 pipefail 前缀且原命令原样保留（候选③ 的构造合同）。
    #[test]
    fn bridge_inner_prefixes_pipefail_and_keeps_command() {
        let s = bridge_inner("/testbed", "python -m pytest 2>&1 | tail -20");
        assert_eq!(
            s,
            "set -o pipefail; cd /testbed && python -m pytest 2>&1 | tail -20"
        );
        let plain = bridge_inner("/app", "echo TASK_COMPLETE");
        assert!(plain.starts_with("set -o pipefail; cd /app && echo TASK_COMPLETE"));
    }

    /// v8-g3 语义合同（真 bash 实证，防回归）：pipefail 下管道取真实失败码；
    /// 同一管道无 pipefail 时被末端 0 掩蔽（= 修正前的旧行为）；非管道命令与
    /// 分号串联的 rc 语义不受影响（纯反馈面、无提前中断的凭据）。
    #[test]
    fn pipefail_reports_true_pipeline_failure() {
        let run = |script: &str| {
            let out = Command::new("bash")
                .arg("-c")
                .arg(script)
                .output()
                .expect("启动 bash");
            out.status.code().expect("bash 进程应有退出码")
        };
        // 修正后的桥接形态：真实失败（首段 exit 3）透传（bash -c 的退出码=末命令
        // 即该管道，与桥接 `__AF_RC=$?` 取内层 bash -lc 退出码同构）。
        assert_eq!(run("set -o pipefail; cd /tmp && sh -c 'exit 3' | cat"), 3);
        // 旧行为对照：无 pipefail 时末端 cat 的 0 掩蔽失败（修正前 exit=0 谎报的根源）。
        assert_eq!(run("cd /tmp && sh -c 'exit 3' | cat"), 0);
        // 非管道命令 rc 不变。
        assert_eq!(run("set -o pipefail; cd /tmp && sh -c 'exit 5'"), 5);
        // 分号串联仍取末段 rc（无提前中断）。
        assert_eq!(run("set -o pipefail; false; true"), 0);
        assert_eq!(run("set -o pipefail; true; false"), 1);
    }
}
