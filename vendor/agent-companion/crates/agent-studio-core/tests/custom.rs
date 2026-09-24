use agent_studio_core::{adapters::Collector, custom, settings};
use serde_json::{json, Value};
use std::path::PathBuf;

const FIXTURES: &str = include_str!("../../../tests/fixtures/custom-hooks.json");

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "custom-hooks-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn collector(&self) -> Collector {
        Collector::new(self.0.clone()).unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).unwrap()
}

fn apply_patch(mut value: Value, patch: &Value) -> Value {
    for step in patch.as_array().unwrap() {
        let tokens: Vec<String> = step["pointer"]
            .as_str()
            .unwrap()
            .split('/')
            .skip(1)
            .map(str::to_owned)
            .collect();
        let mut cursor = &mut value;
        for token in &tokens[..tokens.len() - 1] {
            cursor = match cursor {
                Value::Array(items) => &mut items[token.parse::<usize>().unwrap()],
                _ => &mut cursor[token],
            };
        }
        let last = tokens.last().unwrap();
        match cursor {
            Value::Array(items) if step["op"] == "set" => {
                items[last.parse::<usize>().unwrap()] = step["value"].clone();
            }
            Value::Array(_) => {
                // Only `set` is used on array items by the fixture.
                panic!("cannot remove an array item");
            }
            _ if step["op"] == "set" => cursor[last] = step["value"].clone(),
            _ => {
                cursor.as_object_mut().unwrap().remove(last);
            }
        }
    }
    value
}

fn import(collector: &mut Collector, template: &Value) {
    collector
        .custom_manage(&json!({"action":"import","template":template}))
        .unwrap_or_else(|error| panic!("import failed: {error}"));
}

/// `payloads[].template` names an entry in `templates`; the integration argument
/// is that template's id.
fn integration_id(data: &Value, key: &str) -> String {
    data["templates"][key]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("unknown template {key}"))
        .to_owned()
}

fn apply_payload(collector: &mut Collector, data: &Value, step: &Value) -> Value {
    let id = integration_id(data, step["template"].as_str().unwrap());
    collector.ingest_custom_hook(&json!({"integration": id, "payload": step["payload"].clone()}))
}

#[test]
fn limits_match_the_shared_fixture() {
    assert_eq!(custom::limits_json(), fixtures()["limits"]);
}

#[test]
fn template_validation_matches_the_shared_fixture() {
    let data = fixtures();
    for entry in data["invalid"].as_array().unwrap() {
        let candidate = apply_patch(data["base"].clone(), &entry["mutate"]);
        let error = custom::Template::parse(&candidate)
            .err()
            .unwrap_or_else(|| panic!("{} should be rejected", entry["name"]));
        assert_eq!(
            error.path,
            entry["path"].as_str().unwrap(),
            "{} reported the wrong field",
            entry["name"]
        );
    }
    for (name, template) in data["templates"].as_object().unwrap() {
        custom::Template::parse(template).unwrap_or_else(|error| {
            panic!("template {name} must be valid: {} {}", error.path, error.message)
        });
    }
}

#[test]
fn payload_cases_match_the_shared_fixture() {
    let data = fixtures();
    for case in data["cases"].as_array().unwrap() {
        let mut engine = custom::Engine::new();
        let mut last = None;
        for step in case["payloads"].as_array().unwrap() {
            let template = custom::Template::parse(&data["templates"][step["template"].as_str().unwrap()]).unwrap();
            last = Some(engine.apply(&template, &step["payload"], 1_700_000_000_000));
        }
        let outcome = last.unwrap();
        let expect = &case["expect"];
        assert_eq!(
            outcome.kind.label(),
            expect["outcome"].as_str().unwrap(),
            "case {} outcome",
            case["name"]
        );
        if let Some(reason) = expect["reason"].as_str() {
            assert_eq!(outcome.reason, reason, "case {} reason", case["name"]);
        }
        if let Some(action) = expect["action"].as_str() {
            assert_eq!(outcome.action, Some(action), "case {} action", case["name"]);
        }
        if let Some(path) = expect["path"].as_str() {
            assert_eq!(outcome.path, path, "case {} path", case["name"]);
        }
    }
}

