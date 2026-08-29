//! 本地 WorkBuddy / CodeBuddy CLI JSONL Token 统计。
//!
//! 这个模块是统计数据的唯一归属：日志只在这里解码、去重和按时间聚合，
//! Tauri 与 HTTP 层只负责转发结果。响应只包含聚合数字和脱敏标识，不返回
//! 消息正文、arguments 或认证信息。

use chrono::{Datelike, Local, Timelike};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Usage {
    input: u64,
    output: u64,
    read: u64,
    write: u64,
}

#[derive(Clone, Debug, Default)]
struct Totals {
    usage: Usage,
    records: u64,
}

impl Totals {
    fn add(&mut self, usage: Usage) {
        self.usage.input = self.usage.input.saturating_add(usage.input);
        self.usage.output = self.usage.output.saturating_add(usage.output);
        self.usage.read = self.usage.read.saturating_add(usage.read);
        self.usage.write = self.usage.write.saturating_add(usage.write);
        self.records = self.records.saturating_add(1);
    }

    fn value(&self) -> Value {
        let cache_hit_rate = (self.usage.input > 0)
            .then(|| self.usage.read as f64 / self.usage.input as f64);
        json!({
            "input": self.usage.input,
            "output": self.usage.output,
            "cacheRead": self.usage.read,
            "cacheWrite": self.usage.write,
            "uncachedInput": self.usage.input.saturating_sub(self.usage.read),
            "records": self.records,
            "cacheHitRate": cache_hit_rate,
        })
    }
}

/// Read a non-negative integer from a JSON number or string.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64))
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
}

fn field(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| object.get(*key).and_then(number))
}

fn usage_fields(object: &Map<String, Value>) -> Usage {
    Usage {
        input: field(object, &["input_tokens", "inputTokens", "prompt_tokens"]).unwrap_or(0),
        output: field(
            object,
            &["output_tokens", "outputTokens", "completion_tokens"],
        )
        .unwrap_or(0),
        read: field(
            object,
            &[
                "cache_read_input_tokens",
                "cacheReadInputTokens",
                "prompt_cache_hit_tokens",
                "cached_tokens",
            ],
        )
        .unwrap_or(0),
        write: field(
            object,
            &[
                "cache_write_input_tokens",
                "cacheWriteInputTokens",
                "cache_creation_input_tokens",
                "prompt_cache_write_tokens",
            ],
        )
        .unwrap_or(0),
    }
}

fn usage_object(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value?.as_object().filter(|object| {
        // Input is the required anchor for a usage record. It may legitimately
        // be zero (for example a provider reports output-only retries), so do
        // not use `input > 0` as the validity check.
        field(object, &["input_tokens", "inputTokens", "prompt_tokens"]).is_some()
    })
}

/// Decode one record. Usage precedence is message.usage > providerData.usage >
/// top-level usage. rawUsage is only consulted for cache-write fields that the
/// selected usage object does not expose, so one JSONL record is counted once.
fn usage(value: &Value) -> Option<Usage> {
    let provider = value.get("providerData");
    let candidates = [
        value.get("message").and_then(|message| message.get("usage")),
        provider.and_then(|data| data.get("usage")),
        value.get("usage"),
    ];
    let selected = candidates.iter().copied().find_map(usage_object)?;
    let mut result = usage_fields(selected);

    if result.write == 0 {
        result.write = provider
            .and_then(|data| data.get("rawUsage"))
            .and_then(Value::as_object)
            .and_then(|raw| {
                field(
                    raw,
                    &[
                        "cache_write_input_tokens",
                        "cacheWriteInputTokens",
                        "cache_creation_input_tokens",
                        "prompt_cache_write_tokens",
                    ],
                )
            })
            .unwrap_or(0);
    }

    Some(result)
}

fn timestamp(value: &Value) -> Option<i64> {
    value
        .get("timestamp")
        .or_else(|| value.get("ts"))
        .and_then(|timestamp| {
            timestamp
                .as_i64()
                .or_else(|| timestamp.as_u64().and_then(|n| i64::try_from(n).ok()))
                .or_else(|| timestamp.as_str()?.trim().parse::<i64>().ok())
        })
}

