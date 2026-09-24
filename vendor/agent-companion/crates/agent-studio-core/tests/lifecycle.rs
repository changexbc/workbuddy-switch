use agent_studio_core::{
    adapters::Collector,
    atomic_json,
    host_process::{HostPresence, Presence},
    hub::Hub,
    now, settings,
    tail::Tail,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        // The wall clock alone can repeat inside one process, and a duplicated
        // home lets one test's Drop delete another test's directory.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "studio-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn collector(&self, source: &str) -> Collector {
        let mut s = settings::defaults();
        for id in settings::SOURCES {
            s["sources"][id]["enabled"] = json!(id == source);
        }
        atomic_json(&self.0.join(".agent-studio/settings.json"), &s).unwrap();
        Collector::new(self.0.clone()).unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn anonymous_approval_reconciles_replayed_continuation_without_clearing_questions() {
    let mut h = Hub::new();
    for ev in [
        json!({"type":"start","roundId":"r","ts":100}),
        json!({"type":"wait","callId":"perm:110","ts":110}),
        json!({"type":"wait","callId":"question","ts":111}),
        json!({"type":"tokens","ts":200}),
        json!({"type":"activity","ts":109}),
    ] {
        h.ingest(agent_studio_core::merge(json!({"source":"codex","sessionId":"x"}),ev));
    }
    assert_eq!(h.sessions["codex:x"]["pending"].as_array().unwrap().len(), 2);
    h.ingest(json!({"source":"codex","sessionId":"x","type":"activity","ts":120}));
    assert_eq!(h.sessions["codex:x"]["pending"][0]["id"], "question");
    assert_eq!(h.sessions["codex:x"]["pending"].as_array().unwrap().len(), 1);
    assert_eq!(h.sessions["codex:x"]["status"], "wait");
    h.ingest(json!({"source":"codex","sessionId":"x","type":"resolve","callId":"question","ts":201}));
    h.ingest(json!({"source":"codex","sessionId":"x","type":"wait","callId":"perm:210","ts":210}));
    h.ingest(json!({"source":"codex","sessionId":"x","type":"tokens","ts":250}));
    h.ingest(json!({"source":"codex","sessionId":"x","type":"activity","ts":220}));
    assert_eq!(h.sessions["codex:x"]["status"], "running");
    assert_eq!(h.sessions["codex:x"]["updatedAt"], 250);
}
#[test]
fn rejects_stale_round_end_and_resolves_wait() {
    let mut h = Hub::new();
    let t = now();
    for ev in [
        json!({"type":"start","roundId":"a","ts":t}),
        json!({"type":"wait","callId":"q","ts":t+1}),
        json!({"type":"start","roundId":"b","ts":t+2}),
        json!({"type":"end","roundId":"a","status":"done","ts":t+3}),
    ] {
        h.ingest(agent_studio_core::merge(
            json!({"source":"codex","sessionId":"x"}),
            ev,
        ));
    }
    assert_eq!(h.sessions["codex:x"]["status"], "running");
    assert_eq!(h.sessions["codex:x"]["pending"], json!([]));
}
#[test]
fn hosted_codex_is_hidden_but_wait_survives_staleness() {
    let mut h = Hub::new();
    h.ingest(json!({"source":"codex","sessionId":"x","type":"wait","callId":"q","ts":1}));
    h.ingest(json!({"source":"codeg","sessionId":"y","externalId":"thr_x","type":"meta"}));
    assert_eq!(h.snapshot()["sessions"].as_array().unwrap().len(), 1);
    h.sessions.remove("codeg:y");
    assert_eq!(h.snapshot()["sessions"][0]["status"], "wait");
}
#[test]
fn tail_preserves_utf8_partial_records_and_rotation() {
    use std::io::Write;
    let home = Home::new();
    let p = home.0.join("a.jsonl");
    let bytes = "{\"text\":\"中文\"}\n".as_bytes();
    std::fs::write(&p, &bytes[..11]).unwrap();
    let mut tail = Tail::default();
    assert!(tail.pump(&p, 512).unwrap().1.is_empty());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(&bytes[11..])
        .unwrap();
    assert_eq!(tail.pump(&p, 512).unwrap().1[0]["text"], "中文");
    std::fs::write(&p, b"{}\n").unwrap();
    assert!(tail.pump(&p, 512).unwrap().0);
}
#[test]
fn settings_reject_invalid_paths_without_mutating() {
    let mut s = settings::defaults();
    s["sources"]["codex"]["path"] = json!("relative/path");
    assert!(settings::validate(&s).is_err());
    s = settings::defaults();
    s["schedule"]["end"] = json!("08:00");
    assert!(settings::validate(&s).is_err());
}
#[test]
fn workbuddy_wait_and_authoritative_completion() {
    let home = Home::new();
    let mut c = home.collector("workbuddy");
    let t = now();
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"x",
        "cwd":"/p",
        "timestamp":t,
        "hook_event_name":"UserPromptSubmit",
        "prompt":"hello"
    })));
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"x",
        "cwd":"/p",
        "timestamp":t+1,
        "hook_event_name":"PreToolUse",
        "tool_name":"AskUserQuestion",
        "tool_use_id":"q",
        "tool_input":{"questions":[{"question":"选哪个？"}]}
    })));
    assert_eq!(c.hub.sessions["workbuddy:x"]["status"], "wait");
    assert_eq!(
        c.hub.sessions["workbuddy:x"]["pending"][0]["text"],
        "选哪个？"
    );
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"x",
        "timestamp":t+2,
        "hook_event_name":"PostToolUse",
        "tool_name":"AskUserQuestion",
        "tool_use_id":"q"
    })));
    assert_eq!(c.hub.sessions["workbuddy:x"]["status"], "running");
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"x",
        "timestamp":t+3,
        "hook_event_name":"Stop"
    })));
    assert_eq!(c.hub.sessions["workbuddy:x"]["status"], "done");
    assert_eq!(c.hub.sessions["workbuddy:x"]["agentType"], "workbuddy");
    let mut restarted = home.collector("workbuddy");
    restarted.poll();
    assert!(restarted.hub.sessions.is_empty());
}