#[test]
fn sequences_match_the_shared_fixture() {
    let data = fixtures();
    for sequence in data["sequences"].as_array().unwrap() {
        let home = Home::new();
        let mut collector = home.collector();
        for template in data["templates"].as_object().unwrap().values() {
            import(&mut collector, template);
        }
        for step in sequence["payloads"].as_array().unwrap() {
            apply_payload(&mut collector, &data, step);
        }
        check_sequence(&collector, sequence);
    }
}

fn check_sequence(collector: &Collector, sequence: &Value) {
    let expect = &sequence["expect"];
    let snapshot = collector.hub.snapshot();
    let sessions = snapshot["sessions"].as_array().unwrap();
    for expected in expect["sessions"].as_array().unwrap() {
        let id = expected["id"].as_str().unwrap();
        let session = sessions
            .iter()
            .find(|session| session["id"] == json!(id))
            .unwrap_or_else(|| panic!("{}: session {id} missing", sequence["name"]));
        if let Some(status) = expected["status"].as_str() {
            assert_eq!(session["status"], json!(status), "{id} status");
        }
        if let Some(round) = expected["roundId"].as_str() {
            assert_eq!(session["roundId"], json!(round), "{id} roundId");
        }
        if let Some(prefix) = expected["roundIdPrefix"].as_str() {
            let round = session["roundId"].as_str().unwrap();
            assert!(round.starts_with(prefix), "{id} roundId {round}");
        }
        if let Some(title) = expected["title"].as_str() {
            assert_eq!(session["title"], json!(title), "{id} title");
        }
        if let Some(project) = expected["project"].as_str() {
            assert_eq!(session["project"], json!(project), "{id} project");
        }
        if let Some(pending) = expected["pending"].as_i64() {
            assert_eq!(
                session["pending"].as_array().unwrap().len() as i64,
                pending,
                "{id} pending"
            );
        }
        match expected.get("endedBy") {
            Some(Value::String(value)) => assert_eq!(session["endedBy"], json!(value), "{id} endedBy"),
            Some(_) => assert!(session["endedBy"].is_null(), "{id} endedBy must be absent"),
            None => {}
        }
    }
    let events: Vec<Value> = snapshot["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            let mut value = json!({"kind": event["kind"], "sessionId": event["sessionId"]});
            value["roundId"] = event["roundId"].clone();
            value
        })
        .collect();
    for expected in expect["events"].as_array().unwrap() {
        let kind = expected["kind"].as_str().unwrap();
        let session = expected["sessionId"].as_str().unwrap();
        let found = events.iter().any(|event| {
            event["kind"] == json!(kind)
                && event["sessionId"] == json!(session)
                && match expected["roundId"].as_str() {
                    Some(round) => event["roundId"] == json!(round),
                    None => expected["roundIdPrefix"]
                        .as_str()
                        .is_some_and(|prefix| event["roundId"].as_str().unwrap().starts_with(prefix)),
                }
        });
        assert!(found, "{}: event {kind} for {session} missing", sequence["name"]);
    }
    let diagnostics: Vec<Value> = collector.custom_status()["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| json!({"outcome": item["outcome"], "reason": item["reason"]}))
        .collect();
    assert_eq!(
        json!(diagnostics),
        expect["diagnostics"],
        "{}: diagnostics",
        sequence["name"]
    );
}

#[test]
fn pointer_limits_count_characters_not_bytes() {
    let mut value = fixtures()["templates"]["minimal"].clone();
    value["mapping"]["event"] = json!(format!("/{}", "指".repeat(300)));
    custom::Template::parse(&value).expect("300 CJK characters are 900 bytes and still valid");
    value["mapping"]["event"] = json!(format!("/{}", "😀".repeat(300)));
    custom::Template::parse(&value).expect("emoji pointers count code points, not UTF-16 units");
    value["mapping"]["event"] = json!(format!("/{}", "指".repeat(512)));
    assert_eq!(
        custom::Template::parse(&value).unwrap_err().path,
        "/mapping/event"
    );
}

#[test]
fn source_is_derived_from_the_registry_not_the_payload() {
    let home = Home::new();
    let mut collector = home.collector();
    let data = fixtures();
    import(&mut collector, &data["templates"]["example"]);
    collector.poll();
    assert_eq!(collector.hub.sources["custom:example-agent"]["state"], "ok");
    collector.ingest_custom_hook(&json!({
        "integration":"example-agent",
        "payload":{
            "source":"codex","agent_source":"codex","agent_edition":"domestic",
            "event_name":"prompt_submitted","session_id":"forged","round_id":"r1","ts":1_700_000_000_000i64
        }
    }));
    let snapshot = collector.hub.snapshot();
    assert!(snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["id"] == json!("custom:example-agent:forged")));
    assert!(!snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["source"] == json!("codex")));
    assert!(collector.hub.sessions.get("codex:forged").is_none());
}

