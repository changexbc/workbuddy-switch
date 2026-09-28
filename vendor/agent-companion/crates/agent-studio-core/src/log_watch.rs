//! 观察 CodeBuddy 客户端（国内版 IDE / VS Code 插件）的扩展宿主日志，取出它
//! **自己**的「危险命令」判定结果。
//!
//! 背景：客户端在弹出「包含危险命令，是否仍要运行？」之前会先写一行日志：
//!
//! ```text
//! [TerminalExecutor] [SafetyRule] Dangerous command detected: <命令>, reason: <原因>, riskLevel: <级别>
//! [TerminalExecutor] [beforeExecute] Permission decision: source=safety_rule_ask, allowed=true, needConfirm=true
//! ```
//!
//! 而它**不会**为此发 hook（`Notification`/`permission_prompt` 在客户端里是死代码），
//! 所以悬浮栏过去只能靠「工具开始后长时间没有反馈」去猜。这里改读它自己的判定：
//! 拿到命令文本后交给采集器去匹配刚收到的 PreToolUse，就能把「待确认」精确地补出来，
//! 不用等阈值、也不会把长命令误判成等人确认。
//!
//! 只读、增量：首次看到某个日志文件时从**文件末尾**开始，历史弹窗不会被回放。

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 客户端写日志时的固定前缀。
const MARK: &str = "[SafetyRule] Dangerous command detected:";
/// 一次 poll 允许读入的最大字节数，避免日志暴涨时卡住主循环。
const READ_LIMIT: u64 = 512 * 1024;
/// 重新扫描日志目录的间隔（tick 数）。
const DISCOVER_EVERY: u32 = 10;
/// 未被匹配的危险命令保留多久（毫秒）。
const PENDING_TTL_MS: i64 = 15_000;

/// 客户端判定为危险、需要人确认的一条命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DangerousCommand {
    pub command: String,
    pub reason: String,
    pub ts: i64,
}

/// 从一行日志里解析出危险命令；不是这种行就返回 `None`。
pub fn parse_dangerous(line: &str, ts: i64) -> Option<DangerousCommand> {
    let rest = line.split_once(MARK)?.1;
    let (body, level) = rest.rsplit_once(", riskLevel:").unwrap_or((rest, ""));
    let (command, reason) = body.rsplit_once(", reason:").unwrap_or((body, ""));
    let command = command.trim().to_string();
    if command.is_empty() {
        return None;
    }
    Some(DangerousCommand {
        command,
        reason: format!("{}{}", reason.trim(), if level.trim().is_empty() { String::new() } else { format!("（{}）", level.trim()) }),
        ts,
    })
}

/// 两个命令文本是否指向同一次调用：日志可能截断，所以按前缀互相包含判断。
pub fn same_command(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let head = |s: &str| s.chars().take(60).collect::<String>();
    a == b || head(&a) == head(&b) || a.starts_with(&head(&b)) || b.starts_with(&head(&a))
}

/// 客户端「等待用户确认」的一段区间（进入 / 解除）。
///
/// 依据是插件宿主 agent 日志里的**状态行**，而不是 `[SafetyRule] Dangerous command detected:`
/// —— 2026-09-27 实测：状态行是在**弹窗那一刻**写的（VS Code 宿主 `14:16:25.556`、CN IDE 宿主
/// `15:40:33.774`），而 `Dangerous command detected` 在 CN IDE 那边要等**点完确认**才写
/// （状态 `15:40:39.659` 已恢复 running，那行 `15:40:39.871` 才落盘）⇒ 按它触发就是
/// 「确认后才提示、随即一闪而过」。状态行里还带 conversationId，可直接挂到会话上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmWait {
    /// 客户端会话 id（日志写作 `Session <id>`；与 hub 会话 id 的后半段一致）。
    pub session_id: String,
    /// 事件时刻（毫秒）。
    pub ts: i64,
}

/// 观察到的一次等待状态变化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitEvent {
    /// `running -> waiting_user_input`：弹窗出现。
    Enter(ConfirmWait),
    /// `waiting_user_input -> running`：用户已处理（确认或取消）。
    Exit(ConfirmWait),
}

