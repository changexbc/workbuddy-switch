//! Bounded state for Codex sessions already observed by this application.
//! The hook process and collector serialize all changes through one lock.
use crate::{atomic_json, now, text};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{fs::OpenOptions, path::Path};

const MAX_BYTES: u64 = 512 * 1024;
const MAX_SESSIONS: usize = 128;
const ACTIVE_AGE: i64 = 24 * 60 * 60 * 1000;
const FINISHED_AGE: i64 = 60 * 60 * 1000;

fn path(home: &Path) -> std::path::PathBuf { home.join(".agent-studio/codex-recovery-v1.json") }

fn bounded_text(value: &Value, max_bytes: usize) -> String {
    let mut result = String::new();
    for ch in text(value).chars() {
        if result.len() + ch.len_utf8() > max_bytes { break; }
        result.push(ch);
    }
    result
}

fn allowed(settings: &Value, integrations: &Value) -> bool {
    settings["sources"]["codex"]["enabled"] == true && integrations["codex"] != false
}

fn fresh_record(sid: &str, record: &Value, time: i64) -> bool {
    let s = &record["session"];
    let live = &record["live"];
    if sid.is_empty() || sid.len() > 200 || s["id"] != format!("codex:{sid}")
        || s["source"] != "codex" || s["sessionId"] != sid
        || !s["roundId"].is_string() || text(&s["roundId"]).is_empty() || text(&s["roundId"]).len() > 200 || live["roundId"] != s["roundId"]
        || !s["cwd"].is_string() || !s["title"].is_string()
        || !s["steps"].is_array() || !s["pending"].is_array()
        || !live["calls"].is_object() || !live["permissions"].is_array() || !live["ended"].is_boolean()
        || matches!(text(&s["status"]).as_str(), "done" | "error" | "aborted") && live["ended"] != true
        || !matches!(text(&s["status"]).as_str(), "running" | "wait" | "done" | "error" | "aborted" | "unknown") {
        return false;
    }
    let ts = s["updatedAt"].as_i64().unwrap_or(0);
    if ts <= 0 || ts > time + 60_000 { return false; }
    let finished = matches!(text(&s["status"]).as_str(), "done" | "error" | "aborted");
    time.saturating_sub(ts) <= if finished { FINISHED_AGE } else { ACTIVE_AGE }
}

