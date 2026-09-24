use crate::{now, text};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
pub const STALE: i64 = 45 * 60 * 1000;
pub fn terminal(s: &str) -> bool {
    matches!(s, "done" | "error" | "aborted")
}
#[derive(Default)]
pub struct Hub {
    pub sessions: BTreeMap<String, Value>,
    pub events: VecDeque<Value>,
    pub sources: BTreeMap<String, Value>,
    pub ready: bool,
    pub started: i64,
}
impl Hub {
    pub fn new() -> Self {
        Self {
            started: now(),
            ..Self::default()
        }
    }
    pub fn health(&mut self, source: &str, state: &str, detail: &str) {
        self.sources.insert(
            source.into(),
            json!({"state":state,"detail":detail,"checkedAt":now()}),
        );
    }
    pub fn ingest(&mut self, ev: Value) {
        let source = text(&ev["source"]);
        let sid = text(&ev["sessionId"]);
        if source.is_empty() || sid.is_empty() {
            return;
        }
        let id = format!("{source}:{sid}");
        let ts = ev["ts"].as_i64().unwrap_or_else(now);
        let ty = text(&ev["type"]);
        let round = text(&ev["roundId"]);
        let s=self.sessions.entry(id.clone()).or_insert_with(||json!({"id":id,"source":source,"sessionId":sid,"cwd":"","title":"","roundId":if round.is_empty(){format!("observed:{ts}")}else{round.clone()},"startedAt":ts,"updatedAt":0,"status":"unknown","steps":[],"pending":[],"tokens":null,"endedAt":null}));
        for key in [
            "cwd",
            "title",
            "folderId",
            "agentType",
            "externalId",
            "webPort",
            "hostKind",
            "sourceLabel",
        ] {
            if !ev[key].is_null() && !text(&ev[key]).is_empty() {
                s[key] = if key == "title" {
                    json!(text(&ev[key]).chars().take(240).collect::<String>())
                } else if key == "sourceLabel" {
                    json!(text(&ev[key]).chars().take(60).collect::<String>())
                } else {
                    ev[key].clone()
                };
            }
        }
        if ty == "meta" {
            if !round.is_empty() && text(&s["roundId"]).starts_with("observed:") {
                s["roundId"] = json!(round);
            }
            return;
        }
        // Anonymous hook approvals have no matching tool result id. Reconcile
        // against model continuation, including transcript replay after restore.
        if source == "codex" && ty == "activity" {
            s["pending"].as_array_mut().unwrap().retain(|p| {
                !(text(&p["id"]).starts_with("perm:")
                    && p["ts"].as_i64().unwrap_or(i64::MAX) < ts)
            });
            if s["status"] == "wait" && s["pending"].as_array().unwrap().is_empty() {
                s["status"] = json!("running");
            }
        }
        let same_round_request = source == "codex" && !round.is_empty()
            && round == text(&s["roundId"]) && matches!(ty.as_str(), "wait" | "resolve");
        if ts < s["updatedAt"].as_i64().unwrap_or(0) && !same_round_request {
            return;
        }
        if ty == "start" && round != text(&s["roundId"]) {
            s["roundId"] = json!(if round.is_empty() {
                format!("turn:{ts}")
            } else {
                round.clone()
            });
            s["startedAt"] = json!(ts);
            s["endedAt"] = Value::Null;
            s["pending"] = json!([]);
            s["permissionChecks"] = json!([]);
            s["steps"] = json!([]);
            s["status"] = json!("running");
            s["tokens"] = Value::Null;
            if let Some(o) = s.as_object_mut() {
                o.remove("endedBy");
            }
        }
        if !round.is_empty() && text(&s["roundId"]).starts_with("observed:") {
            s["roundId"] = json!(round);
        }
        if !round.is_empty() && ty != "start" && round != text(&s["roundId"]) {
            return;
        }
        s["updatedAt"] = json!(ts.max(s["updatedAt"].as_i64().unwrap_or(0)));
        let mut event = None;
        match ty.as_str() {
            "start" => s["status"] = json!(if s["pending"].as_array().unwrap().is_empty() { "running" } else { "wait" }),
            "step" if !terminal(&text(&s["status"])) => {
                let a = s["steps"].as_array_mut().unwrap();
                if !a.iter().any(|x| x["id"] == ev["eventId"]) {
                    a.push(json!({"id":ev["eventId"],"ts":ts,"label":text(&ev["label"]).chars().take(200).collect::<String>()}));
                }
                if a.len() > 20 {
                    a.remove(0);
                }
                s["status"] = json!(if s["pending"].as_array().unwrap().is_empty() {
                    "running"
                } else {
                    "wait"
                });
            }
            "wait" if !terminal(&text(&s["status"])) => {
                let a = s["pending"].as_array_mut().unwrap();
                if !a.iter().any(|x| x["id"] == ev["callId"]) {
                    a.push(json!({"id":ev["callId"],"tool":ev["tool"],"text":text(&ev["text"]).chars().take(500).collect::<String>(),"questions":ev["questions"].as_array().cloned().unwrap_or_default(),"ts":ts}));
                }
                s["status"] = json!("wait");
                event = Some(("wait".to_owned(), ev["callId"].clone()));
            }
            "resolve" => {
                s["pending"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|p| p["id"] != ev["callId"]);
                if !terminal(&text(&s["status"])) {
                    s["status"] = json!(if s["pending"].as_array().unwrap().is_empty() {
                        "running"
                    } else {
                        "wait"
                    });
                }
            }
            "permission_check" if !terminal(&text(&s["status"])) => {
                if !s["permissionChecks"].is_array() { s["permissionChecks"] = json!([]); }
                let checks = s["permissionChecks"].as_array_mut().unwrap();
                if !checks.contains(&ev["callId"]) { checks.push(ev["callId"].clone()); }
            }
            "permission_resolve" => {
                if !s["permissionChecks"].is_array() { s["permissionChecks"] = json!([]); }
                s["permissionChecks"].as_array_mut().unwrap().retain(|id| *id != ev["callId"]);
            }
            "end" => {
                s["status"] = ev["status"].clone();
                s["endedAt"] = json!(ts);
                s["pending"] = json!([]);
                s["permissionChecks"] = json!([]);
                if !text(&ev["endedBy"]).is_empty() {
                    s["endedBy"] = ev["endedBy"].clone();
                }
                if matches!(text(&s["status"]).as_str(), "done" | "error") {
                    event = Some((text(&s["status"]), s["roundId"].clone()));
                }
            }
            "tokens" if ev["tokens"].is_number() => s["tokens"] = ev["tokens"].clone(),
            _ => {}
        }
        if let Some((kind, key)) = event {
            let eid = json!([id, s["roundId"], kind, key]).to_string();
            if !self.events.iter().any(|e| e["id"] == eid) {
                self.events.push_back(json!({"id":eid,"sessionId":id,"roundId":s["roundId"],"kind":kind,"ts":ts,"historical":!self.ready||ts<self.started,"title":s["title"]}));
            }
            if self.events.len() > 500 {
                self.events.pop_front();
            }
        }
    }
    pub fn snapshot(&self) -> Value {
        let time = now();
        let hosted: HashSet<String> = self
            .sessions
            .values()
            .filter(|s| s["source"] == "codeg")
            .map(|s| text(&s["externalId"]).trim_start_matches("thr_").to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let mut sessions: Vec<Value> = self
            .sessions
            .values()
            .filter(|s| {
                !(s["source"] == "codex"
                    && hosted.contains(text(&s["sessionId"]).trim_start_matches("thr_")))
            })
            .cloned()
            .map(|mut s| {
                let stale = !terminal(&text(&s["status"]))
                    && s["pending"].as_array().is_some_and(|a| a.is_empty())
                    && time - s["updatedAt"].as_i64().unwrap_or(0) > STALE;
                let cwd = text(&s["cwd"]);
                s["project"] = json!(std::path::Path::new(&cwd)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .filter(|v| !v.is_empty())
                    .unwrap_or(s["source"].as_str().unwrap_or("")));
                s["stale"] = json!(stale);
                if stale {
                    s["status"] = json!("unknown");
                }
                s["elapsed"] = json!(((s["endedAt"].as_i64().unwrap_or(time)
                    - s["startedAt"].as_i64().unwrap_or(time))
                    as f64
                    / 1000.)
                    .max(0.));
                s["progress"] = Value::Null;
                s
            })
            .collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s["updatedAt"].as_i64().unwrap_or(0)));
        let mut events: Vec<_> = self.events.iter().cloned().collect();
        for s in &sessions {
            for p in s["pending"].as_array().into_iter().flatten() {
                let id = json!([s["id"], s["roundId"], "wait", p["id"]]).to_string();
                if !events.iter().any(|e| e["id"] == id) {
                    events.push(json!({"id":id,"sessionId":s["id"],"roundId":s["roundId"],"kind":"wait","ts":p["ts"],"historical":true,"title":s["title"]}));
                }
            }
        }
        json!({"version":1,"ready":self.ready,"ts":time,"sources":self.sources,"sessions":sessions,"events":events})
    }
}