/// 一次增量扫描的结果。
#[derive(Default)]
pub struct LogScan {
    /// 客户端自己判定的危险命令（旧的「危险命令」通路；客户端在**确认之后**才写）。
    pub dangerous: Vec<DangerousCommand>,
    /// 等待状态变化（弹窗出现 / 用户已处理）。
    pub waits: Vec<WaitEvent>,
}

/// 从日志行解析「进入 / 解除等待」；不是状态行就返回 `None`。
///
/// 认 `[AgentSessionManager] Session <id> state: running → waiting_user_input`
/// （箭头有全角 `→` 与 `->` 两种写法）。只有 `MediaWatcherService` 那种不带会话 id 的
/// 状态行不认——挂不到会话上就没法提醒。
pub fn parse_wait_event(line: &str, ts: i64) -> Option<WaitEvent> {
    if !line.contains("state:") || !line.contains("waiting_user_input") {
        return None;
    }
    let session_id = session_of(line)?;
    let normalized = line.replace('→', "->");
    let wait = ConfirmWait { session_id, ts };
    if normalized.contains("state: running -> waiting_user_input") {
        return Some(WaitEvent::Enter(wait));
    }
    if normalized.contains("state: waiting_user_input -> running") {
        return Some(WaitEvent::Exit(wait));
    }
    None
}

/// 取 `Session <id> state:` 里的会话 id（只认 uuid 形态，避免把别的词当 id）。
fn session_of(line: &str) -> Option<String> {
    let id = line.split_once("Session ")?.1.split_whitespace().next()?;
    let id = id.trim_end_matches(':');
    let hex = |c: char| c.is_ascii_hexdigit();
    let plain = id.len() == 32 && id.chars().all(hex);
    let dashed = id.len() == 36 && id.chars().all(|c| hex(c) || c == '-');
    (plain || dashed).then(|| id.to_string())
}


struct Tail {
    path: PathBuf,
    offset: u64,
    carry: String,
}

struct Watcher {
    tails: Vec<Tail>,
    tick: u32,
    seen: VecDeque<DangerousCommand>,
}

/// 客户端日志观察器。按 tick 调用 [`ClientLogWatch::poll`]。
pub struct ClientLogWatch {
    roots: Vec<PathBuf>,
    watcher: Watcher,
}

impl ClientLogWatch {
    pub fn new(_home: &Path) -> Self {
        Self {
            roots: roots(_home),
            watcher: Watcher { tails: Vec::new(), tick: 0, seen: VecDeque::new() },
        }
    }

    /// 增量读取，返回**新出现**（尚未返回过）的危险命令与等待状态变化。
    pub fn poll(&mut self) -> LogScan {
        if self.watcher.tick % DISCOVER_EVERY == 0 || self.watcher.tails.is_empty() {
            self.refresh();
        }
        self.watcher.tick = self.watcher.tick.wrapping_add(1);
        self.read_new()
    }

    /// 丢弃过期的未匹配命令，避免长期占内存。
    pub fn prune(&mut self, now: i64) {
        self.watcher.seen.retain(|d| now - d.ts < PENDING_TTL_MS);
    }

    fn refresh(&mut self) {
        let mut found = Vec::new();
        for root in &self.roots {
            discover(root, &mut found, 0);
        }
        found.sort();
        found.dedup();
        for path in found {
            if self.watcher.tails.iter().any(|t| t.path == path) {
                continue;
            }
            let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.watcher.tails.push(Tail { path, offset, carry: String::new() });
        }
    }

