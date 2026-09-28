mod codeg;
mod codeg_stream;
pub use codeg::{CodegHooks, CODEG_EVENTS, merge_codeg_webhooks};
mod codex;
pub mod custom;
mod ide;
pub use ide::{
    codebuddy_edition, codebuddy_settings_files, merge_codebuddy_ide_hooks,
    CODEBUDDY_IDE_HOOK_EVENTS,
};
mod workbuddy;
pub use workbuddy::{
    merge_workbuddy_hooks, workbuddy_edition, workbuddy_settings_files, WORKBUDDY_HOOK_EVENTS,
};
use crate::{atomic_json, hub::Hub, now, settings, text};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet, VecDeque}, path::PathBuf};
pub fn query(db: &Connection, sql: &str) -> Result<Vec<Value>, String> {
    let mut st = db
        .prepare(sql)
        .map_err(|_| "数据库结构不兼容".to_string())?;
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let rows = st
        .query_map([], |row| {
            let mut v = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let x = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(b) | ValueRef::Blob(b) => json!(String::from_utf8_lossy(b)),
                };
                v.insert(name.clone(), x);
            }
            Ok(Value::Object(v))
        })
        .map_err(|_| "无法查询数据库")?;
    rows.map(|r| r.map_err(|_| "无法读取数据库行".into()))
        .collect()
}
pub fn open(paths: &[PathBuf]) -> Result<Connection, String> {
    for p in paths {
        if let Ok(db) = Connection::open_with_flags(
            p,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            let _ = db.busy_timeout(std::time::Duration::from_millis(150));
            return Ok(db);
        }
    }
    Err("数据库不可读".into())
}
#[derive(Default)]
pub struct Context {
    pub round: String,
    pub cwd: String,
    pub pending: Vec<Value>,
    pub seen: std::collections::HashSet<String>,
}
/// 一次工具调用（PreToolUse）的登记。
///
/// 只用于把客户端日志里的「危险命令」判定回指到具体会话——客户端弹审批框时
/// **不发 hook**，而它自己写的那行日志里也没有会话 id，只能靠命令文本对上。
#[derive(Debug, Clone)]
pub struct RecentToolCall {
    pub ts: i64,
    pub session: String,
    pub round: String,
    pub call_id: String,
    pub command: String,
    pub agent_type: String,
    pub host_kind: String,
}
pub struct Collector {
    pub hub: Hub,
    pub settings: Value,
    pub home: PathBuf,
    pub live: HashMap<String, Value>,
    pub contexts: HashMap<String, Context>,
    pub tail: crate::tail::Tail,
    pub rows: HashMap<String, Value>,
    pub hook_count: u64,
    pub codeg: CodegHooks,
    pub integrations: Value,
    pub last_hook_at: HashMap<String, i64>,
    pub ide_live: HashMap<String, Value>,
    pub ide_hook_count: u64,
    pub workbuddy_live: HashMap<String, Value>,
    pub workbuddy_hook_count: u64,
    pub workbuddy_log_watch: workbuddy::LogWatch,
    /// Ignore completion events after a user closes a task, until new activity arrives.
    pub closed_monitor_sessions: HashSet<String>,
    pub workbuddy_presence: crate::host_process::HostPresence,
    pub ide_presence: crate::host_process::HostPresence,
    pub vscode_presence: crate::host_process::HostPresence,
    pub codex_read_state: crate::codex_read_state::ReadStateObserver,
    pub custom_store: crate::custom::Store,
    pub custom_engine: crate::custom::Engine,
    pub custom_diagnostics: std::collections::VecDeque<Value>,
    pub custom_stats: HashMap<String, (Option<i64>, Option<i64>)>,
    /// PreToolUse 的最近调用，供客户端日志判定回指会话。
    pub recent_calls: VecDeque<RecentToolCall>,
    /// 客户端扩展宿主日志观察（读它自己的「危险命令」判定）。
    pub safety_log: crate::log_watch::ClientLogWatch,
    /// 已读到、还没匹配上会话的危险命令（附过期时间）。
    pending_dangerous: VecDeque<(i64, crate::log_watch::DangerousCommand)>,
}
impl Collector {
    pub fn new(home: PathBuf) -> Result<Self, String> {
        let file = home.join(".agent-studio/settings.json");
        let settings = if file.exists() {
            settings::validate(
                &serde_json::from_slice(&std::fs::read(file).map_err(|e| e.to_string())?)
                    .map_err(|_| "配置文件无效")?,
            )?
        } else {
            settings::defaults()
        };
        let integrations = match std::fs::read(home.join(".agent-studio/integrations.json")) {
            Ok(bytes) => {
                let v: Value = serde_json::from_slice(&bytes).map_err(|_| "接入策略无效")?;
                if !v.is_object() || v.as_object().unwrap().iter().any(|(k,v)| !settings::SOURCES.contains(&k.as_str()) || !v.is_boolean()) { return Err("接入策略无效".into()); }
                v
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.to_string()),
        };
        let safety_log = crate::log_watch::ClientLogWatch::new(&home);
        let mut c = Self {
            integrations,
            last_hook_at: HashMap::new(),
            hub: Hub::new(),
            custom_store: crate::custom::Store::load(&home),
            custom_engine: Default::default(),
            custom_diagnostics: Default::default(),
            custom_stats: HashMap::new(),
            settings,
            home,
            live: HashMap::new(),
            contexts: HashMap::new(),
            tail: Default::default(),
            rows: HashMap::new(),
            hook_count: 0,
            codeg: Default::default(),
            ide_live: HashMap::new(),
            ide_hook_count: 0,
            workbuddy_live: HashMap::new(),
            workbuddy_hook_count: 0,
            workbuddy_log_watch: Default::default(),
            closed_monitor_sessions: HashSet::new(),
            workbuddy_presence: crate::host_process::HostPresence::for_host("workbuddy"),
            ide_presence: crate::host_process::HostPresence::for_host("codebuddy-ide"),
            vscode_presence: crate::host_process::HostPresence::for_host("vscode"),
            codex_read_state: Default::default(),
            recent_calls: VecDeque::new(),
            safety_log,
            pending_dangerous: VecDeque::new(),
        };
        c.restore_codex_recovery();
        Ok(c)
    }
    pub fn poll(&mut self) {
        for id in settings::SOURCES {
            if id == "codeg" {
                if let Err(e)=self.maintain_codeg_webhook(){ self.hub.health(id, if self.settings["sources"][id]["enabled"]==true {"error"}else{"disabled"}, &e); }
                if self.settings["sources"][id]["enabled"] != true { self.hub.health(id,"disabled","已关闭监听"); }
                continue;
            }
            if self.settings["sources"][id]["enabled"] != true {
                self.hub.health(id, "disabled", "已关闭监听");
                continue;
            }
            let result = match id {
                "codex" => self.poll_codex(),
                "workbuddy" => self.poll_workbuddy(),
                _ => self.poll_ide(),
            };
            if let Err(e) = result {
                self.hub.health(id, "error", &e);
            }
        }
        if self.settings["sources"]["codebuddy-ide"]["enabled"] == true {
            self.poll_safety_log();
        }
        self.hub.ready = true;
        self.poll_custom();
        if self.settings["sources"]["codex"]["enabled"] == true {
            let previous_viewed: HashMap<String, Value> = self.hub.sessions.iter()
                .filter(|(_, s)| s["source"] == "codex")
                .map(|(id, s)| (id.clone(), s["viewedRoundId"].clone())).collect();
            let file = self.paths("codex")[0].join(".codex-global-state.json");
            self.codex_read_state.poll(&file, &mut self.hub, now());
            let changed: Vec<String> = self.hub.sessions.iter()
                .filter(|(id, s)| s["source"] == "codex" && !s["viewedRoundId"].is_null()
                    && previous_viewed.get(*id) != Some(&s["viewedRoundId"]))
                .map(|(_, s)| text(&s["sessionId"])).collect();
            for sid in changed { self.save_codex_recovery(&sid); }
        } else {
            self.codex_read_state = Default::default();
        }
    }
    /// 读客户端扩展宿主日志里的「危险命令」判定，把对应会话置为「待确认」。
    ///
    /// 客户端（国内版 IDE / VS Code 插件）在弹「包含危险命令，是否仍要运行？」之前会写
    /// `[SafetyRule] Dangerous command detected: <命令>`，但它**不发 hook**、日志行里也没有
    /// 会话 id。这里用命令文本匹配刚登记的 PreToolUse，就能在弹窗出现的同一瞬间提醒用户，
    /// 而不是等「N 秒没有反馈」的推断。
    fn poll_safety_log(&mut self) {
        let now = now();
        let scan = self.safety_log.poll();
        // 「等待用户确认」以客户端**状态行**为准：`running -> waiting_user_input` 是弹窗那一刻
        // 写的，`waiting_user_input -> running` 是用户处理完那一刻写的，两者都带会话 id。
        //
        // 旧的「危险命令行 + 匹配刚收到的 PreToolUse」通路已停用：实测那行 `Dangerous command
        // detected` 在客户端里是**点完确认之后**才落盘（状态 15:40:39.659 已恢复 running，那行
        // 15:40:39.871 才写），按它触发只会「确认后提示、随即一闪而过」；且它依赖命令文本能
        // 对上 PreToolUse，日志截断就整条失效。
        for event in scan.waits {
            match event {
                crate::log_watch::WaitEvent::Enter(wait) => self.promote_client_wait(&wait, now),
                crate::log_watch::WaitEvent::Exit(wait) => self.clear_client_wait(&wait, now),
            }
        }
        self.safety_log.prune(now);
        while self.pending_dangerous.len() > 32 {
            self.pending_dangerous.pop_front();
        }
        let mut remaining = VecDeque::new();
        while let Some((deadline, found)) = self.pending_dangerous.pop_front() {
            if deadline < now {
                continue;
            }
            let hit = self.recent_calls.iter().rposition(|call| {
                now - call.ts < 60_000
                    && crate::log_watch::same_command(&call.command, &found.command)
            });
            match hit {
                Some(index) => {
                    let call = self.recent_calls[index].clone();
                    self.promote_to_wait(&call, &found, now);
                }
                None => remaining.push_back((deadline, found)),
            }
        }
        self.pending_dangerous = remaining;
    }

