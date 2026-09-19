//! 本机"曾登录账号"发现与补录。
//!
//! 数据源（证据链）：
//! 1. **官方 auth 历史**：`~/AppData/Local/CodeBuddyExtension/Data/Public/auth/`
//!    `workbuddy-desktop.<ts>.<pid>.<uuid>.info` —— WorkBuddy 每次登录/切号留档，
//!    含完整 account + token，是"曾登录过"的最权威记录。
//! 2. **数据残留**：settings.json `claw.users` 键 ∪ `storage/user-<uid>*` 目录 ∪
//!    `memory/<uid>_memory.md`（本模块 [`discover_accounts`]），无凭据，仅证明确实用过。
//!
//! 对照在册账号库（~/.wb-switch/accounts.json）输出"识别到但未登记"的账号，
//! 供 UI 一键补录（adopt_account：凭据来自最新 auth 历史备份）。

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::account::{get_str, load_accounts, save_collected_account};
use crate::modules::auth_file::{auth_file_path, imported_account_from_root};
use crate::modules::config::{backup_dir, home_dir, now_ms, utc_iso};

// ---------------------------------------------------------------------------
// 本机数据落点（`~/.workbuddy/*`）—— 对齐模块（align）也复用这几个路径
// ---------------------------------------------------------------------------

fn workbuddy_root() -> PathBuf {
    home_dir().join(".workbuddy")
}

/// `~/.workbuddy/settings.json`（`claw.users` 账号键的来源）。
pub(crate) fn settings_path() -> PathBuf {
    workbuddy_root().join("settings.json")
}

/// `~/.workbuddy/memory`（画像缓存 `<uid>_memory.md`）。
pub(crate) fn memory_dir() -> PathBuf {
    workbuddy_root().join("memory")
}

/// `~/.workbuddy/storage`（`user-<uid>*` 目录）。
pub(crate) fn storage_dir() -> PathBuf {
    workbuddy_root().join("storage")
}

