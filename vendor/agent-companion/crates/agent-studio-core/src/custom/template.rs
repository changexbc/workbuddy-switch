//! Template parsing and validation for the custom hook v1 contract.
//!
//! Unknown keys are rejected everywhere on purpose: a typo in `mapping` would
//! otherwise be silently ignored and the integration would look installed while
//! never mapping anything.

use super::limits::*;
use super::{is_valid_id, pointer};
use crate::settings::SOURCES;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateError {
    /// JSON-Pointer-ish path of the offending field, e.g. `/mapping/sessionId`.
    pub path: String,
    pub message: String,
}

impl TemplateError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Permission,
    Input,
}

impl WaitReason {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "permission" => Some(Self::Permission),
            "input" => Some(Self::Input),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Input => "input",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishStatus {
    Done,
    Error,
}

impl FinishStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Wait(WaitReason),
    Resume,
    Finish(FinishStatus),
    Close,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Wait(_) => "wait",
            Self::Resume => "resume",
            Self::Finish(_) => "finish",
            Self::Close => "close",
        }
    }
    /// Capability shown to the user; a mapping is a claim, not evidence that the
    /// target tool ever emits the event.
    pub fn capability(self) -> String {
        match self {
            Self::Start => "start".into(),
            Self::Resume => "resume".into(),
            Self::Close => "close".into(),
            Self::Wait(reason) => format!("wait:{}", reason.label()),
            Self::Finish(status) => format!("finish:{}", status.label()),
        }
    }
    fn to_value(self) -> Value {
        match self {
            Self::Start => json!({"action":"start"}),
            Self::Resume => json!({"action":"resume"}),
            Self::Close => json!({"action":"close"}),
            Self::Wait(reason) => json!({"action":"wait","reason":reason.label()}),
            Self::Finish(status) => json!({"action":"finish","status":status.label()}),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Mapping {
    pub event: String,
    pub session_id: String,
    pub round_id: Option<String>,
    pub event_id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub mapping: Mapping,
    pub ignore_if_present: Vec<String>,
    pub events: BTreeMap<String, Action>,
}

const TOP_KEYS: [&str; 7] = [
    "schemaVersion",
    "id",
    "name",
    "transport",
    "mapping",
    "ignoreIfPresent",
    "events",
];
const MAPPING_KEYS: [&str; 8] = [
    "event",
    "sessionId",
    "roundId",
    "eventId",
    "timestamp",
    "cwd",
    "title",
    "requestId",
];
const ACTION_KEYS: [&str; 3] = ["action", "reason", "status"];

fn require(value: &Value, path: &str) -> Result<(), TemplateError> {
    if value.is_null() {
        return Err(TemplateError::new(path, "缺少必填字段"));
    }
    Ok(())
}

fn pointer_field(value: &Value, path: &str) -> Result<String, TemplateError> {
    let message = "必须为以 / 开头的 JSON Pointer（RFC 6901）";
    let text = value
        .as_str()
        .filter(|text| text.chars().count() <= POINTER_MAX)
        .ok_or_else(|| TemplateError::new(path, message))?;
    pointer::validate(text).map_err(|_| TemplateError::new(path, message))?;
    Ok(text.to_owned())
}

impl Template {
    pub fn parse(value: &Value) -> Result<Self, TemplateError> {
        let serialized = serde_json::to_vec(value).map_err(|_| TemplateError::new("", "模板无法序列化"))?;
        if serialized.len() as u64 > TEMPLATE_BYTES {
            return Err(TemplateError::new("", "模板超过 1 MiB 上限"));
        }
        let object = value
            .as_object()
            .ok_or_else(|| TemplateError::new("", "模板必须是 JSON 对象"))?;
        for key in object.keys() {
            if !TOP_KEYS.contains(&key.as_str()) {
                return Err(TemplateError::new(format!("/{key}"), "未知字段，请检查拼写"));
            }
        }
        let version = object.get("schemaVersion").cloned().unwrap_or(Value::Null);
        require(&version, "/schemaVersion")?;
        if version.as_i64() != Some(1) {
            return Err(TemplateError::new("/schemaVersion", "schemaVersion 仅支持 1"));
        }
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        require(&id, "/id")?;
        let id = id
            .as_str()
            .ok_or_else(|| TemplateError::new("/id", "id 必须匹配 [a-z][a-z0-9-]{0,63}"))?;
        if !is_valid_id(id) {
            return Err(TemplateError::new(
                "/id",
                "id 必须匹配 [a-z][a-z0-9-]{0,63}",
            ));
        }
        if SOURCES.contains(&id) {
            return Err(TemplateError::new(
                "/id",
                "id 不能使用内置来源 ID（codex、workbuddy、codebuddy-ide、codeg）",
            ));
        }
        let name = object.get("name").cloned().unwrap_or(Value::Null);
        require(&name, "/name")?;
        let name = name
            .as_str()
            .ok_or_else(|| TemplateError::new("/name", "name 必须为 1-120 字符的非空字符串"))?;
        if name.is_empty() || name.chars().count() > NAME_MAX {
            return Err(TemplateError::new(
                "/name",
                "name 必须为 1-120 字符的非空字符串",
            ));
        }
        let transport = object.get("transport").cloned().unwrap_or(Value::Null);
        require(&transport, "/transport")?;
        if transport.as_str() != Some("hook") {
            return Err(TemplateError::new("/transport", "transport 仅支持 hook"));
        }
        let mapping = object
            .get("mapping")
            .and_then(Value::as_object)
            .ok_or_else(|| TemplateError::new("/mapping", "mapping 必须为对象"))?;
        for key in mapping.keys() {
            if !MAPPING_KEYS.contains(&key.as_str()) {
                return Err(TemplateError::new(
                    format!("/mapping/{key}"),
                    "未知字段，请检查拼写",
                ));
            }
        }
        let mut parsed = Mapping::default();
        for (key, target) in [
            ("event", &mut parsed.event),
            ("sessionId", &mut parsed.session_id),
        ] {
            let value = mapping.get(key).cloned().unwrap_or(Value::Null);
            require(&value, &format!("/mapping/{key}"))?;
            *target = pointer_field(&value, &format!("/mapping/{key}"))?;
        }
        for (key, target) in [
            ("roundId", &mut parsed.round_id),
            ("eventId", &mut parsed.event_id),
            ("timestamp", &mut parsed.timestamp),
            ("cwd", &mut parsed.cwd),
            ("title", &mut parsed.title),
            ("requestId", &mut parsed.request_id),
        ] {
            if let Some(value) = mapping.get(key) {
                *target = Some(pointer_field(value, &format!("/mapping/{key}"))?);
            }
        }
        let events = object
            .get("events")
            .and_then(Value::as_object)
            .filter(|map| !map.is_empty() && map.len() <= EVENTS_MAX)
            .ok_or_else(|| {
                TemplateError::new(
                    "/events",
                    "events 必须为 1-64 个事件的对象（事件名 → 动作）",
                )
            })?;
        let mut actions = BTreeMap::new();
        for (event, definition) in events {
            let base = format!("/events/{event}");
            if event.is_empty() || event.chars().count() > EVENT_NAME_MAX {
                return Err(TemplateError::new(
                    "/events",
                    "事件名必须为 1-200 字符的非空字符串",
                ));
            }
            let definition = definition
                .as_object()
                .ok_or_else(|| TemplateError::new(base.clone(), "事件配置必须为对象"))?;
            for key in definition.keys() {
                if !ACTION_KEYS.contains(&key.as_str()) {
                    return Err(TemplateError::new(
                        format!("{base}/{key}"),
                        "未知字段，请检查拼写",
                    ));
                }
            }
            let action = definition.get("action").cloned().unwrap_or(Value::Null);
            require(&action, &format!("{base}/action"))?;
            let action = action
                .as_str()
                .ok_or_else(|| TemplateError::new(format!("{base}/action"), "action 值无效"))?;
            let action = match action {
                "start" => Action::Start,
                "resume" => Action::Resume,
                "close" => Action::Close,
                "wait" => {
                    if definition.contains_key("status") {
                        return Err(TemplateError::new(
                            format!("{base}/status"),
                            "该 action 不支持此字段",
                        ));
                    }
                    let reason = definition.get("reason").cloned().unwrap_or(Value::Null);
                    require(&reason, &format!("{base}/reason"))?;
                    let reason = reason
                        .as_str()
                        .and_then(WaitReason::parse)
                        .ok_or_else(|| {
                            TemplateError::new(format!("{base}/reason"), "reason 仅支持 permission 或 input")
                        })?;
                    Action::Wait(reason)
                }
                "finish" => {
                    if definition.contains_key("reason") {
                        return Err(TemplateError::new(
                            format!("{base}/reason"),
                            "该 action 不支持此字段",
                        ));
                    }
                    let status = definition.get("status").cloned().unwrap_or(Value::Null);
                    require(&status, &format!("{base}/status"))?;
                    let status = status
                        .as_str()
                        .and_then(FinishStatus::parse)
                        .ok_or_else(|| {
                            TemplateError::new(format!("{base}/status"), "status 仅支持 done 或 error")
                        })?;
                    Action::Finish(status)
                }
                _ => {
                    return Err(TemplateError::new(
                        format!("{base}/action"),
                        "action 仅支持 start、wait、resume、finish、close",
                    ))
                }
            };
            if !matches!(action, Action::Wait(_) | Action::Finish(_)) {
                for key in ["reason", "status"] {
                    if definition.contains_key(key) {
                        return Err(TemplateError::new(
                            format!("{base}/{key}"),
                            "该 action 不支持此字段",
                        ));
                    }
                }
            }
            actions.insert(event.clone(), action);
        }
        let mut ignore_if_present = Vec::new();
        if let Some(value) = object.get("ignoreIfPresent") {
            let items = value.as_array().filter(|items| items.len() <= IGNORE_MAX).ok_or_else(
                || {
                    TemplateError::new(
                        "/ignoreIfPresent",
                        "ignoreIfPresent 必须为最多 16 项的 JSON Pointer 数组",
                    )
                },
            )?;
            for (index, item) in items.iter().enumerate() {
                ignore_if_present.push(pointer_field(item, &format!("/ignoreIfPresent/{index}"))?);
            }
        }
        Ok(Self {
            id: id.to_owned(),
            name: name.to_owned(),
            mapping: parsed,
            ignore_if_present,
            events: actions,
        })
    }

    /// Normalized JSON form; this is what the registry stores and returns.
    pub fn to_value(&self) -> Value {
        let mut mapping = Map::new();
        mapping.insert("event".into(), json!(self.mapping.event));
        mapping.insert("sessionId".into(), json!(self.mapping.session_id));
        for (key, value) in [
            ("roundId", &self.mapping.round_id),
            ("eventId", &self.mapping.event_id),
            ("timestamp", &self.mapping.timestamp),
            ("cwd", &self.mapping.cwd),
            ("title", &self.mapping.title),
            ("requestId", &self.mapping.request_id),
        ] {
            if let Some(value) = value {
                mapping.insert(key.into(), json!(value));
            }
        }
        let mut events = Map::new();
        for (event, action) in &self.events {
            events.insert(event.clone(), action.to_value());
        }
        let mut value = json!({
            "schemaVersion": 1,
            "id": self.id,
            "name": self.name,
            "transport": "hook",
            "mapping": Value::Object(mapping),
            "events": Value::Object(events),
        });
        if !self.ignore_if_present.is_empty() {
            value["ignoreIfPresent"] = json!(self.ignore_if_present);
        }
        value
    }

    pub fn capabilities(&self) -> Vec<String> {
        let mut list: Vec<String> = self.events.values().map(|action| action.capability()).collect();
        list.sort();
        list.dedup();
        list
    }

    /// Event name → capability label, sorted by raw event name.
    pub fn event_table(&self) -> Vec<(String, String)> {
        self.events
            .iter()
            .map(|(event, action)| (event.clone(), action.capability()))
            .collect()
    }

    pub fn action(&self, event: &str) -> Option<Action> {
        self.events.get(event).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Value {
        json!({
            "schemaVersion":1,"id":"example-agent","name":"Example Agent","transport":"hook",
            "mapping":{"event":"/event_name","sessionId":"/session_id"},
            "events":{"prompt_submitted":{"action":"start"},"response_failed":{"action":"finish","status":"error"}}
        })
    }

    #[test]
    fn parses_and_round_trips_a_minimal_template() {
        let template = Template::parse(&valid()).unwrap();
        assert_eq!(template.id, "example-agent");
        assert_eq!(template.capabilities(), vec!["finish:error", "start"]);
        assert_eq!(Template::parse(&template.to_value()).unwrap(), template);
    }

    #[test]
    fn rejects_unknown_keys_and_builtin_ids() {
        let mut extra = valid();
        extra["mapping"]["session_id"] = json!("/sid");
        let error = Template::parse(&extra).unwrap_err();
        assert_eq!(error.path, "/mapping/session_id");
        extra = valid();
        extra["id"] = json!("codex");
        assert_eq!(Template::parse(&extra).unwrap_err().path, "/id");
        extra = valid();
        extra["events"]["prompt_submitted"]["reason"] = json!("input");
        assert_eq!(
            Template::parse(&extra).unwrap_err().path,
            "/events/prompt_submitted/reason"
        );
    }

    #[test]
    fn optional_mapping_fields_are_preserved() {
        let mut value = valid();
        value["mapping"]["roundId"] = json!("/round_id");
        value["ignoreIfPresent"] = json!(["/parent_session_id"]);
        let template = Template::parse(&value).unwrap();
        assert_eq!(template.mapping.round_id.as_deref(), Some("/round_id"));
        assert_eq!(template.ignore_if_present, vec!["/parent_session_id"]);
        assert_eq!(template.to_value()["ignoreIfPresent"], json!(["/parent_session_id"]));
    }
}
