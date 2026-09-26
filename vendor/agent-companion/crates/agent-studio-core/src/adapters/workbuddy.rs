// WorkBuddy is event-driven via Claude Code-compatible command hooks.
// Permissions stay in WorkBuddy's native GUI; this adapter never gates them.
// Sandbox approvals do not emit any hook event (see README known limits), so a
// best-effort watcher below detects them from WorkBuddy's own run logs.
use super::*;
use crate::{content, merge, question, question_tool, questions};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const WORKBUDDY_HOOK_EVENTS: [&str; 10] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Stop",
    "Notification",
    "PreCompact",
];

// ---- 沙箱审批日志观察 ----
// 沙箱类审批（敏感凭证/沙箱写/HTTP 拦截/批量删除）不发送任何 hook 事件，
// 但会写进 WorkBuddy 的会话级运行日志。这里做增量扫描，把弹框/处理事件
// 转成 hub 事件（wait / resolve），使悬浮窗显示「待确认」。
// 只匹配审批行，其余内容读入即弃；不读取会话文件、数据库或 transcript。

// 扫描节流：WorkBuddy 会话日志本身有 10–25 秒的应用层缓冲（实测），
// 端到端延迟由它支配；本侧节流放宽到 10 秒以降低目录扫描开销。
const LOG_SCAN_INTERVAL_MS: i64 = 10_000;
const LOG_ACTIVE_WINDOW_MS: i64 = 10 * 60_000;

#[derive(Debug, PartialEq)]
pub enum ApprovalEvent {
    Request {
        tool: String,
        call_id: String,
        session: String,
    },
    Settled {
        call_id: String,
    },
}

/// 解析一行 WorkBuddy 运行日志；不是审批行则返回 None（纯函数，便于测试）。
/// 超时行（Sandbox approval timed out）不解析：超时后工具以失败结束，
/// 真实的 PostToolUseFailure hook 会兜底清理 pending。
pub fn parse_approval_line(line: &str) -> Option<ApprovalEvent> {
    if let Some(rest) = line
        .split_once("[enqueueSandboxApproval] Enqueued sandbox approval: ")
        .map(|(_, rest)| rest)
    {
        let tool = field(rest, "tool=")?;
        let call_id = field(rest, ", id=")?;
        let session = field(rest, ", session=")?;
        if tool.is_empty() || call_id.is_empty() || session.is_empty() {
            return None;
        }
        return Some(ApprovalEvent::Request {
            tool,
            call_id,
            session,
        });
    }
    for marker in [
        "[Approve] User approved tool: ",
        "[Reject] User rejected tool: ",
    ] {
        if let Some(rest) = line.split_once(marker).map(|(_, rest)| rest) {
            let (tool, tail) = rest.split_once(", id: ")?;
            let call_id = tail.split(',').next().unwrap_or("").trim().to_owned();
            if tool.trim().is_empty() || call_id.is_empty() {
                return None;
            }
            return Some(ApprovalEvent::Settled { call_id });
        }
    }
    None
}

fn field(rest: &str, key: &str) -> Option<String> {
    Some(rest.split_once(key)?.1.split(',').next()?.trim().to_owned())
}

/// 增量扫描状态：文件 offset、审批 callId→会话 映射、节流与首次扫描标记。
#[derive(Default)]
pub struct LogWatch {
    pub offsets: HashMap<PathBuf, u64>,
    pub call_sessions: HashMap<String, String>,
    pub primed: bool,
    pub last_scan: i64,
}

fn collect_log_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&path) {
                for entry in inner.flatten() {
                    let file = entry.path();
                    if is_log_file(&file) {
                        out.push(file);
                    }
                }
            }
        } else if is_log_file(&path) {
            out.push(path);
        }
    }
}

fn is_log_file(path: &Path) -> bool {
    path.is_file() && path.extension().map(|e| e == "log").unwrap_or(false)
}

fn wait_text(p: &Value) -> String {
    let message = content(&p["message"]);
    if !message.trim().is_empty() {
        return message;
    }
    let title = p["title"].as_str().map(str::trim).unwrap_or("");
    if !title.is_empty() {
        return title.to_owned();
    }
    "需要你确认".into()
}