#[test]
fn unknown_or_disabled_integrations_are_rejected_without_sessions() {
    let home = Home::new();
    let mut collector = home.collector();
    let data = fixtures();
    import(&mut collector, &data["templates"]["example"]);
    let unknown = collector.ingest_custom_hook(&json!({
        "integration":"missing-agent",
        "payload":{"event_name":"prompt_submitted","session_id":"s","ts":1_700_000_000_000i64}
    }));
    assert_eq!(unknown["outcome"], json!("rejected"));
    assert_eq!(unknown["reason"], json!("unknown_integration"));
    // A built-in id can never be requested through the custom command.
    let builtin = collector.ingest_custom_hook(&json!({
        "integration":"codex","payload":{"event_name":"prompt_submitted","session_id":"s"}
    }));
    assert_eq!(builtin["reason"], json!("unknown_integration"));
    collector.custom_manage(&json!({"action":"disable","id":"example-agent"})).unwrap();
    let disabled = collector.ingest_custom_hook(&json!({
        "integration":"example-agent",
        "payload":{"event_name":"prompt_submitted","session_id":"s","ts":1_700_000_000_000i64}
    }));
    assert_eq!(disabled["reason"], json!("integration_disabled"));
    assert!(collector.hub.sessions.is_empty());
    assert_eq!(
        collector.hub.sources["custom:example-agent"]["state"],
        json!("disabled")
    );
}

