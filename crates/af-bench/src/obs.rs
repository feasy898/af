//! fix4-observability：事件观测存储（独立于 af-trace 的 append-only 事件流）。
//!
//! 布局（AF_OBS_ROOT 环境变量，缺省 工厂/工具状态备份/observability/）：
//!   runs/<line>/<arm>/<run_id>/events.jsonl      每 run 事件流（信封+payload，seq 单调）
//!   index/runs-<YYYYMM>.jsonl                     每 run 终态索引行
//!   index/batch-<batch_id>.manifest.json          批次清单（Merkle 根，第三方可重算）
//!
//! 纪律：
//! - D-24：观测数据绝不落 tmpfs——根目录解析后落在 /tmp、/dev/shm、/run 或 fstype=tmpfs 即拒绝启动；
//! - D-14/D-22：路径段白名单 `^[A-Za-z0-9._-]{1,64}$` 且禁 `.`/`..`，不合格立即报错，不静默改写；
//! - 密钥零明文：本模块只落指纹与布尔，绝不落密钥值。

use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 判分链常量（与 judge.rs 同源；判分环境指纹的对象）。
pub(crate) const JUDGE_PY: &str = "/home/pan-ding/workspace/agent黑灯工厂/tools/swev-judge.py";
pub(crate) const JUDGE_PY_BIN: &str = "/home/pan-ding/swev-venv/bin/python";

/// AF_OBS_ROOT 缺省值：工厂观测目录（D-24：绝不入 /tmp|tmpfs|/dev/shm|/run）。
pub(crate) const DEFAULT_OBS_ROOT: &str =
    "/home/pan-ding/workspace/agent黑灯工厂/工具状态备份/observability";

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn sha256_bytes(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex(&digest)
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|err| format!("读 {} 失败: {err}", path.display()))?;
    Ok(sha256_bytes(&data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 路径段消毒：白名单 `^[A-Za-z0-9._-]{1,64}$` 且不得为 `.`/`..`（D-14/D-22 前科防线）。
/// 不合格立即报错，不做静默改写。
pub fn sanitize_segment(name: &str, what: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "{what} 段 {name:?} 不合白名单 ^[A-Za-z0-9._-]{{1,64}}$（禁 . 与 ..），拒绝写盘"
        ))
    }
}

/// 解析并校验观测根目录：环境变量覆盖值也不得落在内存盘（D-13 硬编码教训的环境变量化）。
pub fn resolve_root() -> Result<PathBuf, String> {
    let raw = std::env::var("AF_OBS_ROOT").unwrap_or_else(|_| DEFAULT_OBS_ROOT.to_string());
    let path = PathBuf::from(&raw);
    std::fs::create_dir_all(&path).map_err(|err| format!("建观测根 {raw} 失败: {err}"))?;
    let real = std::fs::canonicalize(&path)
        .map_err(|err| format!("realpath {raw} 失败: {err}"))?
        .to_string_lossy()
        .to_string();
    let on_memory_disk = real == "/tmp"
        || real.starts_with("/tmp/")
        || real == "/dev/shm"
        || real.starts_with("/dev/shm/")
        || real == "/run"
        || real.starts_with("/run/");
    if on_memory_disk || fstype_is_tmpfs(&real)? {
        return Err(format!(
            "观测根 {real} 落在内存盘（D-24 红线），拒绝启动；用 AF_OBS_ROOT 指向持久盘"
        ));
    }
    Ok(path)
}

