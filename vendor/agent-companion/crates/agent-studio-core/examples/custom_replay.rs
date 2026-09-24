//! Rust side of the custom-hook parity check. Reads the shared fixture from
//! stdin and prints a normalized result set that `scripts/qa-custom-parity.mjs`
//! compares against the Node collector's output.
//!
//! Timestamps that depend on the wall clock (assigned round ids) and elapsed
//! times are normalized away; everything else must match exactly.

use agent_studio_core::{custom, hub::Hub};
use serde_json::{json, Value};
use std::io::Read;

fn normalize_round(round: &str) -> String {
    match round.strip_prefix("custom:") {
        Some(rest) if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) => {
            "custom:assigned".to_owned()
        }
        _ => round.to_owned(),
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn normalize_event(event: &Value) -> Value {
    let mut out = json!({"type": event["type"]});
    for key in [
        "roundId",
        "callId",
        "status",
        "endedBy",
        "text",
        "tool",
        "source",
        "sourceLabel",
        "title",
        "cwd",
        "sessionId",
    ] {
        if event[key].is_null() {
            continue;
        }
        out[key] = if key == "roundId" {
            json!(normalize_round(&text(&event[key])))
        } else {
            event[key].clone()
        };
    }
    out
}

fn normalize_snapshot(hub: &Hub) -> (Vec<Value>, Vec<Value>) {
    let snapshot = hub.snapshot();
    let sessions = snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| {
            json!({
                "id": session["id"],
                "sessionId": session["sessionId"],
                "source": session["source"],
                "sourceLabel": session["sourceLabel"],
                "status": session["status"],
                "roundId": normalize_round(&text(&session["roundId"])),
                "title": session["title"],
                "cwd": session["cwd"],
                "project": session["project"],
                "pending": session["pending"].as_array().map(|items| items.iter().map(|p| p["id"].clone()).collect::<Vec<_>>()).unwrap_or_default(),
                "ended": !session["endedAt"].is_null(),
                "endedBy": session["endedBy"],
                "stale": session["stale"],
            })
        })
        .collect();
    let events = snapshot["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            json!({
                "kind": event["kind"],
                "sessionId": event["sessionId"],
                "roundId": normalize_round(&text(&event["roundId"])),
            })
        })
        .collect();
    (sessions, events)
}

fn main() {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).unwrap();
    let data: Value = serde_json::from_str(&raw).unwrap();
    let templates = data["templates"].as_object().unwrap();

    let invalid: Vec<Value> = data["invalid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let candidate = patch(data["base"].clone(), &entry["mutate"]);
            match custom::Template::parse(&candidate) {
                Ok(_) => json!({"name": entry["name"], "path": Value::Null}),
                Err(error) => json!({"name": entry["name"], "path": error.path}),
            }
        })
        .collect();

    let cases: Vec<Value> = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let mut engine = custom::Engine::new();
            let mut outcome = None;
            let mut last = None;
            for step in case["payloads"].as_array().unwrap() {
                let template =
                    custom::Template::parse(&templates[step["template"].as_str().unwrap()]).unwrap();
                let payload = &step["payload"];
                let session = text(&payload["session_id"]);
                outcome = Some(engine.apply(&template, payload, 1_700_000_000_000));
                last = Some((text(&templates[step["template"].as_str().unwrap()]["id"]), session));
            }
            let outcome = outcome.unwrap();
            let state = last.as_ref().and_then(|(id, session)| {
                engine
                    .session_state(&custom::source_of(id), session)
                    .map(|(round, active, pending)| {
                        json!({"roundId": normalize_round(&round), "active": active, "pending": pending})
                    })
            });
            json!({
                "name": case["name"],
                "outcome": outcome.kind.label(),
                "action": outcome.action,
                "reason": outcome.reason,
                "path": outcome.path,
                "events": outcome.events.iter().map(normalize_event).collect::<Vec<_>>(),
                "state": state,
            })
        })
        .collect();

    let sequences: Vec<Value> = data["sequences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sequence| {
            let mut engine = custom::Engine::new();
            let mut hub = Hub::new();
            hub.started = 1;
            hub.ready = true;
            let mut diagnostics = Vec::new();
            for step in sequence["payloads"].as_array().unwrap() {
                let template =
                    custom::Template::parse(&templates[step["template"].as_str().unwrap()]).unwrap();
                let outcome = engine.apply(&template, &step["payload"], 1_700_000_000_000);
                diagnostics.push(json!({"outcome": outcome.kind.label(), "reason": outcome.reason}));
                for event in &outcome.events {
                    hub.ingest(event.clone());
                }
            }
            let (sessions, events) = normalize_snapshot(&hub);
            json!({
                "name": sequence["name"],
                "diagnostics": diagnostics,
                "sessions": sessions,
                "events": events,
            })
        })
        .collect();

    println!(
        "{}",
        json!({
            "limits": custom::limits_json(),
            "invalid": invalid,
            "cases": cases,
            "sequences": sequences,
        })
    );
}

fn patch(mut value: Value, patch: &Value) -> Value {
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
            Value::Array(_) => panic!("cannot remove an array item"),
            _ if step["op"] == "set" => cursor[last] = step["value"].clone(),
            _ => {
                cursor.as_object_mut().unwrap().remove(last);
            }
        }
    }
    value
}
