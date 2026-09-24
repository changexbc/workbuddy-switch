//! Custom agent hook integrations: a declarative JSON template maps a third-party
//! hook payload onto the standard hub events. Nothing here executes user code,
//! installs hooks, or touches another tool's configuration.

pub mod engine;
pub mod pointer;
pub mod store;
pub mod template;

pub use engine::{Engine, Outcome};
pub use store::{Entry, Store};
pub use template::{Action, FinishStatus, Mapping, Template, TemplateError, WaitReason};

use serde_json::json;

/// Every custom source is addressed as `custom:<id>` so a template can never
/// masquerade as a built-in source.
pub const SOURCE_PREFIX: &str = "custom:";
/// Identifier grammar for a template id; built-in source ids are rejected separately.
pub const ID_PATTERN: &str = "^[a-z][a-z0-9-]{0,63}$";

pub mod limits {
    // Serialized template and raw payload ceilings, enforced at every entry point.
    pub const TEMPLATE_BYTES: u64 = 1024 * 1024;
    pub const PAYLOAD_BYTES: u64 = 1024 * 1024;
    pub const NAME_MAX: usize = 120;
    pub const POINTER_MAX: usize = 512;
    pub const EVENTS_MAX: usize = 64;
    pub const EVENT_NAME_MAX: usize = 200;
    pub const IGNORE_MAX: usize = 16;
    pub const SESSION_ID_MAX: usize = 256;
    pub const ID_FIELD_MAX: usize = 256;
    pub const EVENT_ID_MAX: usize = 200;
    pub const CWD_MAX: usize = 2048;
    pub const TITLE_MAX: usize = 4096;
    pub const DEDUP_PER_SESSION: usize = 64;
    pub const SESSIONS_MAX: usize = 256;
    pub const DIAGNOSTICS_MAX: usize = 50;
    pub const DETAIL_MAX: usize = 200;
}

pub fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    id.len() <= 64 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn source_of(id: &str) -> String {
    format!("{SOURCE_PREFIX}{id}")
}

/// Returns the template id when `source` is a well-formed custom source.
pub fn parse_source(source: &str) -> Option<&str> {
    source
        .strip_prefix(SOURCE_PREFIX)
        .filter(|id| is_valid_id(id))
}

/// Machine-readable limits, compared against the shared fixture by the tests so
/// the Rust and Node implementations cannot drift apart silently.
pub fn limits_json() -> serde_json::Value {
    json!({
        "templateBytes": limits::TEMPLATE_BYTES,
        "payloadBytes": limits::PAYLOAD_BYTES,
        "nameMax": limits::NAME_MAX,
        "pointerMax": limits::POINTER_MAX,
        "eventsMax": limits::EVENTS_MAX,
        "eventNameMax": limits::EVENT_NAME_MAX,
        "ignoreIfPresentMax": limits::IGNORE_MAX,
        "sessionIdMax": limits::SESSION_ID_MAX,
        "idFieldMax": limits::ID_FIELD_MAX,
        "eventIdMax": limits::EVENT_ID_MAX,
        "cwdMax": limits::CWD_MAX,
        "titleMax": limits::TITLE_MAX,
        "dedupPerSession": limits::DEDUP_PER_SESSION,
        "sessionsMax": limits::SESSIONS_MAX,
        "diagnosticsMax": limits::DIAGNOSTICS_MAX,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_sources_stay_inside_the_custom_namespace() {
        assert!(is_valid_id("example-agent"));
        assert!(is_valid_id("a1"));
        assert!(!is_valid_id("Example"));
        assert!(!is_valid_id("1agent"));
        assert!(!is_valid_id("agent_1"));
        assert!(!is_valid_id(&"a".repeat(65)));
        assert_eq!(source_of("example"), "custom:example");
        assert_eq!(parse_source("custom:example"), Some("example"));
        assert_eq!(parse_source("codex"), None);
        assert_eq!(parse_source("custom:"), None);
        assert_eq!(parse_source("custom:Custom"), None);
    }
}
