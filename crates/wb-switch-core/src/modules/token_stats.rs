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

#[derive(Clone, Debug)]
struct SessionTotals {
    key: String,
    title: Option<String>,
    project: String,
    session_id: String,
    totals: Totals,
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
        // `input` already includes cache reads; expose the same headline total
        // used by the dashboard without double-counting the cached portion.
        let total = self
            .usage
            .input
            .saturating_add(self.usage.output)
            .saturating_add(self.usage.write);
        json!({
            "total": total,
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

fn positive_field(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(number)
            .filter(|value| *value > 0)
    })
}

fn cached_input_field(object: &Map<String, Value>) -> u64 {
    // Providers have emitted both flat aliases and OpenAI-compatible nested
    // details. Prefer a positive flat alias so a stale `cache_read...: 0`
    // field cannot hide a populated `prompt_cache_hit_tokens` value.
    positive_field(
        object,
        &[
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "prompt_cache_hit_tokens",
            "cached_tokens",
        ],
    )
    .or_else(|| {
        object
            .get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| positive_field(details, &["cached_tokens"]))
    })
    .or_else(|| {
        object
            .get("inputTokensDetails")
            .and_then(Value::as_array)
            .and_then(|details| {
                details.iter().find_map(|detail| {
                    detail
                        .as_object()
                        .and_then(|detail| positive_field(detail, &["cached_tokens"]))
                })
            })
    })
    .unwrap_or(0)
}

const CACHE_WRITE_KEYS: &[&str] = &[
    "cache_write_input_tokens",
    "cacheWriteInputTokens",
    "cache_creation_input_tokens",
    "prompt_cache_write_tokens",
];

fn usage_fields(object: &Map<String, Value>) -> Usage {
    Usage {
        input: field(object, &["input_tokens", "inputTokens", "prompt_tokens"]).unwrap_or(0),
        output: field(
            object,
            &["output_tokens", "outputTokens", "completion_tokens"],
        )
        .unwrap_or(0),
        read: cached_input_field(object),
        write: positive_field(object, CACHE_WRITE_KEYS).unwrap_or(0),
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
/// top-level usage. Cache-write metadata may only exist on a non-selected
/// usage object or rawUsage, so those objects are consulted without counting
/// their input/output again.
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
        result.write = candidates
            .iter()
            .copied()
            .filter_map(|candidate| candidate.and_then(Value::as_object))
            .chain(
                provider
                    .and_then(|data| data.get("rawUsage"))
                    .and_then(Value::as_object),
            )
            .find_map(|object| positive_field(object, CACHE_WRITE_KEYS))
            .unwrap_or(0);
    }

    // prompt_cache_miss_tokens is deliberately not a write alias: current
    // WorkBuddy/CodeBuddy logs use it for newly computed (uncached) input,
    // while their explicit cache-write fields may legitimately remain zero.

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

fn non_empty_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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
        total_value(right).cmp(&total_value(left))
    });
    values
}

fn session_groups(sessions: Vec<SessionTotals>) -> Vec<Value> {
    let mut values: Vec<_> = sessions
        .into_iter()
        .map(|session| {
            let mut value = session.totals.value();
            value["key"] = json!(session.key);
            value["title"] = json!(session.title);
            value["project"] = json!(session.project);
            value["sessionId"] = json!(session.session_id);
            value
        })
        .collect();
    values.sort_by(|left, right| {
        total_value(right).cmp(&total_value(left))
    });
    values
}

fn total_value(value: &Value) -> u64 {
    value
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            value
                .get("input")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_add(value.get("output").and_then(Value::as_u64).unwrap_or(0))
                .saturating_add(value.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0))
        })
}

