//! Token 刷新与保活。
//!
//! 对照 server.py `refresh_account_token` / `ensure_fresh_token` /
//! `run_keepalive_cycle`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use crate::modules::account::{build_auth_headers, upsert_account_locked};
use crate::modules::config::{http_request, load_checkin_config, norm_ts, now_ms, RunFlagGuard};

static KEEPALIVE_RUNNING: AtomicBool = AtomicBool::new(false);
const RELOGIN_RECHECK_INTERVAL_MS: i64 = 60_000;

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

/// 刷新单账号 token（POST /v2/plugin/auth/token/refresh），成功则落盘并返回新账号。
///
/// 刷新失败（refresh token 失效等）时给账号标记 needs_relogin，避免无限重试。
pub async fn refresh_account_token(account: Value) -> Value {
    refresh_account_token_with(account, false, send_refresh).await
}

async fn send_refresh(url: String, headers: HashMap<String, String>) -> Value {
    http_request(&url, "POST", Some(json!({})), Some(&headers)).await
}

/// A successful authenticated request can outlive a previous refresh failure.
/// Verify renewal before clearing that failure: access alone does not prove the
/// refresh token works. Reuse the transaction lock and throttle across hosts.
pub(crate) async fn recheck_relogin_after_success(account: &Value) {
    if account["needs_relogin"] == true {
        let _ = refresh_account_token_with(account.clone(), true, send_refresh).await;
    }
}

fn recovery_due(observed: &Value, latest: &Value, now: i64) -> bool {
    latest["needs_relogin"] == true
        && observed["relogin_failure_id"] == latest["relogin_failure_id"]
        && observed["needs_relogin_reason"] == latest["needs_relogin_reason"]
        && latest["refresh_token"]
            .as_str()
            .is_some_and(|token| !token.trim().is_empty())
        && latest["relogin_recheck_at"]
            .as_i64()
            .is_none_or(|last| now.saturating_sub(last) >= RELOGIN_RECHECK_INTERVAL_MS)
}

fn mark_relogin(account: &mut Value, reason: String) {
    account["needs_relogin"] = json!(true);
    account["needs_relogin_reason"] = json!(reason);
    account["relogin_recheck_at"] = json!(now_ms());
    // A late success for an earlier failure must not erase a newer failure.
    account["relogin_failure_id"] = json!(uuid::Uuid::new_v4().to_string());
}