#[test]
fn workbuddy_international_hooks_set_agent_type() {
    let home = Home::new();
    let mut c = home.collector("workbuddy");
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "agent_edition":"international",
        "session_id":"ai",
        "hook_event_name":"UserPromptSubmit",
        "prompt":"hello"
    })));
    assert_eq!(c.hub.sessions["workbuddy:ai"]["agentType"], "workbuddy-ai");
}

#[test]
fn workbuddy_permission_and_notification_become_wait() {
    let home = Home::new();
    let mut c = home.collector("workbuddy");
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"p",
        "hook_event_name":"UserPromptSubmit",
        "prompt":"read secrets"
    })));
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"p",
        "hook_event_name":"PermissionRequest",
        "tool_name":"Read",
        "tool_use_id":"cred",
        "message":"Allow the model to access sensitive credentials?"
    })));
    assert_eq!(c.hub.sessions["workbuddy:p"]["status"], "wait");
    assert_eq!(
        c.hub.sessions["workbuddy:p"]["pending"][0]["text"],
        "Allow the model to access sensitive credentials?"
    );
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"p",
        "hook_event_name":"PostToolUse",
        "tool_name":"Read",
        "tool_use_id":"cred"
    })));
    assert_eq!(c.hub.sessions["workbuddy:p"]["status"], "running");
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"n",
        "hook_event_name":"Notification",
        "message":"Allow the model to access sensitive credentials?"
    })));
    assert_eq!(c.hub.sessions["workbuddy:n"]["status"], "wait");
    assert!(c.ingest_hook(&json!({
        "source":"workbuddy",
        "session_id":"n",
        "hook_event_name":"Notification",
        "notification_type":"auth_success"
    })));
    assert_eq!(c.hub.sessions["workbuddy:n"]["status"], "wait");
    assert_eq!(
        agent_studio_core::adapters::workbuddy_edition(
            &std::path::Path::new("/tmp/.workbuddy-ai/settings.json")
        ),
        "international"
    );
    assert_eq!(
        agent_studio_core::adapters::workbuddy_edition(
            &std::path::Path::new("/tmp/.workbuddy/settings.json")
        ),
        "domestic"
    );
}

#[test]
fn workbuddy_hooks_preserve_foreign_settings_and_skip_permissions() {
    let merged = agent_studio_core::adapters::merge_workbuddy_hooks(
        json!({
            "sandbox":{"extraAllowWrite":["~/tmp"]},
            "hooks":{"PermissionRequest":[{"hooks":[{"type":"http","url":"http://example"}]}]}
        }),
        "bin hook --home /tmp --source workbuddy",
    )
    .unwrap();
    assert_eq!(merged["sandbox"]["extraAllowWrite"][0], "~/tmp");
    assert_eq!(
        merged["hooks"]["PermissionRequest"][0]["hooks"][0]["url"],
        "http://example"
    );
    assert!(merged["hooks"].get("PermissionRequest").is_some());
    let pre = merged["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(pre.contains("--source workbuddy"));
    assert_eq!(merged["hooks"]["PreToolUse"][0]["matcher"], "");
}

#[test]
fn workbuddy_settings_cover_international_and_domestic_editions() {
    let home = Home::new();
    std::fs::create_dir_all(home.0.join(".workbuddy/binaries")).unwrap();
    std::fs::create_dir_all(home.0.join(".workbuddy-ai")).unwrap();
    let files = agent_studio_core::adapters::workbuddy_settings_files(&home.0, "");
    assert_eq!(files, vec![home.0.join(".workbuddy-ai/settings.json")]);
    std::fs::write(home.0.join(".workbuddy/settings.json"), "{}").unwrap();
    let files = agent_studio_core::adapters::workbuddy_settings_files(&home.0, "");
    assert_eq!(
        files,
        vec![
            home.0.join(".workbuddy-ai/settings.json"),
            home.0.join(".workbuddy/settings.json")
        ]
    );
    std::fs::remove_dir_all(home.0.join(".workbuddy-ai")).unwrap();
    let files = agent_studio_core::adapters::workbuddy_settings_files(&home.0, "");
    assert_eq!(files, vec![home.0.join(".workbuddy/settings.json")]);
}
#[test]
fn codeg_never_scans_existing_conversations() {
    let home=Home::new();
    let dir=home.0.join("Library/Application Support/app.codeg");
    std::fs::create_dir_all(&dir).unwrap();
    let db=rusqlite::Connection::open(dir.join("codeg.db")).unwrap();
    db.execute_batch("CREATE TABLE conversation(id TEXT,status TEXT); INSERT INTO conversation VALUES('old','running');").unwrap();
    let mut c=home.collector("codeg");
    c.poll();c.poll();
    assert!(c.hub.sessions.is_empty());
    for (event,status) in [("user_prompt_sent","running"),("question_request","wait"),("turn_complete","done"),("user_prompt_sent","running"),("error","error")] {
        assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"c","event":event,"body":"hello","fields":[{"label":"Question","value":"Pick A"}]})));
        assert_eq!(c.hub.sessions["codeg:connection:c"]["status"],status);
    }
    c.settings["sources"]["codeg"]["enabled"]=json!(false);
    assert!(!c.ingest_hook(&json!({"source":"codeg","connection_id":"c","event":"user_prompt_sent"})));
}

