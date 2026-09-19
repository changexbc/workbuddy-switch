//! 本地 WorkBuddy / WorkBuddy 国际版 / CodeBuddy CLI / CodeBuddy IDE Token 统计。
//!
//! 这个模块是统计数据的唯一归属：日志只在这里解码、去重和按时间聚合，
//! Tauri 与 HTTP 层只负责转发结果。响应只包含聚合数字和脱敏标识，不返回
//! 消息正文、arguments 或认证信息。
//!
//! CodeBuddy IDE 不写 JSONL，用量在 `CodeBuddyExtension/Data/**/history/**/index.json`
//! 的 `requests[].usage` 中；消息正文文件（`messages/`）不会被扫描。
//!
//! 国内版与国际版**分开统计**（`workbuddy` / `workbuddy-ai` 两个 source），
//! 不合并：两档位是不同账号体系，混算会让用量无法归因。

use chrono::{Datelike, Local, Timelike};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::modules::variant::WbVariant;

/// 明细中最多返回的请求条数（响应窗口）。
const REQUEST_WINDOW: usize = 500;
/// 扫描期间明细缓存的内存硬上限；达到该值后立即裁剪到 `REQUEST_WINDOW`，
/// 使峰值内存恒定，不随历史总量增长。
const REQUEST_COLLECT_LIMIT: usize = 4_000;

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

/// 展示总量：`input` 已包含 cache reads，因此只追加 output 与 cache write，
/// 避免把缓存命中重复计入。聚合与请求明细共用这一口径。
fn usage_total(usage: Usage) -> u64 {
    usage
        .input
        .saturating_add(usage.output)
        .saturating_add(usage.write)
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
        let cache_hit_rate =
            (self.usage.input > 0).then(|| self.usage.read as f64 / self.usage.input as f64);
        // `input` already includes cache reads; expose the same headline total
        // used by the dashboard without double-counting the cached portion.
        let total = usage_total(self.usage);
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

/// 一次模型调用的明细行。只携带脱敏标识与 token 数字：不含消息正文、
/// 工具参数、绝对路径或认证信息。
#[derive(Clone, Debug)]
struct RequestRow {
    timestamp: i64,
    model: String,
    project: String,
    session_id: String,
    title: Option<String>,
    usage: Usage,
    /// 思考过程 token 数（`output` 的子集），仅用于拆分「思考 / 回复」。
    thinking: u64,
}

impl RequestRow {
    fn value(&self) -> Value {
        json!({
            "timestamp": self.timestamp,
            "model": self.model,
            "project": self.project,
            "sessionId": self.session_id,
            "title": self.title,
            "input": self.usage.input,
            "output": self.usage.output,
            "cacheRead": self.usage.read,
            "cacheWrite": self.usage.write,
            // 与 `Totals::value()` 同一口径：input 已含 cacheRead，
            // 未命中部分即两者之差，下溢时饱和为 0。
            "uncachedInput": self.usage.input.saturating_sub(self.usage.read),
            // 回复内容 = max(0, output - thinking) 由前端做，这里只给原始计数；
            // 实测存在 thinking > output 的异常记录，故前端必须饱和减。
            "thinking": self.thinking,
            "total": usage_total(self.usage),
        })
    }
}

/// Read a non-negative integer from a JSON number or string.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .map(|n| n as u64)
        })
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
}

fn field(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(number))
}

fn positive_field(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(number).filter(|value| *value > 0))
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
        value
            .get("message")
            .and_then(|message| message.get("usage")),
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
        format!(
            "{}-{}",
            local.weekday().num_days_from_monday(),
            local.hour()
        )
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

