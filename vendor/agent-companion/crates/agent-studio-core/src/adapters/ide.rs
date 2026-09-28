// CodeBuddy IDE command hooks. No database or transcript recovery.
use super::*;
use crate::{content, merge, question, question_tool, questions};
use std::path::{Path, PathBuf};

pub const CODEBUDDY_IDE_HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "PreCompact",
    // 客户端弹「需要你允许」时抛出的通知；是唯一能表达“等待人工授权”的信号。
    "Notification",
];

/// International CodeBuddy uses `~/.codebuddy`; CodeBuddy CN uses `~/.codebuddycn`.
pub fn codebuddy_settings_files(home: &Path, custom: &str) -> Vec<PathBuf> {
    if !custom.is_empty() && !custom.ends_with(".vscdb") {
        let dir = if let Some(rest) = custom.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(custom)
        };
        return dir
            .is_dir()
            .then(|| vec![dir.join("settings.json")])
            .unwrap_or_default();
    }
    [".codebuddy", ".codebuddycn"]
        .into_iter()
        .filter_map(|name| {
            let dir = home.join(name);
            dir.is_dir().then(|| dir.join("settings.json"))
        })
        .collect()
}

pub fn codebuddy_edition(settings_file: &Path) -> &'static str {
    match settings_file
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| settings_file.file_name())
        .and_then(|n| n.to_str())
    {
        Some(".codebuddycn") => "domestic",
        _ => "international",
    }
}

fn codebuddy_agent_type(edition: &str) -> &'static str {
    match edition {
        "domestic" | "codebuddycn" => "codebuddycn",
        _ => "codebuddy",
    }
}

