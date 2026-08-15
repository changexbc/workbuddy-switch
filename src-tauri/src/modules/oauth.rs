//! OAuth 扫码登录采集（复刻 cockpit 流程）。
//!
//! 对照 server.py `oauth_start` / `oauth_poll`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::modules::account;
use crate::modules::config::{
    http_request, norm_ts, now_ms, now_secs, OAUTH_TIMEOUT_SECONDS, WORKBUDDY_API_ENDPOINT,
    WORKBUDDY_API_PREFIX, WORKBUDDY_PLATFORM,
};

#[derive(Default)]
struct OAuthInfo {
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

/// 发起登录：向官方申请 state，返回 loginId / verificationUri / expiresIn。
pub async fn oauth_start() -> Result<Value, String> {
    let login_id = format!("wb_{}", uuid::Uuid::new_v4().simple());
    let url = format!(
        "{WORKBUDDY_API_ENDPOINT}{WORKBUDDY_API_PREFIX}/auth/state?platform={WORKBUDDY_PLATFORM}"
    );
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
        return Err(format!("auth/state 响应缺少 state: {snippet}"));
    }
    let auth_url = data
        .get("authUrl")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("auth_url").and_then(|v| v.as_str()))
        .or_else(|| data.get("url").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{WORKBUDDY_API_ENDPOINT}/login?state={state}"));

    let mut map = oauth_states().lock().unwrap();
    map.insert(
        login_id.clone(),
        OAuthInfo {
            state,
            expires_at: now_secs() + OAUTH_TIMEOUT_SECONDS,
            ..Default::default()
        },
    );
    drop(map);

    Ok(json!({
        "loginId": login_id,
        "verificationUri": auth_url,
        "expiresIn": OAUTH_TIMEOUT_SECONDS,
    }))
}

/// 轮询一次官方 token 接口。成功则拉取账号信息并入库。
pub async fn oauth_poll(login_id: &str) -> Value {
    let state = {
        let mut map = oauth_states().lock().unwrap();
        let Some(info) = map.get_mut(login_id) else {
            return json!({"done": true, "error": "登录请求不存在"});
        };
        if info.done {
            return json!({"done": true, "result": info.result.clone(), "error": info.error.clone()});
        }
        if now_secs() > info.expires_at {
            info.done = true;
            info.error = Some("登录超时".to_string());
            return json!({"done": true, "error": "登录超时"});
        }
        info.state.clone()
    };

    let url = format!("{WORKBUDDY_API_ENDPOINT}{WORKBUDDY_API_PREFIX}/auth/token?state={state}");
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

    // 拉取账号信息
    let account_url = format!("{WORKBUDDY_API_ENDPOINT}{WORKBUDDY_API_PREFIX}/login/account?state={state}");
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {access_token}"));
    let domain = data.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    if !domain.is_empty() {
        headers.insert("X-Domain".to_string(), domain.to_string());
    }
    let acc_resp = http_request(&account_url, "GET", None, Some(&headers)).await;
    let acc_data = acc_resp.get("data").cloned().unwrap_or_else(|| json!({}));

    let uid = acc_data
        .get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let nickname = acc_data
        .get("nickname")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let email = acc_data
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| nickname.clone())
        .or_else(|| uid.clone())
        .unwrap_or_else(|| "unknown".to_string());

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
        "expiresAt": expires_at,
        "refreshExpiresAt": refresh_expires_at,
        "auth_raw": data,
        "profile_raw": acc_data,
        "createdAt": now_ms(),
    });

    // 去重入库（对照 server.py oauth_poll 的三条过滤）
    let mut accounts = account::load_accounts();
    let acc_uid = account.get("uid").and_then(|v| v.as_str()).unwrap_or("");
    let acc_email = account.get("email").and_then(|v| v.as_str()).unwrap_or("");
    // 1) 主键（uid or email）命中目标元组，且两个字段都不等于目标 → 保留
    accounts.retain(|a| {
        let primary = a
            .get("uid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| a.get("email").and_then(|v| v.as_str()).filter(|s| !s.is_empty()));
        let in_target = primary.is_some_and(|p| p == acc_uid || p == acc_email);
        let a_uid = a.get("uid").and_then(|v| v.as_str()).unwrap_or("");
        let a_email = a.get("email").and_then(|v| v.as_str()).unwrap_or("");
        !in_target || (a_uid != acc_uid && a_email != acc_email)
    });
    // 2) uid 命中目标 → 移除
    accounts.retain(|a| {
        !(acc_uid != ""
            && a.get("uid").and_then(|v| v.as_str()).unwrap_or("") == acc_uid)
    });
    // 3) email 命中目标（目标非 "unknown"）→ 移除
    accounts.retain(|a| {
        !(a.get("email").and_then(|v| v.as_str()).unwrap_or("") == acc_email
            && acc_email != "unknown")
    });
    accounts.push(account.clone());
    let _ = account::save_accounts(&accounts);

    let result = account::account_meta(&account);
    let mut map = oauth_states().lock().unwrap();
    if let Some(info) = map.get_mut(login_id) {
        info.done = true;
        info.result = Some(result.clone());
    }
    drop(map);

    json!({"done": true, "result": result})
}
