//! 账号存储：读取/写入 `~/.wb-switch/accounts.json`，与 Python 版共享数据目录。
//!
//! 对照 server.py `load_accounts` / `save_accounts` / `find_account` /
//! `account_display_name` / `account_meta`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::modules::config::{accounts_file, atomic_write, now_ms};
use crate::modules::variant::WbVariant;

/// 判断字段是否为 WorkBuddy 5.6 加密信封对象（`{$wbEncrypted, envelope}`）。
fn is_envelope(v: &Value, key: &str) -> bool {
    matches!(v.get(key), Some(Value::Object(map)) if map.contains_key("$wbEncrypted"))
}

/// 是否持有未过期的明文 access_token（OAuth 扫码所得形态）。
/// 无 expiresAt 时视为有效（保守：不因缺字段丢弃明文凭据）。
fn has_unexpired_plain_token(acc: &Value) -> bool {
    let Some(Value::String(s)) = acc.get("access_token") else {
        return false;
    };
    if s.trim().is_empty() {
        return false;
    }
    match acc.get("expiresAt").and_then(|v| v.as_i64()) {
        Some(exp) => exp > now_ms(),
        None => true,
    }
}

fn load_accounts_from_path(path: &Path) -> Vec<Value> {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(Value::Array(accounts)) = serde_json::from_str::<Value>(&text) {
            return accounts;
        }
    }
    vec![]
}

fn save_accounts_to_path(path: &Path, accounts: &[Value]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(accounts).unwrap_or_default();
    atomic_write(path, &content)
}

fn find_account_in(accounts: &[Value], account_id: &str) -> Option<Value> {
    accounts
        .iter()
        .find(|account| {
            account.get("id").and_then(Value::as_str) == Some(account_id)
                || account.get("uid").and_then(Value::as_str) == Some(account_id)
        })
        .cloned()
}

fn delete_account_from_path(path: &Path, account_id: &str) -> Result<(), String> {
    let mut accounts = load_accounts_from_path(path);
    let before = accounts.len();
    accounts.retain(|account| account.get("id").and_then(Value::as_str) != Some(account_id));
    if accounts.len() == before {
        return Err("账号不存在".to_string());
    }
    save_accounts_to_path(path, &accounts).map_err(|error| error.to_string())
}

/// 读取账号库；文件缺失或损坏返回空列表。
pub fn load_accounts() -> Vec<Value> {
    load_accounts_from_path(&accounts_file())
}

/// 指定工具存储根下的账号库路径（会话操作等注入路径的场景用）。
pub fn accounts_file_in(store_root: &Path) -> std::path::PathBuf {
    store_root.join("accounts.json")
}

/// 读取指定账号库文件；文件缺失或损坏返回空列表。
pub fn load_accounts_at(path: &Path) -> Vec<Value> {
    load_accounts_from_path(path)
}

/// 写回账号库（原子写），保持原 JSON 数组结构。
pub fn save_accounts(accounts: &[Value]) -> std::io::Result<()> {
    save_accounts_to_path(&accounts_file(), accounts)
}

/// 按 id 或 uid 查找账号。
pub fn find_account(account_id: &str) -> Option<Value> {
    find_account_in(&load_accounts(), account_id)
}

/// 账号展示名（email → nickname → uid → unknown）。
pub fn account_display_name(acc: &Value) -> String {
    get_str(acc, "email")
        .or_else(|| get_str(acc, "nickname"))
        .or_else(|| get_str(acc, "uid"))
        .unwrap_or_else(|| "unknown".to_string())
}