fn confirmation_notification(p: &Value) -> bool {
    let kind = p["notification_type"]
        .as_str()
        .or(p["notificationType"].as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if kind == "auth_success" {
        return false;
    }
    if kind == "idle_prompt"
        || kind.contains("permission")
        || kind.contains("confirm")
        || kind.contains("approval")
        || kind.contains("credential")
    {
        return true;
    }
    let text = format!(
        "{} {}",
        content(&p["message"]),
        p["title"].as_str().unwrap_or("")
    );
    text.contains('?')
        || text.contains('？')
        || text.contains("确认")
        || text.contains("Allow ")
        || text.contains("Deny ")
        || text.contains("允许")
        || text.contains("拒绝")
        || text.to_ascii_lowercase().contains("credential")
        || text.contains("凭证")
}

fn settings_json(dir: PathBuf) -> PathBuf {
    dir.join("settings.json")
}

fn looks_like_workbuddy_home(dir: &Path) -> bool {
    dir.is_dir() && (dir.join("settings.json").exists() || dir.join("workbuddy.db").exists())
}

/// International WorkBuddy AI uses `~/.workbuddy-ai`; the China desktop app uses `~/.workbuddy`.
/// Both are live products, so hooks are installed independently when that edition is present.
pub fn workbuddy_settings_files(home: &Path, custom: &str) -> Vec<PathBuf> {
    if !custom.is_empty() {
        let dir = if let Some(rest) = custom.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(custom)
        };
        return dir.is_dir().then(|| vec![settings_json(dir)]).unwrap_or_default();
    }
    let mut files = Vec::new();
    let international = home.join(".workbuddy-ai");
    if international.is_dir() || international.join("settings.json").exists() {
        files.push(settings_json(international));
    }
    let domestic = home.join(".workbuddy");
    // A bare `~/.workbuddy` may only hold toolchain binaries for the international app.
    if looks_like_workbuddy_home(&domestic) {
        files.push(settings_json(domestic));
    }
    files
}

pub fn workbuddy_edition(settings_file: &Path) -> &'static str {
    match settings_file.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        Some(".workbuddy-ai") => "international",
        _ => "domestic",
    }
}

pub fn is_workbuddy_hook_command(command: &str) -> bool {
    command.contains("workbuddy-status.py")
        || command.contains("astra-office-workbuddy")
        || (command.contains("agent-studio-runtime")
            && command.contains(" hook")
            && command.contains("--source workbuddy"))
}

pub fn merge_workbuddy_hooks(mut doc: Value, command: &str) -> Result<Value, String> {
    if !doc.is_object() {
        return Err("现有 WorkBuddy Hook 配置无效".into());
    }
    if doc["hooks"].is_null() {
        doc["hooks"] = json!({});
    }
    if !doc["hooks"].is_object() {
        return Err("现有 WorkBuddy Hook 配置无效".into());
    }
    for event in WORKBUDDY_HOOK_EVENTS {
        let mut groups = doc["hooks"][event].as_array().cloned().unwrap_or_default();
        for group in &mut groups {
            if let Some(hooks) = group["hooks"].as_array_mut() {
                hooks.retain(|h| !is_workbuddy_hook_command(&text(&h["command"])));
            } else if group.get("command").is_some()
                && is_workbuddy_hook_command(&text(&group["command"]))
            {
                *group = json!({"hooks": []});
            }
        }
        groups.retain(|g| {
            if g["hooks"].is_array() {
                !g["hooks"].as_array().unwrap().is_empty()
            } else {
                g.get("command")
                    .map(|c| !is_workbuddy_hook_command(&text(c)))
                    .unwrap_or(true)
            }
        });
        let mut h =
            json!({"type":"command","command":command,"timeout":3,"statusMessage":"Agent Studio"});
        if event != "SessionEnd" {
            h["async"] = json!(true);
        }
        groups.push(json!({"matcher":"","hooks":[h]}));
        doc["hooks"][event] = json!(groups);
    }
    Ok(doc)
}