/// 思考过程 token 数：优先 `providerData.rawUsage.completion_thinking_tokens`，
/// 回退到 OpenAI 形状的 `completion_tokens_details.reasoning_tokens`，都没有则为 0。
///
/// 只读取计数字段：`providerData.reasoning` 是思考**正文**，属隐私红线，
/// 任何情况下都不得读取或返回。
fn thinking(value: &Value) -> u64 {
    let Some(raw_usage) = value
        .get("providerData")
        .and_then(|data| data.get("rawUsage"))
        .and_then(Value::as_object)
    else {
        return 0;
    };
    field(raw_usage, &["completion_thinking_tokens"])
        .or_else(|| {
            raw_usage
                .get("completion_tokens_details")
                .and_then(Value::as_object)
                .and_then(|details| field(details, &["reasoning_tokens"]))
        })
        .unwrap_or(0)
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
        Some(name) if !name.starts_with("Users-") && !name.starts_with("home-") => name.to_string(),
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
    values.sort_by_key(|right| std::cmp::Reverse(total_value(right)));
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
    values.sort_by_key(|right| std::cmp::Reverse(total_value(right)));
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

#[derive(Default)]
struct SourceCollector {
    total: Totals,
    models: HashMap<String, Totals>,
    projects: HashMap<String, Totals>,
    sessions: Vec<SessionTotals>,
    daily: HashMap<String, Totals>,
    daily_by_model: HashMap<String, HashMap<String, Totals>>,
    hours: HashMap<String, Totals>,
    parse_errors: u64,
    coverage_start_at: Option<i64>,
    coverage_end_at: Option<i64>,
    session_key_counts: HashMap<String, usize>,
    requests: Vec<RequestRow>,
    collect_requests: bool,
}

impl SourceCollector {
    fn note_parse_error(&mut self) {
        self.parse_errors = self.parse_errors.saturating_add(1);
    }

    /// Append one request row. Once the in-memory hard limit is exceeded, keep
    /// only the newest window: the sort is stable, so rows sharing a timestamp
    /// keep their collection order and the oldest rows are dropped.
    fn push_request(&mut self, row: RequestRow) {
        if !self.collect_requests {
            return;
        }
        self.requests.push(row);
        if self.requests.len() > REQUEST_COLLECT_LIMIT {
            self.requests
                .sort_by_key(|row| std::cmp::Reverse(row.timestamp));
            self.requests.truncate(REQUEST_WINDOW);
        }
    }

    fn add(&mut self, usage: Usage, value: &Value, project: &str) {
        self.total.add(usage);
        let model_name = model(value);
        self.models
            .entry(model_name.clone())
            .or_default()
            .add(usage);
        self.projects
            .entry(project.to_string())
            .or_default()
            .add(usage);
        if let Some(day) = date(value) {
            self.daily.entry(day.clone()).or_default().add(usage);
            self.daily_by_model
                .entry(model_name)
                .or_default()
                .entry(day)
                .or_default()
                .add(usage);
        }
        if let Some(hour) = hour(value) {
            self.hours.entry(hour).or_default().add(usage);
        }
        if let Some(timestamp) = timestamp(value) {
            self.coverage_start_at = Some(
                self.coverage_start_at
                    .map_or(timestamp, |current| current.min(timestamp)),
            );
            self.coverage_end_at = Some(
                self.coverage_end_at
                    .map_or(timestamp, |current| current.max(timestamp)),
            );
        }
    }

    fn push_session(
        &mut self,
        session_id: String,
        title: Option<String>,
        project: String,
        totals: Totals,
    ) {
        if totals.records == 0 {
            return;
        }
        let base_key = format!("{project} · {session_id}");
        let count = self.session_key_counts.entry(base_key.clone()).or_default();
        *count += 1;
        let key = if *count == 1 {
            base_key
        } else {
            format!("{base_key} · {}", *count)
        };
        self.sessions.push(SessionTotals {
            key,
            title,
            project,
            session_id,
            totals,
        });
    }

    fn into_value(mut self, name: &str, files_scanned: usize) -> Value {
        let daily_by_model = self
            .daily_by_model
            .into_iter()
            .map(|(model, points)| (model, Value::Array(groups(points))))
            .collect::<Map<String, Value>>();
        let mut value = json!({
            "source": name,
            "summary": self.total.value(),
            "models": groups(self.models),
            "projects": groups(self.projects),
            "sessions": session_groups(self.sessions),
            "daily": groups(self.daily),
            "dailyByModel": daily_by_model,
            "hours": groups(self.hours),
            "filesScanned": files_scanned,
            "parseErrors": self.parse_errors,
            "coverageStartAt": self.coverage_start_at,
            "coverageEndAt": self.coverage_end_at,
        });
        // Only detail sources take the new key; every other source keeps its
        // previous response shape byte for byte.
        if self.collect_requests {
            self.requests
                .sort_by_key(|row| std::cmp::Reverse(row.timestamp));
            self.requests.truncate(REQUEST_WINDOW);
            value["requests"] = Value::Array(self.requests.iter().map(RequestRow::value).collect());
        }
        value
    }
}

/// 单根统计：转发到多根实现，供 CodeBuddy CLI 这类固定单根的数据源使用。
/// `detail` additionally keeps a bounded window of per-call request rows; both
/// outputs share the same decode, cutoff, dedupe, and `subagents`-exclusion
/// pipeline so a detail row can never disagree with the aggregate totals.
fn source(root: PathBuf, name: &str, cutoff: Option<i64>, detail: bool) -> Value {
    source_from_roots(std::slice::from_ref(&root), name, cutoff, detail)
}

/// 多根合并统计：同一档位下的多个 jsonl 根合并成**一个** source。
///
/// 国际版数据根与国内版不同构（实测无 `projects/`、只有 `sessions/`），因此按
/// 「存在的根」探测；一个都不存在时返回空集且不报错（前端显示空状态）。
fn source_from_roots(roots: &[PathBuf], name: &str, cutoff: Option<i64>, detail: bool) -> Value {
    let mut paths: Vec<(PathBuf, PathBuf)> = Vec::new();
    for root in roots {
        let mut found = Vec::new();
        files(root, &mut found);
        paths.extend(found.into_iter().map(|file| (root.clone(), file)));
    }
    let mut collector = SourceCollector {
        collect_requests: detail,
        ..SourceCollector::default()
    };

    // Copied/forked sessions replay the parent history (including usage
    // records with their original timestamps) into their own JSONL, so the
    // same request would otherwise be counted once per copy. Process files
    // oldest first and skip fingerprints already seen in an earlier file so
    // usage stays attributed to the original session.
    paths.sort_by_key(|(_, path)| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(u64::MAX)
    });
    let mut seen: HashSet<(i64, u64, u64, u64, u64, String)> = HashSet::new();

    for (root, path) in &paths {
        let session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("未知会话")
            .to_string();
        let fallback_project = project_name(root, path);
        let Ok(file) = std::fs::File::open(path) else {
            collector.note_parse_error();
            continue;
        };

        let mut session_totals = Totals::default();
        // Detail rows are buffered per file so the file-scoped title can be
        // backfilled once the whole file has been read.
        let mut file_requests: Vec<RequestRow> = Vec::new();
        let mut session_project: Option<String> = None;
        let mut ai_title: Option<String> = None;
        let mut summary: Option<String> = None;

        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                collector.note_parse_error();
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                collector.note_parse_error();
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
            // Fingerprint = (timestamp, full usage, model). Records without a
            // timestamp cannot be fingerprinted and are counted as before; they
            // are also unsortable and therefore never appear in the detail
            // window. Reuse the parsed model name for the fingerprint and the
            // detail row instead of decoding it twice.
            let ts = timestamp(&value);
            let model_name = model(&value);
            let duplicate = ts.is_some_and(|ts| {
                !seen.insert((
                    ts,
                    usage.input,
                    usage.output,
                    usage.read,
                    usage.write,
                    model_name.clone(),
                ))
            });
            if duplicate {
                continue;
            }
            let project = record_project(&value, &fallback_project);
            if session_project.is_none() {
                session_project = Some(project.clone());
            }
            session_totals.add(usage);
            collector.add(usage, &value, &project);
            if collector.collect_requests {
                if let Some(ts) = ts {
                    file_requests.push(RequestRow {
                        timestamp: ts,
                        model: model_name,
                        project,
                        session_id: session_id.clone(),
                        title: None,
                        usage,
                        thinking: thinking(&value),
                    });
                }
            }
        }

        // `aiTitle` can appear anywhere in the file (including after the usage
        // records), so the file title is only final once the file is read.
        let title = ai_title.or(summary);
        if !file_requests.is_empty() {
            for row in &mut file_requests {
                row.title = title.clone();
            }
            for row in file_requests {
                collector.push_request(row);
            }
        }
        collector.push_session(
            session_id,
            title,
            session_project.unwrap_or(fallback_project),
            session_totals,
        );
    }

    collector.into_value(name, paths.len())
}