/// 账号清单 = settings `claw.users` 键 ∪ `storage/user-<uid>` 目录 ∪ 画像缓存文件。
///
/// 无凭据，只说明「这个 uid 在本机确实用过」⇒ 发现的 residual 来源；
/// 对齐模块的缺省源选择（`align::pick_source`）也用它。
pub fn discover_accounts() -> Vec<String> {
    let mut accs: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(settings_path()) {
        if let Ok(data) = serde_json::from_str::<Value>(&text) {
            if let Some(users) = data
                .get("claw")
                .and_then(|c| c.get("users"))
                .and_then(|u| u.as_object())
            {
                accs.extend(users.keys().cloned());
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(storage_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(uid) = name.strip_prefix("user-") {
                if !uid.ends_with("-personal") {
                    accs.push(uid.to_string());
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(memory_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(uid) = name.strip_suffix("_memory.md") {
                accs.push(uid.to_string());
            }
        }
    }
    accs.sort();
    accs.dedup();
    accs
}

/// 扫描 auth 目录：`workbuddy-desktop.*.info`（排除当前登录文件），按 uid 保留最新。
fn scan_auth_history() -> Vec<(i64, Value)> {
    let current = auth_file_path(crate::modules::variant::WbVariant::Cn);
    let Some(dir) = current.parent().map(|p| p.to_path_buf()) else {
        return vec![];
    };
    scan_auth_history_in(&dir, &current)
}

/// 可注入目录的实现（单测用）。
fn scan_auth_history_in(dir: &std::path::Path, current: &std::path::Path) -> Vec<(i64, Value)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[discover] 读取 auth 目录失败: {e}");
            return vec![];
        }
    };

    let mut seen: HashMap<String, (String, i64, Value)> = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.starts_with("workbuddy-desktop") || !name.ends_with(".info") {
            continue;
        }
        if path == current {
            continue; // 当前登录态不算历史
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(root) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // 新旧判据：**主判据 = 文件名内嵌的 ISO 时间戳**（`workbuddy-desktop.<ISO8601>.….info`）。
        // 理由：该时间戳由客户端写入备份时生成、天然有序；而 mtime 在快速连续写入时会落在
        // 同一毫秒，导致「同 uid 取最新」取错（2026-09-16 修此 bug，由测试
        // `scan_keeps_newest_per_uid_and_skips_current` 捕获）。
        // ISO 8601 字符串的**字典序 == 时间序**，故直接字符串比较，无需日期运算；
        // 解析不到时间戳时退化为空串，实际由 mtime 决定。
        let stamp = name
            .strip_prefix("workbuddy-desktop.")
            .and_then(|rest| rest.split('.').next())
            .unwrap_or("")
            .to_string();
        let Some(rec) = imported_account_from_root(root, crate::modules::variant::WbVariant::Cn) else {
            continue; // 无 access_token 的备份无恢复价值
        };
        let uid = get_str(&rec, "uid").unwrap_or_default();
        if uid.is_empty() {
            continue;
        }
        match seen.get(&uid) {
            Some((prev_stamp, prev_mtime, _))
                if prev_stamp.as_str() > stamp.as_str()
                    || (prev_stamp.as_str() == stamp.as_str() && *prev_mtime >= mtime) =>
            {
                continue
            }
            _ => {
                seen.insert(uid, (stamp, mtime, rec));
            }
        }
    }
    seen.into_values().map(|(_, mtime, rec)| (mtime, rec)).collect()
}

fn backup_accounts_file() -> Option<PathBuf> {
    let src = crate::modules::config::accounts_file();
    if !src.is_file() {
        return None;
    }
    let dir = backup_dir().join("accounts").join(utc_iso());
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("accounts.json");
    std::fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// 识别本机所有曾登录/留有数据的账号，对照在册账号库。
///
/// 返回每项：
/// ```json
/// { "uid", "nickname", "email", "source": "auth-history" | "residual",
///   "backupFiles": 0, "backedUpAt": 0, "inAccountList": false,
///   "accessTokenExpiresAt": 0, "refreshTokenExpiresAt": 0,
///   "restorable": false }
/// ```
/// `restorable` = 有 auth 历史备份 且 refresh token 未过期（可补录并刷新）。
pub fn discover_known_accounts() -> Value {
    let in_accounts = load_accounts();
    let history = scan_auth_history();

    let mut items: Vec<Value> = Vec::new();
    // 1) auth 历史优先（含凭据，可直接补录）
    for (mtime, rec) in history {
        let uid = get_str(&rec, "uid").unwrap_or_default();
        let now = now_ms();
        let access_exp = rec.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
        let refresh_exp = rec
            .get("refreshExpiresAt")
            .and_then(|v| v.as_i64())
            .unwrap_or(access_exp);
        let in_list = in_accounts.iter().any(|a| {
            get_str(a, "uid").as_deref() == Some(uid.as_str())
                || get_str(a, "id").as_deref() == Some(uid.as_str())
        });
        items.push(json!({
            "uid": uid,
            "nickname": rec.get("nickname").cloned().unwrap_or(Value::Null),
            "email": rec.get("email").cloned().unwrap_or(Value::Null),
            "source": "auth-history",
            "backupFiles": 1,
            "backedUpAt": mtime,
            "inAccountList": in_list,
            "accessTokenExpiresAt": access_exp,
            "refreshTokenExpiresAt": refresh_exp,
            "restorable": refresh_exp == 0 || refresh_exp > now,
        }));
    }

    // 2) 数据残留账号（无凭据备份，仅提示"曾登录"，restorable=false）
    let residual_uids = discover_accounts();
    let known_uids: Vec<String> = items.iter().filter_map(|i| get_str(i, "uid")).collect();
    for uid in residual_uids {
        if known_uids.contains(&uid) {
            continue;
        }
        let in_list = in_accounts.iter().any(|a| {
            get_str(a, "uid").as_deref() == Some(uid.as_str())
                || get_str(a, "id").as_deref() == Some(uid.as_str())
        });
        items.push(json!({
            "uid": uid,
            "nickname": Value::Null,
            "email": Value::Null,
            "source": "residual",
            "backupFiles": 0,
            "backedUpAt": 0,
            "inAccountList": in_list,
            "accessTokenExpiresAt": 0,
            "refreshTokenExpiresAt": 0,
            "restorable": false,
        }));
    }

    items.sort_by_key(|i| {
        (
            i.get("inAccountList")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            std::cmp::Reverse(i.get("backedUpAt").and_then(|v| v.as_i64()).unwrap_or(0)),
        )
    });
    json!({ "accounts": items })
}

/// 补录：用 uid 对应的最新 auth 历史备份构造账号记录并写入账号库。
///
/// 写前先备份 accounts.json 到 `~/.wb-switch/backups/accounts/<ts>/`。
pub fn adopt_account(uid: &str) -> Result<Value, String> {
    let history = scan_auth_history();
    let mut newest: Option<(i64, Value)> = None;
    for (mtime, rec) in history {
        if get_str(&rec, "uid").as_deref() == Some(uid)
            && newest.as_ref().is_none_or(|(m, _)| *m < mtime)
        {
            newest = Some((mtime, rec));
        }
    }
    let Some((_, rec)) = newest else {
        return Err(format!("uid {uid} 无 auth 历史备份，无法补录"));
    };
    backup_accounts_file();
    let saved = save_collected_account(rec).map_err(|e| e.to_string())?;
    Ok(crate::modules::account::account_meta(&saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 在临时 auth 目录构造两个账号的历史备份，验证扫描去重取最新 + 排除当前登录。
    #[test]
    fn scan_keeps_newest_per_uid_and_skips_current() {
        let tmp = std::env::temp_dir().join(format!("wb-discover-test-{}", now_ms()));
        std::fs::create_dir_all(&tmp).unwrap();
        let write = |name: &str, uid: &str, nick: &str, tok: &str| {
            let root = json!({
                "account": {"uid": uid, "nickname": nick},
                "auth": {"accessToken": tok, "refreshToken": "RT",
                         "tokenType": "Bearer", "domain": "d", "expiresAt": 1}
            });
            std::fs::write(tmp.join(name), serde_json::to_string(&root).unwrap()).unwrap();
        };
        // 旧备份 u-1 → 新备份 u-1（应留新）；u-2 一份；当前登录文件应跳过
        write("workbuddy-desktop.2026-09-01T00-00-00-000Z.1.aaaa.info", "u-1", "一号", "AT-1-old");
        write("workbuddy-desktop.2026-09-02T00-00-00-000Z.1.bbbb.info", "u-1", "一号", "AT-1-new");
        write("workbuddy-desktop.2026-09-03T00-00-00-000Z.1.cccc.info", "u-2", "二号", "AT-2");
        write("workbuddy-desktop.info", "u-cur", "当前", "AT-cur"); // 应被排除
        // 非 workbuddy 前缀文件应被忽略
        write("other.info", "u-x", "杂鱼", "AT-x");

        let hits = scan_auth_history_in(&tmp, &tmp.join("workbuddy-desktop.info"));
        let mut by_uid: HashMap<String, String> = HashMap::new();
        for (_, rec) in &hits {
            by_uid.insert(
                get_str(rec, "uid").unwrap(),
                get_str(rec, "access_token").unwrap(),
            );
        }
        assert_eq!(by_uid.len(), 2, "应识别 u-1 + u-2，共两个历史账号");
        assert_eq!(by_uid.get("u-1").unwrap(), "AT-1-new", "同 uid 应取最新备份");
        assert_eq!(by_uid.get("u-2").unwrap(), "AT-2");
        assert!(!by_uid.contains_key("u-cur"), "当前登录文件应被排除");
        assert!(!by_uid.contains_key("u-x"), "非 workbuddy 前缀应被忽略");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 无 token 的备份应被忽略（imported_account_from_root 返回 None）。
    #[test]
    fn no_token_root_is_ignored() {
        let root = json!({ "account": { "uid": "u-1", "nickname": "n" } });
        assert!(imported_account_from_root(root, crate::modules::variant::WbVariant::Cn).is_none());
    }
}