fn fstype_is_tmpfs(real: &str) -> Result<bool, String> {
    let out = Command::new("df")
        .args(["--output=fstype", real])
        .output()
        .map_err(|err| format!("df 启动失败: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "df {} 失败: {}",
            real,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().last().unwrap_or("").trim();
    Ok(last == "tmpfs" || last == "devtmpfs")
}

/// 宿主感知 docker 命令（host=local 直跑，否则经 ssh 桥接，与 swev.rs/tb.rs 同款转义）。
fn docker_cmd(host: &str, args: &[&str]) -> Result<String, String> {
    let output = if host == "local" {
        Command::new("docker").args(args).output()
    } else {
        let script = format!("docker {}", args.join(" "));
        let quoted = format!("bash -c '{}'", script.replace('\'', "'\\''"));
        Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(host)
            .arg(&quoted)
            .output()
    }
    .map_err(|err| format!("docker 桥接启动失败: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker {:?} 退出 {}: {}",
            args,
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// 镜像内容寻址 digest：RepoDigests[0] 优先；本地构建无 RepoDigest 时落 .Id 并标注来源
/// （红队①：:latest 标签不钉版本，四 fp_* 不变的漏洞由此封堵）。
pub fn image_digest(host: &str, image_ref: &str) -> Result<(String, &'static str), String> {
    let out = docker_cmd(host, &["image", "inspect", image_ref])?;
    let value: Value = serde_json::from_str(out.trim())
        .map_err(|err| format!("docker image inspect 输出不是 JSON: {err}"))?;
    let entry = value
        .as_array()
        .and_then(|a| a.first())
        .ok_or("docker image inspect 返回空数组")?;
    if let Some(digest) = entry
        .get("RepoDigests")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok((digest.to_string(), "repodigest"));
    }
    let id = entry
        .get("Id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("docker image inspect 缺 RepoDigests 与 Id")?;
    Ok((id.to_string(), "id"))
}

/// 数据根所在文件系统剩余字节（磁盘门 D-15 的观测输入）。
pub fn disk_free_bytes(path: &Path) -> Result<u64, String> {
    let out = Command::new("df")
        .args(["-B1", "--output=avail", &path.to_string_lossy()])
        .output()
        .map_err(|err| format!("df 启动失败: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "df {} 失败: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .last()
        .and_then(|line| line.trim().parse::<u64>().ok())
        .ok_or_else(|| format!("df {} 输出无法解析: {text:?}", path.display()))
}

/// docker stats 单次采样的 MemUsage 首段（如 `45.2MiB / 6GiB`）换算 KB。
fn mem_usage_kb(host: &str, container: &str) -> Result<u64, String> {
    let out = docker_cmd(
        host,
        &[
            "stats",
            "--no-stream",
            "--format",
            "{{.MemUsage}}",
            container,
        ],
    )?;
    let first = out
        .lines()
        .next()
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .trim();
    parse_size_to_kb(first).ok_or_else(|| format!("docker stats 输出无法解析: {first:?}"))
}

fn parse_size_to_kb(text: &str) -> Option<u64> {
    let (num, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len()));
    let value: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim() {
        "B" => 1.0 / 1024.0,
        "KiB" | "kB" | "k" => 1.0,
        "MiB" => 1024.0,
        "GiB" => 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * mult) as u64)
}

/// 判分 venv 包清单指纹：`pip freeze` 全量输出按行排序后 sha256
/// （红队②：venv 升级改 f2p/p2p 解析而无指纹变化的漏洞由此封堵）。
pub fn judge_env_fingerprint() -> Result<(String, String), String> {
    let out = Command::new(JUDGE_PY_BIN)
        .args(["-m", "pip", "freeze", "--all"])
        .output()
        .map_err(|err| format!("pip freeze 启动失败: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "pip freeze 退出 {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    let fingerprint = sha256_bytes(lines.join("\n").as_bytes());
    Ok((fingerprint, format!("pip-freeze-{}-lines", lines.len())))
}

/// swebench 库版本号（venv 自报，与 fp_judge_env_sha256 互证）。
pub fn swebench_version() -> Result<String, String> {
    let out = Command::new(JUDGE_PY_BIN)
        .arg("-c")
        .arg("import importlib.metadata as m; print(m.version('swebench'))")
        .output()
        .map_err(|err| format!("venv python 启动失败: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "swebench 版本自报失败: {}",
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// afbench 可执行字节指纹（S6 指纹集"判分二进制 sha256"成员）。
pub fn bench_binary_sha256() -> Result<String, String> {
    let data = std::fs::read("/proc/self/exe").map_err(|err| format!("读 /proc/self/exe: {err}"))?;
    Ok(sha256_bytes(&data))
}

/// af 仓库 HEAD sha（只读）。AF_OBS_GIT_REV 可覆盖；工作树不在时落 "unknown"。
pub fn harness_git_rev() -> String {
    if let Ok(rev) = std::env::var("AF_OBS_GIT_REV") {
        let rev = rev.trim().to_string();
        if !rev.is_empty() {
            return rev;
        }
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    if let Ok(out) = Command::new("git").args(["-C", manifest, "rev-parse", "HEAD"]).output() {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
    }
    "unknown".into()
}

/// 一个 run 的事件流写入端：append-only JSONL，seq 从 0 单调，行=信封+payload。
pub struct RunSink {
    file: Mutex<std::fs::File>,
    seq: AtomicU64,
    errors: Mutex<Vec<String>>,
    pub path: PathBuf,
    pub run_id: String,
}

impl RunSink {
    /// 创建 run 目录并打开 events.jsonl。目录已存在且有事件时按 `-x2` 递增避让（不覆写证据）。
    pub fn create(root: &Path, line: &str, arm: &str, run_id: &str) -> Result<Self, String> {
        let line = sanitize_segment(line, "line")?;
        let arm = sanitize_segment(arm, "arm")?;
        let base = sanitize_segment(run_id, "run_id")?;
        let runs_root = root.join("runs").join(&line).join(&arm);
        std::fs::create_dir_all(&runs_root).map_err(|err| format!("建 {} 失败: {err}", runs_root.display()))?;
        let mut chosen = base.clone();
        let mut dir = runs_root.join(&chosen);
        let mut n = 1u32;
        while dir.join("events.jsonl").exists() {
            n += 1;
            chosen = format!("{base}-x{n}");
            dir = runs_root.join(&chosen);
        }
        std::fs::create_dir_all(&dir).map_err(|err| format!("建 {} 失败: {err}", dir.display()))?;
        let path = dir.join("events.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| format!("开 {} 失败: {err}", path.display()))?;
        Ok(Self {
            file: Mutex::new(file),
            seq: AtomicU64::new(0),
            errors: Mutex::new(Vec::new()),
            path,
            run_id: chosen,
        })
    }

    /// 写一条事件并 fsync。失败记入 errors（不 panic、不中断跑测），终局统一上报。
    pub fn write(&self, kind: &str, critical: bool, payload: &Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let line = json!({
            "ts_ms": now_ms(),
            "seq": seq,
            "level": if critical { "critical" } else { "normal" },
            "kind": kind,
            "payload": payload,
        });
        let mut text = line.to_string();
        text.push('\n');
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = file
            .write_all(text.as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_data())
        {
            self.errors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("写事件 seq={seq} 失败: {err}"));
        }
    }

    pub fn error_count(&self) -> usize {
        self.errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// 已分配 seq 数（= 事件条数口径；写失败的事件也占 seq，索引行与文件实况的
    /// 差异由 checker 以 events_sha256/torn_lines 识破——这是特性不是缺陷）。
    pub fn count(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    /// 终态统计（索引行材料）：event_count=分配的 seq 数；events_sha256/torn_lines 读文件实算。
    pub fn finalize_stats(&self) -> Result<(u64, String, u64), String> {
        let data = std::fs::read(&self.path)
            .map_err(|err| format!("读 {} 失败: {err}", self.path.display()))?;
        let events_sha256 = sha256_bytes(&data);
        let torn_lines = count_torn_lines(&data);
        let final_seq = if self.seq.load(Ordering::SeqCst) == 0 {
            0
        } else {
            self.seq.load(Ordering::SeqCst) - 1
        };
        Ok((final_seq, events_sha256, torn_lines))
    }
}

/// 撕裂行计数：末行无换行、或任何解析失败的行（读侧容忍并告警的口径，与 checker 一致）。
pub fn count_torn_lines(data: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(data);
    let mut torn = 0u64;
    let mut saw_error = false;
    for (idx, line) in text.split('\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        if serde_json::from_str::<Value>(line).is_err() {
            torn += 1;
            saw_error = true;
            let _ = idx;
        }
    }
    if saw_error {
        return torn;
    }
    // 无解析失败时，末行缺换行也算撕裂。
    if !data.is_empty() && !data.ends_with(b"\n") {
        torn += 1;
    }
    torn
}

/// RFC 6962 风格 Merkle 根：叶 = SHA256(0x00 || hex 串 ASCII 字节)，
/// 内部节点 = SHA256(0x01 || left || right)，奇数末节点提升，空树 = SHA256("")。
/// 输入为各 run 的 events_sha256 十六进制串按字典序排序。第三方可重算。
pub fn merkle_root_sorted_hex(hashes: &[String]) -> String {
    let mut leaves: Vec<[u8; 32]> = hashes
        .iter()
        .map(|h| {
            let mut buf = Vec::with_capacity(1 + h.len());
            buf.push(0x00);
            buf.extend_from_slice(h.as_bytes());
            let out = Sha256::digest(&buf);
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&out);
            arr
        })
        .collect();
    if leaves.is_empty() {
        return sha256_bytes(b"");
    }
    while leaves.len() > 1 {
        let mut next = Vec::with_capacity((leaves.len() + 1) / 2);
        let mut i = 0;
        while i < leaves.len() {
            if i + 1 == leaves.len() {
                next.push(leaves[i]); // 奇数末节点提升
            } else {
                let mut buf = [0u8; 65];
                buf[0] = 0x01;
                buf[1..33].copy_from_slice(&leaves[i]);
                buf[33..65].copy_from_slice(&leaves[i + 1]);
                let out = Sha256::digest(buf);
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&out);
                next.push(arr);
            }
            i += 2;
        }
        leaves = next;
    }
    hex(&leaves[0])
}

pub const MERKLE_ALGO: &str = "rfc6962-style: leaf=sha256(0x00||ascii(events_sha256)), node=sha256(0x01||l||r), odd-promoted, over lexicographically sorted events_sha256 list";

/// 批次清单落盘：run_ids、计数互证、Merkle 根、创建时刻。
pub fn write_batch_manifest(
    root: &Path,
    batch_id: &str,
    run_ids: &[String],
    event_total: u64,
    status_counts: &Value,
) -> Result<PathBuf, String> {
    let mut sorted_hashes: Vec<(String, String)> = Vec::new(); // (run_id, events_sha256)
    let index_dir = root.join("index");
    std::fs::create_dir_all(&index_dir).map_err(|err| format!("建 index 失败: {err}"))?;
    // 从全部月度索引文件里收集本批 run 的 events_sha256（读侧口径与 checker 一致）。
    let want: std::collections::BTreeSet<&str> =
        run_ids.iter().map(String::as_str).collect();
    let mut found: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let entries = std::fs::read_dir(&index_dir).map_err(|err| format!("读 index 失败: {err}"))?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("runs-") && n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|err| format!("读 {} 失败: {err}", file.display()))?;
        for row in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(v) = serde_json::from_str::<Value>(row) {
                if let (Some(rid), Some(sha)) = (
                    v.get("run_id").and_then(Value::as_str),
                    v.get("events_sha256").and_then(Value::as_str),
                ) {
                    if want.contains(rid) {
                        found.insert(rid.to_string(), sha.to_string());
                    }
                }
            }
        }
    }
    for rid in run_ids {
        match found.get(rid) {
            Some(sha) => sorted_hashes.push((rid.clone(), sha.clone())),
            None => return Err(format!("批次清单：索引里找不到 run {rid} 的 events_sha256")),
        }
    }
    sorted_hashes.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let hashes: Vec<String> = sorted_hashes.into_iter().map(|(_, h)| h).collect();
    let manifest = json!({
        "batch_id": batch_id,
        "created_ts": now_ms(),
        "run_count": run_ids.len(),
        "run_ids": run_ids,
        "event_count_total": event_total,
        "status_counts": status_counts,
        "merkle_root": merkle_root_sorted_hex(&hashes),
        "merkle_algo": MERKLE_ALGO,
    });
    let path = index_dir.join(format!("batch-{batch_id}.manifest.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap_or_default())
        .map_err(|err| format!("写 {} 失败: {err}", path.display()))?;
    Ok(path)
}

/// run 终态索引行落盘：index/runs-<YYYYMM>.jsonl 追加一行。
#[allow(clippy::too_many_arguments)]
pub fn append_index_row(
    root: &Path,
    run_id: &str,
    path: &Path,
    ts_started: u64,
    ts_finished: u64,
    status: &str,
    event_count: u64,
    final_seq: u64,
    events_sha256: &str,
    torn_lines: u64,
    fp_image_digest: &str,
) -> Result<PathBuf, String> {
    let month = ts_finished / 1000;
    // YYYYMM 从 ts_finished 推（civil 算法，免引时间库）。
    let days = month / 86_400;
    let (y, m) = civil_from_days(days as i64);
    let index_dir = root.join("index");
    std::fs::create_dir_all(&index_dir).map_err(|err| format!("建 index 失败: {err}"))?;
    let idx_path = index_dir.join(format!("runs-{y:04}{m:02}.jsonl"));
    let row = json!({
        "run_id": run_id,
        "path": path.to_string_lossy(),
        "ts_started": ts_started,
        "ts_finished": ts_finished,
        "status": status,
        "event_count": event_count,
        "final_seq": final_seq,
        "events_sha256": events_sha256,
        "torn_lines": torn_lines,
        "fp_image_digest": fp_image_digest,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&idx_path)
        .map_err(|err| format!("开 {} 失败: {err}", idx_path.display()))?;
    file.write_all(format!("{row}\n").as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_data())
        .map_err(|err| format!("写 {} 失败: {err}", idx_path.display()))?;
    Ok(idx_path)
}

/// 天数 → (年, 月)（Howard Hinnant civil_from_days，公历）。
fn civil_from_days(z: i64) -> (i64, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let _d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m)
}

/// 周期资源采样器：≥60s 间隔采 disk_free + 容器内存（D-15/D-26 闭环输入）。
/// run 结束前由主线程强制补采一次（stop 后 join，主线程写末次样本）。
pub struct ResourceSampler {
    stop: Arc<AtomicBool>,
    mem_peak_kb: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ResourceSampler {
    pub fn start(
        sink: Arc<RunSink>,
        run_id: String,
        host: String,
        container: String,
        disk_path: PathBuf,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mem_peak_kb = Arc::new(AtomicU64::new(0));
        let stop2 = Arc::clone(&stop);
        let peak2 = Arc::clone(&mem_peak_kb);
        let sink2 = Arc::clone(&sink);
        let run_id2 = run_id.clone();
        let handle = std::thread::Builder::new()
            .name("af-obs-sampler".into())
            .spawn(move || {
                while !stop2.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    sample_once(
                        &sink2,
                        &run_id2,
                        &host,
                        &container,
                        &disk_path,
                        &peak2,
                        "periodic",
                    );
                }
            })
            .ok();
        Self {
            stop,
            mem_peak_kb,
            handle,
        }
    }

    /// 停线程并强制补采一次（run.finished 前的最终样本；内存峰值取全程最大）。
    pub fn stop_and_final_sample(
        &mut self,
        sink: &Arc<RunSink>,
        run_id: &str,
        host: &str,
        container: &str,
        disk_path: &Path,
    ) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        sample_once(
            sink,
            run_id,
            host,
            container,
            disk_path,
            &self.mem_peak_kb,
            "final",
        );
    }
}

fn sample_once(
    sink: &Arc<RunSink>,
    run_id: &str,
    host: &str,
    container: &str,
    disk_path: &Path,
    peak: &AtomicU64,
    phase: &str,
) {
    let disk_free = disk_free_bytes(disk_path).ok();
    let mem_kb = mem_usage_kb(host, container).ok();
    if let Some(kb) = mem_kb {
        peak.fetch_max(kb, Ordering::SeqCst);
    }
    let payload = json!({
        "run_id": run_id,
        "disk_free_bytes": disk_free,
        "disk_path": disk_path.to_string_lossy(),
        // 峰值口径：容器内存采样全程最大值（docker stats --no-stream 的当前值取 max）。
        "container_mem_peak_kb": if peak.load(Ordering::SeqCst) == 0 { None } else { Some(peak.load(Ordering::SeqCst)) },
        "phase": phase,
    });
    sink.write("harness.resource", false, &payload);
}

/// 一个 run 的观测编排：事件流 + 采样器 + 镜像 digest + 索引行落盘。
pub struct RunObs {
    pub sink: Arc<RunSink>,
    pub ts_started: u64,
    host: String,
    agent_container: String,
    root: PathBuf,
    fp_image_digest: String,
    sampler: Option<ResourceSampler>,
}

impl RunObs {
    /// 阶段一：建事件流（可能因避让改名的最终 run_id 在 sink.run_id），此时还未写任何事件。
    pub fn begin(
        root: &Path,
        line: &str,
        arm: &str,
        run_id: &str,
        host: &str,
        agent_container: &str,
    ) -> Result<Self, String> {
        let sink = Arc::new(RunSink::create(root, line, arm, run_id)?);
        Ok(Self {
            sink,
            ts_started: now_ms(),
            host: host.to_string(),
            agent_container: agent_container.to_string(),
            root: root.to_path_buf(),
            fp_image_digest: String::new(),
            sampler: None,
        })
    }

    pub fn set_image_digest(&mut self, digest: String) {
        self.fp_image_digest = digest;
    }

    /// 阶段二：写 harness.run_started（事件流首行，seq=0）并启动周期采样器。
    pub fn emit_run_started(&mut self, payload: &Value) {
        self.sink.write("harness.run_started", false, payload);
        self.sampler = Some(ResourceSampler::start(
            Arc::clone(&self.sink),
            self.sink.run_id.clone(),
            self.host.clone(),
            self.agent_container.clone(),
            self.root.clone(),
        ));
    }

    /// 事件旁观钩子（Launch.obs）：harness 事件原样镜像进事件流。
    pub fn obs_hook(&self) -> af_harness::ObsHook {
        let sink = Arc::clone(&self.sink);
        Arc::new(move |kind: &str, critical: bool, payload: &Value| {
            sink.write(kind, critical, payload);
        })
    }

    /// 收尾：停采样器并强制补采 → 写 harness.run_finished → 落索引行。
    /// ts_finished 由本函数取一次钟并**同时**写进事件与索引行（单一时间源；
    /// 曾因两处各取一次钟被 checker 的互校抓出不一致——这是互校的设计目的）。
    /// 返回本 sink 累积的写错误数。
    pub fn finish(&mut self, status: &str, run_finished_payload: &mut Value) -> Result<usize, String> {
        if let Some(mut sampler) = self.sampler.take() {
            sampler.stop_and_final_sample(
                &self.sink,
                &self.sink.run_id,
                &self.host,
                &self.agent_container,
                &self.root.clone(),
            );
        }
        let ts_finished = now_ms();
        if let Some(obj) = run_finished_payload.as_object_mut() {
            obj.insert("ts_finished".into(), json!(ts_finished));
        }
        self.sink.write("harness.run_finished", false, run_finished_payload);
        let errors = self.sink.error_count();
        self.append_index_row(status, ts_finished)?;
        Ok(errors)
    }

    /// run 终态索引行：index/runs-<YYYYMM>.jsonl 追加（final_seq+events_sha256 使静默截断/篡改可检）。
    fn append_index_row(&self, status: &str, ts_finished: u64) -> Result<(), String> {
        let (final_seq, events_sha256, torn_lines) = self.sink.finalize_stats()?;
        append_index_row(
            &self.root,
            &self.sink.run_id,
            &self.sink.path,
            self.ts_started,
            ts_finished,
            status,
            self.sink.count(),
            final_seq,
            &events_sha256,
            torn_lines,
            &self.fp_image_digest,
        )
        .map(|_| ())
    }
}
