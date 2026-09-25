//! Webhook discovery plus read-only confirmation streams; no session scans.
//! Credentials at registration; keyed session metadata only after an event.
use super::*;
use crate::{merge, question, questions};
use std::collections::VecDeque;
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
/// A delegated child never keeps its temporary card longer than this without
/// an answer; a cancelled child emits no signal at all, so this is the floor.
const CODEG_CHILD_TTL: i64 = 60 * 60 * 1000;
/// Answered request ids a child remembers against late duplicate webhooks.
const CODEG_CHILD_ANSWERED: usize = 8;
/// Child titles are raw prompt fragments; the rail gets one capped line.
const CODEG_CHILD_TITLE: usize = 80;
/// A known delegated child. It stays invisible unless it waits for the user,
/// so this only carries what it takes to ignore its traffic cheaply and to
/// release the temporary card again.
#[derive(Default)]
struct ChildState {
    /// Conversation id of the child; always resolved, never provisional.
    sid: String,
    /// Request id of the card on screen, if any.
    open: Option<String>,
    /// When that card was created, for the TTL sweep.
    started_at: i64,
    /// Recently answered request ids; a late duplicate must not revive them.
    resolved: VecDeque<String>,
}
#[derive(Default)]
pub struct CodegHooks {
    pub url: String,
    pub registered: bool,
    pub reconciled: bool,
    pub next_attempt: Option<Instant>,
    auth: Option<(u16, String)>,
    connections: HashMap<String, String>,
    children: HashMap<String, ChildState>,
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
            // Disabling releases every temporary child card with its stream.
            self.codeg_release_children(None);
        } else {
            // A cancelled or silently finished child emits no signal at all.
            self.codeg_release_children(Some(CODEG_CHILD_TTL));
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
        self.hub.hidden_codeg_codex_ids.clear();
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
        let mut meta = json!({"cwd":cwd,"title":raw["title"],"agentType":get(&["agent_type","agent"]),"externalId":raw["external_id"],"folderId":raw["folder_id"],"isSubagent":!raw["parent_id"].is_null() || raw["kind"] == "delegate"});
        if meta["isSubagent"] == true && !raw["parent_id"].is_null() {
            // One extra keyed lookup: the badge names the session the child was
            // delegated from. An unreadable parent only drops that detail.
            let mut st = db
                .prepare(&format!("SELECT title FROM {table} WHERE id=?1"))
                .map_err(|_| "Codeg 会话表不可读")?;
            if let Ok(Some(title)) = st.query_row([text(&raw["parent_id"])], |r| {
                r.get::<_, Option<String>>(0)
            }) {
                if !title.trim().is_empty() {
                    meta["parentTitle"] = json!(codeg_title(&title));
                }
            }
        }
        Ok(meta)
    }
    pub fn is_codeg_child_codex(&self, external_id: &str) -> bool {
        if self.settings["sources"]["codeg"]["enabled"] != true || !self.integration_automatic("codeg") || external_id.is_empty() { return false; }
        let Ok(db) = open(&self.paths("codeg")) else { return false; };
        let table = if db.prepare("SELECT id FROM conversation LIMIT 0").is_ok() { "conversation" } else { "conversations" };
        let id = external_id.strip_prefix("thr_").unwrap_or(external_id);
        let Ok(mut st) = db.prepare(&format!("SELECT id FROM {table} WHERE external_id IN (?1, ?2) LIMIT 1")) else { return false; };
        let Ok(sid) = st.query_row(rusqlite::params![id, format!("thr_{id}")], |r| r.get::<_, i64>(0)) else { return false; };
        let Ok(meta) = self.codeg_metadata(&sid.to_string()) else { return false; };
        text(&meta["agentType"]).eq_ignore_ascii_case("codex") && meta["isSubagent"] == true
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
        let request = matches!(event.as_str(), "question_request" | "permission_request");
        let known_sid = self.codeg.children.get(&conn).map(|child| child.sid.clone());
        // A known child costs nothing until it waits for the user: no snapshot,
        // no metadata, no session. Only the end of its turn releases the card.
        if known_sid.is_some() && !request {
            let open = self
                .codeg
                .children
                .get(&conn)
                .is_some_and(|child| child.open.is_some());
            if open && matches!(event.as_str(), "turn_complete" | "error") {
                self.codeg_release_child(&conn, None);
            }
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
        } else if let Some(sid) = known_sid.clone() {
            sid
        } else {
            self.codeg
                .connections
                .get(&conn)
                .cloned()
                .unwrap_or(provisional.clone())
        };
        // A silently finished child keeps no conversation id; seeding a session
        // here would leave a `done` ghost for a task that never needed a user.
        if sid == provisional
            && matches!(event.as_str(), "turn_complete" | "error")
            && !self.hub.sessions.contains_key(&format!("codeg:{sid}"))
        {
            return true;
        }
        if sid != provisional {
            self.codeg.connections.insert(conn.clone(), sid.clone());
            self.hub.sessions.remove(&format!("codeg:{provisional}"));
        }
        if self.codeg.connections.len() > 512 {
            if let Some(k) = self.codeg.connections.keys().next().cloned() {
                self.codeg.connections.remove(&k);
            }
        }
        let mut meta = if sid != provisional {
            self.codeg_metadata(&sid).unwrap_or(json!({}))
        } else {
            json!({})
        };
        if meta["isSubagent"] == true || known_sid.is_some() {
            if text(&meta["agentType"]).eq_ignore_ascii_case("codex") {
                let external_id = if text(&snap["external_id"]).is_empty() { text(&meta["externalId"]) } else { text(&snap["external_id"]) };
                self.hub.hide_codeg_child_codex(&external_id);
            }
            self.codeg.children.entry(conn.clone()).or_default().sid = sid.clone();
            if !request {
                // Keep known children ignored during API/database outages; only a
                // waiting request buys a child a temporary card.
                if self
                    .codeg
                    .children
                    .get(&conn)
                    .is_some_and(|child| child.open.is_some())
                {
                    self.codeg_release_child(&conn, None);
                }
                self.codeg.streams.remove(&conn);
                self.hub.sessions.remove(&format!("codeg:{provisional}"));
                self.hub.sessions.remove(&format!("codeg:{sid}"));
                return true;
            }
            self.codeg_child_wait(&conn, &sid, &snap, &mut meta, &p);
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
        let key = format!("codeg:{sid}");
        if self.closed_monitor_sessions.contains(&key) {
            if matches!(event.as_str(), "turn_complete" | "error") { return true; }
            self.closed_monitor_sessions.remove(&key);
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
            let ask = codeg_ask(&snap);
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
            let call = request_id(&ask)
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
            self.reconcile_codeg_snapshot(&conn, &sid, &snap, false);
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
/// The request a snapshot is waiting on, in the precedence every path shares.
/// It parses each pending field with the same helper the stream frames use, so a
/// snapshot and an envelope carrying one request can never describe it
/// differently — the Node twin parses both shapes in one function for the same
/// reason.
fn codeg_ask(snap: &Value) -> Value {
    if !snap["pending_question"].is_null() {
        stream_ask("pending_question", &snap["pending_question"])
    } else if !snap["pending_plan_approval"].is_null() {
        stream_ask("pending_plan_approval", &snap["pending_plan_approval"])
    } else if !snap["pending_permission"].is_null() {
        stream_ask("pending_permission", &snap["pending_permission"])
    } else {
        Value::Null
    }
}
/// Whether a snapshot still carries any pending request.
fn codeg_pending(snap: &Value) -> bool {
    ["pending_question", "pending_permission", "pending_plan_approval"]
        .iter()
        .any(|key| !snap[*key].is_null())
}
/// A child title is a raw prompt fragment: collapse its whitespace so the rail
/// keeps one line, and cap what it has to render.
fn codeg_title(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(CODEG_CHILD_TITLE)
        .collect()
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
                if text(&meta["agentType"]).eq_ignore_ascii_case("codex") {
                    let external_id = if text(&snap["external_id"]).is_empty() { text(&meta["externalId"]) } else { text(&snap["external_id"]) };
                    self.hub.hide_codeg_child_codex(&external_id);
                }
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
            self.reconcile_codeg_snapshot(&conn, &sid, &snap, false);
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
    /// A child shows a temporary card only while a request waits for the user.
    /// The read-only stream is the only source of the answer and a detached
    /// connection can never be attached again, so it is subscribed in this same
    /// pass — never after the fact.
    fn codeg_child_wait(&mut self, conn: &str, sid: &str, snap: &Value, meta: &mut Value, p: &Value) {
        let ask = codeg_ask(snap);
        let Some(call) = request_id(&ask).map(str::to_owned) else {
            // Without the request id in the snapshot no answer could ever be
            // matched, so nothing is shown at all.
            return;
        };
        if self.codeg.children.get(conn).is_some_and(|child| {
            child.resolved.iter().any(|id| *id == call)
                || child.open.as_deref() == Some(call.as_str())
        }) {
            // Already answered, or already on screen: the stream owns the state
            // and a delayed webhook must not rewind it.
            return;
        }
        meta["title"] = json!(codeg_title(&text(&meta["title"])));
        meta["subagent"] = json!(true);
        let ts = now();
        self.codeg.sequence += 1;
        let seq = self.codeg.sequence;
        let key = format!("codeg:{sid}");
        let start_title = self
            .hub
            .sessions
            .get(&key)
            .is_none()
            .then(|| text(&p["body"]));
        let base = self.codeg_seed_session(sid, snap, meta, ts, seq, start_title.as_deref());
        let pending = self.hub.sessions[&key]["pending"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for item in pending {
            self.hub.ingest(merge(
                base.clone(),
                json!({"type":"resolve","callId":item["id"]}),
            ));
        }
        self.hub.ingest(merge(
            base,
            json!({"type":"wait","callId":call,"tool":if p["event"] == "question_request" {"ask"} else {"permission"},"text":question(&ask),"questions":questions(&ask)}),
        ));
        if let Some(child) = self.codeg.children.get_mut(conn) {
            child.open = Some(call);
            child.started_at = ts;
        }
        self.attach_codeg_stream(conn, sid);
        self.codeg_sync_stream_cursor(conn, snap);
        self.hub.health(
            "codeg",
            "ok",
            "已收到 Codeg Webhook；确认状态通过实时事件同步",
        );
    }
    /// Give up a child's temporary card: the hub session goes away, the stream
    /// is released (dropping it stops its socket and retry loop) and the
    /// answered request is remembered so a late duplicate cannot revive it.
    fn codeg_release_child(&mut self, conn: &str, answered: Option<&str>) {
        let Some(child) = self.codeg.children.get_mut(conn) else {
            return;
        };
        let open = child.open.take();
        child.started_at = 0;
        for id in [answered, open.as_deref()].into_iter().flatten() {
            if child.resolved.iter().any(|known| known == id) {
                continue;
            }
            if child.resolved.len() >= CODEG_CHILD_ANSWERED {
                child.resolved.pop_front();
            }
            child.resolved.push_back(id.to_owned());
        }
        let sid = child.sid.clone();
        self.codeg.streams.remove(conn);
        self.hub.sessions.remove(&format!("codeg:{sid}"));
    }
    /// Release cards past their TTL, or every card once the source is disabled.
    /// The known-child list itself survives while the source stays enabled, so
    /// delegated traffic keeps costing nothing.
    fn codeg_release_children(&mut self, ttl: Option<i64>) {
        let time = now();
        let release: Vec<String> = self
            .codeg
            .children
            .iter()
            .filter(|(_, child)| {
                child.open.is_some() && ttl.is_none_or(|ttl| time - child.started_at > ttl)
            })
            .map(|(conn, _)| conn.clone())
            .collect();
        for conn in release {
            self.codeg_release_child(&conn, None);
        }
        if ttl.is_none() {
            self.codeg.children.clear();
        }
    }
    fn reconcile_codeg_snapshot(
        &mut self,
        conn: &str,
        sid: &str,
        snap: &Value,
        authoritative: bool,
    ) {
        // A child card lives exactly as long as the stream still shows a pending
        // request. An authoritative snapshot without one releases it, which
        // covers an answer that landed while the webhook was still in flight.
        if authoritative
            && self
                .codeg
                .children
                .get(conn)
                .is_some_and(|child| child.open.is_some())
            && (snap["conversation_id"].is_null() || text(&snap["conversation_id"]) == sid)
            && !codeg_pending(snap)
        {
            self.codeg_release_child(conn, None);
            return;
        }
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
    fn apply_codeg_envelope(&mut self, conn: &str, sid: &str, envelope: &Value) {
        let kind = text(&envelope["type"]);
        let resolved = matches!(
            kind.as_str(),
            "question_resolved" | "permission_resolved" | "plan_approval_resolved"
        );
        let live = self
            .hub
            .sessions
            .get(&format!("codeg:{sid}"))
            .is_some_and(|session| !crate::hub::terminal(&text(&session["status"])));
        // A child gives its card up on the authoritative answer or on the end of
        // its turn, whether or not the hub still holds the session.
        if (resolved || kind == "turn_complete") && self.codeg.children.contains_key(conn) {
            let answered = if resolved { request_id(envelope) } else { None };
            if live {
                if let Some(id) = answered {
                    self.codeg_send(sid, json!({"type":"resolve","callId":id}));
                } else {
                    self.codeg_send(sid, json!({"type":"end","status":"done"}));
                }
            }
            self.codeg_release_child(conn, answered);
            return;
        }
        if !live {
            return;
        }
        if resolved {
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
                    self.reconcile_codeg_snapshot(&conn, &sid, &frame["snapshot"], true);
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
                    self.apply_codeg_envelope(&conn, &sid, &envelope);
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

#[cfg(test)]
mod child_card_tests {
    use super::*;
    use crate::settings;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    struct Home(std::path::PathBuf);
    impl Home {
        fn new() -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "codeg-child-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Minimal loopback Codeg API: only the keyed snapshot the adapter asks for
    /// after a webhook. Unlisted connections answer `null`, like a connection
    /// that is already gone.
    struct Api {
        port: u16,
        stop: Arc<AtomicBool>,
        snapshots: Arc<Mutex<HashMap<String, Value>>>,
        calls: Arc<Mutex<Vec<String>>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Api {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let snapshots: Arc<Mutex<HashMap<String, Value>>> = Default::default();
            let calls: Arc<Mutex<Vec<String>>> = Default::default();
            let worker = {
                let (stop, snapshots, calls) = (stop.clone(), snapshots.clone(), calls.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let Ok((mut stream, _)) = listener.accept() else {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        };
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let head_end = loop {
                            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break Some(i + 4);
                            }
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break None,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };
                        let Some(head_end) = head_end else { continue };
                        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().ok())?
                            })
                            .unwrap_or(0);
                        while buf.len() < head_end + length {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                        let method = head
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .trim_start_matches("/api/")
                            .to_string();
                        let payload = if method == "acp_get_session_snapshot" {
                            calls.lock().unwrap().push(method);
                            snapshots
                                .lock()
                                .unwrap()
                                .get(body["connectionId"].as_str().unwrap_or(""))
                                .cloned()
                                .unwrap_or(Value::Null)
                        } else {
                            Value::Null
                        };
                        let payload = payload.to_string();
                        let _ = write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len());
                        let _ = stream.flush();
                    }
                })
            };
            Self { port, stop, snapshots, calls, worker: Some(worker) }
        }
        fn set(&self, conn: &str, snapshot: Value) {
            self.snapshots.lock().unwrap().insert(conn.into(), snapshot);
        }
        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }
    impl Drop for Api {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// Codeg home whose database holds a parent task and three delegated
    /// children: a delegate, a regular child of the parent and a `kind=delegate`
    /// task without a parent row.
    fn collector(home: &Home, port: u16) -> Collector {
        let mut s = settings::defaults();
        for id in settings::SOURCES {
            s["sources"][id]["enabled"] = json!(id == "codeg");
        }
        crate::atomic_json(&home.0.join(".agent-studio/settings.json"), &s).unwrap();
        let dir = home.0.join("Library/Application Support/app.codeg");
        std::fs::create_dir_all(&dir).unwrap();
        let db = rusqlite::Connection::open(dir.join("codeg.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE conversation(id INTEGER,title TEXT,agent_type TEXT,external_id TEXT,folder_id INTEGER,status TEXT,parent_id INTEGER,kind TEXT);
             CREATE TABLE folder(id INTEGER,path TEXT);
             INSERT INTO conversation VALUES(214,'Build feature','codex','thr-native',1,'in_progress',NULL,'regular');
             INSERT INTO conversation VALUES(215,'Child task\n<recommended_plugins> Here','codex','thr-child',1,'in_progress',214,'delegate');
             INSERT INTO conversation VALUES(217,'Second child','codex','thr-second',1,'in_progress',214,'regular');
             INSERT INTO conversation VALUES(218,'Root child','codex','thr-root',1,'in_progress',NULL,'delegate');
             INSERT INTO folder VALUES(1,'/project/test');",
        )
        .unwrap();
        db.execute(
            "INSERT INTO conversation VALUES(219,?1,'codex','thr-long',1,'in_progress',214,'delegate')",
            ["word ".repeat(30).trim()],
        )
        .unwrap();
        drop(db);
        let mut c = Collector::new(home.0.clone()).unwrap();
        c.codeg.auth = Some((port, "test-token".into()));
        c.set_codeg_stream_sink(Arc::new(|_| {}));
        c
    }

    /// The snapshot a waiting child reports: its conversation, still prompting,
    /// with one permission request.
    fn pending(sid: i64, request: &str) -> Value {
        json!({"conversation_id":sid,"status":"prompting","event_seq":5,"pending_permission":{"request_id":request,"tool_call":{"title":"Allow shell?"},"options":[{"name":"允许","kind":"allow_once"}]}})
    }

    /// A child runs invisibly, surfaces exactly one temporary card while it
    /// waits for the user and gives that card up with the answer.
    #[test]
    fn codeg_child_requests_surface_a_temporary_card_until_answered() {
        let home = Home::new();
        let api = Api::start();
        api.set("connection-1", json!({"conversation_id":214,"external_id":"thr-native","folder_id":1,"status":"prompting"}));
        api.set("child", json!({"conversation_id":215,"external_id":"thr-child","folder_id":1,"status":"prompting"}));
        let mut c = collector(&home, api.port);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"connection-1","event":"user_prompt_sent","body":"Build"})));
        let parent = c.hub.sessions["codeg:214"].clone();
        // Running children never reach the rail, whatever their metadata says.
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"user_prompt_sent","body":"Child"})));
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        let calls = api.calls();
        api.set("child", Value::Null);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"user_prompt_sent"})));
        assert_eq!(api.calls(), calls, "a known child never queries the API again");
        // The request surfaces the card, its parent and the child's stream.
        api.set("child", pending(215, "p1"));
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        let child = c.hub.sessions["codeg:215"].clone();
        assert_eq!(child["status"], "wait");
        assert_eq!(child["subagent"], true);
        assert_eq!(child["parentTitle"], "Build feature");
        assert_eq!(child["title"], "Child task <recommended_plugins> Here");
        assert_eq!(child["cwd"], "/project/test");
        assert_eq!(child["pending"][0]["id"], "p1");
        assert_eq!(child["pending"][0]["text"], "Allow shell?");
        assert!(c.hub.snapshot()["sessions"].as_array().unwrap().iter().any(|s| s["id"] == "codeg:215"));
        assert!(c.codeg.streams.contains_key("child"), "the wait subscribes the child stream");
        assert_eq!(c.hub.sessions["codeg:214"], parent, "the child never changes its parent");
        // The authoritative answer releases both.
        let subscription = c.codeg.streams["child"].subscription.clone();
        assert!(c.ingest_codeg_stream(&json!({"type":"event","connection_id":"child","subscription_id":subscription,"envelope":{"seq":6,"type":"permission_resolved","connection_id":"child","request_id":"p1"}})));
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        assert!(!c.codeg.streams.contains_key("child"));
        assert_eq!(c.hub.sessions["codeg:214"], parent);
        // A late duplicate of the answered request never revives the card.
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        // A request whose snapshot cannot be read names no request id: nothing is
        // shown, because no answer could ever be matched against it.
        api.set("child", Value::Null);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        assert!(!c.codeg.streams.contains_key("child"));
        // A new request from the same child surfaces again.
        api.set("child", pending(215, "p2"));
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert_eq!(c.hub.sessions["codeg:215"]["pending"][0]["id"], "p2");
        assert!(c.codeg.streams.contains_key("child"));
    }

    /// The end of a child's turn releases the card even without an answer.
    #[test]
    fn codeg_child_turn_completion_releases_the_card() {
        let home = Home::new();
        let api = Api::start();
        api.set("child", pending(215, "p1"));
        let mut c = collector(&home, api.port);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert!(c.hub.sessions.contains_key("codeg:215"));
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"turn_complete"})));
        assert!(!c.hub.sessions.contains_key("codeg:215"), "a child never keeps a done card");
        assert!(!c.codeg.streams.contains_key("child"));
        // The finished request cannot come back through a delayed webhook.
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert!(!c.hub.sessions.contains_key("codeg:215"));
    }

    /// An answer that lands while the webhook is still in flight releases the
    /// card on the stream snapshot instead of leaving a running child behind.
    #[test]
    fn codeg_child_card_released_by_an_idle_stream_snapshot() {
        let home = Home::new();
        let api = Api::start();
        api.set("child", pending(215, "p1"));
        let mut c = collector(&home, api.port);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        let subscription = c.codeg.streams["child"].subscription.clone();
        assert!(c.ingest_codeg_stream(&json!({"type":"snapshot","connection_id":"child","subscription_id":subscription,"event_seq":7,"snapshot":{"conversation_id":215,"status":"prompting"}})));
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        assert!(!c.codeg.streams.contains_key("child"));
        // A snapshot for another conversation settles nothing here.
        api.set("child", pending(215, "p2"));
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert!(c.hub.sessions.contains_key("codeg:215"));
        let subscription = c.codeg.streams["child"].subscription.clone();
        assert!(c.ingest_codeg_stream(&json!({"type":"snapshot","connection_id":"child","subscription_id":subscription,"event_seq":8,"snapshot":{"conversation_id":999,"status":"prompting"}})));
        assert!(c.hub.sessions.contains_key("codeg:215"));
    }

    /// A cancelled child emits no signal at all, so the card has a time bound.
    #[test]
    fn codeg_child_cards_expire_without_an_answer() {
        let home = Home::new();
        let api = Api::start();
        api.set("child", pending(215, "p1"));
        let mut c = collector(&home, api.port);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        c.codeg.children.get_mut("child").unwrap().started_at = now() - CODEG_CHILD_TTL - 1;
        c.maintain_codeg_webhook().unwrap();
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        assert!(!c.codeg.streams.contains_key("child"));
        assert!(c.codeg.children.contains_key("child"), "the child stays known");
        // A fresh request still surfaces after the sweep.
        api.set("child", pending(215, "p2"));
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        assert_eq!(c.hub.sessions["codeg:215"]["pending"][0]["id"], "p2");
        assert!(c.codeg.streams.contains_key("child"));
    }

    /// Disabling the source drops every temporary card with its stream.
    #[test]
    fn codeg_child_cards_are_released_when_the_source_is_disabled() {
        let home = Home::new();
        let api = Api::start();
        api.set("child", pending(215, "p1"));
        let mut c = collector(&home, api.port);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"permission_request","body":"Task"})));
        c.settings["sources"]["codeg"]["enabled"] = json!(false);
        c.maintain_codeg_webhook().unwrap();
        assert!(!c.hub.sessions.contains_key("codeg:215"));
        assert!(!c.codeg.streams.contains_key("child"));
        assert!(c.codeg.children.is_empty());
    }

    /// A child that finishes silently has no conversation to resolve: the
    /// provisional session that path used to seed was a `done` ghost on a task
    /// that never needed the user.
    #[test]
    fn codeg_silent_child_completion_never_seeds_a_session() {
        let home = Home::new();
        let api = Api::start();
        let mut c = collector(&home, api.port);
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"child","event":"turn_complete","body":"CodeBuddy session completed"})));
        assert!(c.hub.sessions.is_empty());
        assert!(c.hub.snapshot()["sessions"].as_array().unwrap().is_empty());
        assert_eq!(api.calls(), 1, "the event was still resolved once");
        // An existing provisional session still ends on its own completion.
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"gone","event":"user_prompt_sent","body":"Task"})));
        assert_eq!(c.hub.sessions["codeg:connection:gone"]["status"], "running");
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"gone","event":"turn_complete"})));
        assert_eq!(c.hub.sessions["codeg:connection:gone"]["status"], "done");
    }

    /// Every child shape the database can report is treated as a child, and a
    /// long prompt fragment is collapsed and capped before the rail sees it.
    #[test]
    fn codeg_child_metadata_covers_every_delegation_shape() {
        let home = Home::new();
        let api = Api::start();
        let c = collector(&home, api.port);
        let meta = |sid: i64| c.codeg_metadata(&sid.to_string()).unwrap();
        for id in [215, 217, 218, 219] {
            assert_eq!(meta(id)["isSubagent"], true, "conversation {id}");
        }
        assert_eq!(meta(214)["isSubagent"], false);
        assert_eq!(meta(215)["title"], "Child task\n<recommended_plugins> Here");
        assert_eq!(meta(215)["parentTitle"], "Build feature");
        assert_eq!(meta(219)["parentTitle"], "Build feature");
        assert_eq!(codeg_title(&text(&meta(219)["title"])).chars().count(), 80);
        assert_eq!(meta(218).get("parentTitle"), None, "a parentless delegate carries no parent title");
    }

    /// The Node twin parses a snapshot and a stream envelope with one function,
    /// so the two Rust parsers must agree too: a tool call carrying only a `name`
    /// and an option carrying only a `label` may not read differently depending
    /// on which path surfaced the card.
    #[test]
    fn codeg_child_permission_ask_matches_the_stream_twin() {
        let snap = json!({"pending_permission":{"request_id":"p1","tool_call":{"name":"Bash"},"options":[{"label":"允许","kind":"allow_once"}]}});
        let ask = codeg_ask(&snap);
        assert_eq!(
            ask,
            stream_ask("pending_permission", &snap["pending_permission"])
        );
        assert_eq!(question(&ask), "Bash");
        assert_eq!(questions(&ask)[0]["options"][0]["label"], "允许");
    }
}