    fn read_new(&mut self) -> LogScan {
        let now = crate::now();
        let mut scan = LogScan::default();
        for tail in &mut self.watcher.tails {
            let Ok(mut file) = std::fs::File::open(&tail.path) else { continue };
            let len = file.metadata().map(|m| m.len()).unwrap_or(tail.offset);
            if len < tail.offset {
                // 日志被轮转/截断，从头再来。
                tail.offset = 0;
                tail.carry.clear();
            }
            if len <= tail.offset {
                continue;
            }
            if file.seek(SeekFrom::Start(tail.offset)).is_err() {
                continue;
            }
            let want = (len - tail.offset).min(READ_LIMIT);
            let mut buf = vec![0u8; want as usize];
            let Ok(read) = file.read(&mut buf) else { continue };
            buf.truncate(read);
            tail.offset += read as u64;
            let chunk = tail.carry.clone() + &String::from_utf8_lossy(&buf);
            let mut lines = chunk.split('\n');
            tail.carry = lines.next_back().unwrap_or("").to_string();
            for line in lines {
                if let Some(found) = parse_dangerous(line, now) {
                    if !scan.dangerous.contains(&found) {
                        scan.dangerous.push(found.clone());
                    }
                }
                if let Some(wait) = parse_wait_event(line, now) {
                    if !scan.waits.contains(&wait) {
                        scan.waits.push(wait.clone());
                    }
                }
            }
        }
        for item in &scan.dangerous {
            self.watcher.seen.push_back(item.clone());
        }
        scan
    }
}

