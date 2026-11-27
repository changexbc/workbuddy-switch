//! Token 刷新与保活。
//!
//! 对照 server.py `refresh_account_token` / `ensure_fresh_token` /
//! `run_keepalive_cycle`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{LazyLock, Mutex};
use tokio::sync::watch;

use crate::modules::account::{self, build_auth_headers, AccountChange};
use crate::modules::config::{
    accounts_file, http_request_raw, load_checkin_config, norm_ts, now_ms, RunFlagGuard,
};

static KEEPALIVE_RUNNING: AtomicBool = AtomicBool::new(false);

type RefreshKey = (PathBuf, AccountIdentity);
type RefreshResult = Result<Value, String>;
type FlightReceiver = watch::Receiver<Option<RefreshResult>>;

static REFRESH_IN_FLIGHT: LazyLock<Mutex<HashMap<RefreshKey, FlightReceiver>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct RefreshFlight {
    key: RefreshKey,
    sender: watch::Sender<Option<RefreshResult>>,
}

impl RefreshFlight {
    fn publish(&self, result: RefreshResult) {
        // 发布完成后再由 Drop 移除条目，期间加入的调用仍可复用结果。
        let _ = self.sender.send(Some(result));
    }
}

impl Drop for RefreshFlight {
    fn drop(&mut self) {
        REFRESH_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// 刷新接口 URL：域名与路径前缀都按账号自身档位选（缺档位字段 → 国内版，零回归）。
fn refresh_url(account: &Value) -> String {
    let variant = crate::modules::account::variant_of(account);
    format!(
        "{}{}/auth/token/refresh",
        variant.api_endpoint(),
        variant.api_prefix()
    )
}

/// 构造刷新请求头：鉴权头 + 刷新会话头。
///
/// `X-Auth-Refresh-Source: plugin` 必须保留：两个档位的官方客户端刷新时都发送该头，
/// 缺失时网关会把这次刷新判定成另一个 client 来源，国际版实测返回
/// `invalid_grant: Invalid refresh token`（code 12153）。抽成纯函数是为了让单测
/// 能把这个头固定住，避免将来被顺手删掉。
fn refresh_headers(account: &Value, refresh_token: &str) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("X-Refresh-Token".to_string(), refresh_token.to_string());
    headers.insert("X-Auth-Refresh-Source".to_string(), "plugin".to_string());
    headers
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum AccountIdentity {
    Id(String),
    LegacyUid(String),
}

impl AccountIdentity {
    fn of(account: &Value) -> Option<Self> {
        account::get_str(account, "id")
            .map(Self::Id)
            .or_else(|| account::get_str(account, "uid").map(Self::LegacyUid))
    }

    fn matches(&self, account: &Value) -> bool {
        match self {
            Self::Id(id) => account::get_str(account, "id").as_ref() == Some(id),
            Self::LegacyUid(uid) => account::get_str(account, "uid").as_ref() == Some(uid),
        }
    }
}

/// 凭据被重新登录、重新导入或另一进程刷新过时，旧请求的结果已经失效。
fn same_credentials(previous: &Value, latest: &Value) -> bool {
    ["uid", "access_token", "refresh_token", "refreshedAt"]
        .into_iter()
        .all(|key| previous.get(key) == latest.get(key))
        && account::variant_of(previous) == account::variant_of(latest)
}

enum RefreshUpdate {
    Tokens(Value),
    Rejected(String),
}

fn response_token<'a>(data: &'a Value, camel: &str, snake: &str) -> Option<&'a str> {
    [camel, snake]
        .into_iter()
        .filter_map(|key| data.get(key).and_then(Value::as_str))
        .find(|token| !token.trim().is_empty())
}

fn plain_refresh_token(account: &Value) -> Option<&str> {
    account
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
}

