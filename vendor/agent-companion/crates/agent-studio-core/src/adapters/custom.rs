//! Custom source adapter. Routing comes from a trusted command argument
//! (`custom:<id>` is built from the registry entry), never from the payload, so a
//! raw payload cannot impersonate a built-in source.

use super::*;
use crate::custom::engine::Outcome;
use crate::custom::limits::{DETAIL_MAX, DIAGNOSTICS_MAX, PAYLOAD_BYTES};
use crate::custom::{self, template::Template};

fn template_error(error: &custom::TemplateError) -> String {
    if error.path.is_empty() {
        format!("模板无效：{}", error.message)
    } else {
        format!("模板字段 {} 无效：{}", error.path, error.message)
    }
}

fn custom_id(payload: &Value) -> Result<&str, String> {
    let id = payload["id"].as_str().unwrap_or("");
    if !custom::is_valid_id(id) {
        return Err("未知的自定义来源".into());
    }
    Ok(id)
}

impl Collector {
    /// `payload` is the RPC envelope `{"integration": "<id>", "payload": <raw>}`.
    pub fn ingest_custom_hook(&mut self, payload: &Value) -> Value {
        let requested = text(&payload["integration"]);
        if !custom::is_valid_id(&requested) || settings::SOURCES.contains(&requested.as_str()) {
            return self.reject_custom(&requested, "unknown_integration", "未注册的自定义来源");
        }
        let Some(entry) = self.custom_store.get(&requested).cloned() else {
            return self.reject_custom(&requested, "unknown_integration", "未注册的自定义来源");
        };
        if !entry.enabled {
            return self.reject_custom(&requested, "integration_disabled", "该自定义来源已停用");
        }
        let raw = payload.get("payload").cloned().unwrap_or(Value::Null);
        let size = serde_json::to_vec(&raw).map(|bytes| bytes.len()).unwrap_or(0);
        if size as u64 > PAYLOAD_BYTES {
            return self.reject_custom(&requested, "payload_too_large", "载荷超过 1 MiB 上限");
        }
        let outcome = self.custom_engine.apply(&entry.template, &raw, now());
        self.record_custom(&requested, &outcome);
        for event in &outcome.events {
            self.hub.ingest(event.clone());
        }
        outcome.to_value()
    }

    pub fn custom_status(&self) -> Value {
        let templates: Vec<Value> = self
            .custom_store
            .entries()
            .iter()
            .map(|(id, entry)| {
                let (received, mapped) = self.custom_stats.get(id).copied().unwrap_or((None, None));
                json!({
                    "id": id,
                    "source": custom::source_of(id),
                    "name": entry.template.name,
                    "enabled": entry.enabled,
                    "importedAt": entry.imported_at,
                    "capabilities": entry.template.capabilities(),
                    "events": entry.template.event_table().iter().map(|(event, action)| json!({"event":event,"action":action})).collect::<Vec<_>>(),
                    "lastReceivedAt": received,
                    "lastMappedAt": mapped,
                    "command": self.custom_command(id),
                })
            })
            .collect();
        json!({
            "version": 1,
            "storage": {"ok": self.custom_store.error().is_none(), "error": self.custom_store.error()},
            "binaryInstalled": crate::hook_binary(&self.home).is_file(),
            "templates": templates,
            "diagnostics": self.custom_diagnostics.iter().cloned().collect::<Vec<_>>(),
        })
    }

    /// Import, enable/disable or remove. Every branch validates before writing and
    /// reports the offending field path on failure.
    pub fn custom_manage(&mut self, payload: &Value) -> Result<Value, String> {
        match payload["action"].as_str().unwrap_or("") {
            "import" => {
                let template =
                    Template::parse(&payload["template"]).map_err(|error| template_error(&error))?;
                self.custom_store.import(template, now())?;
                // The rail hides sessions whose source has no health row. Publish
                // it now; waiting for the next poll drops the first events.
                self.poll_custom();
            }
            "enable" | "disable" => {
                let id = custom_id(payload)?.to_owned();
                let enabled = payload["action"] == "enable";
                self.custom_store.set_enabled(&id, enabled)?;
                self.forget_custom_sessions(&id);
                let source = custom::source_of(&id);
                if enabled {
                    self.poll_custom();
                } else {
                    self.hub.health(&source, "disabled", "已停用");
                }
            }
            "remove" => {
                let id = custom_id(payload)?.to_owned();
                if !self.custom_store.remove(&id)? {
                    return Err("未知的自定义来源".into());
                }
                self.forget_custom_sessions(&id);
                self.custom_stats.remove(&id);
                let source = custom::source_of(&id);
                self.hub.sources.remove(&source);
            }
            _ => return Err("未知的自定义接入操作".into()),
        }
        Ok(self.custom_status())
    }