/// 某档位的候选 jsonl 根（顺序固定；纯函数，便于单测）。
///
/// 国内版保持既有单一 `projects/` 根；国际版两个根都探测（实测没有
/// `projects/`，只有 `sessions/`）。
fn jsonl_root_candidates(variant: WbVariant, root: &Path) -> Vec<PathBuf> {
    match variant {
        WbVariant::Cn => vec![root.join("projects")],
        WbVariant::Ai => vec![root.join("projects"), root.join("sessions")],
    }
}

/// 某档位下实际存在的 jsonl 扫描根；都不存在则返回空列表（空源，不报错）。
fn variant_source_roots(variant: WbVariant) -> Vec<PathBuf> {
    jsonl_root_candidates(variant, &variant.data_root())
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

fn codebuddy_extension_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("CodeBuddyExtension")
        .join("Data")
}

fn is_ide_conversation_index(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("index.json")
        && path
            .parent()
            .and_then(|parent| parent.parent())
            .and_then(|parent| parent.parent())
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some("history")
}

fn ide_index_files(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|name| name.to_str());
            // Message bodies contain chat content and must not be scanned.
            // Checkpoints and the shared Public bucket are unrelated to usage.
            if !matches!(
                name,
                Some("messages" | "check-point" | "backups" | "Public")
            ) {
                ide_index_files(&path, output);
            }
        } else if is_ide_conversation_index(&path) {
            output.push(path);
        }
    }
}

fn decode_genie_workspace(name: &str) -> Option<String> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let try_decode = |value: &str| -> Option<String> {
        let padded = match value.len() % 4 {
            0 => value.to_string(),
            remainder => format!("{value}{}", "=".repeat(4 - remainder)),
        };
        let bytes = engine.decode(padded).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.contains('\0') {
            return None;
        }
        Some(trimmed.to_string())
    };
    try_decode(name)
        .or_else(|| try_decode(&name.replace('_', "/")))
        .or_else(|| try_decode(&name.replace('_', "+")))
}

fn ide_project_by_session() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(root) = crate::modules::vscode_cn_inject::codebuddy_cn_data_dir().map(|dir| {
        dir.join("User")
            .join("globalStorage")
            .join("tencent-cloud.coding-copilot")
            .join("genie-history")
    }) else {
        return map;
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let folder = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let project = decode_genie_workspace(folder)
            .as_deref()
            .map(Path::new)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 120)
            .unwrap_or("未知项目")
            .to_string();
        let Ok(conversations) = std::fs::read_dir(path.join("conversations")) else {
            continue;
        };
        for conversation in conversations.flatten() {
            if let Some(id) = conversation.file_name().to_str() {
                if !id.is_empty() {
                    map.insert(id.to_string(), project.clone());
                }
            }
        }
    }
    map
}

fn ide_workspace_meta(conv_index: &Path, conv_id: &str) -> (Option<String>, String) {
    let Some(ws_index) = conv_index
        .parent()
        .and_then(|parent| parent.parent())
        .map(|parent| parent.join("index.json"))
    else {
        return (None, "未知模型".to_string());
    };
    let Ok(text) = std::fs::read_to_string(ws_index) else {
        return (None, "未知模型".to_string());
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (None, "未知模型".to_string());
    };
    let Some(conversations) = value.get("conversations").and_then(Value::as_array) else {
        return (None, "未知模型".to_string());
    };
    for conversation in conversations {
        if conversation.get("id").and_then(Value::as_str) != Some(conv_id) {
            continue;
        }
        let title = non_empty_text(conversation.get("name"))
            .or_else(|| non_empty_text(conversation.get("title")));
        let model = non_empty_text(conversation.get("selectedModelId"))
            .or_else(|| non_empty_text(conversation.get("modelId")))
            .or_else(|| non_empty_text(conversation.get("model")))
            .unwrap_or_else(|| "未知模型".to_string());
        return (title, model);
    }
    (None, "未知模型".to_string())
}