/// 账号的展示元数据（不泄露 token）。对照 server.py `account_meta`。
/// 展示字段一律走 `display_value`：WorkBuddy 5.6 起 nickname/phoneNumber 可能是
/// 加密信封对象，裸透传会导致前端 React error #31（白屏）。
pub fn account_meta(acc: &Value) -> Value {
    json!({
        "id": display_value(acc, "id"),
        "uid": display_value(acc, "uid"),
        "email": display_value(acc, "email"),
        "nickname": display_value(acc, "nickname"),
        "enterpriseName": display_value(acc, "enterpriseName"),
        "expiresAt": display_value(acc, "expiresAt"),
        "refreshExpiresAt": display_value(acc, "refreshExpiresAt"),
        "refreshedAt": display_value(acc, "refreshedAt"),
        "createdAt": display_value(acc, "createdAt"),
        "needsRelogin": acc.get("needs_relogin").and_then(|v| v.as_bool()) == Some(true),
        "needsReloginReason": display_value(acc, "needs_relogin_reason"),
        // 档位随元数据下发，供宿主按档位过滤列表（缺省国内版，历史数据零迁移）。
        "variant": variant_of(acc).as_str(),
    })
}

/// 取非空字符串字段；空/缺失返回 None。
pub fn get_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 展示型字段安全读取：标量原样返回；对象/数组（如 WorkBuddy 5.6 引入的
/// `{$wbEncrypted, envelope}` 加密信封）折叠为 Null，避免对象漏进前端
/// 被当作 React 子节点渲染导致整树卸载（白屏）。
pub fn display_value(acc: &Value, key: &str) -> Value {
    match acc.get(key) {
        Some(v @ (Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null)) => v.clone(),
        _ => Value::Null,
    }
}

/// 字段值读取：接受明文字符串或 WorkBuddy 5.6 加密信封对象，其他类型返回 None。
/// 信封在本机同一 keyblob 下可由 WorkBuddy 自行解密，导入与切换写回时需原样保留。
/// 空白字符串按「没有值」处理（与 `get_str` 的空值语义一致）：否则 auth 文件里的
/// `"accessToken": ""` 会被判为已有 token，导入一条空凭据账号。
pub fn secret_value(v: &Value, key: &str) -> Option<Value> {
    match v.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(Value::String(s.clone())),
        Some(o @ Value::Object(map)) if map.contains_key("$wbEncrypted") => Some(o.clone()),
        _ => None,
    }
}

/// 账号档位：显式 `variant` 字段优先，缺失时按 `domain` 后缀兜底，缺省国内版。
pub fn variant_of(acc: &Value) -> WbVariant {
    WbVariant::from_account(acc)
}

/// 合并采集结果时保留已有档位：已入库的档位不得因再次采集而丢失。
/// 采集结果自带档位时以它为准（记录里的凭据来自该档位）。
fn inherit_existing_variant(existing: &Value, collected: &mut Value) {
    if get_str(collected, "variant").is_none() {
        if let Some(variant) = get_str(existing, "variant") {
            collected["variant"] = Value::String(variant);
        }
    }
}

/// 返回可用于 UID 缺失场景的真实邮箱。历史展示占位值不参与身份匹配。
fn identity_email(account: &Value) -> Option<String> {
    let email = get_str(account, "email")?;
    if !email.contains('@')
        || email.eq_ignore_ascii_case("unknown")
        || email == "手动添加"
        || get_str(account, "nickname").as_deref() == Some(email.as_str())
        || get_str(account, "uid").as_deref() == Some(email.as_str())
    {
        return None;
    }
    Some(email.to_ascii_lowercase())
}

