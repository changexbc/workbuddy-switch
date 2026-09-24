//! Webhook discovery plus read-only confirmation streams; no session scans.
//! Credentials at registration; keyed session metadata only after an event.
use super::*;
use crate::{merge, question, questions};
use std::time::{Duration, Instant};
pub const CODEG_EVENTS: [&str; 5] = [
    "user_prompt_sent",
    "question_request",
    "permission_request",
    "turn_complete",
    "error",
];
/// The one-shot startup alignment enumerates at most this many connections.
const CODEG_ALIGN_LIMIT: usize = 64;
#[derive(Default)]
pub struct CodegHooks {
    pub url: String,
    pub registered: bool,
    pub reconciled: bool,
    pub next_attempt: Option<Instant>,
    auth: Option<(u16, String)>,
    connections: HashMap<String, String>,
    ignored_connections: std::collections::HashSet<String>,
    sequence: u64,
    pub stream_sink: Option<super::codeg_stream::Sink>,
    streams: HashMap<String, super::codeg_stream::Stream>,
}
pub fn merge_codeg_webhooks(
    existing: &Value,
    owned: &[String],
    url: &str,
) -> Result<Value, String> {
    let list = existing
        .as_array()
        .ok_or("Codeg Webhook 配置无效，未覆盖")?;
    if list
        .iter()
        .any(|w| !w["url"].is_string() || !w["enabled"].is_boolean())
    {
        return Err("Codeg Webhook 配置无效，未覆盖".into());
    }
    let mut next: Vec<_> = list
        .iter()
        .filter(|w| w["url"] != url && !owned.contains(&text(&w["url"])))
        .cloned()
        .collect();
    if !url.is_empty() {
        next.push(json!({"url":url,"enabled":true}));
    }
    Ok(json!(next))
}
fn owned_urls(saved: &Value) -> Result<Vec<String>, String> {
    saved["owned"]
        .as_array()
        .ok_or("Codeg 注册记录无效")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "Codeg 注册记录无效".to_string())
        })
        .collect()
}
fn post(auth: &(u16, String), method: &str, body: Value) -> Result<Value, String> {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(1500))
        .build()
        .post(&format!("http://127.0.0.1:{}/api/{method}", auth.0))
        .set("Authorization", &format!("Bearer {}", auth.1))
        .send_json(body)
        .map_err(|_| "Codeg Web Service 不可用")?
        .into_json()
        .map_err(|_| "Codeg API 返回无效".to_string())
}
impl Collector {
    fn codeg_credentials(&self) -> Result<(u16, String), String> {
        let db = open(&self.paths("codeg"))?;
        let rows = query(&db,"SELECT key,value FROM app_metadata WHERE key IN ('web_service_port','web_service_token')")?;
        let value = |k: &str| {
            rows.iter()
                .find(|r| r["key"] == k)
                .map(|r| text(&r["value"]))
                .unwrap_or_default()
        };
        let port = value("web_service_port")
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .unwrap_or(3080);
        let token = value("web_service_token");
        if token.is_empty() {
            return Err("请启用 Codeg Web Service".into());
        }
        Ok((port, token))
    }
    pub fn set_codeg_stream_sink(&mut self, sink: super::codeg_stream::Sink) {
        self.codeg.stream_sink = Some(sink);
    }
    pub fn configure_codeg_webhook(&mut self, url: String) {
        self.codeg.url = url;
        self.codeg.registered = false;
        self.codeg.reconciled = false;
        self.codeg.next_attempt = None;
    }
    pub fn maintain_codeg_webhook(&mut self) -> Result<(), String> {
        if self.settings["sources"]["codeg"]["enabled"] != true
            || !self.integration_automatic("codeg")
        {
            self.codeg.streams.clear();
        }
        if self.codeg.url.is_empty() {
            self.hub
                .health("codeg", "partial", "Webhook 接收入口尚未启动");
            return Ok(());
        }
        if self.codeg.registered
            || self
                .codeg
                .next_attempt
                .is_some_and(|at| Instant::now() < at)
        {
            return Ok(());
        }
        self.codeg.next_attempt = Some(Instant::now() + Duration::from_secs(60));
        let enabled = self.settings["sources"]["codeg"]["enabled"] == true
            && self.integration_automatic("codeg");
        let file = self.home.join(".agent-studio/codeg-webhook-native.json");
        let saved: Value = match std::fs::read(&file) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| "Codeg 注册记录无效，未覆盖")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"owned":[]}),
            Err(_) => return Err("Codeg 注册记录不可读".into()),
        };
        let mut owned = owned_urls(&saved)?;
        if !enabled && owned.is_empty() {
            self.codeg.registered = true;
            return Ok(());
        }
        let auth = self.codeg_credentials()?;
        let existing = post(&auth, "get_chat_event_webhooks", json!({}))?;
        let url = if enabled {
            self.codeg.url.clone()
        } else {
            String::new()
        };
        let next = merge_codeg_webhooks(&existing, &owned, &url)?;
        if enabled {
            let filter = post(&auth, "get_chat_event_filter", json!({}))?;
            let mut current: Vec<Value> = if filter.is_null() {
                CODEG_EVENTS[1..].iter().map(|e| json!(e)).collect()
            } else {
                filter
                    .as_array()
                    .filter(|a| a.iter().all(Value::is_string))
                    .cloned()
                    .ok_or("Codeg 事件配置无效")?
            };
            if CODEG_EVENTS.iter().any(|e| !current.contains(&json!(e))) {
                let channels = post(&auth, "list_chat_channels", json!({}))?;
                if channels
                    .as_array()
                    .ok_or("Codeg 推送配置无效")?
                    .iter()
                    .any(|c| c["enabled"] == true)
                    || next
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|w| w["url"] != url && w["enabled"] == true)
                {
                    return Err("Codeg 全局开关影响其他推送目标；请先在 Codeg 启用五类事件".into());
                }
                for e in CODEG_EVENTS {
                    if !current.contains(&json!(e)) {
                        current.push(json!(e));
                    }
                }
                post(&auth, "set_chat_event_filter", json!({"filter":current}))?;
            }
        }
        if !url.is_empty() && !owned.contains(&url) {
            owned.push(url.clone());
        }
        // Persist ownership before sending; uncertain network responses are safe to retry.
        atomic_json(&file, &json!({"owned":owned}))?;
        if existing != next {
            post(&auth, "set_chat_event_webhooks", json!({"webhooks":next}))?;
        }
        let verified = post(&auth, "get_chat_event_webhooks", json!({}))?;
        if verified != next {
            return Err("Codeg 未确认 Webhook 配置，请重试".into());
        }
        atomic_json(
            &file,
            &json!({"owned":if url.is_empty(){vec![]}else{vec![url]}}),
        )?;
        self.codeg.auth = Some(auth);
        self.codeg.registered = true;
        self.hub.health(
            "codeg",
            if enabled { "ok" } else { "disabled" },
            if enabled {
                "Webhook 已注册，等待 Codeg 事件（不扫描会话）"
            } else {
                "已关闭监听"
            },
        );
        // One-shot startup alignment; failures only show on codeg health.
        if enabled && !self.codeg.reconciled {
            self.codeg.reconciled = true;
            if let Err(e) = self.reconcile_codeg_connections() {
                self.hub.health("codeg", "error", &e);
            }
        }
        Ok(())
    }
    pub fn inspect_codeg_webhook(&self) -> Result<(&'static str, String), String> {
        let file = self.home.join(".agent-studio/codeg-webhook-native.json");
        let owned = match std::fs::read(file) {
            Ok(bytes) => {
                serde_json::from_slice::<Value>(&bytes).map_err(|_| "Codeg 注册记录无效")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"owned":[]}),
            Err(e) => return Err(e.to_string()),
        };
        let owned = owned_urls(&owned)?;
        let pending = (!self.integration_automatic("codeg")
            || self.settings["sources"]["codeg"]["enabled"] != true)
            && !owned.is_empty();
        let result = self
            .codeg_credentials()
            .and_then(|auth| post(&auth, "get_chat_event_webhooks", json!({})));
        match result {
            Err(e) => Ok((
                if pending { "pending" } else { "unavailable" },
                if pending {
                    format!("待注销；{e}，请启动 Codeg 后重试")
                } else {
                    e
                },
            )),
            Ok(value) => {
                let list = value.as_array().ok_or("Codeg Webhook 配置无效")?;
                let found = list
                    .iter()
                    .any(|v| v["url"] == self.codeg.url && v["enabled"] == true);
                if pending {
                    Ok(("pending", "接入已暂停；待重试注销".into()))
                } else if found {
                    let filter = post(
                        &self.codeg_credentials()?,
                        "get_chat_event_filter",
                        json!({}),
                    )?;
                    if !CODEG_EVENTS
                        .iter()
                        .all(|e| filter.as_array().is_some_and(|a| a.contains(&json!(e))))
                    {
                        Ok(("partial", "Webhook 已注册，但事件开关不完整，请修复".into()))
                    } else {
                        Ok(("installed", "Webhook 已注册".into()))
                    }
                } else {
                    Ok(("not_installed", "Webhook 未注册；注册需要开启监听".into()))
                }
            }
        }
    }
    pub fn stop_codeg_webhook(&mut self) {
        self.codeg.streams.clear();
        self.settings["sources"]["codeg"]["enabled"] = json!(false);
        self.codeg.registered = false;
        self.codeg.next_attempt = None;
        let _ = self.maintain_codeg_webhook();
    }
    fn codeg_metadata(&self, sid: &str) -> Result<Value, String> {
        let db = open(&self.paths("codeg"))?;
        // Parameter binding: the callback never supplies SQL identifiers or SQL text.
        let table = if db.prepare("SELECT id FROM conversation LIMIT 0").is_ok() {
            "conversation"
        } else {
            "conversations"
        };
        let mut st = db
            .prepare(&format!("SELECT * FROM {table} WHERE id=?1"))
            .map_err(|_| "Codeg 会话表不可读")?;
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let raw = st
            .query_row([sid], |r| {
                let mut out = serde_json::Map::new();
                for (i, n) in names.iter().enumerate() {
                    let v = match r.get_ref(i)? {
                        ValueRef::Text(b) => json!(String::from_utf8_lossy(b)),
                        ValueRef::Integer(n) => json!(n),
                        _ => Value::Null,
                    };
                    out.insert(n.clone(), v);
                }
                Ok(Value::Object(out))
            })
            .map_err(|_| "Codeg 会话未找到")?;
        let get = |keys: &[&str]| {
            keys.iter()
                .map(|k| raw[*k].clone())
                .find(|v| !v.is_null() && !text(v).is_empty())
                .unwrap_or(Value::Null)
        };
        let mut cwd = get(&["origin_cwd", "cwd", "workspace"]);
        if text(&cwd).is_empty() {
            if let Ok(p) = db.query_row(
                "SELECT path FROM folder WHERE id=?1",
                [text(&raw["folder_id"])],
                |r| r.get::<_, String>(0),
            ) {
                cwd = json!(p);
            }
        }
        Ok(
            json!({"cwd":cwd,"title":raw["title"],"agentType":get(&["agent_type","agent"]),"externalId":raw["external_id"],"folderId":raw["folder_id"],"isSubagent":!raw["parent_id"].is_null() || raw["kind"] == "delegate"}),
        )
    }
    pub fn ingest_codeg_hook(&mut self, p: &Value) -> bool {
        if self.settings["sources"]["codeg"]["enabled"] != true
            || !self.integration_automatic("codeg")
            || p["source"] != "codeg"
        {
            return false;
        }
        let event = text(&p["event"]);
        let conn = text(&p["connection_id"]);
        if !CODEG_EVENTS.contains(&event.as_str()) || conn.trim().is_empty() || conn.len() > 256 {
            return false;
        }
        if self.codeg.ignored_connections.contains(&conn) {
            return true;
        }
        if self.codeg.auth.is_none() {
            self.codeg.auth = self.codeg_credentials().ok();
        }
        let snap = self
            .codeg
            .auth
            .as_ref()
            .and_then(|a| post(a, "acp_get_session_snapshot", json!({"connectionId":conn})).ok())
            .unwrap_or(Value::Null);
        let provisional = format!("connection:{conn}");
        let sid = if !snap["conversation_id"].is_null() {
            text(&snap["conversation_id"])
        } else {
            self.codeg
                .connections
                .get(&conn)
                .cloned()
                .unwrap_or(provisional.clone())
        };
        if sid != provisional {
            self.codeg.connections.insert(conn.clone(), sid.clone());
            self.hub.sessions.remove(&format!("codeg:{provisional}"));
        }
        if self.codeg.connections.len() > 512 {
            if let Some(k) = self.codeg.connections.keys().next().cloned() {
                self.codeg.connections.remove(&k);
            }
        }
        let meta = if sid != provisional {
            self.codeg_metadata(&sid).unwrap_or(json!({}))
        } else {
            json!({})
        };
        if meta["isSubagent"] == true {
            // Keep known children ignored during subsequent API/database outages.
            self.codeg.streams.remove(&conn);
            self.codeg.ignored_connections.insert(conn);
            self.hub.sessions.remove(&format!("codeg:{provisional}"));
            self.hub.sessions.remove(&format!("codeg:{sid}"));
            return true;
        }
        if self.codeg.streams.get(&conn).is_some_and(|s| s.sid != sid) {
            self.codeg.streams.remove(&conn);
        }
        // HTTP snapshots and webhook deliveries can lag behind the live stream.
        if let Some(stream) = self.codeg.streams.get(&conn) {
            let cursor = stream.seq.load(std::sync::atomic::Ordering::Acquire);
            if cursor != u64::MAX {
                if snap["event_seq"].as_u64().is_some_and(|seq| seq < cursor) {
                    return true;
                }
                if event == "user_prompt_sent"
                    && snap["status"]
                        .as_str()
                        .is_some_and(|status| status != "prompting")
                    && self
                        .hub
                        .sessions
                        .get(&format!("codeg:{sid}"))
                        .is_some_and(|session| crate::hub::terminal(&text(&session["status"])))
                {
                    return true;
                }
            }
        }
        if matches!(event.as_str(), "question_request" | "permission_request")
            && self
                .codeg
                .streams
                .get(&conn)
                .is_some_and(|s| s.seq.load(std::sync::atomic::Ordering::Acquire) != u64::MAX)
        {
            return true;
        }
        if event == "turn_complete" && snap["status"] == "prompting" {
            return true;
        }
        let ts = now();
        self.codeg.sequence += 1;
        let seq = self.codeg.sequence;
        let k = format!("codeg:{sid}");
        let start_title = (event == "user_prompt_sent" || self.hub.sessions.get(&k).is_none())
            .then(|| text(&p["body"]));
        let base = self.codeg_seed_session(&sid, &snap, &meta, ts, seq, start_title.as_deref());
        if matches!(event.as_str(), "question_request" | "permission_request")
            && !crate::hub::terminal(&text(&self.hub.sessions[&k]["status"]))
        {
            let ask = if !snap["pending_question"].is_null() {
                snap["pending_question"].clone()
            } else if !snap["pending_plan_approval"].is_null() {
                json!({"approval_id":snap["pending_plan_approval"]["approval_id"],"questions":[{"question":snap["pending_plan_approval"]["plan_markdown"],"options":[{"label":"批准"},{"label":"拒绝"}]}]})
            } else if !snap["pending_permission"].is_null() {
                let a = &snap["pending_permission"];
                json!({"request_id":a["request_id"],"questions":[{"question":a["tool_call"]["title"].as_str().unwrap_or("需要你的许可"),"options":a["options"].as_array().into_iter().flatten().map(|o|json!({"label":o["name"],"description":o["kind"]})).collect::<Vec<_>>()}]})
            } else {
                Value::Null
            };
            let pending = self.hub.sessions[&k]["pending"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for item in pending {
                self.hub.ingest(merge(
                    base.clone(),
                    json!({"type":"resolve","callId":item["id"]}),
                ));
            }
            let fields = p["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| text(&f["value"]))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let message = if !ask.is_null() {
                question(&ask)
            } else if !fields.is_empty() {
                fields
            } else {
                text(&p["body"])
            };
            let call = ask["question_id"]
                .as_str()
                .or(ask["request_id"].as_str())
                .or(ask["approval_id"].as_str())
                .map(str::to_owned)
                .unwrap_or(format!("codeg:{conn}:{seq}"));
            self.hub.ingest(merge(base,json!({"type":"wait","callId":call,"tool":if event=="question_request"{"ask"}else{"permission"},"text":message,"questions":questions(&ask)})));
        } else if matches!(event.as_str(), "turn_complete" | "error") {
            self.hub.ingest(merge(
                base,
                json!({"type":"end","status":if event=="error"{"error"}else{"done"}}),
            ));
        }
        if snap["status"] == "prompting" {
            self.reconcile_codeg_snapshot(&sid, &snap, false);
        }
        self.attach_codeg_stream(&conn, &sid);
        self.codeg_sync_stream_cursor(&conn, &snap);
        self.hub.health(
            "codeg",
            "ok",
            "已收到 Codeg Webhook；确认状态通过实时事件同步",
        );
        true
    }
}