fn ide_request_usage(request: &Value) -> Option<Usage> {
    let object = request.get("usage")?.as_object()?;
    if field(object, &["inputTokens", "input_tokens", "prompt_tokens"]).is_none() {
        return None;
    }
    Some(Usage {
        input: field(object, &["inputTokens", "input_tokens", "prompt_tokens"]).unwrap_or(0),
        output: field(
            object,
            &["outputTokens", "output_tokens", "completion_tokens"],
        )
        .unwrap_or(0),
        read: field(
            object,
            &[
                "cacheTokens",
                "cacheReadInputTokens",
                "cache_read_input_tokens",
            ],
        )
        .unwrap_or(0),
        write: positive_field(
            object,
            &[
                "cachedWriteTokens",
                "cacheWriteInputTokens",
                "cache_write_input_tokens",
                "cache_creation_input_tokens",
            ],
        )
        .unwrap_or(0),
    })
}

fn ide_request_timestamp(request: &Value) -> Option<i64> {
    timestamp(request).or_else(|| {
        request.get("startedAt").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
    })
}

fn ide_source(
    root: PathBuf,
    name: &str,
    cutoff: Option<i64>,
    project_by_session: &HashMap<String, String>,
) -> Value {
    let mut paths = Vec::new();
    ide_index_files(&root, &mut paths);
    paths.sort();
    let mut collector = SourceCollector::default();

    for path in &paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            collector.note_parse_error();
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            collector.note_parse_error();
            continue;
        };
        let Some(requests) = value.get("requests").and_then(Value::as_array) else {
            continue;
        };
        let session_id = path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("未知会话")
            .to_string();
        let (title, model_name) = ide_workspace_meta(path, &session_id);
        let fallback_project = project_by_session
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| "未知项目".to_string());
        let mut session_totals = Totals::default();
        let mut session_project: Option<String> = None;

        for request in requests {
            let ts = ide_request_timestamp(request);
            if cutoff.is_some_and(|minimum| ts.is_none_or(|value| value < minimum)) {
                continue;
            }
            let Some(usage) = ide_request_usage(request) else {
                continue;
            };
            let project = fallback_project.clone();
            if session_project.is_none() {
                session_project = Some(project.clone());
            }
            session_totals.add(usage);
            collector.add(
                usage,
                &json!({
                    "timestamp": ts,
                    "providerData": { "model": model_name },
                }),
                &project,
            );
        }

        collector.push_session(
            session_id,
            title,
            session_project.unwrap_or(fallback_project),
            session_totals,
        );
    }

    collector.into_value(name, paths.len())
}