fn with_store<T>(home: &Path, settings: &Value, integrations: &Value, write: bool, f: impl FnOnce(&mut Value) -> Result<T, String>) -> Result<T, String> {
    if !allowed(settings, integrations) { return Err("Codex 监听已关闭".into()); }
    let dir = home.join(".agent-studio");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(dir.join("codex-recovery-v1.lock")).map_err(|e| e.to_string())?;
    lock.lock_exclusive().map_err(|e| e.to_string())?;
    // The hook process may have read settings just before the desktop changed
    // them. Verify the current policy again *inside* the common lock.
    let current_settings: Value = match std::fs::read(dir.join("settings.json")) {
        Ok(bytes) => crate::settings::validate(&serde_json::from_slice(&bytes).map_err(|_| "配置文件无效")?)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crate::settings::defaults(),
        Err(e) => return Err(e.to_string()),
    };
    let current_integrations: Value = match std::fs::read(dir.join("integrations.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "接入策略无效")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.to_string()),
    };
    if !current_integrations.is_object() || current_integrations.as_object().unwrap().values().any(|v| !v.is_boolean()) {
        return Err("接入策略无效".into());
    }
    if !allowed(&current_settings, &current_integrations)
        || current_settings["sources"]["codex"] != settings["sources"]["codex"]
        || current_integrations["codex"] != integrations["codex"] {
        return Err("Codex 接入配置已改变".into());
    }
    let configured_path = settings["sources"]["codex"]["path"].as_str().unwrap_or("");
    let mut data: Value = std::fs::metadata(path(home)).ok().filter(|m| m.len() <= MAX_BYTES)
        .and_then(|_| std::fs::read(path(home)).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|v: &Value| v["version"] == 1 && v["path"] == configured_path && v["sessions"].is_object())
        .unwrap_or_else(|| json!({"version":1,"path":configured_path,"sessions":{}}));
    let time = now();
    data["sessions"].as_object_mut().unwrap().retain(|sid, record| fresh_record(sid, record, time));
    let result = f(&mut data)?;
    let sessions = data["sessions"].as_object_mut().unwrap();
    if sessions.len() > MAX_SESSIONS {
        let mut keys: Vec<_> = sessions.iter().map(|(k,v)| (k.clone(), v["session"]["updatedAt"].as_i64().unwrap_or(0))).collect();
        keys.sort_by_key(|(_, ts)| *ts);
        for (key, _) in keys.into_iter().take(sessions.len() - MAX_SESSIONS) { sessions.remove(&key); }
    }
    if write {
        if data.to_string().len() > MAX_BYTES as usize { return Err("Codex 恢复文件超过容量上限".into()); }
        atomic_json(&path(home), &data)?;
    }
    Ok(result)
}

pub fn load(home: &Path, settings: &Value, integrations: &Value) -> Vec<Value> {
    with_store(home, settings, integrations, false, |data| Ok(data["sessions"].as_object().unwrap().values().cloned().collect())).unwrap_or_default()
}

pub fn save(home: &Path, settings: &Value, integrations: &Value, sid: &str, session: &Value, live: &Value) -> Result<Value, String> {
    if sid.is_empty() || sid.len() > 200 { return Err("无效会话 ID".into()); }
    if !session["roundId"].is_string() || text(&session["roundId"]).len() > 200 { return Err("无效轮次 ID".into()); }
    with_store(home, settings, integrations, true, |data| {
        let prior = &data["sessions"][sid];
        let prior_session = &prior["session"];
        let same_round = prior_session["roundId"] == session["roundId"];
        let prior_terminal = matches!(text(&prior_session["status"]).as_str(), "done" | "error" | "aborted");
        let prior_ts = prior_session["updatedAt"].as_i64().unwrap_or(0);
        let current_ts = session["updatedAt"].as_i64().unwrap_or(0);
        let current_terminal = matches!(text(&session["status"]).as_str(), "done" | "error" | "aborted");
        if same_round && prior_terminal && (prior_ts > current_ts || prior_ts == current_ts && !current_terminal) {
            return Ok(prior.clone());
        }
        let compact = json!({
            "id":session["id"], "source":"codex", "sessionId":sid,
            "cwd":bounded_text(&session["cwd"], 512),
            "title":bounded_text(&session["title"], 960),
            "roundId":session["roundId"], "startedAt":session["startedAt"],
            "updatedAt":session["updatedAt"], "endedAt":session["endedAt"],
            "status":session["status"], "steps":[], "pending":[], "tokens":null,
            "viewedRoundId":session["viewedRoundId"]
        });
        let record = json!({"session":compact,"live":{"roundId":live["roundId"],"cwd":bounded_text(&live["cwd"], 512),"calls":{},"permissions":[],"ended":live["ended"]}});
        data["sessions"][sid] = record.clone();
        Ok(record)
    })
}

pub fn record_offline_terminal(home: &Path, hook: &Value) -> Result<bool, String> {
    let settings_path = home.join(".agent-studio/settings.json");
    let settings = match std::fs::read(settings_path) {
        Ok(bytes) => crate::settings::validate(&serde_json::from_slice(&bytes).map_err(|_| "配置文件无效")?)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crate::settings::defaults(),
        Err(e) => return Err(e.to_string()),
    };
    let integrations: Value = match std::fs::read(home.join(".agent-studio/integrations.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "接入策略无效")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.to_string()),
    };
    if !integrations.is_object() || integrations.as_object().unwrap().values().any(|v| !v.is_boolean()) {
        return Err("接入策略无效".into());
    }
    let event = hook["hook_event_name"].as_str().or(hook["hookEventName"].as_str()).unwrap_or("");
    if !matches!(event, "Stop" | "Interrupt" | "SessionEnd") { return Ok(false); }
    let sid = hook["session_id"].as_str().or(hook["sessionId"].as_str()).unwrap_or("");
    if sid.is_empty() || sid.len() > 200 { return Ok(false); }
    with_store(home, &settings, &integrations, true, |data| {
        let record = &mut data["sessions"][sid];
        if !record.is_object() { return Ok(false); }
        let round = hook["turn_id"].as_str().or(hook["turnId"].as_str()).unwrap_or("");
        if !round.is_empty() && record["session"]["roundId"] != round { return Ok(false); }
        let ts = hook["timestamp"].as_i64().filter(|v| *v > 0).unwrap_or_else(now);
        if ts > now() + 60_000 || ts < record["session"]["updatedAt"].as_i64().unwrap_or(0) { return Ok(false); }
        record["session"]["status"] = json!(if event == "Interrupt" { "aborted" } else { "done" });
        record["session"]["updatedAt"] = json!(ts);
        record["session"]["endedAt"] = json!(ts);
        record["live"]["ended"] = json!(true);
        Ok(true)
    })
}

pub fn clear(home: &Path) {
    let lock_path = home.join(".agent-studio/codex-recovery-v1.lock");
    if let Ok(lock) = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path) {
        if lock.lock_exclusive().is_ok() { let _ = std::fs::remove_file(path(home)); }
    }
}

pub fn remove(home: &Path, settings: &Value, integrations: &Value, sid: &str) {
    let _ = with_store(home, settings, integrations, true, |data| {
        data["sessions"].as_object_mut().unwrap().remove(sid);
        Ok(())
    });
}

/// Forget exactly one tracked round. A later nonterminal hook may track it again.
pub fn remove_round(home: &Path, settings: &Value, integrations: &Value, sid: &str, round: &str) -> Result<bool, String> {
    if sid.is_empty() || sid.len() > 200 || round.is_empty() || round.len() > 200 {
        return Err("无效的 Codex 会话或轮次 ID".into());
    }
    with_store(home, settings, integrations, true, |data| {
        let sessions = data["sessions"].as_object_mut().unwrap();
        // A session inherited from a pre-recovery service can still be present
        // in Hub memory with no store record. The caller has already checked
        // its exact round, so durably recording absence is a successful close.
        // A different persisted round, however, must never be removed.
        if sessions.get(sid).is_some_and(|record| record["session"]["roundId"] != round) {
            return Ok(false);
        }
        sessions.remove(sid);
        Ok(true)
    })
}
