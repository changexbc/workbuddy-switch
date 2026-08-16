//! 账号存储：读取/写入 `~/.wb-switch/accounts.json`，与 Python 版共享数据目录。
//!
//! 对照 server.py `load_accounts` / `save_accounts` / `find_account` /
//! `account_display_name` / `account_meta`。

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::modules::config::{accounts_file, atomic_write, store_dir};

/// 读取账号库；文件缺失或损坏返回空列表。
pub fn load_accounts() -> Vec<Value> {
    let f = accounts_file();
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&text) {
                return arr;
            }
        }
    }
    vec![]
}

/// 写回账号库（原子写），保持原 JSON 数组结构。
pub fn save_accounts(accounts: &[Value]) -> std::io::Result<()> {
    let dir = store_dir();
    std::fs::create_dir_all(&dir)?;
    let content = serde_json::to_string_pretty(accounts).unwrap_or_default();
    atomic_write(&accounts_file(), &content)
}

/// 按 id 或 uid 查找账号。
pub fn find_account(account_id: &str) -> Option<Value> {
    load_accounts().into_iter().find(|a| {
        a.get("id").and_then(|v| v.as_str()) == Some(account_id)
            || a.get("uid").and_then(|v| v.as_str()) == Some(account_id)
    })
}

/// 账号展示名（email → nickname → uid → unknown）。
pub fn account_display_name(acc: &Value) -> String {
    get_str(acc, "email")
        .or_else(|| get_str(acc, "nickname"))
        .or_else(|| get_str(acc, "uid"))
        .unwrap_or_else(|| "unknown".to_string())
}

/// 账号的展示元数据（不泄露 token）。对照 server.py `account_meta`。
pub fn account_meta(acc: &Value) -> Value {
    json!({
        "id": acc.get("id"),
        "uid": acc.get("uid"),
        "email": acc.get("email"),
        "nickname": acc.get("nickname"),
        "enterpriseName": acc.get("enterpriseName"),
        "expiresAt": acc.get("expiresAt"),
        "refreshExpiresAt": acc.get("refreshExpiresAt"),
        "refreshedAt": acc.get("refreshedAt"),
        "createdAt": acc.get("createdAt"),
        "needsRelogin": acc.get("needs_relogin").and_then(|v| v.as_bool()) == Some(true),
        "needsReloginReason": acc.get("needs_relogin_reason"),
    })
}

/// 取非空字符串字段；空/缺失返回 None。
pub fn get_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 按 id 覆盖写入账号库（不存在则追加）。对照 server.py `_upsert_account`。
pub fn upsert_account(updated: &Value) -> std::io::Result<()> {
    let mut accounts = load_accounts();
    let id = updated.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let mut replaced = false;
    for a in accounts.iter_mut() {
        if a.get("id").and_then(|v| v.as_str()) == Some(id) {
            *a = updated.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        accounts.push(updated.clone());
    }
    save_accounts(&accounts)
}

/// 构造与官方对齐的请求头。对照 server.py `build_auth_headers`。
pub fn build_auth_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", get_str(account, "access_token").unwrap_or_default()),
    );
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Some(uid) = get_str(account, "uid") {
        headers.insert("X-User-Id".to_string(), uid);
    }
    if let Some(eid) =
        get_str(account, "enterpriseId").or_else(|| get_str(account, "enterprise_id"))
    {
        headers.insert("X-Enterprise-Id".to_string(), eid.clone());
        headers.insert("X-Tenant-Id".to_string(), eid);
    }
    if let Some(domain) = get_str(account, "domain") {
        headers.insert("X-Domain".to_string(), domain);
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn account_meta_strips_tokens() {
        let acc = json!({
            "id": "a1",
            "uid": "u1",
            "email": "x@y.z",
            "nickname": "小明",
            "enterpriseName": "某公司",
            "access_token": "SECRET_ACCESS",
            "refresh_token": "SECRET_REFRESH",
            "expiresAt": 123456,
            "needs_relogin": true,
            "needs_relogin_reason": "刷新失败",
        });
        let meta = account_meta(&acc);
        assert_eq!(meta["id"], "a1");
        assert_eq!(meta["needsRelogin"], true);
        assert_eq!(meta["needsReloginReason"], "刷新失败");
        assert!(meta.get("access_token").is_none(), "不得泄露 token");
        assert!(meta.get("refresh_token").is_none(), "不得泄露 token");
    }

    #[test]
    fn account_display_name_priority() {
        assert_eq!(account_display_name(&json!({"email": "a@b.c", "nickname": "n"})), "a@b.c");
        assert_eq!(account_display_name(&json!({"nickname": "n", "uid": "u"})), "n");
        assert_eq!(account_display_name(&json!({"uid": "u"})), "u");
        assert_eq!(account_display_name(&json!({})), "unknown");
    }

    #[test]
    fn get_str_trims_and_filters_empty() {
        assert_eq!(get_str(&json!({"k": "  v  "}), "k"), Some("v".to_string()));
        assert_eq!(get_str(&json!({"k": "  "}), "k"), None);
        assert_eq!(get_str(&json!({"k": 123}), "k"), None);
    }
}


/// 删除账号（按 id）。
pub fn delete_account(account_id: &str) -> Result<(), String> {
    let mut accounts = load_accounts();
    let before = accounts.len();
    accounts.retain(|a| a.get("id").and_then(|v| v.as_str()) != Some(account_id));
    if accounts.len() == before {
        return Err("账号不存在".to_string());
    }
    save_accounts(&accounts).map_err(|e| e.to_string())
}

/// 导入本机当前账号（从认证文件读取）。
pub fn import_local() -> Result<Value, String> {
    let acc = crate::modules::auth_file::import_from_auth_file()
        .ok_or("未读取到本地 WorkBuddy 登录信息")?;
    let acc_uid = get_str(&acc, "uid");
    let acc_email = get_str(&acc, "email").unwrap_or_default();
    let mut accounts = load_accounts();
    accounts.retain(|a| {
        let a_uid = get_str(a, "uid");
        let a_email = get_str(a, "email").unwrap_or_default();
        !(acc_uid.is_some() && a_uid.is_some() && a_uid == acc_uid) && a_email != acc_email
    });
    accounts.push(acc.clone());
    save_accounts(&accounts).map_err(|e| e.to_string())?;
    Ok(account_meta(&acc))
}

/// 手动添加账号（token 方式）。
pub fn manual_add(
    access_token: &str,
    uid: Option<String>,
    nickname: Option<String>,
    email: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    domain: Option<String>,
    expires_at: Option<i64>,
    refresh_expires_at: Option<i64>,
) -> Result<Value, String> {
    if access_token.trim().is_empty() {
        return Err("缺少 accessToken".to_string());
    }
    let acc = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "uid": uid,
        "nickname": nickname,
        "email": email.unwrap_or_else(|| "手动添加".to_string()),
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": token_type.unwrap_or_else(|| "Bearer".to_string()),
        "domain": domain,
        "expiresAt": expires_at,
        "refreshExpiresAt": refresh_expires_at,
        "createdAt": crate::modules::config::now_ms(),
    });
    let mut accounts = load_accounts();
    accounts.push(acc.clone());
    save_accounts(&accounts).map_err(|e| e.to_string())?;
    Ok(account_meta(&acc))
}