/// Minimal loopback Codeg API so the native startup alignment can be exercised
/// without the real app; mirrors the Node fixture used by tests/codeg-hooks.
struct CodegApi {
    port: u16,
    state: std::sync::Arc<std::sync::Mutex<CodegApiState>>,
}
#[derive(Default)]
struct CodegApiState {
    hooks: serde_json::Value,
    connections: serde_json::Value,
    snapshots: std::collections::HashMap<String, serde_json::Value>,
    calls: Vec<String>,
    fail_list: bool,
}
impl CodegApi {
    fn start() -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = std::sync::Arc::new(std::sync::Mutex::new(CodegApiState {
            hooks: json!([]),
            connections: json!([]),
            ..Default::default()
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { continue };
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
                let body: serde_json::Value =
                    serde_json::from_slice(&buf[head_end..]).unwrap_or(serde_json::Value::Null);
                let method = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .trim_start_matches("/api/")
                    .to_string();
                let (status, result) = {
                    let mut state = shared.lock().unwrap();
                    state.calls.push(method.clone());
                    match method.as_str() {
                        "get_chat_event_webhooks" => ("200 OK", state.hooks.clone()),
                        "set_chat_event_webhooks" => {
                            state.hooks = body["webhooks"].clone();
                            ("200 OK", serde_json::Value::Null)
                        }
                        "get_chat_event_filter" => {
                            ("200 OK", json!(agent_studio_core::adapters::CODEG_EVENTS))
                        }
                        "set_chat_event_filter" => ("200 OK", serde_json::Value::Null),
                        "list_chat_channels" => ("200 OK", json!([])),
                        "acp_list_connections" if state.fail_list => {
                            ("503 Service Unavailable", serde_json::Value::Null)
                        }
                        "acp_list_connections" => ("200 OK", state.connections.clone()),
                        "acp_get_session_snapshot" => (
                            "200 OK",
                            state
                                .snapshots
                                .get(body["connectionId"].as_str().unwrap_or(""))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        ),
                        _ => ("404 Not Found", serde_json::Value::Null),
                    }
                };
                let payload = result.to_string();
                let _ = write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len());
                let _ = stream.flush();
            }
        });
        Self { port, state }
    }
    fn set_connections(&self, rows: serde_json::Value) {
        self.state.lock().unwrap().connections = rows;
    }
    fn set_snapshot(&self, conn: &str, snapshot: serde_json::Value) {
        self.state
            .lock()
            .unwrap()
            .snapshots
            .insert(conn.into(), snapshot);
    }
    fn set_fail_list(&self, fail: bool) {
        self.state.lock().unwrap().fail_list = fail;
    }
    fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }
    fn lists(&self) -> usize {
        self.calls().iter().filter(|m| *m == "acp_list_connections").count()
    }
}
/// Codeg database with credentials plus the conversations used by the fixtures.
fn codeg_home(home: &Home, port: u16) -> Collector {
    let dir = home.0.join("Library/Application Support/app.codeg");
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("codeg.db")).unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE app_metadata(key TEXT,value TEXT);
         CREATE TABLE conversation(id INTEGER,title TEXT,agent_type TEXT,external_id TEXT,folder_id INTEGER,status TEXT,parent_id INTEGER,kind TEXT);
         CREATE TABLE folder(id INTEGER,path TEXT);
         INSERT INTO app_metadata VALUES('web_service_port','{port}');
         INSERT INTO app_metadata VALUES('web_service_token','secret-test-token');
         INSERT INTO conversation VALUES(214,'Build feature','codex','thr-native',1,'in_progress',NULL,'regular');
         INSERT INTO conversation VALUES(215,'Child task','codex','thr-child',1,'in_progress',214,'delegate');
         INSERT INTO conversation VALUES(216,'Idle task','codex','thr-idle',1,'in_progress',NULL,'regular');
         INSERT INTO folder VALUES(1,'/project/test');"
    ))
    .unwrap();
    let mut c = home.collector("codeg");
    c.configure_codeg_webhook("http://127.0.0.1:1/api/codeg-webhook/test".into());
    c
}

