//! Optional, read-only observation of Codex desktop's persisted unread metadata.
//! Runtime state remains hook-driven; missing/ambiguous data never means "read".
use crate::{
    hub::{terminal, Hub},
    text,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
    time::SystemTime,
};
pub const INTERVAL_MS: i64 = 60_000;
const HOLD_MS: i64 = 3_600_000;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
pub struct ReadStateObserver {
    next_check: i64,
    stamp: Option<(SystemTime, u64)>,
    // Only the tiny unread bucket is cached, never the full global state.
    bucket: Option<(String, HashSet<String>)>,
    observed: HashMap<(String, String), String>,
}
impl ReadStateObserver {
    pub fn poll(&mut self, path: &Path, hub: &mut Hub, time: i64) {
        let candidates: Vec<_> = hub
            .sessions
            .values()
            .filter(|s| {
                s["source"] == "codex"
                    && terminal(&text(&s["status"]))
                    && s["viewedRoundId"] != s["roundId"]
                    && time - s["endedAt"].as_i64().unwrap_or(0) < HOLD_MS
            })
            .map(|s| (text(&s["sessionId"]), text(&s["roundId"])))
            .collect();
        self.observed.retain(|key, _| candidates.contains(key));
        if candidates.is_empty() || time < self.next_check {
            return;
        }
        self.next_check = time + INTERVAL_MS;
        if self.refresh(path).is_err() {
            self.stamp = None;
            self.bucket = None;
            self.observed.clear();
            return;
        }
        let Some((bucket, unread)) = &self.bucket else {
            self.observed.clear();
            return;
        };
        for (id, round) in candidates {
            let key = (id.clone(), round.clone());
            if unread.contains(&id) {
                self.observed.insert(key, bucket.clone());
            } else if self.observed.get(&key) == Some(bucket) {
                if let Some(s) = hub.sessions.get_mut(&format!("codex:{id}")) {
                    s["viewedRoundId"] = Value::String(round);
                }
                self.observed.remove(&key);
            }
        }
    }
    fn refresh(&mut self, path: &Path) -> Result<(), ()> {
        let meta = std::fs::metadata(path).map_err(|_| ())?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return Err(());
        }
        let stamp = (meta.modified().map_err(|_| ())?, meta.len());
        if self.stamp == Some(stamp) {
            return Ok(());
        }
        let mut bytes = vec![];
        std::fs::File::open(path)
            .map_err(|_| ())?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(());
        }
        let doc: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
        let state = &doc["electron-thread-read-state-v1"];
        if state["version"] != 1 {
            return Err(());
        }
        let identities = state["unreadByIdentity"].as_object().ok_or(())?;
        let mut buckets = vec![];
        for (identity, hosts) in identities {
            for (host, ids) in hosts.as_object().ok_or(())? {
                if host != "local" && !host.starts_with("local:") {
                    continue;
                }
                let ids = ids
                    .as_array()
                    .ok_or(())?
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned).ok_or(()))
                    .collect::<Result<HashSet<_>, _>>()?;
                buckets.push((format!("{identity}/{host}"), ids));
            }
        }
        // A switch of account/host or an ambiguous file must not hide a session.
        let next = if buckets.len() == 1 {
            buckets.pop()
        } else {
            None
        };
        if self.bucket.as_ref().map(|b| &b.0) != next.as_ref().map(|b| &b.0) {
            self.observed.clear();
        }
        self.bucket = next;
        self.stamp = Some(stamp);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_observed_unread_to_read_transitions_mark_the_matching_round() {
        let dir = std::env::temp_dir().join(format!(
            "read-state-{}-{}",
            std::process::id(),
            crate::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");
        let mut h = Hub::new();
        let mut observer = ReadStateObserver::default();
        let write = |ids: Vec<&str>, account: &str| {
            std::fs::write(&file,json!({"electron-thread-read-state-v1":{"version":1,"unreadByIdentity":{account:{"local:machine":ids}}}}).to_string()).unwrap()
        };
        let finish = |h: &mut Hub, round: &str, time: i64| {
            h.ingest(
                json!({"source":"codex","sessionId":"x","roundId":round,"type":"start","ts":time}),
            );
            h.ingest(json!({"source":"codex","sessionId":"x","roundId":round,"type":"end","status":"done","ts":time}));
        };
        write(vec!["x"], "a");
        observer.poll(&file, &mut h, 1000);
        assert!(observer.stamp.is_none());
        finish(&mut h, "r", 1000);
        write(vec![], "a");
        observer.poll(&file, &mut h, 1000);
        assert!(h.sessions["codex:x"]["viewedRoundId"].is_null());
        write(vec!["x"], "a");
        observer.poll(&file, &mut h, 1001);
        assert!(observer.observed.is_empty());
        observer.poll(&file, &mut h, 61000);
        assert_eq!(observer.observed.len(), 1);
        write(vec![], "b");
        observer.poll(&file, &mut h, 121000);
        assert!(h.sessions["codex:x"]["viewedRoundId"].is_null());
        write(vec!["x"], "a");
        observer.poll(&file, &mut h, 181000);
        write(vec![], "a");
        observer.poll(&file, &mut h, 241000);
        assert_eq!(h.sessions["codex:x"]["viewedRoundId"], "r");
        finish(&mut h, "new", 301000);
        observer.poll(&file, &mut h, 301000);
        assert_ne!(h.sessions["codex:x"]["viewedRoundId"], "new");
        write(vec!["x"], "a");
        observer.poll(&file, &mut h, 361000);
        std::fs::write(&file, "invalid").unwrap();
        observer.poll(&file, &mut h, 421000);
        write(vec![], "a");
        observer.poll(&file, &mut h, 481000);
        assert_ne!(h.sessions["codex:x"]["viewedRoundId"], "new");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