fn date(value: &Value) -> Option<String> {
    let timestamp = timestamp(value)?;
    chrono::DateTime::from_timestamp_millis(timestamp)
        .map(|date| date.with_timezone(&Local).format("%Y-%m-%d").to_string())
}

fn hour(value: &Value) -> Option<String> {
    let timestamp = timestamp(value)?;
    chrono::DateTime::from_timestamp_millis(timestamp).map(|date| {
        let local = date.with_timezone(&Local);
        format!("{}-{}", local.weekday().num_days_from_monday(), local.hour())
    })
}

fn model(value: &Value) -> String {
    value
        .get("providerData")
        .and_then(|data| data.get("model"))
        .or_else(|| value.get("model"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("未知模型")
        .to_string()
}

fn files(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Subagent logs duplicate parent-session context and are not part
            // of either product's primary usage accounting.
            if path.file_name().and_then(|name| name.to_str()) != Some("subagents") {
                files(&path, output);
            }
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            output.push(path);
        }
    }
}

fn project_name(root: &Path, file: &Path) -> String {
    let name = file
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| component.as_os_str().to_str())
        .filter(|name| !name.is_empty() && !name.ends_with(".jsonl"));
    match name {
        // Product directories commonly encode the complete absolute path.
        // Returning that would leak a user name and parent directories.
        Some(name) if !name.starts_with("Users-") && !name.starts_with("home-") => {
            name.to_string()
        }
        _ => "未知项目".to_string(),
    }
}

fn record_project(value: &Value, fallback: &str) -> String {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 120)
        .unwrap_or(fallback)
        .to_string()
}

fn groups(groups: HashMap<String, Totals>) -> Vec<Value> {
    let mut values: Vec<_> = groups
        .into_iter()
        .map(|(key, totals)| {
            let mut value = totals.value();
            value["key"] = json!(key);
            value
        })
        .collect();
    values.sort_by(|left, right| {
        let left_total = left
            .get("input")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + left.get("output").and_then(Value::as_u64).unwrap_or(0)
            + left.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
        let right_total = right
            .get("input")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + right.get("output").and_then(Value::as_u64).unwrap_or(0)
            + right.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
        right_total.cmp(&left_total)
    });
    values
}

fn source(root: PathBuf, name: &str, cutoff: Option<i64>) -> Value {
    let mut paths = Vec::new();
    files(&root, &mut paths);
    let mut total = Totals::default();
    let mut models = HashMap::new();
    let mut projects = HashMap::new();
    let mut sessions = HashMap::new();
    let mut daily = HashMap::new();
    let mut hours = HashMap::new();
    let mut parse_errors = 0_u64;
    let mut coverage_start_at: Option<i64> = None;
    let mut coverage_end_at: Option<i64> = None;

    for path in &paths {
        let session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("未知会话");
        let fallback_project = project_name(&root, path);
        let Ok(file) = std::fs::File::open(path) else {
            parse_errors = parse_errors.saturating_add(1);
            continue;
        };

        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            };
            // Records with a missing timestamp are excluded from a bounded
            // range rather than guessed from file mtime or browser time.
            if cutoff.is_some_and(|minimum| timestamp(&value).is_none_or(|ts| ts < minimum)) {
                continue;
            }
            let Some(usage) = usage(&value) else {
                continue;
            };
            let project = record_project(&value, &fallback_project);
            total.add(usage);
            models
                .entry(model(&value))
                .or_insert_with(Totals::default)
                .add(usage);
            projects
                .entry(project.clone())
                .or_insert_with(Totals::default)
                .add(usage);
            sessions
                .entry(format!("{project} · {session_id}"))
                .or_insert_with(Totals::default)
                .add(usage);
            if let Some(day) = date(&value) {
                daily
                    .entry(day)
                    .or_insert_with(Totals::default)
                    .add(usage);
            }
            if let Some(hour) = hour(&value) {
                hours
                    .entry(hour)
                    .or_insert_with(Totals::default)
                    .add(usage);
            }
            if let Some(timestamp) = timestamp(&value) {
                coverage_start_at = Some(
                    coverage_start_at.map_or(timestamp, |current| current.min(timestamp)),
                );
                coverage_end_at = Some(
                    coverage_end_at.map_or(timestamp, |current| current.max(timestamp)),
                );
            }
        }
    }

    json!({
        "source": name,
        "summary": total.value(),
        "models": groups(models),
        "projects": groups(projects),
        "sessions": groups(sessions),
        "daily": groups(daily),
        "hours": groups(hours),
        "filesScanned": paths.len(),
        "parseErrors": parse_errors,
        "coverageStartAt": coverage_start_at,
        "coverageEndAt": coverage_end_at,
    })
}

