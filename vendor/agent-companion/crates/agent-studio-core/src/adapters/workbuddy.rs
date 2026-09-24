// WorkBuddy is event-driven via Claude Code-compatible command hooks.
// Permissions stay in WorkBuddy's native GUI; this adapter never gates them.
use super::*;
use crate::{content, merge, question, question_tool, questions};
use std::path::{Path, PathBuf};

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
                "已连接 WorkBuddy Hook（不读取会话文件）"
            },
        );
        Ok(())
    }
}
