//! OAuth 扫码登录采集（复刻 cockpit 流程）。
//!
//! 对照 server.py `oauth_start` / `oauth_poll`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::modules::account;
use crate::modules::config::{http_request, norm_ts, now_ms, now_secs, OAUTH_TIMEOUT_SECONDS};
use crate::modules::i18n;
use crate::modules::variant::WbVariant;

struct OAuthInfo {
    /// 发起登录时的档位：轮询必须沿用该档位拼域名与 `platform`，否则会拿着
    /// 国内版的 state 去国际版取 token（或反之）。
    variant: WbVariant,
    state: String,
    expires_at: i64,
    done: bool,
    result: Option<Value>,
    error: Option<String>,
}

static OAUTH_STATES: OnceLock<Mutex<HashMap<String, OAuthInfo>>> = OnceLock::new();

fn oauth_states() -> &'static Mutex<HashMap<String, OAuthInfo>> {
    OAUTH_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 申请 state 的 URL（域名、前缀、platform 全部取自档位）。
fn oauth_state_url(variant: WbVariant) -> String {
    format!(
        "{}{}/auth/state?platform={}",
        variant.api_endpoint(),
        variant.api_prefix(),
        variant.oauth_platform()
    )
}

/// 换取 token 的 URL。必须用**发起档位**，不能跟随当前列表档位。
fn oauth_token_url(variant: WbVariant, state: &str) -> String {
    format!(
        "{}{}/auth/token?state={state}",
        variant.api_endpoint(),
        variant.api_prefix()
    )
}

/// 拉取账号信息的 URL（同档位）。
fn oauth_account_url(variant: WbVariant, state: &str) -> String {
    format!(
        "{}{}/login/account?state={state}",
        variant.api_endpoint(),
        variant.api_prefix()
    )
}

/// 发起登录：向官方申请 state，返回 loginId / verificationUri / expiresIn。
pub async fn oauth_start(variant: WbVariant) -> Result<Value, String> {
    let login_id = format!("wb_{}", uuid::Uuid::new_v4().simple());
    let url = oauth_state_url(variant);
    let resp = http_request(&url, "POST", Some(json!({})), None).await;
    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let state = data
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if state.is_empty() {
        let snippet = serde_json::to_string(&resp)
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect::<String>();
        return Err(i18n::pick(
            format!("The auth/state response has no state: {snippet}"),
            format!("auth/state 응답에 state가 없습니다: {snippet}"),
        ));
    }
    let auth_url = data
        .get("authUrl")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("auth_url").and_then(|v| v.as_str()))
        .or_else(|| data.get("url").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}/login?state={state}", variant.api_endpoint()));

    let mut map = oauth_states().lock().unwrap();
    map.insert(
        login_id.clone(),
        OAuthInfo {
            variant,
            state,
            expires_at: now_secs() + OAUTH_TIMEOUT_SECONDS,
            done: false,
            result: None,
            error: None,
        },
    );
    drop(map);

    Ok(json!({
        "loginId": login_id,
        "verificationUri": auth_url,
        "expiresIn": OAUTH_TIMEOUT_SECONDS,
    }))
}

/// 结束一次轮询并把错误写回 state（供重复轮询与前端读取）。
fn fail_oauth(login_id: &str, error: String) -> Value {
    let mut map = oauth_states().lock().unwrap();
    if let Some(info) = map.get_mut(login_id) {
        info.done = true;
        info.error = Some(error.clone());
    }
    json!({"done": true, "error": error})
}

/// 档位一致性校验：响应 `domain` 与发起档位不符时拒绝入库。
///
/// 为什么必须校验：国际版 state 换回国内版 token（或反之）时，
/// `domain` 会与后续请求的 Origin/X-Domain 不一致，网关按一致性直接拒绝；
/// 一旦把这种账号写进账号库，切换后客户端会一直处于登录失效状态。
fn domain_mismatch_error(variant: WbVariant, domain: &str) -> Option<String> {
    if variant.matches_domain(domain) {
        return None;
    }
    let variant_label = match variant {
        WbVariant::Cn => i18n::tr("Domestic", "국내판"),
        WbVariant::Ai => i18n::tr("International", "국제판"),
    };
    Some(i18n::pick(
        format!(
            "The login response domain ({domain}) does not match the {variant_label} edition, so the account was not saved"
        ),
        format!(
            "로그인 응답의 domain({domain})이 {variant_label} 에디션과 맞지 않아 계정을 저장하지 않았습니다"
        ),
    ))
}