pub fn merge_codebuddy_ide_hooks(mut doc: Value, command: &str) -> Result<Value, String> {
    if !doc.is_object() || (!doc["hooks"].is_null() && !doc["hooks"].is_object()) {
        return Err("现有 CodeBuddy IDE Hook 配置无效，未覆盖".into());
    }
    if doc["hooks"].is_null() {
        doc["hooks"] = json!({});
    }
    for event in CODEBUDDY_IDE_HOOK_EVENTS {
        if !doc["hooks"][event].is_null() && !doc["hooks"][event].is_array() {
            return Err(format!("CodeBuddy IDE {event} Hook 配置无效，未覆盖"));
        }
        let mut groups = doc["hooks"][event].as_array().cloned().unwrap_or_default();
        for group in &mut groups {
            if let Some(hooks) = group["hooks"].as_array_mut() {
                hooks.retain(|h| !owned_hook(&text(&h["command"])));
            }
        }
        groups.retain(|g| !g["hooks"].as_array().is_some_and(|a| a.is_empty()));
        // IDE does not document async handlers. Keep delivery ordered and observational.
        groups
            .push(json!({"matcher":"","hooks":[{"type":"command","command":command,"timeout":3}]}));
        doc["hooks"][event] = json!(groups);
    }
    Ok(doc)
}
fn owned_hook(command: &str) -> bool {
    (command.contains("agent-studio-runtime")
        && command.contains(" hook")
        && command.contains("--source codebuddy-ide"))
        || command.contains("astra-office-codebuddy-ide.py")
}
/// 权限/交互通知展示文案：优先用客户端给的 message。
fn permission_text(p: &Value) -> String {
    let message = content(&p["message"]);
    if !message.trim().is_empty() {
        return message;
    }
    "需要你允许".into()
}
fn unanswered(v: &Value) -> bool {
    if let Some(s) = v.as_str() {
        return serde_json::from_str::<Value>(s)
            .ok()
            .is_some_and(|v| unanswered(&v));
    }
    if v["type"] == "multi_question_result" || v.get("answers").is_some() {
        return v["answers"]
            .as_object()
            .map(|a| {
                a.is_empty()
                    || a.values().all(|x| {
                        x.is_null() || x == "" || x.as_array().is_some_and(|a| a.is_empty())
                    })
            })
            .unwrap_or(true);
    }
    v.get("result").is_some_and(unanswered)
}
impl Collector {
    pub fn ingest_ide_hook(&mut self, p: &Value) -> bool {
        let sid = text(&p["session_id"]);
        let event = text(&p["hook_event_name"]);
        // The user settings are shared with CLI and with the VS Code plugin, so
        // the payload client decides which host kind the hook belongs to.
        let client = text(&p["client"]).to_ascii_lowercase();
        if self.settings["sources"]["codebuddy-ide"]["enabled"] != true
            // `cli` = CodeBuddy 独立 CLI（其 hook payload 的 client 值为 "CLI"）。
            || !matches!(
                client.as_str(),
                "codebuddyide" | "codebuddy" | "vscode" | "cli"
            )
            || sid.is_empty()
            || !CODEBUDDY_IDE_HOOK_EVENTS.contains(&event.as_str())
        {
            return false;
        }
        let host_kind = if client == "vscode" {
            "vscode"
        } else {
            "codebuddy-ide"
        };
        let ts = p["timestamp"]
            .as_i64()
            .filter(|n| *n > 0)
            .unwrap_or_else(now);
        let mut state = self
            .ide_live
            .get(&sid)
            .cloned()
            .unwrap_or(json!({"roundId":"","cwd":"","calls":{},"seq":0,"ended":false,"ts":0,"agentType":"","notifications":[]}));
        if ts < state["ts"].as_i64().unwrap_or(0) {
            return false;
        }
        let generation = text(&p["generation_id"]);
        let begins = event == "UserPromptSubmit";
        if !begins
            && !generation.is_empty()
            && !text(&state["roundId"]).is_empty()
            && state["roundId"] != generation
        {
            return false;
        }
        if !text(&p["cwd"]).is_empty() {
            state["cwd"] = p["cwd"].clone();
        }
        // IDE 的 hook `cwd` 不可靠（窗口没开目录时给的是客户端自己的安装目录，实测
        // `D:\Program Files\CodeBuddy CN`），照它跳转会打开无关目录。这里按会话 id 从
        // 插件历史里反查真正的工程目录，命中就以它为准（每个会话只查一次）。
        // VS Code 插件的 cwd 本来就是对的，不动它。
        if host_kind != "vscode" && state["resolvedFolder"].is_null() {
            state["resolvedFolder"] = json!(
                crate::workspace_history::resolve_session_folder(&sid)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
        }
        if host_kind != "vscode" {
            let resolved = text(&state["resolvedFolder"]);
            if !resolved.is_empty() {
                state["cwd"] = json!(resolved);
            }
        }
        state["ts"] = json!(ts);
        let edition = p["agent_edition"]
            .as_str()
            .or(p["agentEdition"].as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| state["agentType"].as_str().unwrap_or(""));
        let agent_type = codebuddy_agent_type(edition);
        state["agentType"] = json!(agent_type);
        if begins
            || (text(&state["roundId"]).is_empty()
                && matches!(event.as_str(), "PreToolUse" | "PostToolUse" | "PreCompact"))
        {
            state["roundId"] = json!(if generation.is_empty() {
                format!("turn:{ts}")
            } else {
                generation
            });
            state["calls"] = json!({});
            state["ended"] = json!(false);
            state["notifications"] = json!([]);
            state["permChecks"] = json!([]);
            self.hub.ingest(json!({"source":"codebuddy-ide","sessionId":sid,"type":"start","roundId":state["roundId"],"cwd":state["cwd"],"agentType":agent_type,"hostKind":host_kind,"ts":ts}));
        }
        let base = json!({"source":"codebuddy-ide","sessionId":sid,"roundId":state["roundId"],"cwd":state["cwd"],"agentType":agent_type,"hostKind":host_kind,"ts":ts});
        let mut emit = |ev| self.hub.ingest(merge(base.clone(), ev));
        let tool = text(&p["tool_name"]);
        let input = p["tool_input"].clone();
        let ask = tool == "ask_followup_question" || question_tool(&tool);
        // 权限弹窗只要等到工具真正开始/结束、或本轮结束，就算处理完了。
        if matches!(
            event.as_str(),
            "PreToolUse" | "PostToolUse" | "Stop"
        ) {
            let pending = state["notifications"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for call in pending {
                emit(json!({"type":"resolve","callId":call}));
            }
            state["notifications"] = json!([]);
            // 有工具开始/结束或本轮结束，说明推进了：清掉未决的「可能等人确认」检查点。
            let checks = state["permChecks"].as_array().cloned().unwrap_or_default();
            for id in checks {
                emit(json!({"type":"permission_resolve","callId":id}));
            }
            state["permChecks"] = json!([]);
        }
        match event.as_str() {
            "UserPromptSubmit" => {
                let title = content(&p["prompt"]);
                if !title.trim().is_empty() {
                    emit(json!({"type":"meta","title":title}));
                }
            }
            "PreToolUse" if state["ended"] != true => {
                let seq = state["seq"].as_u64().unwrap_or(0) + 1;
                state["seq"] = json!(seq);
                let id = p["tool_use_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or(format!("ide:{seq}"));
                if ask {
                    state["calls"][&id] = json!({"tool":tool,"input":input});
                    emit(
                        json!({"type":"wait","callId":id,"tool":tool,"text":question(&input),"questions":questions(&input)}),
                    );
                } else {
                    emit(json!({"type":"step","eventId":id,"label":tool}));
                    // CodeBuddy IDE / VS Code 插件在弹「需要你允许」时**不会**发 Notification
                    // hook（实测客户端里那条链路是死代码），所以这里额外记一个检查点：工具
                    // 已经开始、却长时间等不到 PostToolUse，就按「可能卡在等人确认」处理
                    // （由 permission-check.ts 与 runtime 的告警各自按阈值判定）。
                    let mut checks = state["permChecks"].as_array().cloned().unwrap_or_default();
                    if !checks.iter().any(|c| c == &json!(&id)) {
                        checks.push(json!(&id));
                    }
                    state["permChecks"] = json!(checks);
                    emit(json!({"type":"permission_check","callId":id}));
                    // 记下这次调用（含命令文本），供客户端日志里的「危险命令」判定回指到本会话。
                    self.recent_calls.push_back(crate::adapters::RecentToolCall {
                        ts,
                        session: sid.clone(),
                        round: text(&state["roundId"]),
                        call_id: id.clone(),
                        command: text(&p["tool_input"]["command"]),
                        agent_type: agent_type.to_string(),
                        host_kind: host_kind.to_string(),
                    });
                    while self.recent_calls.len() > 64 {
                        self.recent_calls.pop_front();
                    }
                }
            }
            "PostToolUse" if state["ended"] != true && ask => {
                let supplied = text(&p["tool_use_id"]);
                let matches: Vec<String> = state["calls"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(id, c)| {
                        if !supplied.is_empty() {
                            **id == supplied
                        } else {
                            c["tool"] == tool && c["input"] == input
                        }
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                // Never guess which concurrent identical question was answered.
                if matches.len() == 1 && !unanswered(&p["tool_response"]) {
                    let id = &matches[0];
                    state["calls"].as_object_mut().unwrap().remove(id);
                    emit(json!({"type":"resolve","callId":id}));
                }
            }
            // 客户端弹出「需要你允许 / 需要你确认」时的通知。
            "Notification" if state["ended"] != true => {
                let raw = text(&p["notification_type"]);
                let raw = if raw.is_empty() {
                    text(&p["notificationType"])
                } else {
                    raw
                };
                let kind = raw.to_ascii_lowercase();
                if matches!(kind.as_str(), "permission_prompt" | "elicitation_dialog") {
                    let id = p["tool_use_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("notify:{ts}"));
                    let key = Value::String(id.clone());
                    let mut pending = state["notifications"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    if !pending.iter().any(|n| n == &key) {
                        pending.push(key);
                    }
                    state["notifications"] = json!(pending);
                    emit(json!({
                        "type":"wait",
                        "callId":id,
                        "tool":"permission",
                        "text":permission_text(p)
                    }));
                }
            }
            "Stop"
                if state["ended"] != true
                    && !text(&state["roundId"]).is_empty()
                    && state["calls"].as_object().unwrap().is_empty() =>
            {
                emit(json!({"type":"end","status":"done"}));
                state["ended"] = json!(true);
            }
            // 客户端在「新建/切换会话、finalize 会话」时都会发 SessionEnd，而 payload 用的是
            // SessionHookManager 里**全局唯一**的 sessionId（SessionCoordinator.endSession 传入的
            // conversationId 并不会进 payload），所以它既可能指向另一个会话，也不代表后台任务真的
            // 结束：实测「新建对话」会立刻让仍在后台跑的旧会话收到 SessionEnd，前端就显示「已终止」。
            // 因此不在这里终止——多会话需要并行显示。会话由 Stop（回合结束 → done）收敛，
            // 长期无更新的会话由 hub 的 45 分钟 stale 兜底。
            "SessionEnd" => {}
            "PreCompact" if state["ended"] != true => {
                emit(json!({"type":"activity"}));
            }
            _ => {}
        }
        self.ide_live.insert(sid, state);
        self.ide_hook_count += 1;
        match host_kind {
            "vscode" => self.vscode_presence.note_hook(),
            _ => self.ide_presence.note_hook(),
        }
        self.poll_ide().ok();
        true
    }
    pub fn poll_ide(&mut self) -> Result<(), String> {
        use crate::host_process::{end_host_sessions, Presence};
        let ide = self.ide_presence.observe();
        let vscode = self.vscode_presence.observe();
        let gone_ide = ide == Presence::Gone;
        let gone_vscode = vscode == Presence::Gone;
        if gone_ide {
            end_host_sessions(&mut self.hub, "codebuddy-ide", Some("codebuddy-ide"));
        }
        if gone_vscode {
            end_host_sessions(&mut self.hub, "codebuddy-ide", Some("vscode"));
        }
        // A live kind, or a kind that never saw a hook (unknown), must not be
        // reported as an exit.
        if !(gone_ide || gone_vscode) || ide == Presence::Alive || vscode == Presence::Alive {
            self.hub.health("codebuddy-ide", "ok", self.ide_health_detail());
        } else {
            self.hub.health(
                "codebuddy-ide",
                "exited",
                match (gone_ide, gone_vscode) {
                    (true, true) => "CodeBuddy IDE 与 VS Code 已退出，未完成的任务已标记中止",
                    (true, false) => "CodeBuddy IDE 已退出，未完成的任务已标记中止",
                    _ => "VS Code 已退出，未完成的任务已标记中止",
                },
            );
        }
        Ok(())
    }
    fn ide_health_detail(&self) -> &'static str {
        if self.ide_hook_count == 0 {
            "等待新的 CodeBuddy Hook；不恢复历史会话"
        } else {
            "已连接 CodeBuddy Hook（不读取会话文件）"
        }
    }
}