fn parse_refresh_response(status: u16, response: Value) -> Result<RefreshUpdate, String> {
    if status == 0 {
        return Err("刷新请求失败，请检查网络后重试".to_string());
    }
    // JSON 业务码可能与 HTTP 状态不同，先排除传输失败、限流和服务端错误。
    let success_status = (200..300).contains(&status);
    if !success_status && !matches!(status, 400 | 401) {
        return Err(format!("刷新请求失败 (HTTP {status})"));
    }

    let code = response.get("code").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    let rejected = status == 401
        || matches!(code, Some(401 | 12153))
        || ["error", "error_description", "message", "msg"]
            .into_iter()
            .filter_map(|key| response.get(key).and_then(Value::as_str))
            .any(|text| {
                let text = text.to_ascii_lowercase();
                text.contains("invalid_grant") || text.contains("invalid refresh token")
            });
    if rejected {
        let code = code.unwrap_or(i64::from(status));
        return Ok(RefreshUpdate::Rejected(format!(
            "刷新凭据已被服务端拒绝 (code={code})，请重新登录"
        )));
    }
    if success_status && matches!(code, Some(0 | 200)) {
        if let Some(data) = response.get("data") {
            if response_token(data, "accessToken", "access_token").is_some() {
                return Ok(RefreshUpdate::Tokens(data.clone()));
            }
        }
        return Err("刷新响应缺少 accessToken".to_string());
    }
    Err(format!(
        "刷新请求失败 (code={})",
        code.unwrap_or(i64::from(status))
    ))
}

fn apply_refresh_tokens(account: &mut Value, data: &Value) {
    for (key, camel, snake) in [
        ("access_token", "accessToken", "access_token"),
        ("refresh_token", "refreshToken", "refresh_token"),
        ("token_type", "tokenType", "token_type"),
    ] {
        if let Some(value) = response_token(data, camel, snake) {
            account[key] = json!(value);
        }
    }
    let now = now_ms();
    let relative_expiry = |key| {
        data.get(key)
            .and_then(Value::as_i64)
            .map(|seconds| now.saturating_add(seconds.saturating_mul(1000)))
    };
    let access_expiry = norm_ts(data.get("expiresAt").or_else(|| data.get("expires_at")))
        .or_else(|| relative_expiry("expiresIn"));
    if let Some(expiry) = access_expiry {
        account["expiresAt"] = json!(expiry);
    }
    let refresh_expiry = norm_ts(
        data.get("refreshExpiresAt")
            .or_else(|| data.get("refresh_expires_at")),
    )
    .or_else(|| relative_expiry("refreshExpiresIn"))
    .or_else(|| norm_ts(account.get("refreshExpiresAt")).filter(|&expiry| expiry > 0))
    .or_else(|| {
        let raw = account.get("auth_raw")?;
        norm_ts(raw.get("refreshExpiresAt"))
            .filter(|&expiry| expiry > 0)
            .or_else(|| norm_ts(raw.get("auth")?.get("refreshExpiresAt")))
    });
    if let Some(expiry) = refresh_expiry {
        account["refreshExpiresAt"] = json!(expiry);
        // 切换账号会从 auth_raw 取过期时间，导入的认证文件也可能把 auth 包在里面。
        let raw = &mut account["auth_raw"];
        if !raw.is_object() {
            *raw = json!({});
        }
        raw["refreshExpiresAt"] = json!(expiry);
        if let Some(auth) = raw.get_mut("auth").and_then(Value::as_object_mut) {
            auth.insert("refreshExpiresAt".to_string(), json!(expiry));
        }
    }
    account["refreshedAt"] = json!(now);
    let map = account.as_object_mut().unwrap();
    map.remove("needs_relogin");
    map.remove("needs_relogin_reason");
}