#[test]
fn codeg_startup_alignment_recovers_prompting_connections() {
    let home = Home::new();
    let api = CodegApi::start();
    api.set_connections(json!([{"id":"connection-1","agent_type":"codex","status":"prompting"}]));
    api.set_snapshot("connection-1",json!({"conversation_id":214,"external_id":"thr-native","folder_id":1,"event_seq":77,"status":"prompting","pending_question":{"question_id":"q1","questions":[{"question":"Pick","options":[{"label":"A"},{"label":"B"}]}]}}));
    let mut c = codeg_home(&home, api.port);
    c.poll();
    let session = c.hub.sessions["codeg:214"].clone();
    assert_eq!(session["status"], "wait", "an in-flight session is seeded without a new event");
    assert_eq!(session["title"], "Build feature");
    assert_eq!(session["cwd"], "/project/test");
    assert_eq!(session["pending"][0]["id"], "q1");
    assert_eq!(session["pending"][0]["text"], "Pick");
    assert_eq!(session["pending"][0]["questions"][0]["options"][1]["label"], "B");
    let snapshot = c.hub.snapshot();
    let wait = snapshot["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "wait")
        .unwrap()
        .clone();
    assert!(wait["roundId"].as_str().unwrap().starts_with("hook:"));
    // One-shot: registration succeeded, no later poll scans again.
    assert_eq!(api.lists(), 1);
    c.poll();
    assert_eq!(api.lists(), 1);
    // A late completion webhook cannot end the recovered round.
    assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"connection-1","event":"turn_complete"})));
    assert_eq!(c.hub.sessions["codeg:214"]["status"], "wait");
    // The alignment records the connection, so a stale snapshot never invents a duplicate.
    api.set_snapshot("connection-1", serde_json::Value::Null);
    assert!(c.ingest_hook(&json!({"source":"codeg","connection_id":"connection-1","event":"question_request"})));
    assert_eq!(c.hub.sessions.len(), 1);
    assert!(c.hub.sessions.contains_key("codeg:214"));
}

#[test]
fn codeg_startup_alignment_seeds_prompting_without_pending_as_running() {
    let home = Home::new();
    let api = CodegApi::start();
    api.set_connections(json!([{"id":"connection-1","agent_type":"codex","status":"prompting"}]));
    api.set_snapshot("connection-1",json!({"conversation_id":214,"external_id":"thr-native","folder_id":1,"status":"prompting","event_seq":9}));
    let mut c = codeg_home(&home, api.port);
    c.poll();
    let session = c.hub.sessions["codeg:214"].clone();
    assert_eq!(session["status"], "running", "a prompting session without pending requests is seeded as running");
    assert!(session["pending"].as_array().unwrap().is_empty());
    assert_eq!(session["title"], "Build feature");
    assert_eq!(session["cwd"], "/project/test");
    let snapshot = c.hub.snapshot();
    assert_eq!(snapshot["sessions"].as_array().unwrap().len(), 1, "the recovered session is visible");
    assert!(snapshot["events"].as_array().unwrap().iter().all(|e| e["kind"] != "wait"), "no pending request means no wait event");
}

#[test]
fn codeg_startup_alignment_seeds_connected_connection_with_pending() {
    let home = Home::new();
    let api = CodegApi::start();
    api.set_connections(json!([{"id":"connection-1","agent_type":"codex","status":"connected"}]));
    api.set_snapshot("connection-1",json!({"conversation_id":214,"status":"connected","event_seq":4,"pending_question":{"question_id":"q7","questions":[{"question":"Keep waiting?"}]}}));
    let mut c = codeg_home(&home, api.port);
    c.poll();
    let session = c.hub.sessions["codeg:214"].clone();
    assert_eq!(session["status"], "wait", "a pending request outranks the connected status");
    assert_eq!(session["pending"][0]["id"], "q7");
    assert_eq!(session["pending"][0]["text"], "Keep waiting?");
    assert!(c.hub.snapshot()["events"].as_array().unwrap().iter().any(|e| e["kind"] == "wait"));
}

#[test]
fn codeg_startup_alignment_skips_idle_child_and_snapshot_less_connections() {
    let home = Home::new();
    let api = CodegApi::start();
    api.set_connections(json!([{"id":"idle"},{"id":"child"},{"id":"orphan"},{"id":""}]));
    api.set_snapshot("idle",json!({"conversation_id":216,"status":"connected","event_seq":3}));
    api.set_snapshot("child",json!({"conversation_id":215,"status":"prompting","event_seq":4,"pending_question":{"question_id":"q9","questions":[{"question":"Child?"}]}}));
    api.set_snapshot("orphan",json!({"status":"prompting","event_seq":5}));
    let mut c = codeg_home(&home, api.port);
    c.poll();
    assert!(c.hub.sessions.is_empty(),"idle, child and snapshot-less connections are never seeded");
    assert!(c.hub.snapshot()["events"].as_array().unwrap().is_empty());
    assert!(!c.ingest_codeg_stream(&json!({"type":"event","connection_id":"child"})),"skipped connections are never subscribed");
}

#[test]
fn codeg_startup_alignment_never_restarts_a_known_round() {
    let home = Home::new();
    let api = CodegApi::start();
    api.set_connections(json!([{"id":"connection-1"}]));
    api.set_snapshot("connection-1",json!({"conversation_id":214,"status":"prompting","event_seq":30,"pending_question":{"question_id":"q1","questions":[{"question":"Pick"}]}}));
    let mut c = codeg_home(&home, api.port);
    // A webhook won the race: the hub already owns this round.
    c.hub.ingest(json!({"source":"codeg","sessionId":"214","type":"start","roundId":"hook:1:1","ts":1}));
    c.poll();
    assert_eq!(c.hub.sessions["codeg:214"]["roundId"], "hook:1:1");
    assert!(c.hub.sessions["codeg:214"]["pending"].as_array().unwrap().is_empty());
    // Re-enabling the integration re-arms exactly one more alignment.
    let before = api.lists();
    c.configure_codeg_webhook("http://127.0.0.1:1/api/codeg-webhook/test".into());
    c.poll();
    assert_eq!(api.lists(), before + 1);
    assert_eq!(c.hub.sessions.len(), 1);
}