#[test]
fn disable_and_remove_clear_only_that_source() {
    let home = Home::new();
    let mut collector = home.collector();
    let data = fixtures();
    import(&mut collector, &data["templates"]["example"]);
    import(&mut collector, &data["templates"]["minimal"]);
    collector.ingest_custom_hook(&json!({
        "integration":"example-agent",
        "payload":{"event_name":"prompt_submitted","session_id":"a","round_id":"r1","ts":1_700_000_000_000i64}
    }));
    collector.ingest_custom_hook(&json!({
        "integration":"minimal-agent","payload":{"kind":"begin","sid":"b"}
    }));
    assert_eq!(collector.hub.sessions.len(), 2);
    collector.custom_manage(&json!({"action":"remove","id":"example-agent"})).unwrap();
    assert_eq!(collector.hub.sessions.len(), 1);
    assert!(collector.hub.sessions.contains_key("custom:minimal-agent:b"));
    assert!(collector.hub.sources.get("custom:example-agent").is_none());
    assert!(collector.custom_status()["templates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["id"] != json!("example-agent")));
    assert!(collector
        .custom_manage(&json!({"action":"remove","id":"example-agent"}))
        .is_err());
    assert!(!home.0.join(".agent-studio/custom-integrations.json").exists() == false);
}

#[test]
fn status_reports_capabilities_command_and_event_times() {
    let home = Home::new();
    let mut collector = home.collector();
    let data = fixtures();
    import(&mut collector, &data["templates"]["example"]);
    collector.poll();
    let status = collector.custom_status();
    let template = &status["templates"][0];
    assert_eq!(template["source"], json!("custom:example-agent"));
    assert_eq!(
        template["capabilities"],
        json!(["close", "finish:done", "finish:error", "resume", "start", "wait:input", "wait:permission"])
    );
    assert!(
        template["lastReceivedAt"].is_null(),
        "an imported template has not received anything yet"
    );
    assert!(template["lastMappedAt"].is_null());
    let command = template["command"].as_str().unwrap();
    assert!(command.contains("custom-hook"));
    assert!(command.contains("--integration example-agent"));
    assert!(command.contains("--home"));
    assert!(status["storage"]["ok"] == json!(true));
    collector.ingest_custom_hook(&json!({
        "integration":"example-agent",
        "payload":{"event_name":"prompt_submitted","session_id":"a","round_id":"r1","ts":1_700_000_000_000i64}
    }));
    assert!(collector.custom_status()["templates"][0]["lastReceivedAt"].is_number());
    assert!(collector.custom_status()["templates"][0]["lastMappedAt"].is_number());
}

#[test]
fn duplicate_import_is_rejected_and_keeps_the_first_template() {
    let home = Home::new();
    let mut collector = home.collector();
    let data = fixtures();
    import(&mut collector, &data["templates"]["example"]);
    let mut renamed = data["templates"]["example"].clone();
    renamed["name"] = json!("Renamed Agent");
    let error = collector
        .custom_manage(&json!({"action":"import","template":renamed}))
        .unwrap_err();
    assert!(error.contains("请先删除"), "{error}");
    assert_eq!(
        collector.custom_status()["templates"][0]["name"],
        json!("Example Agent")
    );
    let reloaded = home.collector();
    assert_eq!(reloaded.custom_status()["templates"][0]["name"], json!("Example Agent"));
}

#[test]
fn preview_maps_without_side_effects() {
    let home = Home::new();
    let collector = home.collector();
    let data = fixtures();
    let template = data["templates"]["example"].clone();
    let preview = collector.custom_preview(&json!({
        "template": template,
        "payload": {"event_name":"prompt_submitted","session_id":"preview","round_id":"r1","prompt":"Hi","ts":1_700_000_000_000i64}
    }));
    assert_eq!(preview["ok"], json!(true));
    assert_eq!(preview["action"], json!("start"));
    assert_eq!(preview["events"][0]["type"], json!("start"));
    assert!(collector.hub.sessions.is_empty(), "preview must not create sessions");
    assert!(collector.custom_status()["templates"].as_array().unwrap().is_empty());
    assert!(collector.custom_status()["diagnostics"].as_array().unwrap().is_empty());
    let rejected = collector.custom_preview(&json!({
        "template": template,
        "payload": {"event_name":"prompt_submitted"}
    }));
    assert_eq!(rejected["ok"], json!(false));
    assert_eq!(rejected["outcome"], json!("rejected"));
    assert_eq!(rejected["path"], json!("/mapping/sessionId"));
    let invalid = collector.custom_preview(&json!({"template": {"schemaVersion": 9}, "payload": {}}));
    assert_eq!(invalid["reason"], json!("template_invalid"));
    assert_eq!(invalid["path"], json!("/schemaVersion"));
}

#[test]
fn damaged_store_is_reported_and_builtins_still_start() {
    let home = Home::new();
    std::fs::create_dir_all(home.0.join(".agent-studio")).unwrap();
    std::fs::write(
        home.0.join(".agent-studio/custom-integrations.json"),
        "{not json",
    )
    .unwrap();
    let mut collector = home.collector();
    let status = collector.custom_status();
    assert_eq!(status["storage"]["ok"], json!(false));
    assert!(status["storage"]["error"].as_str().unwrap().contains("保留原文件"));
    collector.poll();
    for source in settings::SOURCES {
        assert!(
            collector.hub.sources.contains_key(source),
            "built-in source {source} must still publish health"
        );
    }
    assert!(collector
        .custom_manage(&json!({"action":"import","template":fixtures()["templates"]["example"].clone()}))
        .unwrap_err()
        .contains("保留原文件"));
    assert_eq!(
        std::fs::read_to_string(home.0.join(".agent-studio/custom-integrations.json")).unwrap(),
        "{not json"
    );
}

// A registered template must publish health during poll, otherwise the rail
// filters its sessions out as an unknown source.
#[test]
fn enabled_templates_publish_health_rows() {
    let home = Home::new();
    let mut collector = home.collector();
    let data = fixtures();
    import(&mut collector, &data["templates"]["example"]);
    import(&mut collector, &data["templates"]["minimal"]);
    assert_eq!(
        collector.hub.sources["custom:example-agent"]["state"],
        json!("ok"),
        "import publishes health immediately; the rail drops a source with no row"
    );
    collector.poll();
    assert_eq!(collector.hub.sources["custom:example-agent"]["state"], json!("ok"));
    assert_eq!(collector.hub.sources["custom:minimal-agent"]["state"], json!("ok"));
    collector.custom_manage(&json!({"action":"disable","id":"minimal-agent"})).unwrap();
    assert_eq!(collector.hub.sources["custom:minimal-agent"]["state"], json!("disabled"));
    assert_eq!(collector.hub.sources["custom:example-agent"]["state"], json!("ok"));
}