    /// 把「客户端已判定需要人确认」的一次调用提升为会话的待确认状态。
    fn promote_to_wait(
        &mut self,
        call: &RecentToolCall,
        found: &crate::log_watch::DangerousCommand,
        now: i64,
    ) {
        let Some(state) = self.ide_live.get(&call.session).cloned() else {
            return;
        };
        let mut notifications = state["notifications"].as_array().cloned().unwrap_or_default();
        if !notifications.iter().any(|n| n == &json!(call.call_id)) {
            notifications.push(json!(call.call_id));
        }
        if let Some(live) = self.ide_live.get_mut(&call.session) {
            live["notifications"] = json!(notifications);
            // 这条已不是「可能卡住」，撤掉推断用的检查点，避免两套信号同时响。
            if let Some(checks) = live["permChecks"].as_array_mut() {
                checks.retain(|c| c != &json!(call.call_id));
            }
        }
        let round = if call.round.is_empty() {
            text(&state["roundId"])
        } else {
            call.round.clone()
        };
        let base = json!({
            "source": "codebuddy-ide",
            "sessionId": call.session,
            "roundId": round,
            "cwd": text(&state["cwd"]),
            "agentType": call.agent_type,
            "hostKind": call.host_kind,
            "ts": now,
        });
        self.hub.ingest(crate::merge(
            base.clone(),
            json!({"type": "permission_resolve", "callId": call.call_id}),
        ));
        self.hub.ingest(crate::merge(
            base,
            json!({
                "type": "wait",
                "callId": call.call_id,
                "tool": "permission",
                "text": found.reason,
            }),
        ));
    }

