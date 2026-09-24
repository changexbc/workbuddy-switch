//! Persistence for custom templates: `~/.agent-studio/custom-integrations.json`.
//!
//! Deliberately separate from `integrations.json` (the built-in automatic
//! policy) and from `settings.json`. A damaged file is reported and never
//! overwritten: imports fail until the user fixes or removes it.

use super::template::Template;
use crate::atomic_json;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const STORE_VERSION: i64 = 1;
const FILE: &str = ".agent-studio/custom-integrations.json";

#[derive(Clone, Debug)]
pub struct Entry {
    pub template: Template,
    pub enabled: bool,
    pub imported_at: i64,
}

#[derive(Debug)]
pub struct Store {
    home: PathBuf,
    entries: BTreeMap<String, Entry>,
    /// Set when the file exists but cannot be trusted; write operations refuse to
    /// run so a damaged file is never replaced by an empty one.
    pub error: Option<String>,
}

impl Store {
    pub fn load(home: &Path) -> Self {
        let mut store = Self {
            home: home.to_path_buf(),
            entries: BTreeMap::new(),
            error: None,
        };
        let path = store.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return store,
            Err(error) => {
                store.error = Some(format!("无法读取自定义接入配置：{error}"));
                return store;
            }
        };
        match parse(&bytes) {
            Ok(entries) => store.entries = entries,
            Err(message) => {
                store.error = Some(format!(
                    "自定义接入配置不可用（{message}）。已保留原文件，请修复或删除 {}",
                    path.display()
                ))
            }
        }
        store
    }

    pub fn path(&self) -> PathBuf {
        self.home.join(FILE)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn entries(&self) -> &BTreeMap<String, Entry> {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn ensure_writable(&self) -> Result<(), String> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn persist(&self) -> Result<(), String> {
        let mut templates = Map::new();
        for (id, entry) in &self.entries {
            templates.insert(
                id.clone(),
                json!({
                    "enabled": entry.enabled,
                    "importedAt": entry.imported_at,
                    "template": entry.template.to_value(),
                }),
            );
        }
        atomic_json(
            &self.path(),
            &json!({"version": STORE_VERSION, "templates": Value::Object(templates)}),
        )
    }

    /// Rejects a duplicate id instead of silently replacing the existing template.
    pub fn import(&mut self, template: Template, now: i64) -> Result<(), String> {
        self.ensure_writable()?;
        if self.entries.contains_key(&template.id) {
            return Err(format!(
                "已存在 ID 为 {} 的自定义来源，请先删除后再导入",
                template.id
            ));
        }
        let id = template.id.clone();
        self.entries.insert(
            id.clone(),
            Entry {
                template,
                enabled: true,
                imported_at: now,
            },
        );
        if let Err(error) = self.persist() {
            self.entries.remove(&id);
            return Err(error);
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, String> {
        self.ensure_writable()?;
        let Some(previous) = self.entries.remove(id) else {
            return Ok(false);
        };
        if let Err(error) = self.persist() {
            self.entries.insert(id.to_owned(), previous);
            return Err(error);
        }
        Ok(true)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        self.ensure_writable()?;
        let Some(entry) = self.entries.get_mut(id) else {
            return Err("未知的自定义来源".into());
        };
        let previous = entry.enabled;
        entry.enabled = enabled;
        if let Err(error) = self.persist() {
            if let Some(entry) = self.entries.get_mut(id) {
                entry.enabled = previous;
            }
            return Err(error);
        }
        Ok(())
    }
}

fn parse(bytes: &[u8]) -> Result<BTreeMap<String, Entry>, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "不是有效 JSON".to_string())?;
    let object = value.as_object().ok_or("顶层必须是对象")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "version" | "templates") {
            return Err(format!("未知字段 {key}"));
        }
    }
    if object.get("version").and_then(Value::as_i64) != Some(STORE_VERSION) {
        return Err("存储版本无效".into());
    }
    let mut entries = BTreeMap::new();
    let Some(templates) = object.get("templates") else {
        return Ok(entries);
    };
    let templates = templates.as_object().ok_or("templates 必须是对象")?;
    for (id, entry) in templates {
        let entry = entry.as_object().ok_or_else(|| format!("{id} 配置无效"))?;
        for key in entry.keys() {
            if !matches!(key.as_str(), "enabled" | "importedAt" | "template") {
                return Err(format!("{id} 含有未知字段 {key}"));
            }
        }
        if !entry.get("enabled").is_some_and(Value::is_boolean) {
            return Err(format!("{id} 缺少 enabled"));
        }
        if entry.get("importedAt").and_then(Value::as_i64).is_none() {
            return Err(format!("{id} 缺少 importedAt"));
        }
        let template = entry
            .get("template")
            .ok_or_else(|| format!("{id} 缺少 template"))?;
        let template =
            Template::parse(template).map_err(|error| format!("{id} 模板无效：{}{}", error.path, error.message))?;
        if template.id != *id {
            return Err(format!("{id} 与模板 id 不一致"));
        }
        entries.insert(
            id.clone(),
            Entry {
                template,
                enabled: entry["enabled"] == json!(true),
                imported_at: entry["importedAt"].as_i64().unwrap_or(0),
            },
        );
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn home() -> PathBuf {
        std::env::temp_dir().join(format!("custom-store-{}", uuid_like()))
    }
    fn uuid_like() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }
    fn template() -> Template {
        Template::parse(&json!({
            "schemaVersion":1,"id":"example-agent","name":"Example Agent","transport":"hook",
            "mapping":{"event":"/event_name","sessionId":"/session_id"},
            "events":{"prompt_submitted":{"action":"start"}}
        }))
        .unwrap()
    }

    #[test]
    fn round_trips_import_toggle_and_remove() {
        let home = home();
        let mut store = Store::load(&home);
        assert!(store.is_empty());
        store.import(template(), 100).unwrap();
        assert!(store.import(template(), 200).is_err(), "duplicate id is rejected");
        let reloaded = Store::load(&home);
        assert_eq!(reloaded.get("example-agent").unwrap().imported_at, 100);
        assert!(reloaded.get("example-agent").unwrap().enabled);
        let mut store = Store::load(&home);
        store.set_enabled("example-agent", false).unwrap();
        assert!(!Store::load(&home).get("example-agent").unwrap().enabled);
        store.remove("example-agent").unwrap();
        assert!(Store::load(&home).is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn damaged_file_is_reported_and_never_overwritten() {
        let home = home();
        std::fs::create_dir_all(home.join(".agent-studio")).unwrap();
        std::fs::write(Store::load(&home).path(), "not json").unwrap();
        let mut store = Store::load(&home);
        assert!(store.error().is_some());
        assert!(store.is_empty());
        assert!(store.import(template(), 1).is_err());
        assert_eq!(
            std::fs::read_to_string(store.path()).unwrap(),
            "not json",
            "the damaged file is preserved"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