async fn request_refresh(url: String, headers: HashMap<String, String>) -> (u16, Value) {
    let (status, _, body) =
        http_request_raw(&url, "POST", Some(json!({})), Some(&headers), None, true).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn refresh_stored_account<Fut: Future<Output = (u16, Value)>>(
    path: PathBuf,
    identity: AccountIdentity,
    snapshot: Value,
    request: impl FnOnce(String, HashMap<String, String>) -> Fut,
) -> Result<Value, String> {
    let read_path = path.clone();
    let read_identity = identity.clone();
    let account = tokio::task::spawn_blocking(move || {
        account::load_accounts_from_path(&read_path).map(|accounts| {
            accounts
                .into_iter()
                .find(|account| read_identity.matches(account))
        })
    })
    .await
    .map_err(|e| format!("读取账号库失败：{e}"))?
    .map_err(|e| format!("读取账号库失败：{e}"))?
    .ok_or("账号不存在")?;
    if !same_credentials(&snapshot, &account) {
        return Ok(account);
    }

    let refresh_token = plain_refresh_token(&account).ok_or_else(|| {
        if account::is_envelope(&account, "refresh_token") {
            "加密的 refresh token 无法用于刷新，请重新登录"
        } else {
            "缺少 refresh token，无法刷新"
        }
    })?;
    let (status, response) = request(
        refresh_url(&account),
        refresh_headers(&account, refresh_token),
    )
    .await;
    let update = parse_refresh_response(status, response)?;
    let refreshed = matches!(update, RefreshUpdate::Tokens(_));

    tokio::task::spawn_blocking(move || {
        let (latest, written) = account::mutate_at(&path, |accounts| {
            let latest = accounts
                .iter_mut()
                .find(|account| identity.matches(account))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "账号已删除"))?;
            if !same_credentials(&account, latest) {
                return Ok(AccountChange::Unchanged((latest.clone(), false)));
            }
            match update {
                RefreshUpdate::Tokens(data) => {
                    apply_refresh_tokens(latest, &data);
                    if account::get_str(latest, "id").is_none() {
                        latest["id"] = json!(uuid::Uuid::new_v4().to_string());
                    }
                }
                RefreshUpdate::Rejected(reason) => {
                    latest["needs_relogin"] = json!(true);
                    latest["needs_relogin_reason"] = json!(reason);
                }
            }
            Ok(AccountChange::Write((latest.clone(), true)))
        })
        .map_err(|e| format!("保存刷新结果失败：{e}"))?;

        // Windows CLI 的 settings 同步在账号库提交后执行。失败由状态接口提示。
        if cfg!(windows) && refreshed && written && path == accounts_file() {
            let previous = account.get("access_token").and_then(Value::as_str);
            let _ = crate::modules::codebuddy_cli::sync_windows_env_for_account(&latest, previous);
        }
        Ok(latest)
    })
    .await
    .map_err(|e| format!("保存刷新结果失败：{e}"))?
}

async fn refresh_account_token_at<Fut>(
    path: PathBuf,
    snapshot: Value,
    request: impl FnOnce(String, HashMap<String, String>) -> Fut + Send + 'static,
) -> RefreshResult
where
    Fut: Future<Output = (u16, Value)> + Send,
{
    let identity = AccountIdentity::of(&snapshot).ok_or("账号缺少 id 和 uid")?;
    let key = (path.clone(), identity.clone());
    let (mut receiver, leader) = {
        let mut in_flight = REFRESH_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(receiver) = in_flight.get(&key) {
            (receiver.clone(), None)
        } else {
            let (sender, receiver) = watch::channel(None);
            in_flight.insert(key.clone(), receiver.clone());
            (receiver, Some(RefreshFlight { key, sender }))
        }
    };

    if let Some(flight) = leader {
        // 刷新由独立任务收尾，最先发起调用的一方取消时，其他等待者仍能收到结果。
        tokio::spawn(async move {
            let result = refresh_stored_account(path, identity, snapshot, request).await;
            flight.publish(result);
        });
    }
    let result = receiver
        .wait_for(Option::is_some)
        .await
        .map_err(|_| "刷新任务已中断".to_string())?;
    result.clone().expect("wait_for 仅在刷新结果已发布时返回")
}

/// 手动刷新和保活需要回报请求或写入错误；明确拒绝的凭据通过账号重登标记反馈。
pub async fn try_refresh_account_token(snapshot: Value) -> RefreshResult {
    refresh_account_token_at(accounts_file(), snapshot, request_refresh).await
}

/// 签到、积分等调用在刷新暂时失败时继续使用原有账号。
pub async fn refresh_account_token(account: Value) -> Value {
    try_refresh_account_token(account.clone())
        .await
        .unwrap_or(account)
}

