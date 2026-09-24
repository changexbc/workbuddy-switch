mod codeg;
mod codeg_stream;
pub use codeg::{CodegHooks, CODEG_EVENTS, merge_codeg_webhooks};
mod codex;
pub mod custom;
mod ide;
pub use ide::{
    codebuddy_edition, codebuddy_settings_files, merge_codebuddy_ide_hooks,
    CODEBUDDY_IDE_HOOK_EVENTS,
};
mod workbuddy;
pub use workbuddy::{
    merge_workbuddy_hooks, workbuddy_edition, workbuddy_settings_files, WORKBUDDY_HOOK_EVENTS,
};
use crate::{atomic_json, hub::Hub, now, settings, text};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf};
pub fn query(db: &Connection, sql: &str) -> Result<Vec<Value>, String> {
    let mut st = db
        .prepare(sql)
        .map_err(|_| "数据库结构不兼容".to_string())?;
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let rows = st
        .query_map([], |row| {
            let mut v = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let x = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(b) | ValueRef::Blob(b) => json!(String::from_utf8_lossy(b)),
                };
                v.insert(name.clone(), x);
            }
            Ok(Value::Object(v))
        })
        .map_err(|_| "无法查询数据库")?;
    rows.map(|r| r.map_err(|_| "无法读取数据库行".into()))
        .collect()
}
pub fn open(paths: &[PathBuf]) -> Result<Connection, String> {
    for p in paths {
        if let Ok(db) = Connection::open_with_flags(
            p,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            let _ = db.busy_timeout(std::time::Duration::from_millis(150));
            return Ok(db);
        }
    }
    Err("数据库不可读".into())
}
#[derive(Default)]
pub struct Context {
    pub round: String,
    pub cwd: String,
    pub pending: Vec<Value>,
    pub seen: std::collections::HashSet<String>,
}
pub struct Collector {
    pub hub: Hub,
    pub settings: Value,
    pub home: PathBuf,
    pub live: HashMap<String, Value>,
    pub contexts: HashMap<String, Context>,
    pub tail: crate::tail::Tail,
    pub rows: HashMap<String, Value>,
    pub hook_count: u64,
    pub codeg: CodegHooks,
    pub integrations: Value,
    pub last_hook_at: HashMap<String, i64>,
    pub ide_live: HashMap<String, Value>,
    pub ide_hook_count: u64,
    pub workbuddy_live: HashMap<String, Value>,
    pub workbuddy_hook_count: u64,
    pub workbuddy_presence: crate::host_process::HostPresence,
    pub ide_presence: crate::host_process::HostPresence,
    pub vscode_presence: crate::host_process::HostPresence,
    pub codex_read_state: crate::codex_read_state::ReadStateObserver,
    pub custom_store: crate::custom::Store,
    pub custom_engine: crate::custom::Engine,
    pub custom_diagnostics: std::collections::VecDeque<Value>,
    pub custom_stats: HashMap<String, (Option<i64>, Option<i64>)>,
}
impl Collector {
    pub fn new(home: PathBuf) -> Result<Self, String> {
        let file = home.join(".agent-studio/settings.json");
        let settings = if file.exists() {
            settings::validate(
                &serde_json::from_slice(&std::fs::read(file).map_err(|e| e.to_string())?)
                    .map_err(|_| "配置文件无效")?,
            )?
        } else {
            settings::defaults()
        };
        let integrations = match std::fs::read(home.join(".agent-studio/integrations.json")) {
            Ok(bytes) => {
                let v: Value = serde_json::from_slice(&bytes).map_err(|_| "接入策略无效")?;
                if !v.is_object() || v.as_object().unwrap().iter().any(|(k,v)| !settings::SOURCES.contains(&k.as_str()) || !v.is_boolean()) { return Err("接入策略无效".into()); }
                v
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.to_string()),
        };
        let c = Self {
            integrations,
            last_hook_at: HashMap::new(),
            hub: Hub::new(),
            custom_store: crate::custom::Store::load(&home),
            custom_engine: Default::default(),
            custom_diagnostics: Default::default(),
            custom_stats: HashMap::new(),
            settings,
            home,
            live: HashMap::new(),
            contexts: HashMap::new(),
            tail: Default::default(),
            rows: HashMap::new(),
            hook_count: 0,
            codeg: Default::default(),
            ide_live: HashMap::new(),
            ide_hook_count: 0,
            workbuddy_live: HashMap::new(),
            workbuddy_hook_count: 0,
            workbuddy_presence: crate::host_process::HostPresence::for_host("workbuddy"),
            ide_presence: crate::host_process::HostPresence::for_host("codebuddy-ide"),
            vscode_presence: crate::host_process::HostPresence::for_host("vscode"),
            codex_read_state: Default::default(),
        };
        Ok(c)
    }
    pub fn poll(&mut self) {
        for id in settings::SOURCES {
            if id == "codeg" {
                if let Err(e)=self.maintain_codeg_webhook(){ self.hub.health(id, if self.settings["sources"][id]["enabled"]==true {"error"}else{"disabled"}, &e); }
                if self.settings["sources"][id]["enabled"] != true { self.hub.health(id,"disabled","已关闭监听"); }
                continue;
            }
            if self.settings["sources"][id]["enabled"] != true {
                self.hub.health(id, "disabled", "已关闭监听");
                continue;
            }
            let result = match id {
                "codex" => self.poll_codex(),
                "workbuddy" => self.poll_workbuddy(),
                _ => self.poll_ide(),
            };
            if let Err(e) = result {
                self.hub.health(id, "error", &e);
            }
        }
        self.hub.ready = true;
        self.poll_custom();
        if self.settings["sources"]["codex"]["enabled"] == true {
            let file = self.paths("codex")[0].join(".codex-global-state.json");
            self.codex_read_state.poll(&file, &mut self.hub, now());
        } else {
            self.codex_read_state = Default::default();
        }
    }
    pub fn request(&mut self, command: &str, payload: &Value) -> Result<Value, String> {
        match command {
            "settings_get" => Ok(self.settings.clone()),
            "settings_set" => {
                let next = settings::validate(payload)?;
                atomic_json(&self.home.join(".agent-studio/settings.json"), &next)?;
                for id in settings::SOURCES {
                    if next["sources"][id] != self.settings["sources"][id] {
                        for path in self.paths(id) {
                            self.tail.forget_under(&path);
                        }
                        self.hub.sessions.retain(|_, s| s["source"] != id);
                        self.rows.retain(|k, _| !k.starts_with(&format!("{id}:")));
                        self.contexts
                            .retain(|k, _| !k.starts_with(&format!("{id}:")));
                        if id == "codeg" {
                            // Unregister against the old path before switching configuration.
                            self.stop_codeg_webhook();
                            let url=self.codeg.url.clone();
                            let sink=self.codeg.stream_sink.clone();
                            self.codeg=Default::default();
                            self.codeg.stream_sink=sink;
                            self.configure_codeg_webhook(url);
                        }
                        if id == "codex" {
                            self.live.clear();
                            self.codex_read_state = Default::default();
                        }
                        if id == "codebuddy-ide" {
                            self.ide_live.clear();
                            self.ide_hook_count = 0;
                        }
                        if id == "workbuddy" {
                            self.workbuddy_live.clear();
                            self.workbuddy_hook_count = 0;
                        }
                    }
                }
                self.settings = next;
                Ok(self.settings.clone())
            }
            "settings_check" => {
                let id = payload["source"]
                    .as_str()
                    .filter(|s| settings::SOURCES.contains(s))
                    .ok_or("未知 Agent 来源")?;
                let mut v = self.settings.clone();
                v["sources"][id]["path"] = payload["path"].clone();
                let v = settings::validate(&v)?;
                let paths: Vec<_> = settings::paths(&self.home, &v, id)
                    .into_iter()
                    .filter(|p| {
                        if matches!(id, "codex" | "workbuddy" | "codebuddy-ide") {
                            std::fs::read_dir(p).is_ok()
                        } else {
                            std::fs::File::open(p).is_ok()
                        }
                    })
                    .collect();
                Ok(
                    json!({"ok":!paths.is_empty(),"paths":paths,"detail":if paths.is_empty(){"未找到可读取的数据路径"}else{"路径可读取；会话状态以监听结果为准"}}),
                )
            }
            "custom_integrations_get" => Ok(self.custom_status()),
            "custom_integrations_set" => self.custom_manage(payload),
            "custom_preview" => Ok(self.custom_preview(payload)),
            "custom_hook" => Ok(self.ingest_custom_hook(payload)),
            _ => Err("不支持的命令".into()),
        }
    }
    pub fn paths(&self, id: &str) -> Vec<PathBuf> {
        settings::paths(&self.home, &self.settings, id)
    }
    pub fn presence_mut(
        &mut self,
        source: &str,
        kind: &str,
    ) -> Option<&mut crate::host_process::HostPresence> {
        match (source, kind) {
            ("workbuddy", _) => Some(&mut self.workbuddy_presence),
            ("codebuddy-ide", "vscode") => Some(&mut self.vscode_presence),
            ("codebuddy-ide", _) => Some(&mut self.ide_presence),
            _ => None,
        }
    }
    pub fn integration_automatic(&self, source: &str) -> bool { self.integrations[source] != false }
    pub fn set_integration_automatic(&mut self, source: &str, enabled: bool) -> Result<(), String> {
        let mut next = self.integrations.clone();
        next[source] = json!(enabled);
        atomic_json(&self.home.join(".agent-studio/integrations.json"), &next)?;
        self.integrations = next;
        Ok(())
    }
    pub fn ingest_hook(&mut self, p: &Value) -> bool {
        let source = hook_agent(p).to_string();
        if !self.integration_automatic(&source) { return false; }
        let accepted = match source.as_str() {
            "codebuddy-ide" => self.ingest_ide_hook(p),
            "workbuddy" => self.ingest_workbuddy_hook(p),
            "codex" => self.ingest_codex_hook(p),
            "codeg" => self.ingest_codeg_hook(p),
            _ => false,
        };
        if accepted { self.last_hook_at.insert(source, now()); }
        accepted
    }
}
fn hook_agent(p: &Value) -> &str {
    // Codex SessionStart already uses `source` for startup/resume/clear.
    // Routing must not treat that as the Agent Studio source.
    match p["agent_source"]
        .as_str()
        .or_else(|| p["source"].as_str())
        .unwrap_or("codex")
    {
        "codebuddy-ide" => "codebuddy-ide",
        "workbuddy" => "workbuddy",
        "codex" => "codex",
        "codeg" => "codeg",
        _ => "codex",
    }
}