/// 轮询一次官方 token 接口。成功则拉取账号信息并入库。
pub async fn oauth_poll(login_id: &str) -> Value {
    let (state, variant) = {
        let mut map = oauth_states().lock().unwrap();
        let Some(info) = map.get_mut(login_id) else {
            return json!({"done": true, "error": i18n::tr("Login request not found", "로그인 요청을 찾을 수 없습니다")});
        };
        if info.done {
            return json!({"done": true, "result": info.result.clone(), "error": info.error.clone()});
        }
        if now_secs() > info.expires_at {
            info.done = true;
            info.error = Some(i18n::tr("Login timed out", "로그인 시간이 초과되었습니다").to_string());
            return json!({"done": true, "error": i18n::tr("Login timed out", "로그인 시간이 초과되었습니다")});
        }
        // 用发起档位拼域名与 platform，不跟随当前列表档位。
        (info.state.clone(), info.variant)
    };

    let url = oauth_token_url(variant, &state);
    let resp = http_request(&url, "GET", None, None).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 && code != 200 {
        return json!({"done": false});
    }
    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let access_token = data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("access_token").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() {
        return json!({"done": false});
    }

    let domain = data.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(error) = domain_mismatch_error(variant, domain) {
        return fail_oauth(login_id, error);
    }

    // 拉取账号信息
    let account_url = oauth_account_url(variant, &state);
    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {access_token}"),
    );
    if !domain.is_empty() {
        headers.insert("X-Domain".to_string(), domain.to_string());
    }
    let acc_resp = http_request(&account_url, "GET", None, Some(&headers)).await;
    let acc_data = acc_resp.get("data").cloned().unwrap_or_else(|| json!({}));

    let profile_domain = acc_data
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(error) = domain_mismatch_error(variant, profile_domain) {
        return fail_oauth(login_id, error);
    }

    let uid = acc_data
        .get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let nickname = acc_data
        .get("nickname")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let email = oauth_profile_email(&acc_data);

    let expires_at = norm_ts(data.get("expiresAt").or_else(|| data.get("expires_at")));
    let expires_at = match expires_at {
        Some(v) => Some(v),
        None => data
            .get("expiresIn")
            .and_then(|v| v.as_i64())
            .map(|e| now_ms() + e * 1000),
    };
    let refresh_expires_at = norm_ts(
        data.get("refreshExpiresAt")
            .or_else(|| data.get("refresh_expires_at")),
    );
    let refresh_expires_at = match refresh_expires_at {
        Some(v) => Some(v),
        None => data
            .get("refreshExpiresIn")
            .and_then(|v| v.as_i64())
            .map(|e| now_ms() + e * 1000),
    };

    let account = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "uid": uid,
        "nickname": nickname,
        "email": email,
        "enterpriseName": acc_data.get("enterpriseName"),
        "enterpriseId": acc_data.get("enterpriseId"),
        "access_token": access_token,
        "refresh_token": data.get("refreshToken").and_then(|v| v.as_str())
            .or_else(|| data.get("refresh_token").and_then(|v| v.as_str()))
            .map(|s| s.to_string()),
        "token_type": data.get("tokenType").and_then(|v| v.as_str())
            .or_else(|| data.get("token_type").and_then(|v| v.as_str()))
            .unwrap_or("Bearer")
            .to_string(),
        "domain": domain.to_string(),
        // 档位随登录来源写入：后续切换/刷新/积分/签到都以它为准。
        "variant": variant.as_str(),
        "expiresAt": expires_at,
        "refreshExpiresAt": refresh_expires_at,
        "auth_raw": data,
        "profile_raw": acc_data,
        "createdAt": now_ms(),
    });

    let account = match account::save_collected_account(account) {
        Ok(saved) => saved,
        Err(error) => {
            let error = i18n::pick(
                format!("Could not save the account: {error}"),
                format!("계정을 저장하지 못했습니다: {error}"),
            );
            let mut map = oauth_states().lock().unwrap();
            if let Some(info) = map.get_mut(login_id) {
                info.done = true;
                info.error = Some(error.clone());
            }
            return json!({"done": true, "error": error});
        }
    };

    let result = account::account_meta(&account);
    let mut map = oauth_states().lock().unwrap();
    if let Some(info) = map.get_mut(login_id) {
        info.done = true;
        info.result = Some(result.clone());
    }
    drop(map);

    json!({"done": true, "result": result})
}