#[test]
fn codeg_startup_alignment_failure_only_degrades_health() {
    let home = Home::new();
    let api = CodegApi::start();
    api.set_fail_list(true);
    let mut c = codeg_home(&home, api.port);
    c.poll();
    assert!(c.codeg.registered,"registration survives an alignment failure");
    assert_eq!(c.hub.sources["codeg"]["state"], "error");
    assert_eq!(c.hub.sources["codeg"]["detail"], "Codeg Web Service 不可用");
    assert!(c.hub.sessions.is_empty());
    assert_eq!(api.lists(), 1);
    api.set_fail_list(false);
    let calls = api.calls().len();
    c.poll();
    assert_eq!(api.calls().len(), calls, "no retry storm and no second scan");
    assert_eq!(api.lists(), 1);
}

#[test]
fn ide_hooks_only_lifecycle_and_shared_cli_filter() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    c.poll();
    assert!(c.hub.sessions.is_empty());
    assert!(!c.ingest_hook(&json!({"agent_source":"codebuddy-ide","session_id":"cli","hook_event_name":"UserPromptSubmit","client":"cli"})));
    let cases: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/codebuddy-ide-hooks.json")).unwrap();
    let t = now();
    for (i, case) in cases.as_array().unwrap().iter().enumerate() {
        let p = agent_studio_core::merge(json!({"agent_source":"codebuddy-ide","client":"CodeBuddyIDE","session_id":"x","cwd":"/project","timestamp":t+i as i64}), case["hook"].clone());
        assert_eq!(c.ingest_hook(&p), case["accepted"] != false, "case {i}");
        let status = c.hub.sessions.get("codebuddy-ide:x").map(|s|s["status"].clone()).unwrap_or_default();
        assert_eq!(status, case["status"], "case {i}");
    }
    let mut restarted = home.collector("codebuddy-ide");
    restarted.poll();
    assert!(restarted.hub.sessions.is_empty());
    c.settings["sources"]["codebuddy-ide"]["enabled"] = json!(false);
    assert!(!c.ingest_hook(&json!({"agent_source":"codebuddy-ide","client":"CodeBuddyIDE","session_id":"x","hook_event_name":"UserPromptSubmit"})));
}
#[test]
fn ide_hooks_stamp_international_and_domestic_editions() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    assert!(c.ingest_hook(&json!({
        "agent_source":"codebuddy-ide",
        "client":"CodeBuddyIDE",
        "session_id":"intl",
        "hook_event_name":"UserPromptSubmit",
        "agent_edition":"international"
    })));
    assert_eq!(c.hub.sessions["codebuddy-ide:intl"]["agentType"], "codebuddy");
    assert!(c.ingest_hook(&json!({
        "agent_source":"codebuddy-ide",
        "client":"CodeBuddyIDE",
        "session_id":"cn",
        "hook_event_name":"UserPromptSubmit",
        "agent_edition":"domestic"
    })));
    assert_eq!(c.hub.sessions["codebuddy-ide:cn"]["agentType"], "codebuddycn");
    std::fs::create_dir_all(home.0.join(".codebuddy")).unwrap();
    std::fs::create_dir_all(home.0.join(".codebuddycn")).unwrap();
    let files = agent_studio_core::adapters::codebuddy_settings_files(&home.0, "");
    assert_eq!(
        files,
        vec![
            home.0.join(".codebuddy/settings.json"),
            home.0.join(".codebuddycn/settings.json")
        ]
    );
    assert_eq!(
        agent_studio_core::adapters::codebuddy_edition(&home.0.join(".codebuddy/settings.json")),
        "international"
    );
    assert_eq!(
        agent_studio_core::adapters::codebuddy_edition(&home.0.join(".codebuddycn/settings.json")),
        "domestic"
    );
}
#[test]
fn ide_hook_merge_preserves_user_handlers_and_is_idempotent() {
    use agent_studio_core::adapters::{merge_codebuddy_ide_hooks, CODEBUDDY_IDE_HOOK_EVENTS};
    let user = json!({"hooks":{"Stop":[{"hooks":[{"command":"echo keep"}]}],"FinalStop":[{"hooks":[{"command":"echo final"}]}]},"custom":true});
    let command = "'/tmp/agent-studio-runtime-v1' hook --source codebuddy-ide";
    let merged = merge_codebuddy_ide_hooks(user.clone(), command).unwrap();
    assert_eq!(merged["hooks"]["Stop"][0],user["hooks"]["Stop"][0]);
    assert_eq!(merged["hooks"]["FinalStop"],user["hooks"]["FinalStop"]);
    assert_eq!(merged["custom"],true);
    assert_eq!(merge_codebuddy_ide_hooks(merged.clone(), command).unwrap(),merged);
    for e in CODEBUDDY_IDE_HOOK_EVENTS { assert_eq!(merged["hooks"][e].as_array().unwrap().last().unwrap()["hooks"][0]["command"], command); }
    assert!(merge_codebuddy_ide_hooks(json!({"hooks":{"Stop":{}}}),command).is_err());
}

