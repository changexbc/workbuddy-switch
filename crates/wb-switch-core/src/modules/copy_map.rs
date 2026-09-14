//! 会话复制关系表（复制去重的核心）。
//!
//! 存储位置：`~/.wb-switch/copy-map.json`（工具自己的数据目录，与 WorkBuddy 数据隔离）。
//! 结构：
//! ```json
//! {
//!   "version": 1,
//!   "copies": [
//!     { "sourceUid": "a", "sourceCid": "...", "targetUid": "b",
//!       "targetCid": "...", "copiedAt": 1700000000000 }
//!   ]
//! }
//! ```
//!
//! 判重规则：
//! 1. 先查表：同一 (sourceUid, sourceCid, targetUid) 已有记录，且目标会话仍存在
//!    （未删除）→ 视为重复，跳过复制。
//! 2. 双向归一：如果 sourceCid 本身是某次复制的产物，以「根会话」为准，
//!    避免 A→B 之后再从 B 的副本复制回 A 时漏判。
//! 3. 表丢失或查不到的兜底由调用方按「同标题+同目录」匹配处理（见 session.rs）。

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::modules::config::{atomic_write, now_ms, store_dir};

fn copy_map_file() -> PathBuf {
    store_dir().join("copy-map.json")
}

fn empty_map() -> Value {
    json!({ "version": 1, "copies": [] })
}

fn load_map() -> Value {
    let path = copy_map_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| empty_map()),
        Err(_) => empty_map(),
    }
}

fn save_map(map: &Value) {
    let path = copy_map_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = atomic_write(&path, &serde_json::to_string_pretty(map).unwrap_or_default());
}

/// 沿复制链向上找「根会话」id：若 cid 本身是复制产物，返回其源会话的根。
fn root_cid(map: &Value, cid: &str) -> String {
    let mut current = cid.to_string();
    // 防御环状数据，最多回溯 32 层
    for _ in 0..32 {
        let parent = map["copies"].as_array().and_then(|copies| {
            copies.iter().find_map(|c| {
                if c["targetCid"].as_str() == Some(current.as_str()) {
                    c["sourceCid"].as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
        });
        match parent {
            Some(p) => current = p,
            None => break,
        }
    }
    current
}

/// 查询 (source_uid, source_cid) 是否已复制给 target_uid。
///
/// 返回已存在的目标会话 cid（调用方需自行确认该会话在库里未被删除）。
pub fn find_copy(source_uid: &str, source_cid: &str, target_uid: &str) -> Option<String> {
    let map = load_map();
    let root = root_cid(&map, source_cid);
    map["copies"].as_array().and_then(|copies| {
        copies.iter().find_map(|c| {
            let c_root = root_cid(&map, c["sourceCid"].as_str().unwrap_or_default());
            let same_source = c["sourceCid"].as_str() == Some(source_cid)
                || (!c_root.is_empty() && c_root == root);
            if same_source
                && c["sourceUid"].as_str() == Some(source_uid)
                && c["targetUid"].as_str() == Some(target_uid)
            {
                c["targetCid"].as_str().map(|s| s.to_string())
            } else {
                None
            }
        })
    })
}

/// 记录一条复制关系。
pub fn record_copy(
    source_uid: &str,
    source_cid: &str,
    target_uid: &str,
    target_cid: &str,
) {
    let mut map = load_map();
    let entry = json!({
        "sourceUid": source_uid,
        "sourceCid": source_cid,
        "targetUid": target_uid,
        "targetCid": target_cid,
        "copiedAt": now_ms(),
    });
    if let Some(copies) = map["copies"].as_array_mut() {
        // 同一路径的旧记录先移除，保持最新（避免同一源重复积累记录）
        copies.retain(|c| {
            !(c["sourceUid"].as_str() == Some(source_uid)
                && c["sourceCid"].as_str() == Some(source_cid)
                && c["targetUid"].as_str() == Some(target_uid))
        });
        copies.push(entry);
    }
    save_map(&map);
}

/// 目标会话被删除后清理失效映射（清理重复会话时调用）。
pub fn remove_copies_for_target(target_cids: &[String]) {
    if target_cids.is_empty() {
        return;
    }
    let mut map = load_map();
    if let Some(copies) = map["copies"].as_array_mut() {
        copies.retain(|c| {
            c["targetCid"]
                .as_str()
                .is_some_and(|t| !target_cids.iter().any(|x| x == t))
        });
    }
    save_map(&map);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // 文件态测试串行执行，避免并发写 copy-map.json 互相干扰
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn record_and_find_roundtrip() {
        let _g = LOCK.lock().unwrap();
        record_copy("uid-a", "cid-1", "uid-b", "cid-2");
        assert_eq!(
            find_copy("uid-a", "cid-1", "uid-b"),
            Some("cid-2".to_string())
        );
        assert_eq!(find_copy("uid-a", "cid-1", "uid-c"), None);
        remove_copies_for_target(&["cid-2".to_string()]);
    }

    #[test]
    fn chained_copy_resolves_to_root() {
        let _g = LOCK.lock().unwrap();
        // cid-1 → cid-2（A 复制给 B），再从 B 的副本 cid-2 复制回 A：
        // root_cid(cid-2) = cid-1，应命中第一条记录判重
        record_copy("uid-a", "cid-1", "uid-b", "cid-2");
        assert_eq!(
            find_copy("uid-a", "cid-1", "uid-b"),
            Some("cid-2".to_string())
        );
        remove_copies_for_target(&["cid-2".to_string()]);
        assert_eq!(find_copy("uid-a", "cid-1", "uid-b"), None);
    }
}