/// Return independent WorkBuddy (CN), WorkBuddy AI, CodeBuddy CLI, and
/// CodeBuddy IDE aggregates.
/// `days` is interpreted in Rust using the same millisecond clock for every source.
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
    let ide_projects = ide_project_by_session();
    json!({
        "generatedAt": generated_at,
        "rangeDays": range_days,
        "sources": [
            source_from_roots(&variant_source_roots(WbVariant::Cn), "workbuddy", cutoff, false),
            // 国际版独立 source：不与国内版混算（不同账号体系）。
            source_from_roots(&variant_source_roots(WbVariant::Ai), "workbuddy-ai", cutoff, false),
            // 请求明细目前只对 CodeBuddy CLI 开放（见 spec 的 requests 契约）。
            source(home.join(".codebuddy/projects"), "codebuddy-cli", cutoff, true),
            ide_source(
                codebuddy_extension_data_dir(),
                "codebuddy-ide",
                cutoff,
                &ide_projects,
            ),
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
        assert_eq!(
            usage(&value),
            Some(Usage {
                input: 10,
                output: 3,
                read: 4,
                write: 2
            })
        );
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
        fs::write(
            project.join("session.jsonl"),
            format!("{}\nnot-json\n", record),
        )
        .expect("write fixture");
        fs::write(ignored.join("agent.jsonl"), format!("{}\n", record))
            .expect("write ignored fixture");

        let result = source(root.clone(), "fixture", None, false);
        assert_eq!(result["filesScanned"], 1);
        assert_eq!(result["parseErrors"], 1);
        assert_eq!(result["summary"]["input"], 10);
        assert_eq!(result["summary"]["output"], 3);
        assert_eq!(result["summary"]["cacheRead"], 4);
        assert_eq!(result["summary"]["total"], 13);
        assert_eq!(result["summary"]["records"], 1);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(result["dailyByModel"]["fixture-model"][0]["key"], today);
        assert_eq!(result["projects"][0]["key"], "fixture-project");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn source_deduplicates_copied_session_history() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-fork-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let record = json!({
            "timestamp": now,
            "cwd": "/fixture/example-project",
            "message": { "usage": { "input_tokens": 10, "output_tokens": 3 } }
        });
        // The copied session replays the same usage record under a new file.
        fs::write(
            project.join("session-original.jsonl"),
            format!("{}\n", record),
        )
        .expect("write original fixture");
        fs::write(
            project.join("session-forked.jsonl"),
            format!(
                "{}\n{}\n",
                record,
                json!({
                    "timestamp": now + 1_000,
                    "message": { "usage": { "input_tokens": 7, "output_tokens": 2 } }
                })
            ),
        )
        .expect("write forked fixture");
        // Back-to-back writes can land in the same millisecond, and the
        // millisecond mtime key leaves ordering to the filesystem's directory
        // iteration, which is not sorted. The copy would then be processed
        // first, own the replayed record, and leave the original with zero
        // records. Pin the mtimes so the original always precedes its copy.
        // Opened with `write(true)` on purpose: Windows implements
        // `set_modified` via `SetFileTime`, which needs FILE_WRITE_ATTRIBUTES,
        // so a read-only `File::open` handle fails with "access denied" there.
        // A writable handle works on every platform.
        let pin_mtime = |name: &str, when: std::time::SystemTime| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(project.join(name))
                .expect("open fixture to pin mtime")
                .set_modified(when)
                .expect("pin fixture mtime");
        };
        pin_mtime(
            "session-original.jsonl",
            std::time::SystemTime::now() - std::time::Duration::from_secs(60),
        );
        pin_mtime("session-forked.jsonl", std::time::SystemTime::now());

        let result = source(root.clone(), "fixture", None, false);
        // The replayed record counts once; the fork's new record still counts.
        assert_eq!(result["summary"]["records"], 2);
        assert_eq!(result["summary"]["input"], 17);
        assert_eq!(result["summary"]["output"], 5);
        let sessions = result["sessions"].as_array().expect("session groups");
        assert_eq!(sessions.len(), 2);
        let forked = sessions
            .iter()
            .find(|session| session["sessionId"] == "session-forked")
            .expect("forked session kept its new record");
        assert_eq!(forked["input"], 7);
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

        let result = source(root.clone(), "fixture", Some(now - 50_000), false);
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

        let result = source(root.clone(), "fixture", Some(now - 50_000), false);
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

    #[test]
    fn request_detail_key_only_present_for_detail_sources() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-key-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        fs::write(
            project.join("session.jsonl"),
            format!(
                "{}\n",
                json!({
                    "timestamp": now,
                    "message": { "usage": { "input_tokens": 10, "output_tokens": 3 } }
                })
            ),
        )
        .expect("write fixture");

        let aggregate_only = source(root.clone(), "workbuddy", None, false);
        assert!(aggregate_only.get("requests").is_none());

        let detailed = source(root.clone(), "codebuddy-cli", None, true);
        let rows = detailed["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 1);
        // 明细只是附加键，聚合数值与不采集明细时完全一致。
        assert_eq!(detailed["summary"], aggregate_only["summary"]);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_rows_share_the_total_formula_and_sort_newest_first() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-rows-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let record = |timestamp, input, write| {
            json!({
                "timestamp": timestamp,
                "cwd": "/private/example-project",
                "providerData": { "model": "fixture-model" },
                "message": { "usage": {
                    "input_tokens": input,
                    "output_tokens": 2,
                    "cache_read_input_tokens": 5,
                    "cache_write_input_tokens": write
                }}
            })
        };
        // 文件顺序刻意与时间顺序相反，排序必须来自 timestamp。
        fs::write(
            project.join("session-detail.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                record(now - 1_000, 10, 1),
                record(now - 2_000, 20, 2),
                record(now - 3_000, 30, 3),
            ),
        )
        .expect("write fixture");

        let result = source(root.clone(), "codebuddy-cli", None, true);
        assert_eq!(result["summary"]["records"], 3);
        // input 60 + output 6 + cacheWrite 6；cacheRead 不计入合计。
        assert_eq!(result["summary"]["total"], 72);
        let rows = result["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["timestamp"], now - 1_000);
        assert_eq!(rows[2]["timestamp"], now - 3_000);
        assert_eq!(rows[0]["model"], "fixture-model");
        // 只暴露 cwd 的 basename，不返回绝对路径。
        assert_eq!(rows[0]["project"], "example-project");
        assert_eq!(rows[0]["sessionId"], "session-detail");
        assert!(rows[0]["title"].is_null());
        assert_eq!(rows[0]["input"], 10);
        assert_eq!(rows[0]["output"], 2);
        assert_eq!(rows[0]["cacheRead"], 5);
        assert_eq!(rows[0]["cacheWrite"], 1);
        assert_eq!(rows[0]["total"], 13);
        // 缓存未命中 = input - cacheRead，与 `summary` 同口径。
        assert_eq!(rows[0]["uncachedInput"], 5);
        // 无 providerData.rawUsage 时思考过程为 0。
        assert_eq!(rows[0]["thinking"], 0);
        let row_total: u64 = rows
            .iter()
            .map(|row| row["total"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(row_total, result["summary"]["total"].as_u64().unwrap_or(0));
        let row_uncached: u64 = rows
            .iter()
            .map(|row| row["uncachedInput"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(
            row_uncached,
            result["summary"]["uncachedInput"].as_u64().unwrap_or(0)
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_window_keeps_only_the_newest_rows() {
        let row = |timestamp| RequestRow {
            timestamp,
            model: "fixture-model".to_string(),
            project: "fixture-project".to_string(),
            session_id: "fixture-session".to_string(),
            title: None,
            usage: Usage {
                input: 1,
                output: 1,
                read: 0,
                write: 0,
            },
            thinking: 0,
        };

        // 触达内存硬上限后只保留最新窗口，且按时间倒序。
        let mut collector = SourceCollector {
            collect_requests: true,
            ..SourceCollector::default()
        };
        for timestamp in 0..=(REQUEST_COLLECT_LIMIT as i64) {
            collector.push_request(row(timestamp));
        }
        assert_eq!(collector.requests.len(), REQUEST_WINDOW);
        assert_eq!(
            collector.requests[0].timestamp,
            REQUEST_COLLECT_LIMIT as i64
        );

        // 未触达硬上限时，输出窗口同样截断到最新 REQUEST_WINDOW 条。
        let mut bounded = SourceCollector {
            collect_requests: true,
            ..SourceCollector::default()
        };
        for timestamp in 0..(REQUEST_WINDOW as i64 + 100) {
            bounded.push_request(row(timestamp));
        }
        let value = bounded.into_value("fixture", 1);
        let rows = value["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), REQUEST_WINDOW);
        assert_eq!(rows[0]["timestamp"], REQUEST_WINDOW as i64 + 99);
        assert_eq!(rows[REQUEST_WINDOW - 1]["timestamp"], 100);

        // 关闭明细采集时不缓存任何行，也不输出 requests 键。
        let mut off = SourceCollector::default();
        off.push_request(row(1));
        assert!(off.into_value("fixture", 1).get("requests").is_none());
    }

    #[test]
    fn request_detail_window_stays_the_global_newest_after_any_arrival_order() {
        let row = |timestamp| RequestRow {
            timestamp,
            model: "fixture-model".to_string(),
            project: "fixture-project".to_string(),
            session_id: "fixture-session".to_string(),
            title: None,
            usage: Usage {
                input: 1,
                output: 1,
                read: 0,
                write: 0,
            },
            thinking: 0,
        };

        // 文件按 mtime 顺序处理、行内时间戳可以乱序：中途裁剪过之后，后到的
        // 旧行不能占位，后到的新行仍须进入窗口 —— 结果恒等于「全局最新 500」。
        let mut collector = SourceCollector {
            collect_requests: true,
            ..SourceCollector::default()
        };
        for timestamp in 0..=(REQUEST_COLLECT_LIMIT as i64) {
            collector.push_request(row(timestamp));
        }
        collector.push_request(row(-1));
        for timestamp in 10_000..10_100 {
            collector.push_request(row(timestamp));
        }

        let value = collector.into_value("fixture", 1);
        let rows = value["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), REQUEST_WINDOW);
        assert_eq!(rows[0]["timestamp"], 10_099);
        assert_eq!(rows[99]["timestamp"], 10_000);
        // 裁剪后残留的旧行必须让位：第 101 条是最新一轮保留里的最新一行。
        assert_eq!(rows[100]["timestamp"], REQUEST_COLLECT_LIMIT as i64);
        // 全局最新 500 = 100 条新行 + 上一轮保留里最新的 400 条（4000..=3601）。
        assert_eq!(rows[REQUEST_WINDOW - 1]["timestamp"], 3_601);
        assert!(rows.iter().all(|row| row["timestamp"] != -1));
    }

    #[test]
    fn request_detail_deduplicates_copied_session_history() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-fork-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let record = json!({
            "timestamp": now,
            "message": { "usage": { "input_tokens": 10, "output_tokens": 3 } }
        });
        fs::write(
            project.join("session-original.jsonl"),
            format!("{}\n", record),
        )
        .expect("write original fixture");
        fs::write(
            project.join("session-forked.jsonl"),
            format!(
                "{}\n{}\n",
                record,
                json!({
                    "timestamp": now + 1_000,
                    "message": { "usage": { "input_tokens": 7, "output_tokens": 2 } }
                })
            ),
        )
        .expect("write forked fixture");
        // 与聚合去重用例相同：固定 mtime，保证原始会话先于副本被处理。
        // Windows 上 `set_modified` 走 SetFileTime 需要 FILE_WRITE_ATTRIBUTES,
        // 只读句柄会 Access Denied ⇒ 用 write(true) 打开（同 pin_mtime 模式）。
        std::fs::OpenOptions::new()
            .write(true)
            .open(project.join("session-original.jsonl"))
            .expect("open original fixture")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
            .expect("pin original mtime");
        std::fs::OpenOptions::new()
            .write(true)
            .open(project.join("session-forked.jsonl"))
            .expect("open forked fixture")
            .set_modified(std::time::SystemTime::now())
            .expect("pin forked mtime");

        let result = source(root.clone(), "fixture", None, true);
        assert_eq!(result["summary"]["records"], 2);
        let rows = result["requests"].as_array().expect("request rows");
        // 复制会话重放的记录沿用同一指纹，明细里只出现一次。
        assert_eq!(rows.len(), 2);
        let inputs: Vec<u64> = rows
            .iter()
            .filter_map(|row| row["input"].as_u64())
            .collect();
        assert_eq!(inputs, [7, 10]);
        assert_eq!(rows[0]["sessionId"], "session-forked");
        assert_eq!(rows[1]["sessionId"], "session-original");

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_skips_records_without_timestamp() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-undated-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        fs::write(
            project.join("session.jsonl"),
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp": now,
                    "message": { "usage": { "input_tokens": 10, "output_tokens": 3 } }
                }),
                // 没有 timestamp 的记录仍然计入聚合，但无法排序，不进明细。
                json!({ "message": { "usage": { "input_tokens": 90, "output_tokens": 9 } } }),
            ),
        )
        .expect("write fixture");

        let result = source(root.clone(), "fixture", None, true);
        assert_eq!(result["summary"]["records"], 2);
        assert_eq!(result["summary"]["input"], 100);
        let rows = result["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["input"], 10);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_backfills_file_title_after_usage() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-title-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        let record = |timestamp| {
            json!({
                "timestamp": timestamp,
                "message": { "usage": { "input_tokens": 10, "output_tokens": 3 } }
            })
        };
        fs::write(
            project.join("session.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({ "type": "summary", "summary": "摘要回退标题" }),
                record(now - 1_000),
                // aiTitle 出现在 usage 之后：文件读完后必须回填到所有明细行。
                json!({ "type": "ai-title", "aiTitle": "最新 AI 标题" }),
            ),
        )
        .expect("write fixture");

        let result = source(root.clone(), "fixture", None, true);
        let rows = result["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], "最新 AI 标题");

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_thinking_prefers_completion_thinking_tokens() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-thinking-priority-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        fs::write(
            project.join("session.jsonl"),
            format!(
                "{}\n",
                json!({
                    "timestamp": now,
                    "message": { "usage": { "input_tokens": 10, "output_tokens": 40 } },
                    // 两个来源同时存在时取扁平的 completion_thinking_tokens。
                    "providerData": { "rawUsage": {
                        "completion_thinking_tokens": 7,
                        "completion_tokens_details": { "reasoning_tokens": 3 }
                    }}
                })
            ),
        )
        .expect("write fixture");

        let result = source(root.clone(), "fixture", None, true);
        let rows = result["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["thinking"], 7);
        assert_eq!(rows[0]["output"], 40);
        // 思考过程只是 output 的拆分，不改变合计口径。
        assert_eq!(rows[0]["total"], result["summary"]["total"]);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_thinking_falls_back_to_reasoning_tokens_and_defaults_to_zero() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-thinking-fallback-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        fs::write(
            project.join("session.jsonl"),
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp": now,
                    "message": { "usage": { "input_tokens": 11, "output_tokens": 21 } },
                    // rawUsage 只有思考计数字段时取 OpenAI 形状的 reasoning_tokens。
                    "providerData": { "rawUsage": {
                        "completion_tokens_details": { "reasoning_tokens": 5 }
                    }}
                }),
                json!({
                    "timestamp": now - 1_000,
                    "message": { "usage": { "input_tokens": 10, "output_tokens": 20 } },
                    // rawUsage 缺少思考计数 -> 0；`reasoning` 是思考正文，
                    // 只用来验证它不会被当成计数读取（隐私红线）。
                    "providerData": {
                        "reasoning": "思考正文不得进入明细",
                        "rawUsage": { "prompt_cache_write_tokens": 1 }
                    }
                })
            ),
        )
        .expect("write fixture");

        let result = source(root.clone(), "fixture", None, true);
        let rows = result["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["thinking"], 5);
        assert_eq!(rows[1]["thinking"], 0);
        assert_eq!(rows[1]["cacheWrite"], 1);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn request_detail_thinking_over_output_saturates_reply_to_zero() {
        // 实测存在 thinking > output 的异常记录：后端如实返回两个原始计数，
        // 前端的 `output.saturating_sub(thinking)` 必须得到 0 而不是负数。
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-detail-thinking-overflow-{}-{now}",
            std::process::id()
        ));
        let project = root.join("fixture-project");
        fs::create_dir_all(&project).expect("create fixture dirs");
        fs::write(
            project.join("session.jsonl"),
            format!(
                "{}\n",
                json!({
                    "timestamp": now,
                    "message": { "usage": { "input_tokens": 10, "output_tokens": 2 } },
                    "providerData": { "rawUsage": { "completion_thinking_tokens": 9 } }
                })
            ),
        )
        .expect("write fixture");

        let result = source(root.clone(), "fixture", None, true);
        let rows = result["requests"].as_array().expect("request rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["output"], 2);
        assert_eq!(rows[0]["thinking"], 9);
        let reply = rows[0]["output"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(rows[0]["thinking"].as_u64().unwrap_or(0));
        assert_eq!(reply, 0);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn ide_source_reads_request_usage_and_skips_message_bodies() {
        let now = crate::modules::config::now_ms();
        let root = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-ide-{}-{now}",
            std::process::id()
        ));
        let history = root
            .join("uid")
            .join("CodeBuddyIDE")
            .join("uid")
            .join("history")
            .join("workspace-hash");
        let conv_a = history.join("conv-a");
        let conv_b = history.join("conv-b");
        let messages = conv_a.join("messages");
        fs::create_dir_all(&messages).expect("create ide fixture dirs");
        fs::create_dir_all(&conv_b).expect("create second conversation");

        fs::write(
            history.join("index.json"),
            json!({
                "conversations": [
                    {
                        "id": "conv-a",
                        "name": "IDE 会话标题",
                        "selectedModelId": "deepseek-v4-flash"
                    },
                    {
                        "id": "conv-b",
                        "name": "范围外会话",
                        "selectedModelId": "hy4-preview"
                    }
                ]
            })
            .to_string(),
        )
        .expect("write workspace index");
        fs::write(
            conv_a.join("index.json"),
            json!({
                "messages": [{ "id": "m1", "role": "assistant", "isComplete": true }],
                "requests": [{
                    "id": "req-1",
                    "state": "complete",
                    "startedAt": now,
                    "usage": {
                        "inputTokens": 100,
                        "outputTokens": 20,
                        "cacheTokens": 40,
                        "cachedWriteTokens": 5
                    }
                }]
            })
            .to_string(),
        )
        .expect("write conversation index");
        fs::write(
            conv_b.join("index.json"),
            json!({
                "requests": [{
                    "id": "req-old",
                    "state": "complete",
                    "startedAt": now - 100_000,
                    "usage": {
                        "inputTokens": 999,
                        "outputTokens": 9,
                        "cacheTokens": 1,
                        "cachedWriteTokens": 0
                    }
                }]
            })
            .to_string(),
        )
        .expect("write out-of-range conversation");
        fs::write(
            messages.join("ignored.json"),
            json!({
                "role": "assistant",
                "usage": { "inputTokens": 10_000, "outputTokens": 10_000 }
            })
            .to_string(),
        )
        .expect("write ignored message body");

        let mut projects = HashMap::new();
        projects.insert("conv-a".to_string(), "example-project".to_string());
        let result = ide_source(root.clone(), "codebuddy-ide", Some(now - 50_000), &projects);

        assert_eq!(result["source"], "codebuddy-ide");
        assert_eq!(result["filesScanned"], 2);
        assert_eq!(result["summary"]["records"], 1);
        assert_eq!(result["summary"]["input"], 100);
        assert_eq!(result["summary"]["output"], 20);
        assert_eq!(result["summary"]["cacheRead"], 40);
        assert_eq!(result["summary"]["cacheWrite"], 5);
        assert_eq!(result["summary"]["total"], 125);
        assert_eq!(result["summary"]["uncachedInput"], 60);
        assert_eq!(result["models"][0]["key"], "deepseek-v4-flash");
        assert_eq!(result["projects"][0]["key"], "example-project");
        assert_eq!(result["sessions"][0]["sessionId"], "conv-a");
        assert_eq!(result["sessions"][0]["title"], "IDE 会话标题");
        assert_eq!(result["sessions"].as_array().map(Vec::len), Some(1));
        // IDE 来源不是明细来源，响应形状保持不变。
        assert!(result.get("requests").is_none());
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(result["dailyByModel"]["deepseek-v4-flash"][0]["key"], today);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn decode_genie_workspace_recovers_unix_project_path() {
        assert_eq!(
            decode_genie_workspace("L1VzZXJzL2FwcGxlL0RvY3VtZW50cy9Qcm9qZWN0L215LWFnZW50")
                .as_deref(),
            Some("/Users/apple/Documents/Project/my-agent")
        );
    }

    #[test]
    fn get_statistics_returns_four_isolated_sources() {
        let value = get_statistics(None);
        let sources = value["sources"].as_array().expect("sources");
        let names: Vec<_> = sources
            .iter()
            .map(|source| source["source"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            [
                "workbuddy",
                "workbuddy-ai",
                "codebuddy-cli",
                "codebuddy-ide"
            ]
        );
        // 国际版与国内版是两个独立 source，不合并。
        assert_ne!(sources[0]["source"], sources[1]["source"]);
    }

    /// 国际版数据根缺少 jsonl 根时返回空集，不报错。
    #[test]
    fn ai_source_is_empty_and_error_free_when_roots_are_missing() {
        let value = source_from_roots(&[], "workbuddy-ai", None, false);
        assert_eq!(value["source"], "workbuddy-ai");
        assert_eq!(value["filesScanned"], 0);
        assert_eq!(value["summary"]["input"], 0);
        assert_eq!(value["parseErrors"], 0);
        assert_eq!(value["sessions"].as_array().map(Vec::len), Some(0));
        assert_eq!(value["projects"].as_array().map(Vec::len), Some(0));
    }

    /// 多根共享同一个指纹集：国际版两个根之间重放的同一请求只计一次，
    /// 同时第二个根自身的新记录仍要计入（证明第二个根确实被扫了）。
    #[test]
    fn multi_root_source_deduplicates_across_roots() {
        let now = crate::modules::config::now_ms();
        let base = std::env::temp_dir().join(format!(
            "wb-switch-token-stats-multiroot-{}-{now}",
            std::process::id()
        ));
        let projects = base.join("projects");
        let sessions = base.join("sessions");
        fs::create_dir_all(&projects).expect("create projects root");
        fs::create_dir_all(&sessions).expect("create sessions root");
        let replayed = json!({
            "timestamp": now,
            "cwd": "/fixture/example-project",
            "message": { "usage": { "input_tokens": 10, "output_tokens": 3 } }
        });
        fs::write(
            projects.join("session-original.jsonl"),
            format!("{}\n", replayed),
        )
        .expect("write original fixture");
        // 第二个根里的会话重放了同一个请求，并另有一条自己的新请求。
        fs::write(
            sessions.join("session-root-two.jsonl"),
            format!(
                "{}\n{}\n",
                replayed,
                json!({
                    "timestamp": now + 1_000,
                    "message": { "usage": { "input_tokens": 7, "output_tokens": 2 } }
                })
            ),
        )
        .expect("write second-root fixture");
        // 与 source_deduplicates_copied_session_history 同理：毫秒级 mtime 并列时
        // 顺序退化为 readdir，重放记录可能先被第二个根认领。pin 住 mtime 让
        // 原始会话先处理，断言才稳定。（Windows 只读句柄 set_modified 会
        // Access Denied ⇒ 用 write(true) 打开，同 pin_mtime 模式。）
        std::fs::OpenOptions::new()
            .write(true)
            .open(projects.join("session-original.jsonl"))
            .expect("open original fixture")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
            .expect("pin original mtime");
        std::fs::OpenOptions::new()
            .write(true)
            .open(sessions.join("session-root-two.jsonl"))
            .expect("open second-root fixture")
            .set_modified(std::time::SystemTime::now())
            .expect("pin second-root mtime");

        let result = source_from_roots(
            &[projects.clone(), sessions.clone()],
            "workbuddy-ai",
            None,
            false,
        );
        // 两个根都被扫到；重放记录计一次，第二个根的新记录另计一次。
        assert_eq!(result["filesScanned"], 2);
        assert_eq!(result["summary"]["records"], 2);
        assert_eq!(result["summary"]["input"], 17);
        assert_eq!(result["summary"]["output"], 5);
        fs::remove_dir_all(base).expect("remove fixture");
    }

    /// 候选根按档位不同：国内版单根，国际版双根（无 projects/ 时只扫 sessions/）。
    #[test]
    fn jsonl_root_candidates_differ_by_variant() {
        let root = PathBuf::from("/tmp/wb-root");
        assert_eq!(
            jsonl_root_candidates(WbVariant::Cn, &root),
            vec![PathBuf::from("/tmp/wb-root/projects")]
        );
        assert_eq!(
            jsonl_root_candidates(WbVariant::Ai, &root),
            vec![
                PathBuf::from("/tmp/wb-root/projects"),
                PathBuf::from("/tmp/wb-root/sessions")
            ]
        );
    }
}