#[test]
fn codex_session_start_source_is_not_treated_as_agent_source() {
    let home = Home::new();
    let mut c = home.collector("codex");
    assert!(c.ingest_hook(&json!({
        "session_id":"x",
        "hook_event_name":"SessionStart",
        "source":"startup"
    })));
    assert_eq!(c.hub.sessions["codex:x"]["status"], "running");
    assert!(c.ingest_hook(&json!({
        "session_id":"x",
        "hook_event_name":"UserPromptSubmit",
        "prompt":"hello",
        "source":"resume"
    })));
    assert_eq!(c.hub.sessions["codex:x"]["title"], "hello");
}

#[test]
fn codex_hook_only_lifecycle_and_no_restart_restore() {
    let home = Home::new();
    let mut c = home.collector("codex");
    let fixture: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/codex-hooks.json")).unwrap();
    let t = now();
    let transcript = home.0.join("transcript.jsonl");
    std::fs::write(&transcript, format!("{}\n",json!({"type":"event_msg","timestamp":t+9999,"payload":{"type":"task_complete","turn_id":"r"}}))).unwrap();
    let checkpoint = home.0.join(".agent-studio/codex-resume.json");
    let old = json!({"sessions":[{"source":"codex","sessionId":"old","id":"codex:old","status":"running","roundId":"r","updatedAt":t,"steps":[],"pending":[]}]}).to_string();
    std::fs::write(&checkpoint, &old).unwrap();
    for (i,case) in fixture.as_array().unwrap().iter().enumerate() {
        let hook=agent_studio_core::merge(json!({"session_id":"x","turn_id":"r","cwd":"/project","timestamp":t+i as i64,"transcript_path":transcript}),case["hook"].clone());
        c.ingest_hook(&hook);
        c.poll();
        assert_eq!(c.hub.sessions["codex:x"]["status"],case["status"],"case {i}");
        assert_eq!(c.hub.sessions["codex:x"]["pending"].as_array().unwrap().len(),case["pending"].as_u64().unwrap() as usize,"case {i}");
    }
    assert_eq!(std::fs::read_to_string(&checkpoint).unwrap(),old);
    let mut restarted=Collector::new(home.0.clone()).unwrap();restarted.poll();
    assert!(restarted.hub.sessions.is_empty());
    assert!(restarted.live.is_empty());
}