/// 各平台客户端日志根目录。
///
/// 两类根都要收：
/// - **插件宿主 agent 日志**（`<数据根>/CodeBuddyExtension/Logs/<宿主>/<日期>/<工作区>__<hash>.log`）：
///   「需要你允许」的信号**只**写在这里 —— 2026-09-27 实测：客户端在弹窗同一毫秒写下
///   `[SafetyRule] Dangerous command detected:` / `emit event: user_confirm_required` /
///   `请求用户确认: <callId>` / `running -> waiting_user_input`，而两个 exthost 日志树里
///   这类行是 **0 命中**。以前只盯 exthost 树 ⇒ 该通路从未触发过。
/// - **exthost 日志树**（`Code/logs`、`CodeBuddy CN/logs`、`CodeBuddy/logs`）：历史路径，保留以免回归。
fn roots(_home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let base = PathBuf::from(appdata);
            out.push(base.join("Code").join("logs"));
            out.push(base.join("CodeBuddy CN").join("logs"));
            out.push(base.join("CodeBuddy").join("logs"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(plugin_host_logs_root(&PathBuf::from(local)));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let base = _home.join("Library").join("Application Support");
        out.push(base.join("Code").join("logs"));
        out.push(base.join("CodeBuddy CN").join("logs"));
        out.push(base.join("CodeBuddy").join("logs"));
        out.push(plugin_host_logs_root(&base));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = _home.join(".config");
        out.push(base.join("Code").join("logs"));
        out.push(plugin_host_logs_root(&base));
    }
    out.retain(|p| p.is_dir());
    out
}

/// 插件宿主日志根：`<数据根>/CodeBuddyExtension/Logs`。
fn plugin_host_logs_root(base: &Path) -> PathBuf {
    base.join("CodeBuddyExtension").join("Logs")
}

/// 目录名是否形如 `2026-09-27`（插件宿主按日期分目录）。
///
/// 用日期形态限定插件宿主树里的文件，避免把 `Logs/` 下的杂项日志也读进来。
fn looks_like_date_dir(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
}

/// 递归找出客户端日志文件：扩展宿主的（路径里带 `coding-copilot`）与插件宿主 agent 日志。
fn discover(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            discover(&path, out, depth + 1);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let has_marker = path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().contains("coding-copilot"));
        // 插件宿主 agent 日志：`…/CodeBuddyExtension/Logs/<宿主>/<日期>/<工作区>__<hash>.log`，
        // 没有 `coding-copilot` 那一层，按「树名 + 日期目录」识别。
        let in_plugin_host = path
            .components()
            .any(|c| c.as_os_str() == "CodeBuddyExtension")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(looks_like_date_dir);
        if has_marker || in_plugin_host {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 插件宿主 agent 日志必须被发现：它是「需要你允许」信号的**唯一**来源
    /// （2026-09-27 实测：exthost 树 0 命中，只在
    /// `<数据根>/CodeBuddyExtension/Logs/VSCode/<日期>/<工作区>__<hash>.log` 里）。
    #[test]
    fn discovers_plugin_host_agent_logs() {
        let base = std::env::temp_dir().join(format!("wb-log-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let logs = plugin_host_logs_root(&base);
        let day = logs.join("VSCode").join("2026-09-27");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("newmes__ad40e5e5.log"), "").unwrap();
        // 非日期目录下的（`Logs/VSCode/other/x.log`）不收：避免把杂项日志读进来。
        let other = logs.join("VSCode").join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("misc.log"), "").unwrap();

        let mut found = Vec::new();
        discover(&logs, &mut found, 0);
        assert_eq!(
            found,
            vec![day.join("newmes__ad40e5e5.log")],
            "只认日期目录下的插件宿主 agent 日志"
        );

        assert!(looks_like_date_dir("2026-09-27"));
        assert!(!looks_like_date_dir("other"));
        assert!(!looks_like_date_dir("2026-9-27"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 状态行解析：进入 / 解除都要认，且必须带会话 id。
    ///
    /// 用的是本机实测原文（VS Code 宿主与 CN IDE 宿主各一条）。
    #[test]
    fn parses_wait_state_lines() {
        let enter = "[2026/9/27 14:16:25.556] [Info] [AgentSessionManager] Session 8d821770c2544202910fc023ae5f8e12 state: running → waiting_user_input";
        assert_eq!(
            parse_wait_event(enter, 1_789_490_585_556),
            Some(WaitEvent::Enter(ConfirmWait {
                session_id: "8d821770c2544202910fc023ae5f8e12".into(),
                ts: 1_789_490_585_556,
            }))
        );
        let exit = "[2026/9/27 15:40:39.659] [Info] [ToolService] Restored session state from waiting_user_input to RUNNING for conversation: d8231a1e06e549d1ab0baf63fbd92aaf";
        assert_eq!(parse_wait_event(exit, 7), None, "非状态行不认");

        let exit_state = "[2026/9/27 15:40:39.659] [Info] [AgentSessionManager] Session d8231a1e06e549d1ab0baf63fbd92aaf state: waiting_user_input → running";
        assert_eq!(
            parse_wait_event(exit_state, 7),
            Some(WaitEvent::Exit(ConfirmWait {
                session_id: "d8231a1e06e549d1ab0baf63fbd92aaf".into(),
                ts: 7,
            }))
        );
        // 不带会话 id 的汇总行不认（挂不到会话上就没法提醒）。
        let aggregate = "[2026/9/27 14:16:25.556] [Info] [MediaWatcherService] Agent state changed: running -> waiting_user_input, active sessions: 1";
        assert_eq!(parse_wait_event(aggregate, 1), None);
    }

    #[test]
    fn parses_dangerous_command_line() {
        let line = "2026-09-27 14:15:49.914 [info] [TerminalExecutor] [SafetyRule] Dangerous command detected: cd d:\\code; Stop-Process -Force, reason: 进程控制, riskLevel: high";
        let found = parse_dangerous(line, 123).expect("should parse");
        assert_eq!(found.command, "cd d:\\code; Stop-Process -Force");
        assert_eq!(found.reason, "进程控制（high）");
        assert_eq!(found.ts, 123);
    }

    #[test]
    fn ignores_other_lines() {
        assert!(parse_dangerous("2026-09-27 [info] [TerminalExecutor] 执行普通命令: echo hi", 1).is_none());
        assert!(parse_dangerous("[SafetyRule] Dangerous command detected:   , reason: x, riskLevel: low", 1).is_none());
    }

    #[test]
    fn matches_by_prefix_with_truncation() {
        let long = "cd d:\\File\\code\\workbuddy-switch; Get-Process wb-switch-rust,agent-studio-runtime -ErrorAction SilentlyContinue | Stop-Process -Force; if (Get-Process -Id 53456) { taskkill /PID 53456 /T /F }";
        let logged = "cd d:\\File\\code\\workbuddy-switch; Get-Process wb-switch-rust,agent-studio-runtime -ErrorAction SilentlyContinue | Stop-Process -Force; if (Get-Process -Id 53";
        assert!(same_command(long, logged));
        assert!(!same_command("echo hi", "echo bye"));
        assert!(!same_command("", "echo hi"));
    }
}
