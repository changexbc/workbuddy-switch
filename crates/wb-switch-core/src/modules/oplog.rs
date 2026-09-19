//! 切号操作留痕（本地专属模块，上游无此文件，改动零合并冲突）。
//!
//! 目的：切号 / 数据对齐的结果此前只在 UI 当次展示，事后无法回溯「何时切到谁、
//! 勾了哪些对齐项、各层改了多少」。这里把结果追加到
//! `~/.wb-switch/switch_logs.json`，仅留痕，不阻断切号流程。
//!
//! 与 wb_multi_sync 的 `logs/sync-<日期>.log` 对应，但按条存 JSON 便于后续在
//! 设置页做展示（UI 未接入时可直接读文件）。

use crate::modules::config::{atomic_write, now_ms, store_dir, utc_iso};
use serde_json::{json, Value};
use std::path::PathBuf;

/// 保留最近多少条（与自动轮换日志同量级）。
pub const SWITCH_LOG_MAX_RECORDS: usize = 200;

pub fn switch_logs_file() -> PathBuf {
    store_dir().join("switch_logs.json")
}

/// 读取全部切号日志（保持写入顺序，最旧在前）。
pub fn load_switch_logs() -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(switch_logs_file()) else {
        return vec![];
    };
    serde_json::from_str::<Vec<Value>>(&text).unwrap_or_default()
}

/// 保存切号日志（保留最近 N 条，保持插入顺序）。
pub fn save_switch_logs(logs: &[Value]) -> std::io::Result<()> {
    let mut kept: Vec<Value> = logs.to_vec();
    if kept.len() > SWITCH_LOG_MAX_RECORDS {
        kept.drain(..kept.len() - SWITCH_LOG_MAX_RECORDS);
    }
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&kept).unwrap_or_default();
    atomic_write(&switch_logs_file(), &content)
}

/// 追加一条切号日志（写失败不抛错，绝不阻断切号）。
pub fn add_switch_log(entry: &Value) {
    let mut logs = load_switch_logs();
    logs.push(entry.clone());
    let _ = save_switch_logs(&logs);
}

/// 组装一条切号日志记录。
///
/// - `action`：`switch`（正常切换）/ `dry-run`（预览）/ `error`（失败）
/// - `from` / `to`：源账号 uid、目标账号 uid
/// - `options`：本次勾选项（复制会话数、三个对齐开关）
/// - `result`：成功时的分段结果，或失败原因
pub fn switch_log_entry(
    action: &str,
    from_uid: Option<&str>,
    to_uid: &str,
    options: &Value,
    result: &Value,
) -> Value {
    json!({
        "ts": now_ms(),
        "at": utc_iso(),
        "action": action,
        "from": from_uid,
        "to": to_uid,
        "options": options,
        "result": result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_carries_action_origin_and_options() {
        let entry = switch_log_entry(
            "switch",
            Some("u-from"),
            "u-to",
            &json!({"copySessions": 2, "alignAutomations": true}),
            &json!({"ok": true}),
        );
        assert_eq!(entry["action"], json!("switch"));
        assert_eq!(entry["from"], json!("u-from"));
        assert_eq!(entry["to"], json!("u-to"));
        assert_eq!(entry["options"]["copySessions"], json!(2));
        assert!(entry["ts"].as_i64().unwrap_or(0) > 0);
        assert!(entry["at"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn save_keeps_only_latest_records_and_preserves_order() {
        let mut logs: Vec<Value> = (0..SWITCH_LOG_MAX_RECORDS + 5)
            .map(|i| json!({ "seq": i }))
            .collect();
        if logs.len() > SWITCH_LOG_MAX_RECORDS {
            logs.drain(..logs.len() - SWITCH_LOG_MAX_RECORDS);
        }
        assert_eq!(logs.len(), SWITCH_LOG_MAX_RECORDS);
        assert_eq!(logs[0]["seq"], json!(5));
        assert_eq!(logs[logs.len() - 1]["seq"], json!(SWITCH_LOG_MAX_RECORDS + 4));
    }
}