#[test]
fn permission_checks_do_not_generate_wait_notifications() {
    let home=Home::new();let mut c=home.collector("codex");
    for (event,tool,id) in [("UserPromptSubmit","",""),("PermissionRequest","Bash","a")] {
        c.ingest_hook(&json!({"session_id":"x","turn_id":"r","hook_event_name":event,"tool_name":tool,"tool_use_id":id}));
    }
    assert_eq!(c.hub.sessions["codex:x"]["status"],"running");
    assert_eq!(c.hub.sessions["codex:x"]["permissionChecks"].as_array().unwrap().len(),1);
    assert!(c.hub.snapshot()["events"].as_array().unwrap().is_empty());
    c.ingest_hook(&json!({"session_id":"x","turn_id":"r","hook_event_name":"PreToolUse","tool_name":"request_user_input","tool_use_id":"q"}));
    c.ingest_hook(&json!({"session_id":"x","turn_id":"r","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"a"}));
    assert_eq!(c.hub.sessions["codex:x"]["status"],"wait");
    assert_eq!(c.hub.sessions["codex:x"]["permissionChecks"],json!([]));
    assert_eq!(c.hub.snapshot()["events"].as_array().unwrap().len(),1);
}

#[test]
fn internal_codex_tasks_never_reappear_on_late_hooks() {
    let home = Home::new();
    let mut c = home.collector("codex");
    let prompts: Vec<String> = serde_json::from_str(include_str!("../../../src/monitor/codex-internal-prompts.json")).unwrap();
    for (i, prompt) in prompts.iter().enumerate() {
        let id = format!("internal-{i}");
        c.ingest_hook(&json!({"session_id":id,"hook_event_name":"SessionStart"}));
        assert!(!c.ingest_hook(&json!({"session_id":id,"hook_event_name":"UserPromptSubmit","prompt":prompt})));
        for event in ["PreToolUse", "PostToolUse", "Stop", "SessionStart"] {
            assert!(!c.ingest_hook(&json!({"session_id":id,"hook_event_name":event})));
        }
        assert!(!c.hub.sessions.contains_key(&format!("codex:{id}")));
    }
    assert!(c.ingest_hook(&json!({"session_id":"user","hook_event_name":"UserPromptSubmit","prompt":"帮我实现个性化建议和 memory consolidation"})));
    assert!(c.hub.sessions.contains_key("codex:user"));
}

#[test]
fn host_exit_marks_unfinished_sessions_aborted_and_recovers() {
    let home = Home::new();
    let mut c = home.collector("workbuddy");
    c.ingest_workbuddy_hook(&json!({
        "session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"build","timestamp":now()
    }));
    assert_eq!(c.hub.sessions["workbuddy:s1"]["status"], json!("running"));
    *c.presence_mut("workbuddy", "workbuddy").unwrap() = HostPresence::with_runner(
        "workbuddy",
        Duration::ZERO,
        2,
        Box::new(|| Ok("/sbin/launchd\n".into())),
    );
    c.presence_mut("workbuddy", "workbuddy").unwrap().note_hook();
    c.poll_workbuddy().unwrap();
    assert_eq!(c.hub.sessions["workbuddy:s1"]["status"], json!("running"));
    c.poll_workbuddy().unwrap();
    assert_eq!(c.hub.sessions["workbuddy:s1"]["status"], json!("aborted"));
    assert_eq!(c.hub.sessions["workbuddy:s1"]["endedBy"], json!("host"));
    assert_eq!(c.hub.sources["workbuddy"]["state"], json!("exited"));
    c.ingest_workbuddy_hook(&json!({
        "session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"again","timestamp":now()+1
    }));
    assert_eq!(c.hub.sources["workbuddy"]["state"], json!("ok"));
    assert_eq!(c.hub.sessions["workbuddy:s1"]["status"], json!("running"));
    assert!(c.hub.sessions["workbuddy:s1"]["endedBy"].is_null());
}

// The VS Code plugin shares the IDE settings file, so a session belongs to the
// host kind its hook payload named; each kind is probed and ended on its own.
const PS_NO_HOST: &str = "/sbin/launchd\n";
const PS_VSCODE: &str = "/Applications/Visual Studio Code.app/Contents/MacOS/Electron\n";
const PS_IDE: &str = "/Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy\n";

fn ide_presence(kind: &str, output: &'static str) -> HostPresence {
    HostPresence::with_runner(kind, Duration::ZERO, 2, Box::new(move || Ok(output.into())))
}

fn ide_hook(c: &mut Collector, client: &str, sid: &str, ts: i64) {
    assert!(c.ingest_hook(&json!({
        "agent_source":"codebuddy-ide","client":client,"session_id":sid,"cwd":"/project",
        "timestamp":ts,"hook_event_name":"UserPromptSubmit","prompt":"work"
    })));
}

#[test]
fn ide_host_kinds_are_probed_and_ended_independently() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    let t = now();
    ide_hook(&mut c, "CodeBuddyIDE", "ide", t);
    ide_hook(&mut c, "VSCode", "code", t);
    assert_eq!(
        c.hub.sessions["codebuddy-ide:ide"]["hostKind"],
        json!("codebuddy-ide")
    );
    assert_eq!(
        c.hub.sessions["codebuddy-ide:code"]["hostKind"],
        json!("vscode")
    );
    // Only VS Code is still running.
    *c.presence_mut("codebuddy-ide", "codebuddy-ide").unwrap() =
        ide_presence("codebuddy-ide", PS_NO_HOST);
    *c.presence_mut("codebuddy-ide", "vscode").unwrap() = ide_presence("vscode", PS_VSCODE);
    for kind in ["codebuddy-ide", "vscode"] {
        c.presence_mut("codebuddy-ide", kind).unwrap().note_hook();
    }
    c.poll_ide().unwrap();
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("ok"));
    c.poll_ide().unwrap();
    assert_eq!(
        c.presence_mut("codebuddy-ide", "codebuddy-ide")
            .unwrap()
            .observe(),
        Presence::Gone
    );
    assert_eq!(
        c.presence_mut("codebuddy-ide", "vscode").unwrap().observe(),
        Presence::Alive
    );
    assert_eq!(c.hub.sessions["codebuddy-ide:ide"]["status"], json!("aborted"));
    assert_eq!(c.hub.sessions["codebuddy-ide:ide"]["endedBy"], json!("host"));
    assert_eq!(c.hub.sessions["codebuddy-ide:code"]["status"], json!("running"));
    assert!(c.hub.sessions["codebuddy-ide:code"]["endedBy"].is_null());
    // A live kind keeps the source connected even while its sibling exited.
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("ok"));
    assert_eq!(
        c.hub.sources["codebuddy-ide"]["detail"],
        json!("已连接 CodeBuddy Hook（不读取会话文件）")
    );
}

#[test]
fn ide_exit_names_only_the_exited_kind() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    ide_hook(&mut c, "CodeBuddyIDE", "ide", now());
    *c.presence_mut("codebuddy-ide", "codebuddy-ide").unwrap() =
        ide_presence("codebuddy-ide", PS_NO_HOST);
    // The VS Code kind never saw a hook: it must stay unknown and unmentioned.
    *c.presence_mut("codebuddy-ide", "vscode").unwrap() = ide_presence("vscode", PS_NO_HOST);
    c.presence_mut("codebuddy-ide", "codebuddy-ide").unwrap().note_hook();
    c.poll_ide().unwrap();
    c.poll_ide().unwrap();
    assert_eq!(c.hub.sessions["codebuddy-ide:ide"]["status"], json!("aborted"));
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("exited"));
    assert_eq!(
        c.hub.sources["codebuddy-ide"]["detail"],
        json!("CodeBuddy IDE 已退出，未完成的任务已标记中止")
    );
    assert_eq!(
        c.presence_mut("codebuddy-ide", "vscode").unwrap().observe(),
        Presence::Unknown
    );
}