/// 惰性刷新：expiresAt 缺失或剩余 < lazy_refresh_hours 则刷新。返回最新账号。
pub async fn ensure_fresh_token(mut account: Value, cfg: &Value) -> Value {
    let lazy_h = cfg
        .get("lazy_refresh_hours")
        .and_then(|v| v.as_i64())
        .unwrap_or(24);
    let exp = account.get("expiresAt").and_then(|v| v.as_i64());
    let stale = match exp {
        Some(e) => now_ms() >= e || e - now_ms() < lazy_h * 3600 * 1000,
        None => true,
    };
    let has_rt = plain_refresh_token(&account).is_some();
    if stale && has_rt {
        account = refresh_account_token(account).await;
    }
    account
}

fn mark_missing_keepalive_token(path: &Path, snapshot: &Value) -> io::Result<()> {
    let identity =
        AccountIdentity::of(snapshot).ok_or_else(|| io::Error::other("账号缺少 id 和 uid"))?;
    account::mutate_at(path, |accounts| {
        let Some(latest) = accounts
            .iter_mut()
            .find(|account| identity.matches(account))
        else {
            return Ok(AccountChange::Unchanged(()));
        };
        if !same_credentials(snapshot, latest) || plain_refresh_token(latest).is_some() {
            return Ok(AccountChange::Unchanged(()));
        }
        let reason = if account::is_envelope(latest, "refresh_token") {
            "加密的 refresh token 无法用于保活，请重新登录"
        } else {
            "缺少 refresh token，无法保活，需重新登录"
        };
        if latest["needs_relogin"] == true && latest["needs_relogin_reason"] == reason {
            return Ok(AccountChange::Unchanged(()));
        }
        latest["needs_relogin"] = json!(true);
        latest["needs_relogin_reason"] = json!(reason);
        Ok(AccountChange::Write(()))
    })
}

/// 保活检查：每天由后台循环调用一次，默认（keepalive_days <= 0）无条件刷新
/// 全部带 refresh token 的账号；keepalive_days > 0 时仅刷新剩余不足该天数的账号。
///
/// 高频保活是为了避免官方服务端清理闲置的 refresh 会话——曾出现闲置数天后
/// 刷新返回 12153 invalid_grant（Session doesn't have required client）导致
/// 账号被迫重新登录。
pub async fn run_keepalive_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&KEEPALIVE_RUNNING) else {
        return json!({"skipped": "already_running"});
    };
    let cfg = load_checkin_config();
    let keep_days = cfg
        .get("keepalive_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    run_keepalive_cycle_at(accounts_file(), keep_days).await
}