/// Return independent WorkBuddy and CodeBuddy CLI aggregates. `days` is
/// interpreted in Rust using the same millisecond clock for both sources.
pub fn get_statistics(days: Option<i64>) -> Value {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let generated_at = crate::modules::config::now_ms();
    let range_days = match days {
        Some(7) => Some(7),
        Some(30) => Some(30),
        Some(90) => Some(90),
        _ => None,
    };
    let cutoff = range_days.map(|value| generated_at - value * 86_400_000);
    json!({
        "generatedAt": generated_at,
        "rangeDays": range_days,
        "sources": [
            source(home.join(".workbuddy/projects"), "workbuddy", cutoff),
            source(home.join(".codebuddy/projects"), "codebuddy-cli", cutoff),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn usage_priority_aliases_and_raw_cache_write() {
        let value = json!({
            "providerData": {
                "usage": { "inputTokens": 99, "outputTokens": 22 },
                "rawUsage": { "prompt_cache_write_tokens": 2 }
            },
            "message": { "usage": {
                "input_tokens": 10,
                "output_tokens": 3,
                "cache_read_input_tokens": 4
            }}
        });
        assert_eq!(usage(&value), Some(Usage { input: 10, output: 3, read: 4, write: 2 }));
    }

    #[test]
    fn source_excludes_subagents_and_counts_each_record_once() {
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-{}-{}",
            std::process::id(),
            crate::modules::config::now_ms()
        ));
        let project = root.join("fixture-project");
        let ignored = project.join("subagents");
        fs::create_dir_all(&ignored).expect("create fixture dirs");
        let record = json!({
            "timestamp": crate::modules::config::now_ms(),
            "providerData": {
                "model": "fixture-model",
                "usage": { "inputTokens": 20, "outputTokens": 5 }
            },
            "message": { "usage": {
                "input_tokens": 10,
                "output_tokens": 3,
                "cache_read_input_tokens": 4
            }}
        });
        fs::write(project.join("session.jsonl"), format!("{}\nnot-json\n", record))
            .expect("write fixture");
        fs::write(ignored.join("agent.jsonl"), format!("{}\n", record)).expect("write ignored fixture");

        let result = source(root.clone(), "fixture", None);
        assert_eq!(result["filesScanned"], 1);
        assert_eq!(result["parseErrors"], 1);
        assert_eq!(result["summary"]["input"], 10);
        assert_eq!(result["summary"]["output"], 3);
        assert_eq!(result["summary"]["cacheRead"], 4);
        assert_eq!(result["summary"]["records"], 1);
        assert_eq!(result["projects"][0]["key"], "fixture-project");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn bounded_source_excludes_records_before_cutoff() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-range-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let record = |timestamp| {
            json!({
                "timestamp": timestamp,
                "cwd": "/fixture/example-project",
                "message": { "usage": { "input_tokens": 10, "output_tokens": 2 } }
            })
        };
        fs::write(
            project.join("session.jsonl"),
            format!("{}\n{}\n", record(now - 10_000), record(now - 100_000)),
        )
        .expect("write fixture");

        let result = source(root.clone(), "fixture", Some(now - 50_000));
        assert_eq!(result["summary"]["records"], 1);
        assert_eq!(result["summary"]["input"], 10);
        assert_eq!(result["projects"][0]["key"], "example-project");
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