impl Collector {
    pub fn ingest_workbuddy_hook(&mut self, p: &Value) -> bool {
        if self.settings["sources"]["workbuddy"]["enabled"] != true {
            return false;
        }
        let get = |a: &str, b: &str| p[a].as_str().or(p[b].as_str()).unwrap_or("").to_owned();
        let sid = get("session_id", "sessionId");
        let event = get("hook_event_name", "hookEventName");
        if sid.is_empty()
            || !matches!(
                event.as_str(),
                "SessionStart"
                    | "SessionEnd"
                    | "UserPromptSubmit"
                    | "PreToolUse"
                    | "PostToolUse"
                    | "PostToolUseFailure"
                    | "PermissionRequest"
                    | "Stop"
                    | "Notification"
                    | "PreCompact"
                    | "Interrupt"
            )
        {
            return false;
        }
        let key = format!("workbuddy:{sid}");
        if self.closed_monitor_sessions.contains(&key) {
            if matches!(event.as_str(), "Stop" | "SessionEnd" | "Interrupt") { return false; }
            if matches!(event.as_str(), "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PermissionRequest") {
                self.closed_monitor_sessions.remove(&key);
            }
        }
        let ts = p["timestamp"]
            .as_i64()
            .filter(|n| *n > 0)
            .unwrap_or_else(now);
        let previous = self.workbuddy_live.get(&sid).cloned();
        let mut state = previous
            .clone()
            .unwrap_or(json!({"roundId":"","cwd":"","calls":{}}));
        let turn = get("turn_id", "turnId");
        let begins = matches!(event.as_str(), "SessionStart" | "UserPromptSubmit");
        let round = if !turn.is_empty() {
            turn
        } else if begins {
            format!("turn:{ts}")
        } else {
            state["roundId"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or(format!("turn:{ts}"))
        };
        if previous.is_some() && state["roundId"] != round && !begins {
            return false;
        }
        if previous.is_none() || state["roundId"] != round {
            state["calls"] = json!({});
        }
        state["roundId"] = json!(round);
        if let Some(cwd) = p["cwd"].as_str().filter(|s| !s.is_empty()) {
            state["cwd"] = json!(cwd);
        }
        let edition = get("agent_edition", "agentEdition");
        let agent_type = if matches!(edition.as_str(), "international" | "workbuddy-ai") {
            "workbuddy-ai".to_owned()
        } else if matches!(edition.as_str(), "domestic" | "workbuddy") {
            "workbuddy".to_owned()
        } else {
            state["agentType"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("workbuddy")
                .to_owned()
        };
        state["agentType"] = json!(agent_type);
        let base = json!({
            "source":"workbuddy",
            "sessionId":sid,
            "cwd":state["cwd"],
            "agentType":agent_type,
            "roundId":round,
            "ts":ts
        });
        let mut emit = |ev| self.hub.ingest(merge(base.clone(), ev));
        if previous.is_none() || begins {
            emit(json!({"type":"start"}));
        }
        let title = p["session_title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_default();
        let tool = get("tool_name", "toolName");
        let id = get("tool_use_id", "toolUseId");
        let input = p
            .get("tool_input")
            .or(p.get("toolInput"))
            .cloned()
            .unwrap_or(Value::Null);
        match event.as_str() {
            "UserPromptSubmit" => {
                for (call, c) in state["calls"].as_object().unwrap() {
                    if c["async"] == true || c["permission"] == true {
                        emit(json!({"type":"resolve","callId":call}));
                    }
                }
                let prompt = content(&p["prompt"]);
                let label = if !title.is_empty() { title } else { prompt };
                if !label.trim().is_empty() {
                    emit(json!({"type":"meta","title":label}));
                }
            }
            "PreToolUse" => {
                if !id.is_empty() && state["calls"][&id]["resolved"] != true {
                    state["calls"][&id] = json!({
                        "tool":tool,
                        "resolved":false,
                        "async":tool.ends_with("request_user_input_async"),
                        "ts":ts
                    });
                    if question_tool(&tool) {
                        emit(json!({
                            "type":"wait",
                            "callId":id,
                            "tool":tool,
                            "text":question(&input),
                            "questions":questions(&input)
                        }));
                    } else {
                        emit(json!({"type":"step","eventId":id,"label":tool}));
                    }
                }
            }
            "PostToolUse" | "PostToolUseFailure" => {
                if !id.is_empty() {
                    let is_async = state["calls"][&id]["async"] == true
                        || tool.ends_with("request_user_input_async");
                    state["calls"][&id] = json!({
                        "tool":tool,
                        "resolved":true,
                        "async":is_async,
                        "ts":ts
                    });
                    if !is_async {
                        emit(json!({"type":"resolve","callId":id}));
                    }
                }
            }
            "PermissionRequest" => {
                let call = if id.is_empty() {
                    format!("perm:{ts}")
                } else {
                    id.clone()
                };
                if state["calls"][&call]["resolved"] != true {
                    state["calls"][&call] = json!({
                        "tool":if tool.is_empty() {"permission"} else {&tool},
                        "resolved":false,
                        "permission":true,
                        "ts":ts
                    });
                    emit(json!({
                        "type":"wait",
                        "callId":call,
                        "tool":if tool.is_empty() {"permission"} else {&tool},
                        "text":wait_text(p)
                    }));
                }
            }
            "Stop" | "SessionEnd" | "Interrupt" => {
                emit(json!({
                    "type":"end",
                    "status":if event=="Interrupt" {"aborted"} else {"done"}
                }));
            }
            "Notification" => {
                if !title.is_empty() {
                    emit(json!({"type":"meta","title":title}));
                }
                if confirmation_notification(p) {
                    let call = if id.is_empty() {
                        format!("notify:{ts}")
                    } else {
                        id.clone()
                    };
                    if state["calls"][&call]["resolved"] != true {
                        state["calls"][&call] = json!({
                            "tool":"notification",
                            "resolved":false,
                            "permission":true,
                            "ts":ts
                        });
                        emit(json!({
                            "type":"wait",
                            "callId":call,
                            "tool":"notification",
                            "text":wait_text(p)
                        }));
                    }
                } else {
                    emit(json!({"type":"activity"}));
                }
            }
            _ => {
                if !title.is_empty() {
                    emit(json!({"type":"meta","title":title}));
                }
            }
        }
        while state["calls"].as_object().unwrap().len() > 256 {
            let oldest = state["calls"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, v)| v["resolved"] == true)
                .min_by_key(|(_, v)| v["ts"].as_i64().unwrap_or(0))
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest {
                state["calls"].as_object_mut().unwrap().remove(&key);
            } else {
                break;
            }
        }
        self.workbuddy_live.insert(sid, state);
        self.workbuddy_hook_count += 1;
        self.workbuddy_presence.note_hook();
        self.poll_workbuddy().ok();
        true
    }

    pub fn poll_workbuddy(&mut self) -> Result<(), String> {
        self.poll_workbuddy_log_watch();
        if self.workbuddy_presence.observe() == crate::host_process::Presence::Gone {
            crate::host_process::end_host_sessions(&mut self.hub, "workbuddy", None);
            self.hub.health(
                "workbuddy",
                "exited",
                "WorkBuddy 已退出，未完成的任务已标记中止",
            );
            return Ok(());
        }
        self.hub.health(
            "workbuddy",
            "ok",
            if self.workbuddy_hook_count == 0 {
                "等待新的 WorkBuddy Hook；不恢复历史会话"
            } else {
                "已连接 WorkBuddy Hook 与审批日志（不读取会话文件）"
            },
        );
        Ok(())
    }

    pub fn poll_workbuddy_log_watch(&mut self) {
        if self.settings["sources"]["workbuddy"]["logWatch"] == false {
            return;
        }
        // 审批只会发生在回合进行中的会话上；没有活跃会话时不必扫日志
        // （此时候选注入目标也不存在，扫了也无处可用）。
        let active = self.hub.sessions.values().any(|s| {
            s["source"] == "workbuddy"
                && matches!(text(&s["status"]).as_str(), "running" | "wait")
        });
        if !active {
            return;
        }
        let time = now();
        if self.workbuddy_log_watch.primed
            && time - self.workbuddy_log_watch.last_scan < LOG_SCAN_INTERVAL_MS
        {
            return;
        }
        self.workbuddy_log_watch.last_scan = time;
        let mut files = Vec::new();
        for root in [
            self.home.join(".workbuddy-ai").join("logs"),
            self.home.join(".workbuddy").join("logs"),
        ] {
            collect_log_files(&root, &mut files);
        }
        let prime_only = !self.workbuddy_log_watch.primed;
        for path in files {
            self.consume_approval_log(&path, time, prime_only);
        }
        self.workbuddy_log_watch.primed = true;
    }

    fn consume_approval_log(&mut self, path: &Path, time: i64, prime_only: bool) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let size = meta.len();
        if prime_only {
            // 首次扫描只登记 offset：历史内容一律不看，只跟踪此后的新行。
            self.workbuddy_log_watch
                .offsets
                .insert(path.to_path_buf(), size);
            return;
        }
        let offset = self
            .workbuddy_log_watch
            .offsets
            .get(path)
            .copied()
            .unwrap_or(0);
        let offset = if size < offset { 0 } else { offset };
        if size <= offset {
            self.workbuddy_log_watch
                .offsets
                .insert(path.to_path_buf(), offset);
            return;
        }
        let active = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64 > time - LOG_ACTIVE_WINDOW_MS)
            .unwrap_or(false);
        if !active {
            self.workbuddy_log_watch
                .offsets
                .insert(path.to_path_buf(), offset);
            return;
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return;
        }
        let mut buffer = Vec::new();
        if file.take(1 << 20).read_to_end(&mut buffer).is_err() {
            return;
        }
        self.workbuddy_log_watch
            .offsets
            .insert(path.to_path_buf(), offset + buffer.len() as u64);
        for line in String::from_utf8_lossy(&buffer).lines() {
            match parse_approval_line(line) {
                Some(ApprovalEvent::Request {
                    tool,
                    call_id,
                    session,
                }) => {
                    self.workbuddy_log_watch
                        .call_sessions
                        .insert(call_id.clone(), session.clone());
                    self.apply_approval_wait(&session, &tool, &call_id, time);
                }
                Some(ApprovalEvent::Settled { call_id }) => {
                    if let Some(session) = self.workbuddy_log_watch.call_sessions.remove(&call_id)
                    {
                        self.apply_approval_settled(&session, &call_id, time);
                    }
                }
                None => {}
            }
        }
    }

