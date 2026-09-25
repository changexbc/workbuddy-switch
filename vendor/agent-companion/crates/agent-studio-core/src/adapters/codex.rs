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
    pub(crate) fn restore_codex_recovery(&mut self) {
        for record in crate::codex_recovery::load(&self.home, &self.settings, &self.integrations) {
            let session = &record["session"];
            let sid = text(&session["sessionId"]);
            if sid.is_empty() || session["id"] != format!("codex:{sid}") { continue; }
            let id = format!("codex:{sid}");
            let newer = self.hub.sessions.get(&id).is_some_and(|current|
                current["roundId"] != session["roundId"] && current["updatedAt"].as_i64().unwrap_or(0) > session["updatedAt"].as_i64().unwrap_or(0));
            if newer { continue; }
            let replace = self.hub.sessions.get(&id).is_none_or(|current|
                current["roundId"] == session["roundId"] &&
                (session["updatedAt"].as_i64().unwrap_or(0) > current["updatedAt"].as_i64().unwrap_or(0)
                    || session["updatedAt"] == current["updatedAt"]
                        && crate::hub::terminal(&text(&session["status"]))
                        && !crate::hub::terminal(&text(&current["status"]))));
            if replace {
                let mut restored = session.clone();
                restored["recovered"] = json!(true);
                self.hub.sessions.insert(id, restored);
                self.live.insert(sid, record["live"].clone());
            }
        }
    }

    pub(crate) fn save_codex_recovery(&mut self, sid: &str) {
        if self.hub.hidden_codeg_codex_ids.contains(sid.trim_start_matches("thr_")) { return; }
        let id = format!("codex:{sid}");
        if let (Some(session), Some(live)) = (self.hub.sessions.get(&id), self.live.get(sid)) {
            if let Ok(record) = crate::codex_recovery::save(&self.home, &self.settings, &self.integrations, sid, session, live) {
                if record["session"]["updatedAt"].as_i64().unwrap_or(0) > session["updatedAt"].as_i64().unwrap_or(0)
                    || record["session"]["updatedAt"] == session["updatedAt"]
                        && crate::hub::terminal(&text(&record["session"]["status"]))
                        && !crate::hub::terminal(&text(&session["status"])) {
                    self.restore_codex_recovery();
                }
            }
        }
    }

    pub fn ingest_codex_hook(&mut self, p: &Value) -> bool {
        self.restore_codex_recovery();
        if self.settings["sources"]["codex"]["enabled"] != true {
            return false;
        }
        let get = |a: &str, b: &str| p[a].as_str().or(p[b].as_str()).unwrap_or("").to_owned();
        let sid = get("session_id", "sessionId");
        let event = get("hook_event_name", "hookEventName");
        if sid.is_empty() || sid.len() > 200
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
            crate::codex_recovery::remove(&self.home, &self.settings, &self.integrations, &sid);
            return false;
        }
        let ts = p["timestamp"]
            .as_i64()
            .filter(|n| *n > 0)
            .unwrap_or_else(now);
        let previous = self.live.get(&sid).cloned();
        if previous.is_none() && matches!(event.as_str(), "Stop" | "Interrupt" | "SessionEnd") {
            return false;
        }
        let mut state = previous
            .clone()
            .unwrap_or(json!({"roundId":"","cwd":"","calls":{},"permissions":[]}));
        let turn = get("turn_id", "turnId");
        if turn.len() > 200 { return false; }
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
        if begins && previous.is_some() && state["roundId"] != round {
            let current_ts = self.hub.sessions.get(&format!("codex:{sid}"))
                .and_then(|s| s["updatedAt"].as_i64()).unwrap_or(0);
            if ts <= current_ts { return false; }
        }
        // Tool callbacks from an older turn cannot mutate the current one.
        if previous.is_some() && state["roundId"] != round && !begins {
            return false;
        }
        if state["ended"] == true && state["roundId"] == round { return false; }
        if previous.is_none() || state["roundId"] != round {
            state["calls"] = json!({});
            state["permissions"] = json!([]);
            state["ended"] = json!(false);
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
                state["ended"] = json!(true);
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
        self.live.insert(sid.clone(), state);
        self.save_codex_recovery(&sid);
        self.hook_count += 1;
        self.poll_codex().ok();
        true
    }

    pub fn poll_codex(&mut self) -> Result<(), String> {
        // Reconcile an end hook written by the hook process while RPC was unavailable.
        self.restore_codex_recovery();
        self.hub.health(
            "codex",
            "ok",
            if self.hook_count == 0 {
                "等待新的 Codex Hook；仅恢复本应用已跟踪的会话"
            } else {
                "已连接 Codex Hook（不读取会话文件）"
            },
        );
        Ok(())
    }
}