async fn refresh_account_token_with<F, Fut>(mut account: Value, recovery: bool, request: F) -> Value
where
    F: FnOnce(String, HashMap<String, String>) -> Fut,
    Fut: std::future::Future<Output = Value>,
{
    let request_context = account.clone();
    let guard =
        match crate::modules::state_lock::wait_auth(std::time::Duration::from_secs(30)).await {
            Ok(guard) => guard,
            Err(error) => return refresh_error(account, error.to_string()),
        };
    // Reload under the same lock that covers the network refresh and persistence.
    // A contender must use credentials already rotated by the first host rather
    // than reuse the old refresh token. Never resurrect an account deleted meanwhile.
    let id = crate::modules::account::get_str(&account, "id")
        .or_else(|| crate::modules::account::get_str(&account, "uid"));
    let latest = id
        .as_deref()
        .and_then(crate::modules::account::find_account);
    match refresh_input(&account, latest) {
        Ok((latest, already_refreshed)) => {
            if already_refreshed {
                return latest;
            }
            if recovery && !recovery_due(&request_context, &latest, now_ms()) {
                return latest;
            }
            account = latest;
        }
        Err(error) => return refresh_error(account, error),
    }
    let previous_access_token = account
        .get("access_token")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let rt = account
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if rt.is_empty() {
        mark_relogin(
            &mut account,
            "缺少 refresh token，无法刷新，需重新登录".into(),
        );
        if let Err(error) = upsert_account_locked(&account, &guard) {
            return refresh_error(account, error.to_string());
        }
        return account;
    }

    if recovery {
        account["relogin_recheck_at"] = json!(now_ms());
        if let Err(error) = upsert_account_locked(&account, &guard) {
            return refresh_error(account, error.to_string());
        }
    }

    // Tenant context is needed by the refresh request too, but does not belong
    // in the stored account when it was only resolved for this query.
    let request_account =
        crate::modules::account::with_request_enterprise_context(account.clone(), &request_context);
    let headers = refresh_headers(&request_account, &rt);
    // 刷新必须走账号自身档位的域名：把国际版 token 打到国内网关等于登录失效。
    let url = refresh_url(&account);
    let resp = request(url, headers).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 && code != 200 {
        let reason = format!(
            "刷新失败(code={code}): {}",
            resp.get("message")
                .or_else(|| resp.get("msg"))
                .and_then(|v| v.as_str())
                .unwrap_or("未知错误")
        );
        mark_relogin(&mut account, reason);
        if let Err(error) = upsert_account_locked(&account, &guard) {
            return refresh_error(account, error.to_string());
        }
        return account;
    }

    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let new_at = data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("access_token").and_then(|v| v.as_str()))
        .filter(|token| !token.trim().is_empty())
        .map(|s| s.to_string());
    let Some(new_at) = new_at else {
        mark_relogin(&mut account, "刷新响应缺少 accessToken".into());
        if let Err(error) = upsert_account_locked(&account, &guard) {
            return refresh_error(account, error.to_string());
        }
        return account;
    };

    account["access_token"] = json!(new_at);
    if let Some(new_rt) = data
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("refresh_token").and_then(|v| v.as_str()))
    {
        account["refresh_token"] = json!(new_rt);
    }
    // 官方接口只返回相对 expiresIn（秒），需换算为绝对时间戳
    let new_exp = norm_ts(data.get("expiresAt").or_else(|| data.get("expires_at")));
    let new_exp = match new_exp {
        Some(v) => Some(v),
        None => data
            .get("expiresIn")
            .and_then(|v| v.as_i64())
            .map(|e| now_ms() + e * 1000),
    };
    if let Some(v) = new_exp {
        account["expiresAt"] = json!(v);
    }
    let fallback_rt_exp = norm_ts(
        account
            .get("auth_raw")
            .and_then(|a| a.get("refreshExpiresAt")),
    );
    let mut new_rt_exp = norm_ts(
        data.get("refreshExpiresAt")
            .or_else(|| data.get("refresh_expires_at")),
    );
    if new_rt_exp.is_none() {
        new_rt_exp = fallback_rt_exp;
    }
    let new_rt_exp = match new_rt_exp {
        Some(v) => Some(v),
        None => data
            .get("refreshExpiresIn")
            .and_then(|v| v.as_i64())
            .map(|e| now_ms() + e * 1000),
    };
    if let Some(v) = new_rt_exp {
        account["refreshExpiresAt"] = json!(v);
    }
    account["refreshedAt"] = json!(now_ms());
    let map = account.as_object_mut().unwrap();
    map.remove("needs_relogin");
    map.remove("needs_relogin_reason");
    map.remove("relogin_failure_id");
    map.remove("relogin_recheck_at");
    if let Err(error) = upsert_account_locked(&account, &guard) {
        return refresh_error(account, error.to_string());
    }
    // Windows 不执行 apiKeyHelper；当前 CLI 账号刷新后同步 settings env。
    // 同步失败不阻断 WorkBuddy 保活；状态接口会根据 settings 与账号库是否
    // 一致显示“待同步”，避免把认证配置错误混入账号数据。
    if cfg!(windows) {
        if let Err(error) = crate::modules::codebuddy_cli::sync_windows_env_for_account_locked(
            &account,
            previous_access_token.as_deref(),
            &guard,
        ) {
            // Tokens have already committed. A client-specific configuration
            // warning must not stop other clients using the fresh credentials.
            account["cli_sync_error"] = json!(error);
        }
    }
    account
}