    /// 客户端日志通路的 callId：会话级固定值（进入与解除必须一致，日志里没有 callId）。
    fn client_wait_id(session_id: &str) -> String {
        format!("client-log:{session_id}")
    }

    /// 客户端日志状态行 `running -> waiting_user_input` ⇒ 会话进入「待确认」。
    ///
    /// 与 [`Self::promote_to_wait`] 同一出口（hub 的 `wait` 事件）；区别是会话 id 直接来自
    /// 日志行（日志带 `Session <id>`），不再依赖匹配刚收到的 PreToolUse ⇒ 弹窗瞬间就挂上，
    /// 命令文本被截断也不会漏。callId 用会话级固定值，便于解除时精确对应。
    fn promote_client_wait(&mut self, wait: &crate::log_watch::ConfirmWait, now: i64) {
        let Some(state) = self.ide_live.get(&wait.session_id).cloned() else {
            return;
        };
        let call_id = Self::client_wait_id(&wait.session_id);
        let mut notifications = state["notifications"].as_array().cloned().unwrap_or_default();
        if !notifications.iter().any(|n| n == &json!(call_id)) {
            notifications.push(json!(call_id));
        }
        if let Some(live) = self.ide_live.get_mut(&wait.session_id) {
            live["notifications"] = json!(notifications);
        }
        self.hub.ingest(crate::merge(
            json!({
                "source": "codebuddy-ide",
                "sessionId": wait.session_id,
                "roundId": text(&state["roundId"]),
                "cwd": text(&state["cwd"]),
                "ts": now,
            }),
            json!({
                "type": "wait",
                "callId": call_id,
                "tool": "permission",
                "text": "客户端在等待你确认",
            }),
        ));
    }