async fn run_keepalive_cycle_at(path: PathBuf, keep_days: i64) -> Value {
    let accounts = account::load_accounts_at(&path);
    let total = accounts.len();
    let mut results: Vec<Value> = Vec::new();
    for acc in accounts {
        let exp = acc.get("expiresAt").and_then(|v| v.as_i64());
        let stale = keep_days <= 0
            || match exp {
                Some(e) => now_ms() >= e || e - now_ms() < keep_days * 24 * 3600 * 1000,
                None => true,
            };
        if !stale {
            continue;
        }
        let email = account::account_display_name(&acc);
        if plain_refresh_token(&acc).is_none() {
            let store = path.clone();
            let result =
                tokio::task::spawn_blocking(move || mark_missing_keepalive_token(&store, &acc))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|result| result.map_err(|e| e.to_string()));
            results.push(match result {
                Ok(()) => json!({"email": email, "status": "missing_rt"}),
                Err(error) => json!({"email": email, "status": "failed", "error": error}),
            });
            continue;
        }
        let fresh = match refresh_account_token_at(path.clone(), acc, request_refresh).await {
            Ok(fresh) => fresh,
            Err(error) => {
                results.push(json!({"email": email, "status": "failed", "error": error}));
                continue;
            }
        };
        let failed = fresh.get("needs_relogin").and_then(|v| v.as_bool()) == Some(true);
        results.push(json!({
            "email": crate::modules::account::account_display_name(&fresh),
            "status": if failed { "failed" } else { "ok" },
            "error": if failed {
                fresh.get("needs_relogin_reason").and_then(|v| v.as_str()).map(|s| s.to_string())
            } else {
                None
            },
        }));
    }
    json!({"checked": total, "refreshed": results})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_account(acc: &Value) -> PathBuf {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = account::accounts_file_in(&dir);
        account::mutate_at(&path, |accounts| {
            accounts.push(acc.clone());
            Ok(AccountChange::Write(()))
        })
        .unwrap();
        path
    }

    fn test_account() -> Value {
        json!({"id": "account", "uid": "user", "access_token": "old-at", "refresh_token": "old-rt"})
    }

    fn successful_response() -> (u16, Value) {
        (
            200,
            json!({"code": 0, "data": {
                "accessToken": "new-at", "refreshToken": "new-rt",
                "expiresAt": 1_900_000_000_000_i64, "refreshExpiresIn": 3600
            }}),
        )
    }

    #[tokio::test]
    async fn legacy_snapshot_finds_account_after_first_refresh_assigns_id() {
        let mut snapshot = test_account();
        snapshot.as_object_mut().unwrap().remove("id");
        let path = store_account(&snapshot);
        let first = refresh_account_token_at(path.clone(), snapshot.clone(), |_, _| async {
            successful_response()
        })
        .await
        .unwrap();
        let id = account::get_str(&first, "id").expect("首次刷新应补齐 id");
        let latest = refresh_account_token_at(path.clone(), snapshot, |_, _| async {
            panic!("旧快照应直接拿到已刷新的凭据");
        })
        .await
        .unwrap();
        assert_eq!(latest, first);
        assert_eq!(latest["access_token"], "new-at");

        let mut snapshot = latest;
        snapshot.as_object_mut().unwrap().remove("id");
        let edit_path = path.clone();
        let latest = refresh_account_token_at(path.clone(), snapshot, move |_, _| async move {
            account::mutate_at(&edit_path, |accounts| {
                accounts[0]["nickname"] = json!("Edited");
                Ok(AccountChange::Write(()))
            })
            .unwrap();
            (200, json!({"code": 0, "data": {"accessToken": "next-at"}}))
        })
        .await
        .unwrap();
        assert_eq!(latest["id"], id);
        assert_eq!(latest["nickname"], "Edited");
        assert_eq!(latest["access_token"], "next-at");
        assert_eq!(
            account::load_accounts_from_path(&path).unwrap(),
            vec![latest]
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn refresh_merges_only_token_fields_after_concurrent_account_edit() {
        let mut snapshot = test_account();
        snapshot["nickname"] = json!("old");
        snapshot["needs_relogin"] = json!(true);
        let path = store_account(&snapshot);
        let edit_path = path.clone();
        let identity = AccountIdentity::of(&snapshot).unwrap();
        let fresh = refresh_stored_account(path.clone(), identity, snapshot, |_, _| async {
            account::mutate_at(&edit_path, |accounts| {
                accounts[0]["nickname"] = json!("new nickname");
                accounts[0]["note"] = json!("new note");
                accounts[0]["auth_raw"] = json!({"value": "new raw", "refreshExpiresAt": 123});
                Ok(AccountChange::Write(()))
            })
            .unwrap();
            successful_response()
        })
        .await
        .unwrap();

        assert_eq!(
            account::load_accounts_from_path(&path).unwrap(),
            vec![fresh.clone()]
        );
        assert_eq!(fresh["nickname"], "new nickname");
        assert_eq!(fresh["note"], "new note");
        assert_eq!(
            fresh["auth_raw"],
            json!({"value": "new raw", "refreshExpiresAt": fresh["refreshExpiresAt"]})
        );
        assert_eq!(fresh["access_token"], "new-at");
        assert_eq!(fresh["refresh_token"], "new-rt");
        assert_eq!(fresh["expiresAt"], 1_900_000_000_000_i64);
        assert_eq!(
            fresh["refreshExpiresAt"].as_i64().unwrap(),
            fresh["refreshedAt"].as_i64().unwrap() + 3_600_000
        );
        assert!(fresh.get("needs_relogin").is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn refresh_expiry_is_written_back_to_client_auth() {
        use crate::modules::auth_file::build_auth_obj;

        let old_expiry = 1_700_000_000_000_i64;
        let new_expiry = 4_000_000_000_000_i64;
        for raw in [
            Value::Null,
            json!({"refreshExpiresAt": old_expiry, "scope": "custom"}),
            json!({"auth": {"refreshExpiresAt": old_expiry, "scope": "custom"}}),
        ] {
            let mut account = test_account();
            account["expiresAt"] = json!(1_900_000_000_000_i64);
            account["refreshExpiresAt"] = json!(old_expiry);
            account["auth_raw"] = raw.clone();
            apply_refresh_tokens(
                &mut account,
                &json!({"accessToken": "new-at", "refreshExpiresAt": new_expiry}),
            );

            assert_eq!(account["refreshExpiresAt"], new_expiry);
            assert_eq!(account["auth_raw"]["refreshExpiresAt"], new_expiry);
            if raw.get("auth").is_some() {
                assert_eq!(account["auth_raw"]["auth"]["refreshExpiresAt"], new_expiry);
            }
            let auth = build_auth_obj(&account);
            assert_eq!(auth["refreshExpiresAt"], new_expiry);
            assert!(auth["refreshExpiresIn"].as_i64().unwrap() > 0);
            if raw.is_object() {
                assert_eq!(auth["scope"], "custom");
            }
        }
    }

    #[test]
    fn refresh_without_expiry_keeps_the_current_account_expiry() {
        let mut account = test_account();
        let expiry = 1_900_000_000_000_i64;
        account["refreshExpiresAt"] = json!(expiry);
        account["auth_raw"] = json!({"refreshExpiresAt": 1_700_000_000_000_i64});
        apply_refresh_tokens(&mut account, &json!({"accessToken": "new-at"}));
        assert_eq!(account["refreshExpiresAt"], expiry);
        assert_eq!(account["auth_raw"]["refreshExpiresAt"], expiry);

        account["refreshExpiresAt"] = json!(0);
        account["auth_raw"] = json!({"refreshExpiresAt": 0, "auth": {"refreshExpiresAt": expiry}});
        apply_refresh_tokens(&mut account, &json!({"accessToken": "newer-at"}));
        assert_eq!(account["refreshExpiresAt"], expiry);
        assert_eq!(account["auth_raw"]["refreshExpiresAt"], expiry);
        assert_eq!(account["auth_raw"]["auth"]["refreshExpiresAt"], expiry);
    }

    #[tokio::test]
    async fn refresh_does_not_restore_a_deleted_account() {
        let snapshot = test_account();
        let identity = AccountIdentity::of(&snapshot).unwrap();
        let path = store_account(&snapshot);
        let delete_path = path.clone();
        let result = refresh_stored_account(path.clone(), identity, snapshot, |_, _| async {
            account::mutate_at(&delete_path, |accounts| {
                accounts.clear();
                Ok(AccountChange::Write(()))
            })
            .unwrap();
            successful_response()
        })
        .await;

        assert!(result.unwrap_err().contains("账号已删除"));
        assert!(account::load_accounts_from_path(&path).unwrap().is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn refresh_failure_with_old_credentials_keeps_new_envelopes() {
        let snapshot = test_account();
        let identity = AccountIdentity::of(&snapshot).unwrap();
        let path = store_account(&snapshot);
        let edit_path = path.clone();
        let envelope = json!({"$wbEncrypted": true, "envelope": "fixture"});
        let fresh = refresh_stored_account(path.clone(), identity, snapshot, |_, _| async {
            account::mutate_at(&edit_path, |accounts| {
                accounts[0]["access_token"] = envelope.clone();
                accounts[0]["refresh_token"] = envelope.clone();
                Ok(AccountChange::Write(()))
            })
            .unwrap();
            (400, json!({"error": "invalid_grant"}))
        })
        .await
        .unwrap();

        assert_eq!(
            account::load_accounts_from_path(&path).unwrap(),
            vec![fresh.clone()]
        );
        assert_eq!(fresh["access_token"], envelope);
        assert_eq!(fresh["refresh_token"], envelope);
        assert!(fresh.get("needs_relogin").is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn refresh_transient_errors_leave_account_bytes_unchanged() {
        let snapshot = test_account();
        let identity = AccountIdentity::of(&snapshot).unwrap();
        let path = store_account(&snapshot);
        let original = std::fs::read(&path).unwrap();
        for response in [
            (0, Value::Null),
            (408, Value::Null),
            (503, json!({"code": 12153})),
            (400, json!({"error": "bad_request"})),
            (200, json!({"code": 0})),
        ] {
            let result = refresh_stored_account(
                path.clone(),
                identity.clone(),
                snapshot.clone(),
                |_, _| async { response },
            )
            .await;
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn refresh_rejection_marks_account_for_relogin() {
        let snapshot = test_account();
        let identity = AccountIdentity::of(&snapshot).unwrap();
        let path = store_account(&snapshot);
        let fresh = refresh_stored_account(path.clone(), identity, snapshot, |_, _| async {
            (400, json!({"code": 12153, "error": "invalid_grant"}))
        })
        .await
        .unwrap();

        assert_eq!(fresh["needs_relogin"], true);
        assert_eq!(fresh["access_token"], "old-at");
        assert_eq!(fresh["refresh_token"], "old-rt");
        assert_eq!(
            account::load_accounts_from_path(&path).unwrap(),
            vec![fresh]
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn keepalive_persists_missing_token_relogin_marker() {
        let snapshot = json!({"uid": "user", "nickname": "Nick", "access_token": "at"});
        let path = store_account(&snapshot);
        let result = run_keepalive_cycle_at(path.clone(), 0).await;
        assert_eq!(result["refreshed"][0]["status"], "missing_rt");
        let mut expected = snapshot;
        expected["needs_relogin"] = json!(true);
        expected["needs_relogin_reason"] = json!("缺少 refresh token，无法保活，需重新登录");
        assert_eq!(
            account::load_accounts_from_path(&path).unwrap(),
            vec![expected]
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_token_marker_preserves_concurrent_edits_and_skips_restored_token() {
        let snapshot = json!({"id": "account", "nickname": "Old"});
        let path = store_account(&snapshot);
        account::mutate_at(&path, |accounts| {
            accounts[0]["nickname"] = json!("Edited");
            Ok(AccountChange::Write(()))
        })
        .unwrap();
        mark_missing_keepalive_token(&path, &snapshot).unwrap();
        let stored = account::load_accounts_from_path(&path).unwrap();
        assert_eq!(stored[0]["nickname"], "Edited");
        assert_eq!(stored[0]["needs_relogin"], true);

        account::mutate_at(&path, |accounts| {
            accounts[0]["refresh_token"] = json!("restored-rt");
            let latest = accounts[0].as_object_mut().unwrap();
            latest.remove("needs_relogin");
            latest.remove("needs_relogin_reason");
            Ok(AccountChange::Write(()))
        })
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        mark_missing_keepalive_token(&path, &snapshot).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn published_result_stays_available_until_flight_is_removed() {
        let key = (
            PathBuf::from(uuid::Uuid::new_v4().to_string()),
            AccountIdentity::Id("account".to_string()),
        );
        let (sender, receiver) = watch::channel(None);
        REFRESH_IN_FLIGHT
            .lock()
            .unwrap()
            .insert(key.clone(), receiver);
        let flight = RefreshFlight {
            key: key.clone(),
            sender,
        };
        let result = Err("temporary failure".to_string());

        flight.publish(result.clone());
        let joined = REFRESH_IN_FLIGHT.lock().unwrap().get(&key).unwrap().clone();
        assert_eq!(*joined.borrow(), Some(result));
        drop(flight);
        assert!(!REFRESH_IN_FLIGHT.lock().unwrap().contains_key(&key));
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_request_and_allow_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let snapshot = test_account();
        let path = store_account(&snapshot);
        let requests = Arc::new(AtomicUsize::new(0));
        let request = {
            let requests = Arc::clone(&requests);
            move |_, _| async move {
                requests.fetch_add(1, Ordering::Relaxed);
                (0, Value::Null)
            }
        };
        let (first, second, third) = tokio::join!(
            refresh_account_token_at(path.clone(), snapshot.clone(), request.clone()),
            refresh_account_token_at(path.clone(), snapshot.clone(), request.clone()),
            refresh_account_token_at(path.clone(), snapshot.clone(), request),
        );
        assert!(first.is_err());
        assert_eq!(first, second);
        assert_eq!(first, third);
        assert_eq!(requests.load(Ordering::Relaxed), 1);

        let retried = refresh_account_token_at(path.clone(), snapshot, |_, _| async {
            successful_response()
        })
        .await
        .unwrap();
        assert_eq!(retried["access_token"], "new-at");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn cancelling_first_caller_preserves_shared_refresh() {
        let snapshot = test_account();
        let path = store_account(&snapshot);
        let (started, pending) = tokio::sync::oneshot::channel();
        let (complete, finish) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(refresh_account_token_at(
            path.clone(),
            snapshot.clone(),
            |_, _| async {
                started.send(()).unwrap();
                finish.await.unwrap();
                successful_response()
            },
        ));
        pending.await.unwrap();
        let unexpected_request = |_, _| async { panic!("等待方不得再次发送刷新请求") };
        let mut second = std::pin::pin!(refresh_account_token_at(
            path.clone(),
            snapshot.clone(),
            unexpected_request,
        ));
        let mut third = std::pin::pin!(refresh_account_token_at(
            path.clone(),
            snapshot,
            unexpected_request,
        ));
        // 先轮询两个等待者，确保取消时两者都已经加入同一次刷新。
        tokio::select! {
            biased;
            _ = &mut second => panic!("刷新完成前第二个调用不应返回"),
            _ = &mut third => panic!("刷新完成前第三个调用不应返回"),
            _ = std::future::ready(()) => {}
        }
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        complete.send(()).unwrap();

        let (second, third) = tokio::join!(second, third);
        let fresh = second.unwrap();
        assert_eq!(third.unwrap(), fresh);
        assert_eq!(fresh["access_token"], "new-at");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn refresh_url_follows_account_variant() {
        // 旧账号（无档位字段）→ 国内版，与改造前逐字一致。
        assert_eq!(
            refresh_url(&json!({"uid": "u-1"})),
            format!(
                "{}/v2/plugin/auth/token/refresh",
                crate::modules::variant::WbVariant::Cn.api_endpoint()
            )
        );
        assert_eq!(
            refresh_url(&json!({"uid": "u-1", "access_token": "t"})),
            "https://www.codebuddy.cn/v2/plugin/auth/token/refresh"
        );
        // 国际版账号 → 国际版域名。
        assert_eq!(
            refresh_url(&json!({"uid": "u-2", "variant": "ai"})),
            format!(
                "{}/v2/plugin/auth/token/refresh",
                crate::modules::variant::WbVariant::Ai.api_endpoint()
            )
        );
        assert_ne!(
            refresh_url(&json!({"variant": "ai"})),
            refresh_url(&json!({"variant": "cn"}))
        );
    }

    /// AC11：刷新请求头必须带 `X-Auth-Refresh-Source: plugin`，两个档位都覆盖。
    ///
    /// 断言为 `Some("plugin")`：头被删掉时为 `None`，用例立即失败，不会被写成恒真。
    #[test]
    fn refresh_headers_always_declare_plugin_refresh_source() {
        let cases = [
            // 国内版：旧账号（无档位字段）与显式档位各一例。
            json!({"uid": "u-cn", "access_token": "at", "refresh_token": "rt"}),
            json!({"uid": "u-cn", "access_token": "at", "variant": "cn"}),
            // 国际版：D1 的失败现场。
            json!({"uid": "u-ai", "access_token": "at", "variant": "ai"}),
        ];
        for account in cases {
            let headers = refresh_headers(&account, "rt-value");
            assert_eq!(
                headers.get("X-Auth-Refresh-Source").map(String::as_str),
                Some("plugin"),
                "刷新请求头缺少 X-Auth-Refresh-Source: plugin（{account}）"
            );
            // 刷新会话头不能被这次改动挤掉。
            assert_eq!(
                headers.get("X-Refresh-Token").map(String::as_str),
                Some("rt-value")
            );
            // 刷新不带 platform 查询参数/头：官方刷新同样不带（research §2 已推翻旧假设）。
            assert!(!headers.contains_key("platform"));
        }
    }
}