#[test]
fn vscode_exit_names_only_the_exited_kind() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    ide_hook(&mut c, "VSCode", "code", now());
    *c.presence_mut("codebuddy-ide", "vscode").unwrap() = ide_presence("vscode", PS_NO_HOST);
    *c.presence_mut("codebuddy-ide", "codebuddy-ide").unwrap() =
        ide_presence("codebuddy-ide", PS_NO_HOST);
    c.presence_mut("codebuddy-ide", "vscode").unwrap().note_hook();
    c.poll_ide().unwrap();
    c.poll_ide().unwrap();
    assert_eq!(c.hub.sessions["codebuddy-ide:code"]["status"], json!("aborted"));
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("exited"));
    assert_eq!(
        c.hub.sources["codebuddy-ide"]["detail"],
        json!("VS Code 已退出，未完成的任务已标记中止")
    );
    assert_eq!(
        c.presence_mut("codebuddy-ide", "codebuddy-ide")
            .unwrap()
            .observe(),
        Presence::Unknown
    );
}

#[test]
fn vscode_exit_ends_only_vscode_sessions() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    let t = now();
    ide_hook(&mut c, "CodeBuddyIDE", "ide", t);
    ide_hook(&mut c, "VSCode", "code", t);
    *c.presence_mut("codebuddy-ide", "vscode").unwrap() = ide_presence("vscode", PS_NO_HOST);
    *c.presence_mut("codebuddy-ide", "codebuddy-ide").unwrap() =
        ide_presence("codebuddy-ide", PS_IDE);
    for kind in ["codebuddy-ide", "vscode"] {
        c.presence_mut("codebuddy-ide", kind).unwrap().note_hook();
    }
    c.poll_ide().unwrap();
    c.poll_ide().unwrap();
    assert_eq!(c.hub.sessions["codebuddy-ide:code"]["status"], json!("aborted"));
    assert_eq!(c.hub.sessions["codebuddy-ide:code"]["endedBy"], json!("host"));
    assert_eq!(c.hub.sessions["codebuddy-ide:ide"]["status"], json!("running"));
    assert!(c.hub.sessions["codebuddy-ide:ide"]["endedBy"].is_null());
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("ok"));
}

#[test]
fn both_kinds_gone_reports_once_for_both() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    let t = now();
    ide_hook(&mut c, "CodeBuddyIDE", "ide", t);
    ide_hook(&mut c, "VSCode", "code", t);
    for kind in ["codebuddy-ide", "vscode"] {
        *c.presence_mut("codebuddy-ide", kind).unwrap() = ide_presence(kind, PS_NO_HOST);
        c.presence_mut("codebuddy-ide", kind).unwrap().note_hook();
    }
    c.poll_ide().unwrap();
    c.poll_ide().unwrap();
    assert_eq!(c.hub.sessions["codebuddy-ide:ide"]["status"], json!("aborted"));
    assert_eq!(c.hub.sessions["codebuddy-ide:code"]["status"], json!("aborted"));
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("exited"));
    assert_eq!(
        c.hub.sources["codebuddy-ide"]["detail"],
        json!("CodeBuddy IDE 与 VS Code 已退出，未完成的任务已标记中止")
    );
}

#[test]
fn unhooked_host_kinds_never_conclude_an_exit() {
    let home = Home::new();
    let mut c = home.collector("codebuddy-ide");
    for kind in ["codebuddy-ide", "vscode"] {
        *c.presence_mut("codebuddy-ide", kind).unwrap() = ide_presence(kind, PS_NO_HOST);
    }
    c.poll_ide().unwrap();
    c.poll_ide().unwrap();
    assert_eq!(c.hub.sources["codebuddy-ide"]["state"], json!("ok"));
    assert_eq!(
        c.hub.sources["codebuddy-ide"]["detail"],
        json!("等待新的 CodeBuddy Hook；不恢复历史会话")
    );
    assert!(c.hub.sessions.is_empty());
}

#[test]
fn codex_async_questions_never_notify_or_clear_synchronous_waits() {
    let home = Home::new();
    let mut c = home.collector("codex");
    let mut ts = now();
    let mut hook = |c: &mut Collector, event: &str, tool: &str, id: &str| {
        ts += 1;
        c.ingest_hook(&json!({"session_id":"x","turn_id":"r","hook_event_name":event,"tool_name":tool,"tool_use_id":id,"timestamp":ts}));
    };
    hook(&mut c, "UserPromptSubmit", "", "");
    for tool in ["request_user_input_async", "functions.request_user_input_async", "mcp__codex__request_user_input_async"] {
        for event in ["PreToolUse", "PostToolUse", "PreToolUse"] {
            hook(&mut c, event, tool, tool);
            assert_eq!(c.hub.sessions["codex:x"]["status"], "running");
            assert_eq!(c.hub.sessions["codex:x"]["pending"], json!([]));
            assert_eq!(c.hub.snapshot()["events"], json!([]));
        }
    }
    hook(&mut c, "PreToolUse", "functions.request_user_input", "sync");
    assert_eq!(c.hub.snapshot()["events"].as_array().unwrap().len(), 1);
    for event in ["PreToolUse", "PostToolUse", "PreToolUse"] {
        hook(&mut c, event, "functions.request_user_input_async", "parallel");
        assert_eq!(c.hub.sessions["codex:x"]["status"], "wait");
        let pending = c.hub.sessions["codex:x"]["pending"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["id"], "sync");
        assert_eq!(c.hub.snapshot()["events"].as_array().unwrap().len(), 1);
    }
    hook(&mut c, "PostToolUse", "functions.request_user_input", "sync");
    assert_eq!(c.hub.sessions["codex:x"]["status"], "running");
}
