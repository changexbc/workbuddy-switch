pub mod adapters;
pub mod codex_recovery;
pub mod codex_read_state;
pub mod custom;
pub mod host_process;
pub mod hub;
pub mod settings;
pub mod tail;
use serde_json::{json, Value};
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
/// Stable hook executable the app publishes under the user's home. Both the
/// built-in integrations and the custom hook command point at this path.
pub fn hook_binary(home: &std::path::Path) -> std::path::PathBuf {
    home.join(if cfg!(windows) {
        ".agent-studio/bin/agent-studio-runtime-v1.exe"
    } else {
        ".agent-studio/bin/agent-studio-runtime-v1"
    })
}
/// Single-quoted shell argument, safe to paste into another tool's hook config.
pub fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}
pub fn timestamp(v: &Value, fallback: i64) -> i64 {
    v.as_i64()
        .filter(|n| *n > 0)
        .or_else(|| {
            v.as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp_millis())
        })
        .unwrap_or(fallback)
}
pub fn parsed(v: &Value) -> Value {
    v.as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| v.clone())
}
pub fn question_tool(name: &str) -> bool {
    matches!(
        name.rsplit("__")
            .next()
            .unwrap_or(name)
            .rsplit('.')
            .next()
            .unwrap_or(name),
        "request_user_input"
            | "request_user_input_async"
            | "AskUserQuestion"
            | "ask_user_question"
            | "RequestUserInput"
    )
}
pub fn content(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_else(|| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    })
}
pub fn questions(v: &Value) -> Value {
    let p = parsed(v);
    json!(p["questions"].as_array().map(|a|a.iter().map(|q|json!({"text":q["question"].as_str().or(q["title"].as_str()).unwrap_or(""),"header":text(&q["header"]),"options":q["options"].as_array().map(|o|o.iter().map(|v|json!({"label":v.as_str().unwrap_or(v["label"].as_str().unwrap_or("")),"description":text(&v["description"])})).collect::<Vec<_>>()).unwrap_or_default()})).collect::<Vec<_>>()).unwrap_or_default())
}
pub fn question(v: &Value) -> String {
    let qs = questions(v);
    let s = qs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|q| q["text"].as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if s.is_empty() {
        "等待用户输入".into()
    } else {
        s
    }
}
pub fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    base
}
pub fn atomic_json(path: &std::path::Path, v: &Value) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("invalid path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
    f.write_all(v.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}