/// 按稳定身份将采集结果合并到账号列表，并返回最终持久化的账号。
///
/// 非空 UID 始终优先；仅当新账号没有 UID 时，才使用真实邮箱兜底。
/// 命中已有身份时保留本地 id，避免调用方持有的账号引用失效。
pub fn upsert_collected_account(accounts: &mut Vec<Value>, mut collected: Value) -> Value {
    let collected_uid = get_str(&collected, "uid");
    let collected_email = identity_email(&collected);
    let matches_identity = |existing: &Value| {
        if let Some(uid) = collected_uid.as_deref() {
            return get_str(existing, "uid").as_deref() == Some(uid);
        }
        collected_email
            .as_deref()
            .is_some_and(|email| identity_email(existing).as_deref() == Some(email))
    };

    let matching_indexes: Vec<usize> = accounts
        .iter()
        .enumerate()
        .filter_map(|(index, existing)| matches_identity(existing).then_some(index))
        .collect();

    if let Some(&first_index) = matching_indexes.first() {
        let existing = &accounts[first_index];

        // WorkBuddy 5.6 加密态保护：本机重导入得到的是加密信封 token；若已有
        // 记录仍持有未过期的明文 token（OAuth 扫码所得），不得让信封覆盖明文
        // —— 否则 UI 每次自动 importLocal 都会把扫码凭据冲掉，签到/积分等
        // 需要明文 token 的功能随之失效。明文过期后才放行信封接管。
        if is_envelope(&collected, "access_token") && has_unexpired_plain_token(existing) {
            return existing.clone();
        }
        // 展示字段兜底：新采集为信封时保留已有记录的明文展示值。
        for key in ["nickname", "email", "enterpriseName"] {
            if is_envelope(&collected, key) {
                if let Some(v) = existing.get(key) {
                    collected[key] = v.clone();
                }
            }
        }

        if let Some(existing_id) = existing.get("id").cloned() {
            collected["id"] = existing_id;
        }
        if get_str(&collected, "uid").is_none() {
            if let Some(existing_uid) = existing.get("uid").cloned() {
                collected["uid"] = existing_uid;
            }
        }
        if let Some(created_at) = existing.get("createdAt").cloned() {
            collected["createdAt"] = created_at;
        }
        inherit_existing_variant(existing, &mut collected);

        for index in matching_indexes.into_iter().rev() {
            accounts.remove(index);
        }
        accounts.insert(first_index.min(accounts.len()), collected.clone());
    } else {
        accounts.push(collected.clone());
    }

    collected
}

/// 使用统一身份规则保存采集到的账号。
pub fn save_collected_account(collected: Value) -> std::io::Result<Value> {
    let mut accounts = load_accounts();
    let saved = upsert_collected_account(&mut accounts, collected);
    save_accounts(&accounts)?;
    Ok(saved)
}

/// 按 id 覆盖写入账号库（不存在则追加）。对照 server.py `_upsert_account`。
pub fn upsert_account(updated: &Value) -> std::io::Result<()> {
    let mut accounts = load_accounts();
    upsert_account_in(&mut accounts, updated);
    save_accounts(&accounts)
}

/// 覆盖写入的内存实现：命中已有 id 时保留其档位，避免覆盖写入丢字段。
fn upsert_account_in(accounts: &mut Vec<Value>, updated: &Value) {
    let id = updated.get("id").and_then(|v| v.as_str()).unwrap_or("");
    for a in accounts.iter_mut() {
        if a.get("id").and_then(|v| v.as_str()) == Some(id) {
            let mut next = updated.clone();
            inherit_existing_variant(a, &mut next);
            *a = next;
            return;
        }
    }
    accounts.push(updated.clone());
}

/// WorkBuddy 5.6 加密信封凭据的可读错误：`access_token` 为信封形态时返回提示文案。
///
/// 信封 token 无法解出明文，不能用于签到 / 积分 / 旅行等 API 请求；此前会经
/// [`build_auth_headers`] 的 `unwrap_or_default()` 兜底成空 `Bearer`，被网关
/// 401 后再把 HTML 错误页原样回显到界面（issue #94）。需要账号身份的请求
/// 发出前应先用本函数短路。
pub fn envelope_token_error(account: &Value) -> Option<String> {
    if is_envelope(account, "access_token") {
        return Some(
            "该账号凭据为 WorkBuddy 加密信封态，无法直接调用签到 / 积分 / Token 统计等接口；\
             切换功能不受影响，如需上述功能请删除该账号后改用「OAuth 扫码添加」获取明文凭据。"
                .to_string(),
        );
    }
    None
}

