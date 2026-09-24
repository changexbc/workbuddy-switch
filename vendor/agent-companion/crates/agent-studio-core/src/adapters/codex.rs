// Codex is event-driven: no database, transcript, or persisted-session reads.
use super::*;
use crate::{content, merge, question_tool};
use std::hash::{Hash, Hasher};

fn internal_prompt(prompt: &str) -> bool {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let prefixes: Vec<String> = serde_json::from_str(include_str!(
        "../../../../src/monitor/codex-internal-prompts.json"
    )).expect("valid internal prompt templates");
    prefixes.iter().any(|prefix| normalized.starts_with(prefix))
}

impl Collector {
    pub fn ingest_codex_hook(&mut self, p: &Value) -> bool {
        if self.settings["sources"]["codex"]["enabled"] != true {
            return false;
        }
        let get = |a: &str, b: &str| p[a].as_str().or(p[b].as_str()).unwrap_or("").to_owned();
        let sid = get("session_id", "sessionId");
        let event = get("hook_event_name", "hookEventName");
        if sid.is_empty()
            || !matches!(
                event.as_str(),
                "SessionStart"
                    | "UserPromptSubmit"
                    | "PreToolUse"
                    | "PostToolUse"
                    | "PermissionRequest"
                    | "Stop"
                    | "Interrupt"
                    | "SessionEnd"
            )
        {
            return false;
        }
        // Internal tasks receive the same hooks as user chats. Once recognized,
        // suppress their entire lifecycle so late tool/stop hooks cannot revive them.
        let internal = self.live.get(&sid).is_some_and(|s| s["internal"] == true)
            || event == "UserPromptSubmit" && internal_prompt(&content(&p["prompt"]));
        if internal {
            self.live.insert(sid.clone(), json!({"internal":true}));
            self.hub.sessions.remove(&format!("codex:{sid}"));
            return false;
        }
        let ts = p["timestamp"]
            .as_i64()
            .filter(|n| *n > 0)
            .unwrap_or_else(now);
        let previous = self.live.get(&sid).cloned();
        let mut state = previous
            .clone()
            .unwrap_or(json!({"roundId":"","cwd":"","calls":{},"permissions":[]}));
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
        // Tool callbacks from an older turn cannot mutate the current one.
        if previous.is_some() && state["roundId"] != round && !begins {
            return false;
        }
        if previous.is_none() || state["roundId"] != round {
            state["calls"] = json!({});
            state["permissions"] = json!([]);
        }
        state["roundId"] = json!(round);
        if let Some(cwd) = p["cwd"].as_str().filter(|s| !s.is_empty()) {
            state["cwd"] = json!(cwd);
        }
        let base =
            json!({"source":"codex","sessionId":sid,"cwd":state["cwd"],"roundId":round,"ts":ts});
        let mut emit = |ev| self.hub.ingest(merge(base.clone(), ev));
        if previous.is_none() || begins {
            emit(json!({"type":"start"}));
        }
        let tool = get("tool_name", "toolName");
        let id = get("tool_use_id", "toolUseId");
        let input = p
            .get("tool_input")
            .or(p.get("toolInput"))
            .cloned()
            .unwrap_or(Value::Null);
        let command = if input["command"].is_null() {
            Value::Null
        } else {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            input["command"].to_string().hash(&mut hash);
            json!(hash.finish().to_string())
        };
        match event.as_str() {
            "UserPromptSubmit" => {
                // Prompt text is a hook-provided label, not the database conversation title.
                let title = content(&p["prompt"]);
                if !title.trim().is_empty() {
                    emit(json!({"type":"meta","title":title}));
                }
            }
            "PreToolUse" => {
                if !id.is_empty() && state["calls"][&id]["resolved"] != true {
                    state["calls"][&id] = json!({"tool":tool,"command":command,"resolved":false,"async":tool.ends_with("request_user_input_async"),"ts":ts});
                    // Optional async questions do not block the Codex turn.
                    if question_tool(&tool) && !tool.ends_with("request_user_input_async") {
                        emit(json!({"type":"wait","callId":id,"tool":tool,"text":"需要你确认"}));
                    } else {
                        emit(json!({"type":"step","eventId":id,"label":tool}));
                    }
                }
            }
            "PermissionRequest" => {
                let matching = state["calls"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(key, c)| {
                        if !id.is_empty() {
                            *key == &id
                        } else {
                            c["tool"] == tool && c["command"] == command
                        }
                    })
                    .max_by_key(|(_, c)| (c["resolved"] != true, c["ts"].as_i64().unwrap_or(0)))
                    .map(|(key, c)| (key.clone(), c["resolved"] == true));
                if !matching.as_ref().is_some_and(|(_, resolved)| *resolved) {
                    let call = matching.map(|(id, _)| id).unwrap_or(id);
                    let key = format!(
                        "perm:{}",
                        if call.is_empty() {
                            ts.to_string()
                        } else {
                            call.clone()
                        }
                    );
                    state["permissions"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"id":key,"call":call,"tool":tool,"command":command}));
                    // PermissionRequest precedes any auto-approval or UI decision.
                    // It is not evidence that the user has an approval dialog.
                    emit(json!({"type":"permission_check","callId":key}));
                }
            }
            "PostToolUse" => {
                if !id.is_empty() {
                    let is_async = state["calls"][&id]["async"] == true
                        || tool.ends_with("request_user_input_async");
                    state["calls"][&id] = json!({"tool":tool,"command":command,"resolved":true,"async":is_async,"ts":ts});
                    // Async questions never create waits, so their completion must
                    // not resolve any pending synchronous question.
                    if !is_async {
                        emit(json!({"type":"resolve","callId":id}));
                    }
                    let mut remaining = vec![];
                    for permission in state["permissions"].as_array().unwrap() {
                        let matches = permission["call"] == id
                            || (text(&permission["call"]).is_empty()
                                && permission["tool"] == tool
                                && permission["command"] == command);
                        if matches {
                            emit(json!({"type":"permission_resolve","callId":permission["id"]}));
                        } else {
                            remaining.push(permission.clone());
                        }
                    }
                    state["permissions"] = json!(remaining);
                }
            }
            "Stop" | "SessionEnd" | "Interrupt" => {
                emit(
                    json!({"type":"end","status":if event=="Interrupt" {"aborted"} else {"done"}}),
                );
                state["permissions"] = json!([]);
            }
            _ => {}
        }
        // Bound per-round callback bookkeeping while keeping recent completions
        // so delayed PreToolUse callbacks cannot resurrect a finished wait.
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
        self.live.insert(sid, state);
        self.hook_count += 1;
        self.poll_codex().ok();
        true
    }

    pub fn poll_codex(&mut self) -> Result<(), String> {
        // This is an in-memory health update only. Never discover or restore sessions.
        self.hub.health(
            "codex",
            "ok",
            if self.hook_count == 0 {
                "等待新的 Codex Hook；不恢复历史会话"
            } else {
                "已连接 Codex Hook（不读取会话文件）"
            },
        );
        Ok(())
    }
}