fn stream_ask(kind: &str, value: &Value) -> Value {
    match kind {
        "pending_question" => value.clone(),
        "pending_plan_approval" => {
            json!({"approval_id":value["approval_id"],"questions":[{"question":value["plan_markdown"],"options":[{"label":"批准"},{"label":"拒绝"}]}]})
        }
        _ => {
            json!({"request_id":value["request_id"],"questions":[{"question":value["tool_call"]["title"].as_str().or(value["tool_call"]["name"].as_str()).unwrap_or("需要你的许可"),"options":value["options"].as_array().into_iter().flatten().map(|o| json!({"label":o["name"].as_str().or(o["label"].as_str()).unwrap_or(""),"description":o["kind"]})).collect::<Vec<_>>()}]})
        }
    }
}
fn request_id(value: &Value) -> Option<&str> {
    value["question_id"]
        .as_str()
        .or(value["request_id"].as_str())
        .or(value["approval_id"].as_str())
        .filter(|s| !s.is_empty())
}
impl Collector {
    /// One-shot alignment at startup or integration enablement: enumerate live
    /// connections, then re-seed only the in-flight ones, because a session that
    /// waits for a delegated child emits no events at all. Never called again;
    /// runtime stays purely event-driven.
    fn reconcile_codeg_connections(&mut self) -> Result<(), String> {
        let auth = self
            .codeg
            .auth
            .clone()
            .ok_or_else(|| "Codeg Web Service 不可用".to_string())?;
        let listed = post(&auth, "acp_list_connections", json!({}))?;
        let rows = listed
            .as_array()
            .cloned()
            .ok_or("Codeg 连接列表无效".to_string())?;
        let mut attempted = 0usize;
        let mut failed = 0usize;
        for row in rows.iter().take(CODEG_ALIGN_LIMIT) {
            let conn = text(&row["id"]);
            if conn.trim().is_empty() || conn.len() > 256 {
                continue;
            }
            attempted += 1;
            let snap = match post(&auth, "acp_get_session_snapshot", json!({"connectionId":conn})) {
                Ok(snap) => snap,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let sid = text(&snap["conversation_id"]);
            if sid.is_empty() {
                continue;
            }
            let meta = self.codeg_metadata(&sid).unwrap_or(json!({}));
            if meta["isSubagent"] == true {
                continue;
            }
            // A webhook may have won the race; never start a round twice or
            // revive the one the hub already finished.
            if self.hub.sessions.contains_key(&format!("codeg:{sid}")) {
                continue;
            }
            let pending = [
                "pending_question",
                "pending_permission",
                "pending_plan_approval",
            ]
            .iter()
            .any(|key| !snap[*key].is_null());
            if snap["status"] != "prompting" && !pending {
                continue;
            }
            self.codeg.connections.insert(conn.clone(), sid.clone());
            let ts = now();
            self.codeg.sequence += 1;
            let seq = self.codeg.sequence;
            self.codeg_seed_session(&sid, &snap, &meta, ts, seq, Some(""));
            self.reconcile_codeg_snapshot(&sid, &snap, false);
            self.attach_codeg_stream(&conn, &sid);
            self.codeg_sync_stream_cursor(&conn, &snap);
        }
        // One health write: a second note must not erase the first.
        let mut notes = Vec::new();
        if attempted > 0 && failed == attempted {
            notes.push("启动对齐未能读取会话快照");
        }
        if rows.len() > CODEG_ALIGN_LIMIT {
            notes.push("Codeg 连接数超出上限，仅对齐前 64 条");
        }
        if !notes.is_empty() {
            self.hub.health("codeg", "partial", &notes.join("；"));
        }
        Ok(())
    }
    /// Shared by the webhook path and the startup alignment so both emit the
    /// same `start` shape and round id. `start_body` only fills in when the
    /// session metadata has no title.
    fn codeg_seed_session(
        &mut self,
        sid: &str,
        snap: &Value,
        meta: &Value,
        ts: i64,
        seq: u64,
        start_body: Option<&str>,
    ) -> Value {
        let mut base = merge(
            meta.clone(),
            json!({"source":"codeg","sessionId":sid,"ts":ts}),
        );
        if !snap["external_id"].is_null() {
            base["externalId"] = snap["external_id"].clone();
        }
        if !snap["folder_id"].is_null() {
            base["folderId"] = snap["folder_id"].clone();
        }
        if let Some(body) = start_body {
            let title = if text(&base["title"]).is_empty() {
                body.to_string()
            } else {
                text(&base["title"])
            };
            self.hub.ingest(merge(
                base.clone(),
                json!({"type":"start","roundId":format!("hook:{ts}:{seq}"),"title":title}),
            ));
        }
        base
    }
    fn codeg_sync_stream_cursor(&mut self, conn: &str, snap: &Value) {
        if let (Some(seq), Some(stream)) =
            (snap["event_seq"].as_u64(), self.codeg.streams.get(conn))
        {
            let old = stream.seq.load(std::sync::atomic::Ordering::Acquire);
            stream.seq.store(
                if old == u64::MAX { seq } else { old.max(seq) },
                std::sync::atomic::Ordering::Release,
            );
        }
    }
    fn attach_codeg_stream(&mut self, conn: &str, sid: &str) {
        if self.codeg.streams.get(conn).is_some_and(|s| s.sid == sid) {
            return;
        }
        let (Some(auth), Some(sink)) = (self.codeg.auth.clone(), self.codeg.stream_sink.clone())
        else {
            return;
        };
        if self.codeg.streams.len() >= 512 {
            if let Some(key) = self.codeg.streams.keys().next().cloned() {
                self.codeg.streams.remove(&key);
            }
        }
        self.codeg.sequence += 1;
        let subscription = format!("companion-{}-{}", now(), self.codeg.sequence);
        self.codeg.streams.insert(
            conn.into(),
            super::codeg_stream::Stream::new(conn.into(), sid.into(), subscription, auth, sink),
        );
    }
    fn codeg_send(&mut self, sid: &str, event: Value) {
        self.hub.ingest(merge(
            json!({"source":"codeg","sessionId":sid,"ts":now()}),
            event,
        ));
    }
    fn reconcile_codeg_snapshot(&mut self, sid: &str, snap: &Value, authoritative: bool) {
        let Some(session) = self.hub.sessions.get(&format!("codeg:{sid}")) else {
            return;
        };
        if crate::hub::terminal(&text(&session["status"])) {
            return;
        }
        let asks: Vec<_> = [
            "pending_question",
            "pending_permission",
            "pending_plan_approval",
        ]
        .iter()
        .filter_map(|kind| {
            let ask = stream_ask(kind, &snap[*kind]);
            request_id(&ask).map(|id| {
                (
                    id.to_string(),
                    ask.clone(),
                    if *kind == "pending_question" {
                        "ask"
                    } else {
                        "permission"
                    },
                )
            })
        })
        .collect();
        if authoritative
            && snap["status"] == "connected"
            && [
                "pending_question",
                "pending_permission",
                "pending_plan_approval",
            ]
            .iter()
            .all(|key| snap[*key].is_null())
        {
            self.codeg_send(sid, json!({"type":"end","status":"done"}));
            return;
        }
        let pending = session["pending"].as_array().cloned().unwrap_or_default();
        if snap["status"] == "prompting" {
            for p in pending {
                if !asks.iter().any(|(id, _, _)| p["id"] == *id) {
                    self.codeg_send(sid, json!({"type":"resolve","callId":p["id"]}));
                }
            }
        }
        for (id, ask, tool) in asks {
            self.codeg_send(sid,json!({"type":"wait","callId":id,"tool":tool,"text":question(&ask),"questions":questions(&ask)}));
        }
    }
    fn apply_codeg_envelope(&mut self, sid: &str, envelope: &Value) {
        let Some(session) = self.hub.sessions.get(&format!("codeg:{sid}")) else {
            return;
        };
        if crate::hub::terminal(&text(&session["status"])) {
            return;
        }
        let kind = text(&envelope["type"]);
        if matches!(
            kind.as_str(),
            "question_resolved" | "permission_resolved" | "plan_approval_resolved"
        ) {
            if let Some(id) = request_id(envelope) {
                self.codeg_send(sid, json!({"type":"resolve","callId":id}));
            }
        } else if matches!(
            kind.as_str(),
            "question_request" | "permission_request" | "plan_approval_request"
        ) {
            let key = match kind.as_str() {
                "question_request" => "pending_question",
                "permission_request" => "pending_permission",
                _ => "pending_plan_approval",
            };
            let ask = stream_ask(key, envelope);
            if let Some(id) = request_id(envelope) {
                self.codeg_send(sid,json!({"type":"wait","callId":id,"tool":if key=="pending_question"{"ask"}else{"permission"},"text":question(&ask),"questions":questions(&ask)}));
            }
        } else if kind == "turn_complete" {
            self.codeg_send(sid, json!({"type":"end","status":"done"}));
        }
    }
    pub fn ingest_codeg_stream(&mut self, frame: &Value) -> bool {
        use std::sync::atomic::Ordering;
        if self.settings["sources"]["codeg"]["enabled"] != true
            || !self.integration_automatic("codeg")
        {
            return false;
        }
        let conn = text(&frame["connection_id"]);
        let Some(stream) = self.codeg.streams.get(&conn) else {
            return false;
        };
        if frame["subscription_id"] != stream.subscription {
            return false;
        }
        let sid = stream.sid.clone();
        let cursor = stream.seq.clone();
        let mut changed = false;
        match frame["type"].as_str() {
            Some("snapshot") => {
                if let Some(seq) = frame["event_seq"].as_u64() {
                    let old = cursor.load(Ordering::Acquire);
                    if old != u64::MAX && seq < old {
                        return false;
                    }
                    changed = true;
                    cursor.store(seq, Ordering::Release);
                    self.reconcile_codeg_snapshot(&sid, &frame["snapshot"], true);
                }
            }
            Some("event" | "replay") => {
                let envelopes = if frame["type"] == "event" {
                    vec![frame["envelope"].clone()]
                } else {
                    frame["events"].as_array().cloned().unwrap_or_default()
                };
                for envelope in envelopes {
                    let Some(seq) = envelope["seq"].as_u64() else {
                        continue;
                    };
                    let old = cursor.load(Ordering::Acquire);
                    if envelope["connection_id"] != conn || (old != u64::MAX && seq <= old) {
                        continue;
                    }
                    changed |= matches!(
                        envelope["type"].as_str(),
                        Some(
                            "question_request"
                                | "permission_request"
                                | "plan_approval_request"
                                | "question_resolved"
                                | "permission_resolved"
                                | "plan_approval_resolved"
                                | "turn_complete"
                        )
                    );
                    cursor.store(seq, Ordering::Release);
                    self.apply_codeg_envelope(&sid, &envelope);
                }
                if frame["type"] == "replay" {
                    if let Some(high) = frame["high_water_seq"].as_u64() {
                        let old = cursor.load(Ordering::Acquire);
                        cursor.store(
                            if old == u64::MAX { high } else { old.max(high) },
                            Ordering::Release,
                        );
                    }
                }
            }
            Some("detached") => {
                self.codeg.streams.remove(&conn);
            }
            _ => return false,
        }
        changed
    }
}