    /// Side-effect free preview: the same pure mapping logic, a throwaway engine
    /// and no hub or registry writes.
    pub fn custom_preview(&self, payload: &Value) -> Value {
        let template = match Template::parse(&payload["template"]) {
            Ok(template) => template,
            Err(error) => {
                return json!({
                    "ok": false,
                    "outcome": "rejected",
                    "reason": "template_invalid",
                    "path": error.path,
                    "error": error.message,
                    "events": [],
                })
            }
        };
        let mut engine = crate::custom::Engine::new();
        let outcome = engine.apply(&template, &payload["payload"], now());
        let mut value = outcome.to_value();
        value["ok"] = json!(outcome.is_accepted());
        if !outcome.is_accepted() {
            value["error"] = json!(outcome.detail);
        }
        value["capabilities"] = json!(template.capabilities());
        value
    }

    /// Health rows for every registered template. The rail only shows sessions of
    /// a healthy source, so an enabled template must be published here.
    pub fn poll_custom(&mut self) {
        let rows: Vec<(String, bool)> = self
            .custom_store
            .entries()
            .iter()
            .map(|(id, entry)| (custom::source_of(id), entry.enabled))
            .collect();
        for (source, enabled) in rows {
            if enabled {
                self.hub.health(&source, "ok", "等待自定义 Hook 事件");
            } else {
                self.hub.health(&source, "disabled", "已停用");
            }
        }
    }

    fn custom_command(&self, id: &str) -> String {
        format!(
            "{} custom-hook --home {} --integration {}",
            crate::shell_quote(&crate::hook_binary(&self.home)),
            crate::shell_quote(&self.home),
            id
        )
    }

    fn reject_custom(&mut self, id: &str, reason: &str, detail: &str) -> Value {
        let source = if custom::is_valid_id(id) {
            custom::source_of(id)
        } else {
            custom::SOURCE_PREFIX.to_owned()
        };
        self.push_custom_diagnostic(json!({
            "at": now(),
            "source": source,
            "event": Value::Null,
            "outcome": "rejected",
            "reason": reason,
            "detail": detail,
        }));
        json!({
            "outcome": "rejected",
            "action": Value::Null,
            "reason": reason,
            "detail": detail,
            "path": "",
            "event": Value::Null,
            "sessionId": Value::Null,
            "roundId": Value::Null,
            "events": [],
        })
    }

    fn record_custom(&mut self, id: &str, outcome: &Outcome) {
        let at = now();
        let stat = self.custom_stats.entry(id.to_owned()).or_insert((None, None));
        stat.0 = Some(at);
        if outcome.is_accepted() {
            stat.1 = Some(at);
        }
        self.push_custom_diagnostic(json!({
            "at": at,
            "source": custom::source_of(id),
            "event": outcome.raw_event,
            "outcome": outcome.kind.label(),
            "reason": outcome.reason,
            "detail": outcome.detail.chars().take(DETAIL_MAX).collect::<String>(),
        }));
    }

    fn push_custom_diagnostic(&mut self, value: Value) {
        while self.custom_diagnostics.len() >= DIAGNOSTICS_MAX {
            self.custom_diagnostics.pop_front();
        }
        self.custom_diagnostics.push_back(value);
    }

    /// Drops the running state of one custom source: sessions and engine rounds.
    /// Only that source is touched.
    fn forget_custom_sessions(&mut self, id: &str) {
        let source = custom::source_of(id);
        self.custom_engine.forget_source(&source);
        self.hub
            .sessions
            .retain(|_, session| text(&session["source"]) != source);
    }
}