fn source(root: PathBuf, name: &str, cutoff: Option<i64>) -> Value {
    let mut paths = Vec::new();
    files(&root, &mut paths);
    let mut total = Totals::default();
    let mut models = HashMap::new();
    let mut projects = HashMap::new();
    let mut sessions = Vec::new();
    let mut daily = HashMap::new();
    let mut hours = HashMap::new();
    let mut parse_errors = 0_u64;
    let mut coverage_start_at: Option<i64> = None;
    let mut coverage_end_at: Option<i64> = None;

    paths.sort();
    let mut session_key_counts = HashMap::<String, usize>::new();
    for path in &paths {
        let session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("未知会话")
            .to_string();
        let fallback_project = project_name(&root, path);
        let Ok(file) = std::fs::File::open(path) else {
            parse_errors = parse_errors.saturating_add(1);
            continue;
        };

        let mut session_totals = Totals::default();
        let mut session_project: Option<String> = None;
        let mut ai_title: Option<String> = None;
        let mut summary: Option<String> = None;

        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            };
            // Title metadata belongs to the whole JSONL session file. Read it
            // before applying the usage cutoff so an older title can still
            // label usage that falls inside the selected range. aiTitle has
            // precedence over summary regardless of event order.
            if let Some(title) = non_empty_text(value.get("aiTitle")) {
                ai_title = Some(title);
            }
            if let Some(value) = non_empty_text(value.get("summary")) {
                summary = Some(value);
            }
            // Records with a missing timestamp are excluded from a bounded
            // range rather than guessed from file mtime or browser time.
            if cutoff.is_some_and(|minimum| timestamp(&value).is_none_or(|ts| ts < minimum)) {
                continue;
            }
            let Some(usage) = usage(&value) else {
                continue;
            };
            let project = record_project(&value, &fallback_project);
            if session_project.is_none() {
                session_project = Some(project.clone());
            }
            session_totals.add(usage);
            total.add(usage);
            models
                .entry(model(&value))
                .or_insert_with(Totals::default)
                .add(usage);
            projects
                .entry(project.clone())
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

        if session_totals.records > 0 {
            let project = session_project.unwrap_or(fallback_project);
            let base_key = format!("{project} · {session_id}");
            let count = session_key_counts.entry(base_key.clone()).or_default();
            *count += 1;
            let key = if *count == 1 {
                base_key
            } else {
                format!("{base_key} · {}", *count)
            };
            sessions.push(SessionTotals {
                key,
                title: ai_title.or(summary),
                project,
                session_id,
                totals: session_totals,
            });
        }
    }

    json!({
        "source": name,
        "summary": total.value(),
        "models": groups(models),
        "projects": groups(projects),
        "sessions": session_groups(sessions),
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
    fn cache_write_uses_explicit_aliases_but_never_cache_miss() {
        let provider_usage_write = json!({
            "providerData": {
                "usage": {
                    "inputTokens": 99,
                    "outputTokens": 22,
                    "cache_write_input_tokens": 0,
                    "cache_creation_input_tokens": 7
                },
                "rawUsage": {
                    "prompt_cache_miss_tokens": 91,
                    "prompt_cache_write_tokens": 0
                }
            },
            "message": { "usage": {
                "input_tokens": 10,
                "output_tokens": 3,
                "cache_read_input_tokens": 4
            }}
        });
        assert_eq!(
            usage(&provider_usage_write),
            Some(Usage {
                input: 10,
                output: 3,
                read: 4,
                write: 7,
            })
        );

        let cache_miss_only = json!({
            "providerData": {
                "rawUsage": { "prompt_cache_miss_tokens": 91 }
            },
            "message": { "usage": {
                "input_tokens": 10,
                "output_tokens": 3
            }}
        });
        assert_eq!(
            usage(&cache_miss_only),
            Some(Usage {
                input: 10,
                output: 3,
                read: 0,
                write: 0,
            })
        );
    }

    #[test]
    fn cache_read_accepts_nested_provider_details() {
        let value = json!({
            "providerData": {
                "usage": {
                    "inputTokens": 99,
                    "outputTokens": 3,
                    "inputTokensDetails": [{ "cached_tokens": 7 }]
                }
            }
        });
        assert_eq!(
            usage(&value),
            Some(Usage {
                input: 99,
                output: 3,
                read: 7,
                write: 0,
            })
        );

        let raw = json!({
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 2,
                "cache_read_input_tokens": 0,
                "prompt_cache_hit_tokens": 12
            }
        });
        assert_eq!(
            usage(&raw),
            Some(Usage {
                input: 20,
                output: 2,
                read: 12,
                write: 0,
            })
        );

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
        assert_eq!(result["summary"]["total"], 13);
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

    #[test]
    fn session_titles_are_file_scoped_and_independent_from_usage_cutoff() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-titles-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let usage_record = |input| {
            json!({
                "timestamp": now,
                "cwd": "/private/example-project",
                "message": { "usage": { "input_tokens": input, "output_tokens": 2 } }
            })
        };

        fs::write(
            project.join("session-a.jsonl"),
            format!(
                "{}\n{}\n{}\n{}\n",
                json!({ "type": "summary", "summary": "摘要不应覆盖 AI 标题" }),
                json!({ "type": "ai-title", "aiTitle": "旧标题" }),
                usage_record(10),
                json!({ "type": "ai-title", "aiTitle": "最新 AI 标题" }),
            ),
        )
        .expect("write ai title fixture");
        fs::write(
            project.join("session-b.jsonl"),
            format!(
                "{}\n{}\n",
                json!({
                    "type": "ai-title",
                    "timestamp": now - 100_000,
                    "aiTitle": "范围外保留标题"
                }),
                usage_record(20),
            ),
        )
        .expect("write cutoff title fixture");
        fs::write(
            project.join("session-c.jsonl"),
            format!(
                "{}\n{}\n",
                usage_record(30),
                json!({ "type": "summary", "summary": "摘要回退标题" }),
            ),
        )
        .expect("write summary fixture");
        fs::write(
            project.join("session-d.jsonl"),
            format!("{}\n", usage_record(40)),
        )
        .expect("write untitled fixture");
        fs::write(
            project.join("session-e.jsonl"),
            format!(
                "{}\n{}\n",
                json!({ "type": "ai-title", "aiTitle": "最新 AI 标题" }),
                usage_record(50),
            ),
        )
        .expect("write duplicate title fixture");

        let result = source(root.clone(), "fixture", Some(now - 50_000));
        let sessions = result["sessions"].as_array().expect("session groups");
        let by_id = |session_id: &str| {
            sessions
                .iter()
                .find(|session| session["sessionId"] == session_id)
                .expect("session group by id")
        };

        assert_eq!(result["summary"]["input"], 150);
        assert_eq!(result["summary"]["records"], 5);
        assert_eq!(sessions.len(), 5);
        assert_eq!(by_id("session-a")["title"], "最新 AI 标题");
        assert_eq!(by_id("session-b")["title"], "范围外保留标题");
        assert_eq!(by_id("session-c")["title"], "摘要回退标题");
        assert!(by_id("session-d")["title"].is_null());
        assert_eq!(by_id("session-a")["project"], "example-project");
        assert_ne!(by_id("session-a")["key"], by_id("session-e")["key"]);

        fs::remove_dir_all(root).expect("remove fixture");
    }
}