    /// 客户端日志状态行 `waiting_user_input -> running` ⇒ 撤销该会话的「待确认」。
    ///
    /// 解除必须带上与进入**同一个轮次**（`roundId`）等上下文：hub 的 `permission_resolve`
    /// 按 (source, sessionId, roundId, callId) 定位那条 pending，缺轮次就落不到原会话上
    /// （实测：只带 source/sessionId/callId 时状态仍是 `wait`）。
    fn clear_client_wait(&mut self, wait: &crate::log_watch::ConfirmWait, now: i64) {
        let call_id = Self::client_wait_id(&wait.session_id);
        let Some(state) = self.ide_live.get(&wait.session_id).cloned() else {
            return;
        };
        if let Some(live) = self.ide_live.get_mut(&wait.session_id) {
            if let Some(notifications) = live["notifications"].as_array_mut() {
                notifications.retain(|n| n != &json!(call_id));
            }
        }
        self.hub.ingest(crate::merge(
            json!({
                "source": "codebuddy-ide",
                "sessionId": wait.session_id,
                "roundId": text(&state["roundId"]),
                "cwd": text(&state["cwd"]),
                "ts": now,
            }),
            // 清 pending 的事件名是 `resolve`（`permission_resolve` 是 hook 侧那条，hub 不认）。
            json!({"type": "resolve", "callId": call_id}),
        ));
    }