/// 构造与官方对齐的请求头。对照 server.py `build_auth_headers`。
///
/// 注意：`access_token` 为加密信封对象时 `get_str` 取不到值，这里会产出空
/// `Bearer`——调用方必须先用 [`envelope_token_error`] 拦截，不要把空凭据
/// 真的发出去（issue #94）。
pub fn build_auth_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!(
            "Bearer {}",
            get_str(account, "access_token").unwrap_or_default()
        ),
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

    /// 回归 issue #94：信封凭据要能被识别并给出可读错误，明文/缺字段不误报。
    #[test]
    fn envelope_token_error_only_fires_on_envelope_access_token() {
        let envelope = json!({
            "id": "a1",
            "access_token": {"$wbEncrypted": true, "envelope": "…"},
            "refresh_token": {"$wbEncrypted": true, "envelope": "…"},
        });
        let err = envelope_token_error(&envelope).expect("信封 access_token 应返回错误");
        assert!(err.contains("信封"), "错误文案应可读：{err}");
        assert!(err.contains("OAuth"), "应给出扫码重新添加的指引：{err}");

        let plain = json!({"id": "a2", "access_token": "SECRET", "refresh_token": "R"});
        assert!(envelope_token_error(&plain).is_none(), "明文凭据不应报错");

        let legacy = json!({"id": "a3"});
        assert!(
            envelope_token_error(&legacy).is_none(),
            "缺 access_token 的历史账号不在此拦截（保持既有行为）"
        );
    }

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
    fn account_meta_carries_variant_for_filtering() {
        // 无字段的历史账号按国内版解释，前端仍能按 variant 过滤。
        assert_eq!(account_meta(&json!({"id": "a1"}))["variant"], "cn");
        assert_eq!(
            account_meta(&json!({"id": "a2", "variant": "ai"}))["variant"],
            "ai"
        );
        // 域名为空的账号同样落回国内版（domain 兜底见 variant 单测）。
        assert_eq!(
            account_meta(&json!({"id": "a3", "domain": "", "variant": ""}))["variant"],
            "cn"
        );
    }

    #[test]
    fn account_display_name_priority() {
        assert_eq!(
            account_display_name(&json!({"email": "a@b.c", "nickname": "n"})),
            "a@b.c"
        );
        assert_eq!(
            account_display_name(&json!({"nickname": "n", "uid": "u"})),
            "n"
        );
        assert_eq!(account_display_name(&json!({"uid": "u"})), "u");
        assert_eq!(account_display_name(&json!({})), "unknown");
    }

    #[test]
    fn get_str_trims_and_filters_empty() {
        assert_eq!(get_str(&json!({"k": "  v  "}), "k"), Some("v".to_string()));
        assert_eq!(get_str(&json!({"k": "  "}), "k"), None);
        assert_eq!(get_str(&json!({"k": 123}), "k"), None);
    }

    fn account(id: &str, uid: Option<&str>, nickname: &str, email: Option<&str>) -> Value {
        json!({
            "id": id,
            "uid": uid,
            "nickname": nickname,
            "email": email,
            "access_token": format!("token-{id}"),
            "createdAt": 1,
        })
    }

    #[test]
    fn same_nickname_with_different_uids_is_retained() {
        let mut accounts = vec![account("old", Some("uid-1"), "同名", Some("同名"))];
        let saved =
            upsert_collected_account(&mut accounts, account("new", Some("uid-2"), "同名", None));

        assert_eq!(accounts.len(), 2);
        assert_eq!(saved["id"], "new");
    }

    #[test]
    fn same_uid_refresh_preserves_local_id_and_removes_duplicates() {
        let mut accounts = vec![
            account("stable", Some("uid-1"), "旧名称", Some("old@example.com")),
            account("duplicate", Some("uid-1"), "重复记录", None),
        ];
        let saved = upsert_collected_account(
            &mut accounts,
            account("generated", Some("uid-1"), "新名称", None),
        );

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["id"], "stable");
        assert_eq!(saved["nickname"], "新名称");
        assert_eq!(saved["access_token"], "token-generated");
    }

    #[test]
    fn different_uids_with_same_real_email_are_retained() {
        let mut accounts = vec![account(
            "old",
            Some("uid-1"),
            "账号一",
            Some("shared@example.com"),
        )];
        upsert_collected_account(
            &mut accounts,
            account("new", Some("uid-2"), "账号二", Some("shared@example.com")),
        );

        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn real_email_is_fallback_only_when_collected_uid_is_missing() {
        let mut accounts = vec![account("stable", None, "旧名称", Some("user@example.com"))];
        let saved = upsert_collected_account(
            &mut accounts,
            account("generated", None, "新名称", Some("USER@example.com")),
        );

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["id"], "stable");
        assert_eq!(saved["nickname"], "新名称");
    }

    #[test]
    fn legacy_synthetic_email_does_not_merge_accounts() {
        let mut accounts = vec![account("old", None, "同名", Some("同名"))];
        upsert_collected_account(&mut accounts, account("new", None, "同名", Some("同名")));

        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn variant_of_defaults_to_cn_and_reads_domain_fallback() {
        assert_eq!(variant_of(&json!({"uid": "u-1"})), WbVariant::Cn);
        assert_eq!(
            variant_of(&json!({"uid": "u-1", "variant": "ai"})),
            WbVariant::Ai
        );
        assert_eq!(
            variant_of(&json!({"uid": "u-1", "domain": "www.workbuddy.ai"})),
            WbVariant::Ai
        );
    }

    #[test]
    fn upsert_keeps_variant_of_recollected_account() {
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "variant": "ai",
            "access_token": "old-token",
        })];
        // 不含档位字段的再次采集（如刷新）不得丢掉已入库档位
        let saved =
            upsert_collected_account(&mut accounts, account("a-1", Some("uid-1"), "新名称", None));

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["variant"], "ai");
        assert_eq!(accounts[0]["variant"], "ai");
    }

    #[test]
    fn envelope_reimport_keeps_unexpired_plain_oauth_token() {
        let envelope = json!({"$wbEncrypted": 1, "envelope": "enc"});
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "nickname": "明文昵称",
            "access_token": "plain-token",
            "refresh_token": "plain-refresh",
            "expiresAt": crate::modules::config::now_ms() + 86_400_000_i64,
        })];
        // UI 自动 importLocal 会拿本机加密态重采集同一 uid：
        // 不得让信封覆盖仍未过期的明文 token（否则签到/积分失效）。
        let collected = json!({
            "uid": "uid-1",
            "nickname": envelope,
            "access_token": {"$wbEncrypted": 1, "envelope": "a"},
            "refresh_token": {"$wbEncrypted": 1, "envelope": "r"},
        });
        let saved = upsert_collected_account(&mut accounts, collected);

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["access_token"], "plain-token");
        assert_eq!(saved["refresh_token"], "plain-refresh");
        assert_eq!(saved["nickname"], "明文昵称");
    }

    #[test]
    fn envelope_reimport_takes_over_after_plain_token_expired() {
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "access_token": "stale-plain",
            "expiresAt": crate::modules::config::now_ms() - 1_000_i64,
        })];
        let collected = json!({
            "uid": "uid-1",
            "access_token": {"$wbEncrypted": 1, "envelope": "a"},
        });
        let saved = upsert_collected_account(&mut accounts, collected);

        assert_eq!(accounts.len(), 1);
        // 明文已过期：信封接管（切换仍可用，由 WorkBuddy 自解）。
        assert!(saved.get("access_token").and_then(|v| v.as_str()).is_none());
    }

    #[test]
    fn fresh_plain_oauth_token_replaces_envelope_record() {
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "access_token": {"$wbEncrypted": 1, "envelope": "old"},
        })];
        // 重新扫码得到新明文：应正常替换。
        let collected = json!({
            "uid": "uid-1",
            "access_token": "fresh-plain",
            "expiresAt": crate::modules::config::now_ms() + 86_400_000_i64,
        });
        let saved = upsert_collected_account(&mut accounts, collected);

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["access_token"], "fresh-plain");
    }

    #[test]
    fn upsert_accepts_caller_supplied_variant_for_new_account() {
        let mut accounts = vec![];
        let saved = upsert_collected_account(
            &mut accounts,
            json!({"id": "a-ai", "uid": "uid-ai", "variant": "ai", "access_token": "t"}),
        );

        assert_eq!(saved["variant"], "ai");
        assert_eq!(variant_of(&accounts[0]), WbVariant::Ai);
    }

    #[test]
    fn upsert_account_keeps_existing_variant_when_updated_lacks_it() {
        let mut accounts =
            vec![json!({"id": "a-1", "uid": "uid-1", "variant": "ai", "access_token": "old"})];

        let mut refreshed = accounts[0].clone();
        refreshed["access_token"] = json!("new");
        refreshed.as_object_mut().unwrap().remove("variant");
        upsert_account_in(&mut accounts, &refreshed);

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["access_token"], "new");
        assert_eq!(accounts[0]["variant"], "ai", "覆盖写入不得丢档位");

        // 追加新账号时按调用方给定的档位入库
        upsert_account_in(
            &mut accounts,
            &json!({"id": "a-2", "uid": "uid-2", "variant": "cn", "access_token": "t2"}),
        );
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1]["variant"], "cn");
    }

    #[test]
    fn persisted_same_name_accounts_can_be_found_and_deleted_independently() {
        let test_dir = std::env::temp_dir().join(format!(
            "wb-switch-same-name-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = test_dir.join("accounts.json");
        let mut accounts = vec![];
        upsert_collected_account(
            &mut accounts,
            account("account-1", Some("uid-1"), "同名用户", None),
        );
        upsert_collected_account(
            &mut accounts,
            account("account-2", Some("uid-2"), "同名用户", None),
        );
        save_accounts_to_path(&path, &accounts).expect("same-name accounts should persist");

        let persisted = load_accounts_from_path(&path);
        assert_eq!(
            find_account_in(&persisted, "account-1").unwrap()["uid"],
            "uid-1"
        );
        assert_eq!(
            find_account_in(&persisted, "account-2").unwrap()["uid"],
            "uid-2"
        );

        delete_account_from_path(&path, "account-1").expect("first account should delete");
        let after_first_delete = load_accounts_from_path(&path);
        assert!(find_account_in(&after_first_delete, "account-1").is_none());
        assert_eq!(
            find_account_in(&after_first_delete, "account-2").unwrap()["uid"],
            "uid-2"
        );

        delete_account_from_path(&path, "account-2").expect("second account should delete");
        assert!(load_accounts_from_path(&path).is_empty());
        std::fs::remove_dir_all(&test_dir).expect("temporary account store should clean up");
    }
}

/// 删除账号（按 id）。
pub fn delete_account(account_id: &str) -> Result<(), String> {
    delete_account_from_path(&accounts_file(), account_id)
}

/// 导入本机当前账号（从该档位的登录态文件读取）。
pub fn import_local(variant: WbVariant) -> Result<Value, String> {
    let acc = crate::modules::auth_file::import_from_auth_file(variant)
        .ok_or("未读取到本地 WorkBuddy 登录信息")?;
    let saved = save_collected_account(acc).map_err(|e| e.to_string())?;
    Ok(account_meta(&saved))
}

// 手动添加账号（token 方式）已随 UI 入口「手动添加」一并下线；
// `identity_email` 中的 "手动添加" 占位过滤保留，用于兼容历史手动添加的旧账号。