fn refresh_error(mut account: Value, message: String) -> Value {
    // Transient coordination/persistence errors are returned, never saved as a
    // permanent needs_relogin flag (which would poison another host's credentials).
    account["refresh_error"] = json!(message);
    account
}

/// Every consumer of a refreshed account must stop before sending a request if
/// coordination/persistence failed. Keep this transient marker out of storage.
pub fn refresh_failure(account: &Value) -> Option<&str> {
    account.get("refresh_error").and_then(Value::as_str)
}

fn refresh_input(original: &Value, latest: Option<Value>) -> Result<(Value, bool), String> {
    let latest = latest.ok_or_else(|| "ACCOUNT_NOT_FOUND: 刷新前账号已被删除".to_string())?;
    let changed = ["access_token", "refresh_token", "refreshedAt"]
        .iter()
        .any(|key| original[*key] != latest[*key]);
    Ok((latest, changed))
}

/// 惰性刷新：expiresAt 缺失或剩余 < lazy_refresh_hours 则刷新。返回最新账号。
pub async fn ensure_fresh_token(mut account: Value, cfg: &Value) -> Value {
    if let Some(latest) = crate::modules::account::get_str(&account, "id")
        .as_deref()
        .and_then(crate::modules::account::find_account)
    {
        account = latest;
    }
    let lazy_h = cfg
        .get("lazy_refresh_hours")
        .and_then(|v| v.as_i64())
        .unwrap_or(24);
    let exp = account.get("expiresAt").and_then(|v| v.as_i64());
    let stale = match exp {
        Some(e) => now_ms() >= e || e - now_ms() < lazy_h * 3600 * 1000,
        None => true,
    };
    let has_rt = !account
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty();
    if stale && has_rt {
        account = refresh_account_token(account).await;
    }
    account
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
    let accounts = crate::modules::account::load_accounts();
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
        let fresh = refresh_account_token(acc).await;
        let failed = fresh.get("needs_relogin").and_then(|v| v.as_bool()) == Some(true)
            || fresh.get("refresh_error").is_some();
        results.push(json!({
            "email": crate::modules::account::account_display_name(&fresh),
            "status": if !failed { "ok" } else if fresh.get("refresh_error").is_some() { "failed" }
                else if fresh.get("refresh_token").and_then(Value::as_str).is_none_or(|token| token.is_empty()) { "missing_rt" } else { "failed" },
            "error": if failed {
                fresh.get("refresh_error").or_else(|| fresh.get("needs_relogin_reason")).and_then(|v| v.as_str()).map(|s| s.to_string())
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

    #[test]
    fn recovery_requires_the_same_failure_and_bounds_rechecks() {
        let old =
            json!({"needs_relogin":true,"refresh_token":"rt","needs_relogin_reason":"failure"});
        assert!(recovery_due(&old, &old, 100_000));
        for patch in [
            json!({"needs_relogin":false}),
            json!({"refresh_token":""}),
            json!({"relogin_failure_id":"newer failure"}),
            json!({"needs_relogin_reason":"different failure"}),
            json!({"relogin_recheck_at":40_001}),
        ] {
            let mut latest = old.clone();
            latest
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(!recovery_due(&old, &latest, 100_000), "{patch}");
        }
        let mut latest = old.clone();
        latest["relogin_recheck_at"] = json!(40_000);
        assert!(recovery_due(&old, &latest, 100_000));
    }

    #[test]
    fn recovery_transactions_are_isolated_from_real_accounts() {
        for case in [
            "success",
            "failure",
            "concurrent",
            "stale",
            "deleted",
            "healthy",
            "malformed",
        ] {
            let root = std::env::temp_dir().join(format!("wb-recovery-{}", uuid::Uuid::new_v4()));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "modules::refresh::tests::recovery_transaction_worker",
                    "--nocapture",
                ])
                .env(crate::modules::config::HOME_ENV_VAR, &root)
                .env("WB_RECOVERY_TEST_CASE", case)
                .env_remove("CODEBUDDY_AUTH_TOKEN")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn recovery_transaction_worker() {
        let Ok(case) = std::env::var("WB_RECOVERY_TEST_CASE") else {
            return;
        };
        use crate::modules::account::{find_account, save_accounts};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let old = json!({"id":"a","uid":"u","access_token":"old","refresh_token":"old-rt","expiresAt":1,"needs_relogin":true,"needs_relogin_reason":"old failure","note":"original"});
        let other =
            json!({"id":"b","uid":"other","access_token":"other-token","refresh_token":"other-rt"});
        save_accounts(&[old.clone(), other.clone()]).unwrap();
        let calls = AtomicUsize::new(0);
        let success = || json!({"code":0,"data":{"accessToken":"new","refreshToken":"new-rt","expiresIn":3600}});
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            match case.as_str() {
                "success" => {
                    let mut edited = old.clone();
                    edited["note"] = json!("edited during query");
                    save_accounts(&[edited, other.clone()]).unwrap();
                    let fresh =
                        refresh_account_token_with(old.clone(), true, |_, headers| async move {
                            assert_eq!(headers["X-Refresh-Token"], "old-rt");
                            success()
                        })
                        .await;
                    assert!(fresh.get("needs_relogin").is_none());
                    let saved = find_account("a").unwrap();
                    assert_eq!(saved["access_token"], "new");
                    assert_eq!(saved["refresh_token"], "new-rt");
                    assert_eq!(saved["note"], "edited during query");
                    for key in [
                        "needs_relogin",
                        "needs_relogin_reason",
                        "relogin_failure_id",
                        "relogin_recheck_at",
                    ] {
                        assert!(saved.get(key).is_none(), "{key}");
                    }
                    assert_eq!(find_account("b").unwrap(), other);
                    assert!(
                        !crate::modules::account::account_meta(&saved)["needsRelogin"]
                            .as_bool()
                            .unwrap()
                    );
                    assert!(crate::modules::state_lock::committed_revision().is_some());
                }
                "failure" => {
                    refresh_account_token_with(old.clone(), true, |_, _| async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        json!({"code":12153,"message":"refresh token is invalid"})
                    })
                    .await;
                    let failed = find_account("a").unwrap();
                    refresh_account_token_with(failed.clone(), true, |_, _| async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        success()
                    })
                    .await;
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    let saved = find_account("a").unwrap();
                    assert_eq!(saved, failed);
                    assert_eq!(saved["access_token"], old["access_token"]);
                    assert_eq!(saved["refresh_token"], old["refresh_token"]);
                    assert_eq!(saved["needs_relogin"], true);
                    let mut retryable = saved.clone();
                    retryable["relogin_recheck_at"] = json!(now_ms() - RELOGIN_RECHECK_INTERVAL_MS);
                    save_accounts(&[retryable.clone(), other.clone()]).unwrap();
                    let recovered = refresh_account_token_with(retryable, true, |_, _| async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        success()
                    })
                    .await;
                    assert_eq!(calls.load(Ordering::SeqCst), 2);
                    assert!(recovered.get("needs_relogin").is_none());
                }
                "concurrent" => {
                    let attempt = || {
                        refresh_account_token_with(old.clone(), true, |_, _| async {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                            success()
                        })
                    };
                    let (a, b, c) = tokio::join!(attempt(), attempt(), attempt());
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    for value in [a, b, c] {
                        assert_eq!(value["access_token"], "new");
                    }
                }
                "stale" => {
                    let mut newer = old.clone();
                    mark_relogin(&mut newer, "old failure".into());
                    save_accounts(&[newer.clone(), other.clone()]).unwrap();
                    refresh_account_token_with(old.clone(), true, |_, _| async {
                        panic!("late success must not clear newer failure")
                    })
                    .await;
                    assert_eq!(find_account("a").unwrap(), newer);
                    newer["access_token"] = json!("different credentials");
                    save_accounts(&[newer.clone(), other.clone()]).unwrap();
                    refresh_account_token_with(old.clone(), true, |_, _| async {
                        panic!("do not recheck different credentials")
                    })
                    .await;
                    assert_eq!(find_account("a").unwrap(), newer);
                }
                "deleted" => {
                    save_accounts(std::slice::from_ref(&other)).unwrap();
                    let result = refresh_account_token_with(old.clone(), true, |_, _| async {
                        panic!("do not resurrect a deleted account")
                    })
                    .await;
                    assert!(refresh_failure(&result)
                        .unwrap()
                        .contains("ACCOUNT_NOT_FOUND"));
                    assert!(find_account("a").is_none());
                    assert_eq!(find_account("b").unwrap(), other);
                }
                "healthy" => {
                    let mut healthy = old.clone();
                    healthy.as_object_mut().unwrap().remove("needs_relogin");
                    save_accounts(&[healthy.clone(), other.clone()]).unwrap();
                    refresh_account_token_with(healthy.clone(), true, |_, _| async {
                        panic!("healthy accounts must not send additional requests")
                    })
                    .await;
                    assert_eq!(find_account("a").unwrap(), healthy);
                }
                "malformed" => {
                    let failed = refresh_account_token_with(old.clone(), true, |_, _| async {
                        json!({"code":0,"data":{"accessToken":""}})
                    })
                    .await;
                    assert_eq!(failed["needs_relogin"], true);
                    assert_eq!(failed["access_token"], old["access_token"]);
                    assert_eq!(failed["refresh_token"], old["refresh_token"]);
                }
                _ => panic!("unknown test case"),
            }
        });
    }

    #[test]
    fn resolved_tenant_is_carried_with_latest_refresh_credentials_only() {
        let latest = json!({"id":"id","uid":"uid","access_token":"latest-token","refresh_token":"latest-refresh","note":"latest-note"});
        let context = json!({"id":"id","uid":"uid","access_token":"stale","refresh_token":"stale-refresh","enterpriseId":"resolved","note":"stale-note"});
        let request =
            crate::modules::account::with_request_enterprise_context(latest.clone(), &context);
        let headers = refresh_headers(&request, request["refresh_token"].as_str().unwrap());
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer latest-token")
        );
        assert_eq!(
            headers.get("X-Refresh-Token").map(String::as_str),
            Some("latest-refresh")
        );
        assert_eq!(
            headers.get("X-Enterprise-Id").map(String::as_str),
            Some("resolved")
        );
        assert_eq!(
            headers.get("X-Tenant-Id").map(String::as_str),
            Some("resolved")
        );
        assert_eq!(request["note"], "latest-note");
        assert!(latest.get("enterpriseId").is_none());
    }

    #[test]
    fn refresh_reloads_rotated_credentials_preserves_metadata_and_never_resurrects() {
        let old =
            json!({"id":"id", "access_token":"old", "refresh_token":"old-rt", "note":"old note"});
        let fresh =
            json!({"id":"id", "access_token":"new", "refresh_token":"new-rt", "note":"edited"});
        let (value, skip) = refresh_input(&old, Some(fresh.clone())).unwrap();
        assert!(skip);
        assert_eq!(value, fresh);
        let edited =
            json!({"id":"id", "access_token":"old", "refresh_token":"old-rt", "note":"edited"});
        let (value, skip) = refresh_input(&old, Some(edited)).unwrap();
        assert!(!skip);
        assert_eq!(value["note"], "edited");
        assert!(refresh_input(&old, None).is_err());
        let failed = refresh_error(old, "AUTH_STATE_BUSY".into());
        assert!(failed.get("needs_relogin").is_none());
        assert_eq!(refresh_failure(&failed), Some("AUTH_STATE_BUSY"));
        assert!(refresh_failure(
            &json!({"access_token":"fresh", "cli_sync_error":"settings write failed"})
        )
        .is_none());
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