    fn apply_approval_wait(&mut self, session: &str, tool: &str, call_id: &str, ts: i64) {
        if !self.hub.sessions.contains_key(&format!("workbuddy:{session}")) {
            return; // 只为已跟踪的会话补充信号，不创建幽灵会话
        }
        self.hub.ingest(json!({
            "source":"workbuddy",
            "sessionId":session,
            "type":"wait",
            "callId":call_id,
            "tool":tool,
            "text":format!("WorkBuddy 沙箱审批待确认：{tool}"),
            "ts":ts
        }));
    }

    fn apply_approval_settled(&mut self, session: &str, call_id: &str, ts: i64) {
        if !self.hub.sessions.contains_key(&format!("workbuddy:{session}")) {
            return;
        }
        self.hub.ingest(json!({
            "source":"workbuddy",
            "sessionId":session,
            "type":"resolve",
            "callId":call_id,
            "ts":ts
        }));
    }
}

#[cfg(test)]
mod approval_log_tests {
    use super::*;

    #[test]
    fn parses_approval_and_settle_lines() {
        let request = "[9/26/2026, 2:44:29 AM.430] [Info] [pid=79853] [enqueueSandboxApproval] Enqueued sandbox approval: tool=Bash, id=chatcmpl-tool-b76a22bd4ea6be30, session=90b92496-4265-46dc-a497-10b119927240, mainSession=90b92496-4265-46dc-a497-10b119927240, queueSize=1";
        assert_eq!(
            parse_approval_line(request),
            Some(ApprovalEvent::Request {
                tool: "Bash".into(),
                call_id: "chatcmpl-tool-b76a22bd4ea6be30".into(),
                session: "90b92496-4265-46dc-a497-10b119927240".into(),
            })
        );
        let approved = "[9/26/2026, 2:44:45 AM.120] [Info] [pid=79853] [Approve] User approved tool: Bash, id: chatcmpl-tool-b76a22bd4ea6be30, alwaysApprove: false, scope: session";
        assert_eq!(
            parse_approval_line(approved),
            Some(ApprovalEvent::Settled {
                call_id: "chatcmpl-tool-b76a22bd4ea6be30".into()
            })
        );
        let rejected = "[9/26/2026, 2:32:01 AM.719] [Info] [pid=70427] [Reject] User rejected tool: Bash, id: chatcmpl-tool-cfc21a572fa2488a815a1f792f807d16";
        assert_eq!(
            parse_approval_line(rejected),
            Some(ApprovalEvent::Settled {
                call_id: "chatcmpl-tool-cfc21a572fa2488a815a1f792f807d16".into()
            })
        );
        assert_eq!(parse_approval_line("[Info] unrelated line"), None);
        assert_eq!(
            parse_approval_line("Sandbox approval timed out after 1800000ms, denying"),
            None
        );
    }
}