fn oauth_profile_email(profile: &Value) -> Option<String> {
    profile
        .get("email")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_profile_without_email_does_not_use_nickname_or_uid() {
        let profile = json!({"uid": "u-1", "nickname": "同名用户"});
        assert_eq!(oauth_profile_email(&profile), None);
    }

    #[test]
    fn oauth_profile_keeps_factual_email() {
        let profile = json!({"email": " user@example.com "});
        assert_eq!(
            oauth_profile_email(&profile).as_deref(),
            Some("user@example.com")
        );
    }

    /// 国际版必须带 `platform=workbuddy-ai` 且走国际版端点；国内版保持原样。
    #[test]
    fn oauth_urls_follow_the_initiating_variant() {
        assert_eq!(
            oauth_state_url(WbVariant::Cn),
            format!(
                "{}/v2/plugin/auth/state?platform={}",
                WbVariant::Cn.api_endpoint(),
                WbVariant::Cn.oauth_platform()
            )
        );
        assert_eq!(
            oauth_state_url(WbVariant::Ai),
            format!(
                "{}/v2/plugin/auth/state?platform=workbuddy-ai",
                WbVariant::Ai.api_endpoint()
            )
        );
        assert!(oauth_state_url(WbVariant::Ai).contains("workbuddy-ai"));
        assert!(!oauth_state_url(WbVariant::Cn).contains("workbuddy-ai"));

        assert_eq!(
            oauth_token_url(WbVariant::Ai, "st-1"),
            format!(
                "{}/v2/plugin/auth/token?state=st-1",
                WbVariant::Ai.api_endpoint()
            )
        );
        assert_eq!(
            oauth_account_url(WbVariant::Ai, "st-1"),
            format!(
                "{}/v2/plugin/login/account?state=st-1",
                WbVariant::Ai.api_endpoint()
            )
        );
        assert_eq!(
            oauth_account_url(WbVariant::Cn, "st-1"),
            format!(
                "{}/v2/plugin/login/account?state=st-1",
                WbVariant::Cn.api_endpoint()
            )
        );
    }

    /// 每个 loginId 独立记录发起档位：轮询时不得被其它档位的登录影响。
    #[test]
    fn oauth_state_records_its_own_variant() {
        let cn_id = format!("test-cn-{}", uuid::Uuid::new_v4().simple());
        let ai_id = format!("test-ai-{}", uuid::Uuid::new_v4().simple());
        let mut map = oauth_states().lock().unwrap();
        for (id, variant) in [(&cn_id, WbVariant::Cn), (&ai_id, WbVariant::Ai)] {
            map.insert(
                id.clone(),
                OAuthInfo {
                    variant,
                    state: "state-1".to_string(),
                    expires_at: now_secs() + OAUTH_TIMEOUT_SECONDS,
                    done: false,
                    result: None,
                    error: None,
                },
            );
        }
        assert_eq!(map.get(&cn_id).unwrap().variant, WbVariant::Cn);
        assert_eq!(map.get(&ai_id).unwrap().variant, WbVariant::Ai);
        map.remove(&cn_id);
        map.remove(&ai_id);
    }

    #[test]
    fn domain_mismatch_is_rejected_per_variant() {
        // AI 发起：CN 域名的响应必须拒绝。
        assert!(domain_mismatch_error(WbVariant::Ai, "www.codebuddy.cn").is_some());
        assert!(domain_mismatch_error(WbVariant::Ai, "www.workbuddy.cn").is_some());
        assert!(domain_mismatch_error(WbVariant::Ai, "www.workbuddy.ai").is_none());
        assert!(domain_mismatch_error(WbVariant::Ai, "").is_none());

        // CN 发起：AI 域名的响应必须拒绝。
        assert!(domain_mismatch_error(WbVariant::Cn, "www.workbuddy.ai").is_some());
        assert!(domain_mismatch_error(WbVariant::Cn, "www.codebuddy.cn").is_none());
        assert!(domain_mismatch_error(WbVariant::Cn, "www.workbuddy.cn").is_none());
        assert!(domain_mismatch_error(WbVariant::Cn, "").is_none());
    }
}