    pub fn request(&mut self, command: &str, payload: &Value) -> Result<Value, String> {
        match command {
            "session_monitor_close" => {
                let source = payload["source"].as_str().filter(|s|
                    settings::SOURCES.contains(s) || crate::custom::parse_source(s).is_some())
                    .ok_or("无效 Agent 来源")?;
                let sid = payload["sessionId"].as_str().filter(|s| !s.is_empty()).ok_or("无效会话 ID")?;
                let round = payload["roundId"].as_str().filter(|s| !s.is_empty()).ok_or("无效轮次 ID")?;
                let id = format!("{source}:{sid}");
                if self.hub.sessions.get(&id).is_none_or(|session|
                    session["source"] != source || session["sessionId"] != sid || session["roundId"] != round)
                {
                    return Ok(json!({"closed":false}));
                }
                if source == "codex" {
                    // Commit removal under the same lock used by offline Hooks before
                    // clearing memory. A failed write must leave the avatar intact.
                    if !crate::codex_recovery::remove_round(&self.home, &self.settings, &self.integrations, sid, round)? {
                        return Ok(json!({"closed":false}));
                    }
                    self.live.remove(sid);
                } else if source == "codebuddy-ide" {
                    self.ide_live.remove(sid);
                } else if source == "workbuddy" {
                    self.workbuddy_live.remove(sid);
                } else if source.starts_with(crate::custom::SOURCE_PREFIX) {
                    self.custom_engine.forget(&id);
                }
                self.hub.sessions.remove(&id);
                self.hub.events.retain(|event| event["sessionId"] != id || event["roundId"] != round);
                if matches!(source, "workbuddy" | "codeg") {
                    if self.closed_monitor_sessions.len() >= 512 { self.closed_monitor_sessions.clear(); }
                    self.closed_monitor_sessions.insert(id);
                }
                Ok(json!({"closed":true}))
            }
            "settings_get" => Ok(self.settings.clone()),
            "settings_set" => {
                let next = settings::validate(payload)?;
                atomic_json(&self.home.join(".agent-studio/settings.json"), &next)?;
                for id in settings::SOURCES {
                    if next["sources"][id] != self.settings["sources"][id] {
                        for path in self.paths(id) {
                            self.tail.forget_under(&path);
                        }
                        self.hub.sessions.retain(|_, s| s["source"] != id);
                        self.rows.retain(|k, _| !k.starts_with(&format!("{id}:")));
                        self.contexts
                            .retain(|k, _| !k.starts_with(&format!("{id}:")));
                        if id == "codeg" {
                            // Unregister against the old path before switching configuration.
                            self.stop_codeg_webhook();
                            let url=self.codeg.url.clone();
                            let sink=self.codeg.stream_sink.clone();
                            self.codeg=Default::default();
                            self.codeg.stream_sink=sink;
                            self.configure_codeg_webhook(url);
                        }
                        if id == "codex" {
                            self.live.clear();
                            self.codex_read_state = Default::default();
                            crate::codex_recovery::clear(&self.home);
                        }
                        if id == "codebuddy-ide" {
                            self.ide_live.clear();
                            self.ide_hook_count = 0;
                        }
                        if id == "workbuddy" {
                            self.workbuddy_live.clear();
                            self.workbuddy_hook_count = 0;
                        }
                    }
                }
                self.settings = next;
                Ok(self.settings.clone())
            }
            "settings_check" => {
                let id = payload["source"]
                    .as_str()
                    .filter(|s| settings::SOURCES.contains(s))
                    .ok_or("未知 Agent 来源")?;
                let mut v = self.settings.clone();
                v["sources"][id]["path"] = payload["path"].clone();
                let v = settings::validate(&v)?;
                let paths: Vec<_> = settings::paths(&self.home, &v, id)
                    .into_iter()
                    .filter(|p| {
                        if matches!(id, "codex" | "workbuddy" | "codebuddy-ide") {
                            std::fs::read_dir(p).is_ok()
                        } else {
                            std::fs::File::open(p).is_ok()
                        }
                    })
                    .collect();
                Ok(
                    json!({"ok":!paths.is_empty(),"paths":paths,"detail":if paths.is_empty(){"未找到可读取的数据路径"}else{"路径可读取；会话状态以监听结果为准"}}),
                )
            }
            "custom_integrations_get" => Ok(self.custom_status()),
            "custom_integrations_set" => self.custom_manage(payload),
            "custom_preview" => Ok(self.custom_preview(payload)),
            "custom_hook" => Ok(self.ingest_custom_hook(payload)),
            _ => Err("不支持的命令".into()),
        }
    }
    pub fn paths(&self, id: &str) -> Vec<PathBuf> {
        settings::paths(&self.home, &self.settings, id)
    }
    pub fn presence_mut(
        &mut self,
        source: &str,
        kind: &str,
    ) -> Option<&mut crate::host_process::HostPresence> {
        match (source, kind) {
            ("workbuddy", _) => Some(&mut self.workbuddy_presence),
            ("codebuddy-ide", "vscode") => Some(&mut self.vscode_presence),
            ("codebuddy-ide", _) => Some(&mut self.ide_presence),
            _ => None,
        }
    }
    pub fn integration_automatic(&self, source: &str) -> bool { self.integrations[source] != false }
    pub fn set_integration_automatic(&mut self, source: &str, enabled: bool) -> Result<(), String> {
        let mut next = self.integrations.clone();
        next[source] = json!(enabled);
        atomic_json(&self.home.join(".agent-studio/integrations.json"), &next)?;
        self.integrations = next;
        if source == "codex" && !enabled {
            crate::codex_recovery::clear(&self.home);
            self.hub.sessions.retain(|_, s| s["source"] != "codex");
            self.live.clear();
        }
        Ok(())
    }
    pub fn ingest_hook(&mut self, p: &Value) -> bool {
        let source = hook_agent(p).to_string();
        if !self.integration_automatic(&source) { return false; }
        if source == "codex" && matches!(p["hook_event_name"].as_str().or_else(|| p["hookEventName"].as_str()), Some("SessionStart" | "UserPromptSubmit")) {
            let sid = p["session_id"].as_str().or_else(||p["sessionId"].as_str()).unwrap_or("");
            if self.is_codeg_child_codex(sid) {
                self.hub.hide_codeg_child_codex(sid);
                crate::codex_recovery::remove(&self.home, &self.settings, &self.integrations, sid);
            }
        }
        let accepted = match source.as_str() {
            "codebuddy-ide" => self.ingest_ide_hook(p),
            "workbuddy" => self.ingest_workbuddy_hook(p),
            "codex" => self.ingest_codex_hook(p),
            "codeg" => self.ingest_codeg_hook(p),
            _ => false,
        };
        if accepted { self.last_hook_at.insert(source, now()); }
        accepted
    }
}
fn hook_agent(p: &Value) -> &str {
    // Codex SessionStart already uses `source` for startup/resume/clear.
    // Routing must not treat that as the Agent Studio source.
    match p["agent_source"]
        .as_str()
        .or_else(|| p["source"].as_str())
        .unwrap_or("codex")
    {
        "codebuddy-ide" => "codebuddy-ide",
        "workbuddy" => "workbuddy",
        "codex" => "codex",
        "codeg" => "codeg",
        _ => "codex",
    }
}
