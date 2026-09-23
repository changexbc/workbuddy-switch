//! WorkBuddy 积分资源查询。
//!
//! WorkBuddy 套餐页使用 summary/paid/free 三个资源接口；旧的
//! `POST /v2/billing/meter/get-user-resource` 仍作为兼容回退。
//! 这里仅返回脱敏后的资源摘要，不把 token 或完整响应交给前端。

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::modules::account::{
    account_display_name, build_auth_headers, envelope_token_error, variant_of,
};
use crate::modules::config::{
    http_request, is_route_missing, load_checkin_config, now_ms, CHECKIN_API_PREFIX,
    WORKBUDDY_API_ENDPOINT,
};
use crate::modules::credit_usage;
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};
use crate::modules::variant::WbVariant;

/// 旧资源接口路径后缀（按档位生成完整候选，见 `WbVariant::billing_paths`）。
const USER_RESOURCE_SUFFIX: &str = "/get-user-resource";
const WORKBUDDY_WEB_ENDPOINT: &str = "https://www.workbuddy.cn";
const RESOURCE_SUMMARY_PATH: &str = "/billing/meter/get-user-resource-summary";
const RESOURCE_PAID_PACKAGES_PATH: &str = "/billing/meter/get-user-resource-paid-packages";
const RESOURCE_FREE_PACKAGES_PATH: &str = "/billing/meter/get-user-resource-free-packages";
const PRODUCT_CODE: &str = "p_tcaca";
const EXPIRING_SOON_DAYS: i64 = 7;
/// DeductionEndTime 比 CycleEndTime 晚超过该天数时，视前者为长期占位
/// （官方数据形态：如 035 的 DeductionEndTime=2049 与 CycleEndTime=当月月底并存），改用 CycleEndTime。
const EXPIRY_CYCLE_OVERRIDE_DAYS: i64 = 365;
/// 最终解析出的到期时间距 now 超过该天数时视为长期有效（expireAt=null），
/// 避免 2049 这类占位值流入前端。
const FAR_FUTURE_EXPIRY_DAYS: i64 = 730;

// 付费/免费包查询码表：国内版公开套餐配置 ∪ 官网 usercenter 国际版码集
// ∪ 官方客户端 PAID/FREE_PACKAGE_CODES。请求体多带码对不存在的包无副作用；
// 解析器不依赖这份清单，summary 仍可带回未列出的包（但无时间字段）。
const PAID_PACKAGE_CODES: &[&str] = &[
    "TCACA_code_002_AkiJS3ZHF5",
    "TCACA_code_023_4xbGhMrE6q",
    "TCACA_code_026_BaESVICNoi",
    "TCACA_code_027_0FCGVA6vSa",
    "TCACA_code_009_0XmEQc2xOf",
    "TCACA_code_038_OhvqZtiPKr",
    "TCACA_code_003_FAnt7lcmRT",
    "TCACA_code_036_lupO5WgNdG",
];
const FREE_PACKAGE_CODES: &[&str] = &[
    "TCACA_code_008_cfWoLwvjU4",
    "TCACA_code_007_nzdH5h4Nl0",
    "TCACA_code_028_NtpWi0jzXs",
    "TCACA_code_029_6wCGEWquYy",
    "TCACA_code_030_BjSt89qTvr",
    "TCACA_code_001_PqouKr6QWV",
    "TCACA_code_006_DbXS0lrypC",
    "TCACA_code_035_ArVxJcGDsm",
    "TCACA_code_037_WxOD3MpI2o",
    "TCACA_code_039_KRcQj7wUat",
    "TCACA_code_040_mi9rCYg46x",
];

fn first_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn parse_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn first_number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| parse_number(value.get(*key)))
}

fn parse_timestamp_ms(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(number) = parse_number(Some(value)) {
        let millis = if number.abs() < 10_000_000_000.0 {
            number * 1000.0
        } else {
            number
        };
        return Some(millis.round() as i64);
    }

    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }

    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(parsed.timestamp_millis());
    }
    if let Ok(parsed) = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return Local
            .from_local_datetime(&parsed)
            .single()
            .map(|date| date.timestamp_millis());
    }
    if let Ok(parsed) = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f") {
        return Local
            .from_local_datetime(&parsed)
            .single()
            .map(|date| date.timestamp_millis());
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(23, 59, 59))
        .and_then(|date| Local.from_local_datetime(&date).single())
        .map(|date| date.timestamp_millis())
}

fn value_at_path<'a>(mut current: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn resource_accounts(response: &Value) -> Vec<&Value> {
    let paths: &[&[&str]] = &[
        &["data", "Accounts"],
        &["data", "data", "Accounts"],
        &["data", "Response", "Data", "Accounts"],
        &["data", "data", "Response", "Data", "Accounts"],
        &["data", "accounts"],
        &["data", "data", "accounts"],
    ];

    for path in paths {
        if let Some(items) = value_at_path(response, path).and_then(Value::as_array) {
            return items.iter().collect();
        }
    }
    Vec::new()
}

fn resource_packages(response: &Value) -> Vec<&Value> {
    let paths: &[&[&str]] = &[
        &["data", "Packages"],
        &["data", "data", "Packages"],
        &["data", "Response", "Data", "Packages"],
        &["data", "data", "Response", "Data", "Packages"],
        &["data", "packages"],
        &["data", "data", "packages"],
    ];

    for path in paths {
        if let Some(items) = value_at_path(response, path).and_then(Value::as_array) {
            return items.iter().collect();
        }
    }
    Vec::new()
}

fn has_resource_accounts(response: &Value) -> bool {
    let paths: &[&[&str]] = &[
        &["data", "Accounts"],
        &["data", "data", "Accounts"],
        &["data", "Response", "Data", "Accounts"],
        &["data", "data", "Response", "Data", "Accounts"],
        &["data", "accounts"],
        &["data", "data", "accounts"],
    ];
    paths.iter().any(|path| {
        value_at_path(response, path)
            .and_then(Value::as_array)
            .is_some()
    })
}

fn has_resource_packages(response: &Value) -> bool {
    let paths: &[&[&str]] = &[
        &["data", "Packages"],
        &["data", "data", "Packages"],
        &["data", "Response", "Data", "Packages"],
        &["data", "data", "Response", "Data", "Packages"],
        &["data", "packages"],
        &["data", "data", "packages"],
    ];
    paths.iter().any(|path| {
        value_at_path(response, path)
            .and_then(Value::as_array)
            .is_some()
    })
}

/// 到期时间：优先 DeductionEndTime/ExpiredTime；仅当 CycleEndTime 比其早超过
/// `EXPIRY_CYCLE_OVERRIDE_DAYS` 时改用周期结束时间（视前者为长期占位）。
/// 最终值距 now 超过 `FAR_FUTURE_EXPIRY_DAYS` 则视为长期有效。
fn resolve_expire_at(raw: &Value, now: i64) -> Option<i64> {
    let deduction_end = parse_timestamp_ms(first_value(
        raw,
        &[
            "DeductionEndTime",
            "deductionEndTime",
            "ExpiredTime",
            "expiredTime",
        ],
    ));
    let cycle_end = parse_timestamp_ms(first_value(raw, &["CycleEndTime", "cycleEndTime"]));
    let override_ms = EXPIRY_CYCLE_OVERRIDE_DAYS * 24 * 3600 * 1000;
    let expire_at = match (deduction_end, cycle_end) {
        (Some(deduction), Some(cycle)) if deduction.saturating_sub(cycle) > override_ms => {
            Some(cycle)
        }
        (Some(deduction), _) => Some(deduction),
        (None, cycle) => cycle,
    };
    let far_future_ms = FAR_FUTURE_EXPIRY_DAYS * 24 * 3600 * 1000;
    expire_at.filter(|value| value.saturating_sub(now) <= far_future_ms)
}

fn resource_summary(raw: &Value, now: i64) -> Value {
    let slice = first_value(raw, &["SlicePeriodUsageDetails", "slicePeriodUsageDetails"])
        .and_then(Value::as_array)
        .and_then(|items| items.first());
    let total_keys = [
        "CycleCapacitySizePrecise",
        "CycleCapacitySize",
        "CycleTotalCapacity",
        "CapacitySizePrecise",
        "CapacitySize",
        "SlicePeriodCapacitySizePrecise",
        "SlicePeriodCapacitySize",
    ];
    let remaining_keys = [
        "CycleCapacityRemainPrecise",
        "CycleCapacityRemain",
        "CycleRemainCapacity",
        "CapacityRemainPrecise",
        "CapacityRemain",
        "SlicePeriodCapacityRemainPrecise",
        "SlicePeriodCapacityRemain",
    ];
    let used_keys = [
        "CycleCapacityUsedPrecise",
        "CycleCapacityUsed",
        "CycleUsedCapacity",
        "CapacityUsedPrecise",
        "CapacityUsed",
        "SlicePeriodCapacityUsedPrecise",
        "SlicePeriodCapacityUsed",
    ];
    let raw_total = first_number(raw, &total_keys)
        .or_else(|| slice.and_then(|value| first_number(value, &total_keys)));
    let raw_remaining = first_number(raw, &remaining_keys)
        .or_else(|| slice.and_then(|value| first_number(value, &remaining_keys)));
    let raw_used = first_number(raw, &used_keys)
        .or_else(|| slice.and_then(|value| first_number(value, &used_keys)));
    let total = raw_total
        .or_else(|| {
            raw_remaining
                .zip(raw_used)
                .map(|(remaining, used)| remaining + used)
        })
        .or(raw_remaining)
        .or(raw_used)
        .unwrap_or(0.0)
        .max(0.0);
    let remaining = raw_remaining
        .unwrap_or_else(|| (total - raw_used.unwrap_or(0.0)).max(0.0))
        .max(0.0);
    let used = raw_used
        .unwrap_or_else(|| (total - remaining).max(0.0))
        .max(0.0);
    let expire_at = resolve_expire_at(raw, now);
    let expired = expire_at.map(|value| value <= now).unwrap_or(false);
    let expiring_soon = expire_at
        .map(|value| value > now && value - now <= EXPIRING_SOON_DAYS * 24 * 3600 * 1000)
        .unwrap_or(false);
    let status = first_value(raw, &["Status", "status"])
        .and_then(|value| parse_number(Some(value)))
        .map(|value| value as i64);

    json!({
        "packageCode": first_value(raw, &["PackageCode", "packageCode"]),
        "packageName": first_value(raw, &["PackageName", "packageName"]),
        "total": total,
        "remaining": remaining,
        "used": used,
        "status": status,
        "expireAt": expire_at,
        "expired": expired,
        "expiringSoon": expiring_soon,
    })
}

fn response_error(response: &Value) -> String {
    let nested = response.get("data").filter(|value| value.is_object());
    let code = response_code(response).unwrap_or(-1);
    response
        .get("message")
        .or_else(|| response.get("msg"))
        .or_else(|| nested.and_then(|value| value.get("message")))
        .or_else(|| nested.and_then(|value| value.get("msg")))
        .and_then(|value| value.as_str())
        .filter(|message| !message.trim().is_empty())
        .map(|message| message.chars().take(160).collect::<String>())
        .unwrap_or_else(|| format!("积分查询失败（code={code}）"))
}

fn response_code(response: &Value) -> Option<i64> {
    fn parse_code(value: &Value) -> Option<i64> {
        value.as_i64().or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
    }
    response
        .get("code")
        .and_then(parse_code)
        .or_else(|| response.get("data")?.get("code").and_then(parse_code))
}

fn is_success(response: &Value) -> bool {
    if !response.is_object() {
        return false;
    }
    match response_code(response) {
        Some(0) | Some(200) => true,
        Some(_) => false,
        None => {
            response.get("data").is_some()
                && response.get("ok").and_then(Value::as_bool) != Some(false)
                && response.get("success").and_then(Value::as_bool) != Some(false)
        }
    }
}

fn is_unauthorized(response: &Value) -> bool {
    let code = response_code(response).unwrap_or(-1);
    // 网关 WAF 10085 是客户端指纹拦截，不是 token 过期；刷新无效。
    if code == 10085 {
        return false;
    }
    if code == 401 || code == 403 {
        return true;
    }
    let message = response
        .get("message")
        .or_else(|| response.get("msg"))
        .or_else(|| response.get("data").and_then(|value| value.get("message")))
        .or_else(|| response.get("data").and_then(|value| value.get("msg")))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_lowercase();
    ["unauthorized", "401", "登录", "失效", "过期", "token"]
        .iter()
        .any(|keyword| message.contains(keyword))
}

fn is_transport_error(response: &Value) -> bool {
    response_code(response) == Some(-1)
        && response
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.trim().is_empty())
}

/// 发起需要账号身份的 JSON POST 请求。
///
/// 资源查询和官方用量查询必须共用这条链路：先按现有惰性策略保证 token
/// 新鲜，遇到未授权时使用 refresh token 重试一次。调用方只拿到上游 JSON，
/// 不会把认证字段拼进返回值。
pub async fn authenticated_post(account: &Value, url: &str, body: Value) -> Value {
    // 加密信封凭据短路：不发空 Bearer，直接给出可读错误（issue #94）。
    if let Some(err) = envelope_token_error(account) {
        return json!({"code": -2, "message": err});
    }
    let config = load_checkin_config();
    let mut working_account = ensure_fresh_token(account.clone(), &config).await;
    let mut response = post_with_account(&working_account, url, body.clone()).await;

    if is_unauthorized(&response)
        && !working_account
            .get("refresh_token")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .is_empty()
    {
        working_account = refresh_account_token(working_account).await;
        response = post_with_account(&working_account, url, body).await;
    }

    response
}

async fn post_with_account(account: &Value, url: &str, body: Value) -> Value {
    let headers = resource_auth_headers(account, request_origin(url));
    let response = http_request(url, "POST", Some(body.clone()), Some(&headers)).await;
    if is_transport_error(&response) {
        http_request(url, "POST", Some(body), Some(&headers)).await
    } else {
        response
    }
}

fn request_origin(url: &str) -> &'static str {
    // Origin 跟随本次请求 host（契约不变）；新增国际版域后必须同步登记，
    // 否则国际版请求会带上国内 Origin 并被网关的一致性校验拒绝。
    // 只认「host 完全相同或后接 `/`」的前缀，避免相似域名被误当成已知 origin。
    [
        WbVariant::Ai.api_endpoint(),
        WORKBUDDY_WEB_ENDPOINT,
        WORKBUDDY_API_ENDPOINT,
    ]
    .into_iter()
    .find(|origin| match url.strip_prefix(origin) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    })
    .unwrap_or(WORKBUDDY_API_ENDPOINT)
}

fn resource_auth_headers(
    account: &Value,
    origin: &str,
) -> std::collections::HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    // WorkBuddy 用户中心的 Axios 拦截器始终携带该头。桌面端使用同一组
    // billing 接口时也保持一致，避免网关把请求当成未知客户端。
    headers.insert("X-Client-Platform".to_string(), "web".to_string());
    headers.insert(
        "Accept".to_string(),
        "application/json, text/plain, */*".to_string(),
    );
    headers.insert("Origin".to_string(), origin.to_string());
    headers.insert(
        "Referer".to_string(),
        format!("{origin}/profile/plans-usage"),
    );
    headers
}

fn paid_packages_body() -> Value {
    json!({
        "PageNumber": 1,
        "PageSize": 200,
        "Status": [0, 3],
        "PackageCodes": PAID_PACKAGE_CODES,
        "NeedRenewInfo": true,
    })
}

fn free_packages_body() -> Value {
    let now = Local::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or(now.naive_local());
    let end = now
        .date_naive()
        .and_hms_opt(23, 59, 59)
        .unwrap_or(now.naive_local());
    json!({
        "PageNumber": 1,
        "PageSize": 200,
        "Status": [0, 3],
        "SlicePeriodStartTime": start.format("%Y-%m-%d %H:%M:%S").to_string(),
        "SlicePeriodEndTime": end.format("%Y-%m-%d %H:%M:%S").to_string(),
        "PackageCodes": FREE_PACKAGE_CODES,
    })
}

/// 按账号档位/域名选本次请求的基址。
///
/// - 国际版：固定国际版 API 域（不做域名兜底，避免把 AI token 打到国内域）；
/// - 国内版：保持既有 domain 逻辑（workbuddy.cn / codebuddy.cn 二选一）。
pub fn api_base_for(account: &Value) -> &'static str {
    if variant_of(account) == WbVariant::Ai {
        return WbVariant::Ai.api_endpoint();
    }
    // 官网脚本使用相对路径，实际请求的是当前登录 origin。账号库中的 CN
    // OAuth token 默认签发给 www.codebuddy.cn；若把它固定发往
    // www.workbuddy.cn，令牌域和 X-Domain 会不一致并被网关拒绝。
    // 这里只在已知官方 origin 间选择，不允许账号数据拼出任意主机。
    match account
        .get("domain")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("workbuddy.cn") | Some("www.workbuddy.cn") => WORKBUDDY_WEB_ENDPOINT,
        _ => WORKBUDDY_API_ENDPOINT,
    }
}

/// 依次尝试路径候选，**只有 404 才回落**到下一个候选。
///
/// 401/403/10085/传输错误都必须原样返回：把它们当成「路径不对」会掩盖真因，
/// 也会让上层误判为可重试（见 design D4 与 credits 既有契约）。
async fn post_with_fallback(account: &Value, paths: &[String], body: Value) -> Value {
    let base = api_base_for(account);
    let mut last = json!({"code": -1, "message": "无可用接口路径"});
    for (index, path) in paths.iter().enumerate() {
        let url = format!("{base}{path}");
        let response = post_with_account(account, &url, body.clone()).await;
        if index + 1 == paths.len() || !is_route_missing(&response) {
            return response;
        }
        last = response;
    }
    last
}

struct NewResourceResponses {
    account: Value,
    summary: Value,
    paid: Value,
    free: Value,
    refresh_attempted: bool,
}

async fn retry_new_response_if_unauthorized(
    account: &Value,
    response: Value,
    paths: &[String],
    body: Value,
) -> Value {
    if is_unauthorized(&response) {
        post_with_fallback(account, paths, body).await
    } else {
        response
    }
}

/// 统一惰性刷新后并行请求三类新资源接口；若任一路返回未授权，只刷新一次，
/// 然后仅重试该分支，避免三个 future 同时刷新并覆盖账号库中的 token。
async fn fetch_new_resource_responses(account: &Value) -> NewResourceResponses {
    let config = load_checkin_config();
    let working_account = ensure_fresh_token(account.clone(), &config).await;
    // 路径候选按档位生成（国际版先 /billing/meter/... 再回落 /v2/billing/meter/...）。
    let variant = variant_of(&working_account);
    let summary_paths = variant.billing_paths(RESOURCE_SUMMARY_PATH);
    let paid_paths = variant.billing_paths(RESOURCE_PAID_PACKAGES_PATH);
    let free_paths = variant.billing_paths(RESOURCE_FREE_PACKAGES_PATH);
    let summary_body = json!({});
    let paid_body = paid_packages_body();
    let free_body = free_packages_body();
    let (summary, paid, free) = tokio::join!(
        post_with_fallback(&working_account, &summary_paths, summary_body.clone()),
        post_with_fallback(&working_account, &paid_paths, paid_body.clone()),
        post_with_fallback(&working_account, &free_paths, free_body.clone()),
    );

    if !(is_unauthorized(&summary) || is_unauthorized(&paid) || is_unauthorized(&free)) {
        return NewResourceResponses {
            account: working_account,
            summary,
            paid,
            free,
            refresh_attempted: false,
        };
    }

    let can_refresh = working_account
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.trim().is_empty());
    if !can_refresh {
        return NewResourceResponses {
            account: working_account,
            summary,
            paid,
            free,
            refresh_attempted: false,
        };
    }
    let refreshed = refresh_account_token(working_account).await;
    let (summary, paid, free) = tokio::join!(
        retry_new_response_if_unauthorized(&refreshed, summary, &summary_paths, summary_body),
        retry_new_response_if_unauthorized(&refreshed, paid, &paid_paths, paid_body),
        retry_new_response_if_unauthorized(&refreshed, free, &free_paths, free_body),
    );
    NewResourceResponses {
        account: refreshed,
        summary,
        paid,
        free,
        refresh_attempted: true,
    }
}

async fn fetch_legacy_user_resource(account: &Value) -> Value {
    let now = Local::now();
    let begin = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let end = (now + chrono::Duration::days(365 * 101))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let body = json!({
        "PageNumber": 1,
        "PageSize": 100,
        "ProductCode": PRODUCT_CODE,
        "Status": [0, 3],
        "PackageEndTimeRangeBegin": begin,
        "PackageEndTimeRangeEnd": end,
    });
    // 旧接口同样按档位走候选回落（国内版只有一个候选，与改造前一致）。
    let paths =
        variant_of(account).billing_paths(&format!("{CHECKIN_API_PREFIX}{USER_RESOURCE_SUFFIX}"));
    // 新接口编排已经统一执行过惰性刷新，并在任一路未授权时只刷新一次。
    // 旧接口回退必须直接复用该账号，不能重新进入 authenticated_post，
    // 否则可能重复刷新并用旧 refresh token 覆盖刚落盘的新 token。
    post_with_fallback(account, &paths, body).await
}

fn merge_resources(summary_resources: Vec<Value>, detail_resources: Vec<Value>) -> Vec<Value> {
    let detail_codes: HashSet<String> = detail_resources
        .iter()
        .filter_map(|resource| {
            first_value(resource, &["packageCode", "PackageCode"])
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut resources = detail_resources;
    resources.extend(summary_resources.into_iter().filter(|resource| {
        first_value(resource, &["packageCode", "PackageCode"])
            .and_then(Value::as_str)
            .map(|code| !detail_codes.contains(code))
            .unwrap_or(true)
    }));
    resources
}

fn normalized_new_resources(
    summary_response: &Value,
    paid_response: &Value,
    free_response: &Value,
    now: i64,
) -> Option<Vec<Value>> {
    let summary_ok = is_success(summary_response) && has_resource_packages(summary_response);
    let paid_ok = is_success(paid_response) && has_resource_accounts(paid_response);
    let free_ok = is_success(free_response) && has_resource_accounts(free_response);
    if !(summary_ok || paid_ok || free_ok) {
        return None;
    }

    let summary_resources = if summary_ok {
        resource_packages(summary_response)
            .into_iter()
            .map(|resource| resource_summary(resource, now))
            .collect()
    } else {
        Vec::new()
    };
    let mut detail_resources = Vec::new();
    if paid_ok {
        detail_resources.extend(
            resource_accounts(paid_response)
                .into_iter()
                .map(|resource| resource_summary(resource, now)),
        );
    }
    if free_ok {
        detail_resources.extend(
            resource_accounts(free_response)
                .into_iter()
                .map(|resource| resource_summary(resource, now)),
        );
    }
    Some(merge_resources(summary_resources, detail_resources))
}

fn credit_result(account: &Value, resources: Vec<Value>, now: i64) -> Value {
    let total_remaining: f64 = resources
        .iter()
        .filter_map(|resource| resource.get("remaining").and_then(|value| value.as_f64()))
        .sum();
    let total_capacity: f64 = resources
        .iter()
        .filter_map(|resource| resource.get("total").and_then(|value| value.as_f64()))
        .sum();
    let soonest_expire_at = resources
        .iter()
        .filter(|resource| {
            resource
                .get("remaining")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                > 0.0
        })
        .filter_map(|resource| resource.get("expireAt").and_then(|value| value.as_i64()))
        .min();
    let expiring_soon = resources.iter().any(|resource| {
        resource
            .get("expiringSoon")
            .and_then(|value| value.as_bool())
            == Some(true)
            && resource
                .get("remaining")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                > 0.0
    });
    let expired = resources.iter().any(|resource| {
        resource.get("expired").and_then(|value| value.as_bool()) == Some(true)
            && resource
                .get("remaining")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                > 0.0
    });
    let expiring_soon_remaining: f64 = resources
        .iter()
        .filter(|resource| {
            resource
                .get("expiringSoon")
                .and_then(|value| value.as_bool())
                == Some(true)
        })
        .filter_map(|resource| resource.get("remaining").and_then(|value| value.as_f64()))
        .sum();
    let expired_remaining: f64 = resources
        .iter()
        .filter(|resource| resource.get("expired").and_then(|value| value.as_bool()) == Some(true))
        .filter_map(|resource| resource.get("remaining").and_then(|value| value.as_f64()))
        .sum();
    let account_id = account.get("id").cloned().unwrap_or(Value::Null);
    let account_name = account_display_name(account);
    if let Some(account_id) = account_id.as_str() {
        let _ = credit_usage::record_snapshot(
            account_id,
            &account_name,
            total_capacity,
            total_remaining,
            variant_of(account),
        );
    }

    json!({
        "ok": true,
        "accountId": account_id,
        "accountName": account_name,
        "updatedAt": now,
        "totalCapacity": total_capacity,
        "totalRemaining": total_remaining,
        "expiringSoonRemaining": expiring_soon_remaining,
        "expiredRemaining": expired_remaining,
        "soonestExpireAt": soonest_expire_at,
        "expiringSoon": expiring_soon,
        "expired": expired,
        "resources": resources,
    })
}

/// 是否走 summary/paid/free 三路新接口。
///
/// 三路只对国内版存在；国际版只用 `get-user-resource`（用户 2026-09-16 HAR
/// 抓包证实），对它发三路等于 3×2 个 404，故按档位分派取数入口。
fn uses_three_endpoint_query(variant: WbVariant) -> bool {
    matches!(variant, WbVariant::Cn)
}

/// 查询单账号的积分资源及到期时间。
pub async fn get_credit_expiry(account: &Value) -> Value {
    // 加密信封凭据短路：明文解不出来，任何请求都只会发出空 Bearer 并换回网关 401。
    // 本入口同时覆盖新三路接口与旧接口回退两条取数链路，不再空跑请求。
    if let Some(error) = envelope_token_error(account) {
        return json!({
            "ok": false,
            "accountId": account.get("id").cloned().unwrap_or(Value::Null),
            "accountName": account_display_name(account),
            "error": error,
        });
    }

    let now = now_ms();

    if !uses_three_endpoint_query(variant_of(account)) {
        return fetch_legacy_credit(account, now).await;
    }

    let responses = fetch_new_resource_responses(account).await;
    if let Some(resources) =
        normalized_new_resources(&responses.summary, &responses.paid, &responses.free, now)
    {
        return credit_result(account, resources, now);
    }

    // 旧接口回退必须复用三路请求已刷新过的账号，避免再次拿原始 refresh token
    // 发起第二次刷新并把刚落盘的新 token 覆盖成失效状态。
    let mut fallback_account = responses.account;
    let mut response = fetch_legacy_user_resource(&fallback_account).await;
    if is_unauthorized(&response)
        && !responses.refresh_attempted
        && !fallback_account
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
    {
        // 新接口没有触发过 401 刷新时，仍保留旧接口原有的一次重试能力；
        // 若新接口已刷新过，则禁止这里再次刷新，保证一次查询最多一次 401 refresh。
        fallback_account = refresh_account_token(fallback_account).await;
        response = fetch_legacy_user_resource(&fallback_account).await;
    }
    legacy_credit_result(account, &response, now)
}

/// 单接口档位（国际版）的取数：旧接口自带完整鉴权链路。
///
/// 不能复用「三路已刷新账号」的前提——国际版不经过 `fetch_new_resource_responses`，
/// 必须自己完成惰性刷新与 401 后最多一次的 token 刷新。
async fn fetch_legacy_credit(account: &Value, now: i64) -> Value {
    let config = load_checkin_config();
    let mut working_account = ensure_fresh_token(account.clone(), &config).await;
    let mut response = fetch_legacy_user_resource(&working_account).await;
    if is_unauthorized(&response)
        && !working_account
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
    {
        working_account = refresh_account_token(working_account).await;
        response = fetch_legacy_user_resource(&working_account).await;
    }
    legacy_credit_result(account, &response, now)
}

/// 旧接口响应 → 积分结果：成功走正常投影，失败带可读错误。
fn legacy_credit_result(account: &Value, response: &Value, now: i64) -> Value {
    if is_success(response) && has_resource_accounts(response) {
        let resources = resource_accounts(response)
            .into_iter()
            .map(|resource| resource_summary(resource, now))
            .collect();
        return credit_result(account, resources, now);
    }
    json!({
        "ok": false,
        "accountId": account.get("id").cloned().unwrap_or(Value::Null),
        "accountName": account_display_name(account),
        "error": response_error(response),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归 issue #94：信封凭据在 authenticated_post 入口短路，不发空 Bearer，
    /// 也不会进入刷新重试链路。
    #[tokio::test]
    async fn envelope_credentials_short_circuit_before_request() {
        let account = json!({
            "id": "envelope-only",
            "access_token": {"$wbEncrypted": true, "envelope": "…"},
            "refresh_token": {"$wbEncrypted": true, "envelope": "…"},
        });
        let resp = authenticated_post(&account, "https://example.invalid/api", json!({})).await;
        assert_eq!(resp["code"], -2);
        let msg = resp["message"].as_str().expect("message 应为字符串");
        assert!(msg.contains("信封"), "错误文案应可读：{msg}");
    }

    /// 回归 issue #94 遗留：积分查询入口（新三路接口与旧接口回退的唯一入口）
    /// 也要对加密信封凭据短路，不能空跑请求后回显网关 401 文案。
    #[tokio::test]
    async fn envelope_credentials_short_circuit_in_credit_query() {
        let account = json!({
            "id": "envelope-only",
            "access_token": {"$wbEncrypted": true, "envelope": "…"},
            "refresh_token": {"$wbEncrypted": true, "envelope": "…"},
        });
        let resp = get_credit_expiry(&account).await;
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["accountId"], "envelope-only");
        let msg = resp["error"].as_str().expect("error 应为字符串");
        assert!(msg.contains("信封"), "错误文案应可读：{msg}");
    }

    #[test]
    fn parses_cockpit_resource_shape_and_marks_expiry() {
        let now = 1_800_000_000_000_i64;
        let resource = resource_summary(
            &json!({
                "PackageCode": "TCACA_code_007_nzdH5h4Nl0",
                "PackageName": "活动赠送包",
                "CycleCapacitySizePrecise": "100.5",
                "CycleCapacityRemainPrecise": "75.25",
                "DeductionEndTime": now + 2 * 24 * 3600 * 1000,
                "Status": 0,
            }),
            now,
        );

        assert_eq!(resource["packageName"], "活动赠送包");
        assert_eq!(resource["total"], 100.5);
        assert_eq!(resource["remaining"], 75.25);
        assert_eq!(resource["used"], 25.25);
        assert_eq!(resource["expiringSoon"], true);
        assert_eq!(resource["expired"], false);
    }

    #[test]
    fn parses_second_millisecond_and_datetime_timestamps() {
        assert_eq!(
            parse_timestamp_ms(Some(&json!(1_800_000_000))),
            Some(1_800_000_000_000)
        );
        assert_eq!(
            parse_timestamp_ms(Some(&json!(1_800_000_000_000_i64))),
            Some(1_800_000_000_000)
        );
        assert!(parse_timestamp_ms(Some(&json!("2099-01-02 03:04:05"))).is_some());
    }

    #[test]
    fn extracts_nested_accounts() {
        let response = json!({
            "code": 0,
            "data": {"Response": {"Data": {"Accounts": [{"PackageName": "基础包"}]}}}
        });
        let accounts = resource_accounts(&response);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["PackageName"], "基础包");
    }

    #[test]
    fn extracts_new_top_level_accounts_and_packages() {
        let response = json!({
            "code": 0,
            "data": {
                "Accounts": [{"PackageCode": "paid"}],
                "Packages": [{"PackageCode": "summary"}]
            }
        });
        assert_eq!(resource_accounts(&response).len(), 1);
        assert_eq!(resource_accounts(&response)[0]["PackageCode"], "paid");
        assert_eq!(resource_packages(&response).len(), 1);
        assert_eq!(resource_packages(&response)[0]["PackageCode"], "summary");
        assert!(has_resource_accounts(&response));
        assert!(has_resource_packages(&response));
    }

    #[test]
    fn parses_summary_capacity_fields_and_explicit_used_value() {
        let resource = resource_summary(
            &json!({
                "PackageCode": "summary",
                "CycleTotalCapacity": "4485",
                "CycleUsedCapacity": "2156.70999737",
                "CycleRemainCapacity": "2328.29000263",
                "CapacityUnit": "credits"
            }),
            1_800_000_000_000,
        );
        assert_eq!(resource["total"], 4485.0);
        assert_eq!(resource["used"], 2156.70999737);
        assert_eq!(resource["remaining"], 2328.29000263);
        assert_eq!(resource["expireAt"], Value::Null);
    }

    #[test]
    fn keeps_detail_batches_and_only_fills_missing_summary_packages() {
        let summary = vec![
            resource_summary(
                &json!({
                    "PackageCode": "activity",
                    "CycleTotalCapacity": 100,
                    "CycleRemainCapacity": 80
                }),
                1_800_000_000_000,
            ),
            resource_summary(
                &json!({
                    "PackageCode": "free",
                    "CycleTotalCapacity": 500,
                    "CycleRemainCapacity": 300
                }),
                1_800_000_000_000,
            ),
        ];
        let details = vec![
            resource_summary(
                &json!({
                    "PackageCode": "activity",
                    "CycleCapacitySizePrecise": "60",
                    "CycleCapacityRemainPrecise": "40",
                    "DeductionEndTime": 1_800_000_100_000_i64
                }),
                1_800_000_000_000,
            ),
            resource_summary(
                &json!({
                    "PackageCode": "activity",
                    "CycleCapacitySizePrecise": "40",
                    "CycleCapacityRemainPrecise": "40",
                    "DeductionEndTime": 1_800_000_200_000_i64
                }),
                1_800_000_000_000,
            ),
        ];
        let merged = merge_resources(summary, details);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0]["remaining"], 40.0);
        assert_eq!(merged[1]["remaining"], 40.0);
        assert_eq!(merged[2]["packageCode"], "free");
        assert_eq!(merged[2]["remaining"], 300.0);
    }

    #[test]
    fn accepts_empty_detail_accounts_as_a_valid_success() {
        let response = json!({"code": 0, "data": {"Accounts": []}});
        assert!(is_success(&response));
        assert!(has_resource_accounts(&response));
        assert!(resource_accounts(&response).is_empty());
    }

    #[test]
    fn partial_new_success_returns_available_resources() {
        let resources = normalized_new_resources(
            &json!({"code": 500, "message": "summary failed"}),
            &json!({"code": 0, "data": {"Accounts": []}}),
            &json!({
                "code": 0,
                "data": {"data": {"Accounts": [{
                    "PackageCode": "free",
                    "CycleCapacitySizePrecise": "100",
                    "CycleCapacityRemainPrecise": "75"
                }]}}
            }),
            1_800_000_000_000,
        )
        .expect("合法空 paid 和可用 free 明细应视为部分成功");

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["packageCode"], "free");
        assert_eq!(resources[0]["remaining"], 75.0);
    }

    #[test]
    fn valid_empty_new_arrays_do_not_trigger_legacy_fallback() {
        let resources = normalized_new_resources(
            &json!({"code": 0, "data": {"Packages": []}}),
            &json!({"code": 0, "data": {"Accounts": []}}),
            &json!({"code": 0, "data": {"Accounts": []}}),
            1_800_000_000_000,
        );
        assert_eq!(resources, Some(Vec::new()));
    }

    #[test]
    fn all_invalid_new_responses_require_legacy_fallback() {
        let resources = normalized_new_resources(
            &json!({"code": 500, "message": "summary failed"}),
            &json!({"code": 500, "message": "paid failed"}),
            &json!({"code": 0, "data": {}}),
            1_800_000_000_000,
        );
        assert_eq!(resources, None);
    }

    #[test]
    fn new_request_bodies_match_workbuddy_filters() {
        let paid = paid_packages_body();
        assert_eq!(paid["PageNumber"], 1);
        assert_eq!(paid["PageSize"], 200);
        assert_eq!(paid["Status"], json!([0, 3]));
        assert_eq!(paid["NeedRenewInfo"], true);
        assert!(paid["PackageCodes"]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|code| code == "TCACA_code_038_OhvqZtiPKr")));

        let free = free_packages_body();
        assert_eq!(free["PageNumber"], 1);
        assert_eq!(free["PageSize"], 200);
        assert_eq!(free["Status"], json!([0, 3]));
        assert!(free["SlicePeriodStartTime"].as_str().is_some());
        assert!(free["SlicePeriodEndTime"].as_str().is_some());
        assert!(free["PackageCodes"]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|code| code == "TCACA_code_007_nzdH5h4Nl0")));
        assert!(paid.get("NeedInUsage").is_none());
        assert!(free.get("NeedInUsage").is_none());
    }

    #[test]
    fn selects_endpoint_from_known_account_domain_and_keeps_headers_aligned() {
        let codebuddy = json!({
            "domain": "www.codebuddy.cn",
            "access_token": "redacted",
            "uid": "u1"
        });
        let workbuddy = json!({
            "domain": "www.workbuddy.cn",
            "access_token": "redacted",
            "uid": "u2"
        });
        let unknown = json!({"domain": "attacker.example", "access_token": "redacted"});
        let ai = json!({
            "domain": "www.workbuddy.ai",
            "variant": "ai",
            "access_token": "redacted",
            "uid": "u3"
        });

        assert_eq!(
            format!("{}{}", api_base_for(&codebuddy), RESOURCE_SUMMARY_PATH),
            "https://www.codebuddy.cn/billing/meter/get-user-resource-summary"
        );
        assert_eq!(
            format!("{}{}", api_base_for(&workbuddy), RESOURCE_SUMMARY_PATH),
            "https://www.workbuddy.cn/billing/meter/get-user-resource-summary"
        );
        assert_eq!(
            format!("{}{}", api_base_for(&unknown), RESOURCE_SUMMARY_PATH),
            "https://www.codebuddy.cn/billing/meter/get-user-resource-summary"
        );
        // 国际版固定国际版域，绝不落到国内域。
        assert_eq!(api_base_for(&ai), WbVariant::Ai.api_endpoint());
        assert_eq!(
            format!("{}{}", api_base_for(&ai), RESOURCE_SUMMARY_PATH),
            format!(
                "{}/billing/meter/get-user-resource-summary",
                WbVariant::Ai.api_endpoint()
            )
        );

        let headers = resource_auth_headers(&codebuddy, api_base_for(&codebuddy));
        assert_eq!(
            headers.get("X-Client-Platform").map(String::as_str),
            Some("web")
        );
        assert_eq!(
            headers.get("Accept").map(String::as_str),
            Some("application/json, text/plain, */*")
        );
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer redacted")
        );
        assert_eq!(headers.get("X-User-Id").map(String::as_str), Some("u1"));
        assert_eq!(
            headers.get("X-Domain").map(String::as_str),
            Some("www.codebuddy.cn")
        );
        assert_eq!(
            headers.get("Origin").map(String::as_str),
            Some("https://www.codebuddy.cn")
        );
        assert_eq!(
            headers.get("Referer").map(String::as_str),
            Some("https://www.codebuddy.cn/profile/plans-usage")
        );

        // 国际版账号：Origin/Referer 跟随国际版域，X-Domain 仍用账号自身 domain。
        let ai_headers = resource_auth_headers(&ai, api_base_for(&ai));
        assert_eq!(
            ai_headers.get("Origin").map(String::as_str),
            Some(WbVariant::Ai.api_endpoint())
        );
        assert_eq!(
            ai_headers.get("Referer").map(String::as_str),
            Some(format!("{}/profile/plans-usage", WbVariant::Ai.api_endpoint()).as_str())
        );
        assert_eq!(
            ai_headers.get("X-Domain").map(String::as_str),
            Some("www.workbuddy.ai")
        );

        let workbuddy_headers = resource_auth_headers(&workbuddy, api_base_for(&workbuddy));
        assert_eq!(
            workbuddy_headers.get("Origin").map(String::as_str),
            Some("https://www.workbuddy.cn")
        );
        assert_eq!(
            workbuddy_headers.get("Referer").map(String::as_str),
            Some("https://www.workbuddy.cn/profile/plans-usage")
        );
        assert_eq!(
            workbuddy_headers.get("X-Domain").map(String::as_str),
            Some("www.workbuddy.cn")
        );

        let unknown_headers = resource_auth_headers(&unknown, api_base_for(&unknown));
        assert_eq!(
            unknown_headers.get("Origin").map(String::as_str),
            Some("https://www.codebuddy.cn")
        );
        assert_eq!(
            unknown_headers.get("Referer").map(String::as_str),
            Some("https://www.codebuddy.cn/profile/plans-usage")
        );

        // 官方用量 URL 固定 workbuddy.cn，Origin 必须跟请求 host，X-Domain 仍用账号域。
        let usage_url = "https://www.workbuddy.cn/billing/meter/get-user-request-usage";
        assert_eq!(request_origin(usage_url), WORKBUDDY_WEB_ENDPOINT);
        let usage_headers = resource_auth_headers(&codebuddy, request_origin(usage_url));
        assert_eq!(
            usage_headers.get("Origin").map(String::as_str),
            Some("https://www.workbuddy.cn")
        );
        assert_eq!(
            usage_headers.get("X-Domain").map(String::as_str),
            Some("www.codebuddy.cn")
        );
        assert_eq!(
            request_origin("https://www.codebuddy.cn/v2/billing/meter/get-user-resource"),
            WORKBUDDY_API_ENDPOINT
        );
    }

    /// Origin 必须能识别第三个域；未知域仍回落国内版（既有契约）。
    #[test]
    fn request_origin_covers_ai_domain_and_keeps_fallback() {
        assert_eq!(
            request_origin(&format!(
                "{}/billing/meter/daily-checkin",
                WbVariant::Ai.api_endpoint()
            )),
            WbVariant::Ai.api_endpoint()
        );
        assert_eq!(
            request_origin("https://www.workbuddy.cn/x"),
            WORKBUDDY_WEB_ENDPOINT
        );
        assert_eq!(
            request_origin("https://www.codebuddy.cn/x"),
            WORKBUDDY_API_ENDPOINT
        );
        assert_eq!(request_origin("attacker://x"), WORKBUDDY_API_ENDPOINT);
        assert_eq!(
            request_origin("https://www.workbuddy.ai.evil.com/x"),
            WORKBUDDY_API_ENDPOINT
        );
    }

    /// 路径候选：国际版先 /billing/meter/... 再 /v2/billing/meter/...；国内版单候选。
    #[test]
    fn billing_path_candidates_by_variant() {
        let ai = json!({"variant": "ai", "access_token": "t"});
        let cn = json!({"access_token": "t"});

        assert_eq!(
            variant_of(&ai).billing_paths(RESOURCE_SUMMARY_PATH),
            vec![
                "/billing/meter/get-user-resource-summary",
                "/v2/billing/meter/get-user-resource-summary"
            ]
        );
        assert_eq!(
            variant_of(&cn).billing_paths(RESOURCE_SUMMARY_PATH),
            vec!["/billing/meter/get-user-resource-summary"]
        );

        let legacy = format!("{CHECKIN_API_PREFIX}{USER_RESOURCE_SUFFIX}");
        assert_eq!(
            variant_of(&ai).billing_paths(&legacy),
            vec![
                "/billing/meter/get-user-resource",
                "/v2/billing/meter/get-user-resource"
            ]
        );
        assert_eq!(
            variant_of(&cn).billing_paths(&legacy),
            vec!["/v2/billing/meter/get-user-resource"]
        );
    }

    /// 只有 404 允许回落；401/10085/传输错误必须原样返回。
    #[test]
    fn only_route_missing_allows_fallback() {
        assert!(is_route_missing(&json!({"code": 404})));
        assert!(!is_route_missing(
            &json!({"code": 401, "message": "unauthorized"})
        ));
        assert!(!is_route_missing(&json!({
            "code": 10085,
            "msg": "请求不合法，如有疑问请联系客服"
        })));
        assert!(!is_route_missing(
            &json!({"code": -1, "message": "error sending request"})
        ));
        assert!(!is_route_missing(
            &json!({"code": 0, "data": {"Accounts": []}})
        ));

        // 也复核「不该触发刷新」的三类：401 之外的都不刷新。
        assert!(!is_unauthorized(&json!({
            "code": 10085,
            "msg": "请求不合法，如有疑问请联系客服"
        })));
        assert!(!is_unauthorized(
            &json!({"code": 404, "message": "not found"})
        ));
    }

    #[test]
    fn transport_error_is_code_minus_one_with_message() {
        assert!(is_transport_error(&json!({
            "code": -1,
            "message": "error sending request for url (https://www.workbuddy.cn/billing/meter/get-user-resource-summary)"
        })));
        assert!(!is_transport_error(&json!({"code": -1, "message": ""})));
        assert!(!is_transport_error(&json!({"code": -1, "message": "   "})));
        assert!(!is_transport_error(&json!({"code": -1})));
        assert!(!is_transport_error(&json!({
            "code": 10085,
            "msg": "请求不合法，如有疑问请联系客服"
        })));
        assert!(!is_transport_error(
            &json!({"code": 401, "message": "unauthorized"})
        ));
        assert!(!is_transport_error(&json!({"code": 0, "data": {}})));
        assert!(!is_unauthorized(&json!({
            "code": 10085,
            "msg": "请求不合法，如有疑问请联系客服"
        })));
    }

    #[test]
    fn accepts_object_response_without_code() {
        assert!(is_success(&json!({"data": {"Response": {"Data": {}}}})));
        assert!(is_success(
            &json!({"data": {"Response": {"Data": {"Accounts": []}}}})
        ));
        assert!(is_success(&json!({"code": "0", "data": {}})));
        assert!(!is_success(&json!({"message": "failed"})));
        assert!(!is_success(&json!({"data": {}, "ok": false})));
        assert!(!is_success(&Value::Null));
        assert!(!is_success(&json!({"code": 500, "message": "failed"})));
    }

    #[test]
    fn sums_only_resources_that_are_expiring_soon() {
        let now = 1_800_000_000_000_i64;
        let resources = vec![
            resource_summary(
                &json!({
                    "CycleCapacityRemainPrecise": 80,
                    "DeductionEndTime": now + 2 * 24 * 3600 * 1000,
                }),
                now,
            ),
            resource_summary(
                &json!({
                    "CycleCapacityRemainPrecise": 20,
                    "DeductionEndTime": now + 20 * 24 * 3600 * 1000,
                }),
                now,
            ),
        ];
        let expiring: f64 = resources
            .iter()
            .filter(|resource| resource["expiringSoon"] == true)
            .map(|resource| resource["remaining"].as_f64().unwrap())
            .sum();
        assert_eq!(expiring, 80.0);
    }

    #[test]
    fn prefers_cycle_end_when_deduction_end_is_a_far_placeholder() {
        // 035：DeductionEndTime=2049 占位，CycleEndTime=月底 → 取月底
        let now = 1_800_000_000_000_i64;
        let cycle_end = now + 14 * 24 * 3600 * 1000;
        let resource = resource_summary(
            &json!({
                "PackageCode": "TCACA_code_035_ArVxJcGDsm",
                "CycleTotalCapacity": 100,
                "CycleRemainCapacity": 80,
                "DeductionEndTime": 2_049_792_688_000_i64,
                "CycleEndTime": cycle_end,
            }),
            now,
        );
        assert_eq!(resource["expireAt"].as_i64(), Some(cycle_end));
        assert_eq!(resource["expired"], false);
        assert_eq!(resource["expiringSoon"], false);
    }

    #[test]
    fn uses_deduction_end_when_only_real_expiry_is_present() {
        // 006：只有 DeductionEndTime=14 天后 → 取 DeductionEndTime
        let now = 1_800_000_000_000_i64;
        let deduction_end = now + 14 * 24 * 3600 * 1000;
        let resource = resource_summary(
            &json!({
                "PackageCode": "TCACA_code_006_DbXS0lrypC",
                "CycleTotalCapacity": 250,
                "CycleRemainCapacity": 250,
                "DeductionEndTime": deduction_end,
            }),
            now,
        );
        assert_eq!(resource["expireAt"].as_i64(), Some(deduction_end));
    }

    #[test]
    fn keeps_subscription_deduction_end_when_cycle_is_within_override_window() {
        // 订阅型：两者相差 14 天 → 取 DeductionEndTime（回归保护）
        let now = 1_800_000_000_000_i64;
        let cycle_end = now + 16 * 24 * 3600 * 1000;
        let deduction_end = cycle_end + 14 * 24 * 3600 * 1000;
        let resource = resource_summary(
            &json!({
                "PackageCode": "TCACA_code_002_AkiJS3ZHF5",
                "CycleCapacitySizePrecise": "500",
                "CycleCapacityRemainPrecise": "400",
                "DeductionEndTime": deduction_end,
                "CycleEndTime": cycle_end,
            }),
            now,
        );
        assert_eq!(resource["expireAt"].as_i64(), Some(deduction_end));
    }

    #[test]
    fn drops_far_future_placeholder_when_no_cycle_end() {
        // 远期单值：只有 DeductionEndTime=2049 → expireAt 为 null
        let now = 1_800_000_000_000_i64;
        let resource = resource_summary(
            &json!({
                "PackageCode": "placeholder",
                "CycleTotalCapacity": 100,
                "CycleRemainCapacity": 100,
                "DeductionEndTime": 2_049_792_688_000_i64,
            }),
            now,
        );
        assert_eq!(resource["expireAt"], Value::Null);
        assert_eq!(resource["expired"], false);
        assert_eq!(resource["expiringSoon"], false);
    }

    #[test]
    fn package_code_tables_include_intl_codes() {
        let paid: HashSet<&str> = PAID_PACKAGE_CODES.iter().copied().collect();
        let free: HashSet<&str> = FREE_PACKAGE_CODES.iter().copied().collect();
        for code in ["TCACA_code_003_FAnt7lcmRT", "TCACA_code_036_lupO5WgNdG"] {
            assert!(paid.contains(code), "missing paid code {code}");
        }
        for code in [
            "TCACA_code_001_PqouKr6QWV",
            "TCACA_code_006_DbXS0lrypC",
            "TCACA_code_035_ArVxJcGDsm",
            "TCACA_code_037_WxOD3MpI2o",
            "TCACA_code_039_KRcQj7wUat",
            "TCACA_code_040_mi9rCYg46x",
        ] {
            assert!(free.contains(code), "missing free code {code}");
        }
    }

    #[test]
    fn parses_string_cycle_end_from_official_detail_payload() {
        // 官方明细响应里 CycleEndTime 是 "YYYY-MM-DD HH:mm:ss" 字符串（035 的真实形态），
        // 与 2049 的 DeductionEndTime 并存时取字符串周期时间。
        let now = Local
            .with_ymd_and_hms(2026, 9, 16, 12, 0, 0)
            .unwrap()
            .timestamp_millis();
        let expected_cycle_end = Local
            .with_ymd_and_hms(2026, 9, 30, 23, 59, 59)
            .unwrap()
            .timestamp_millis();
        let resource = resource_summary(
            &json!({
                "PackageCode": "TCACA_code_035_ArVxJcGDsm",
                "PackageName": "Free Plan Subscription",
                "CapacitySize": 100,
                "CapacityRemain": 100,
                "CycleCapacitySize": 100,
                "CycleCapacityRemain": 100,
                "CycleEndTime": "2026-09-30 23:59:59",
                "DeductionEndTime": 2_049_792_688_000_i64,
                "ExpiredTime": "",
            }),
            now,
        );
        assert_eq!(resource["expireAt"].as_i64(), Some(expected_cycle_end));
        assert_eq!(resource["expired"], false);
        assert_eq!(resource["expiringSoon"], false);
    }

    #[test]
    fn three_endpoint_query_only_for_domestic_variant() {
        // 国际版没有 summary/paid/free 三路（官网只用 get-user-resource），
        // 发三路等于 3×2 个 404，故只对国内版启用。
        assert!(uses_three_endpoint_query(WbVariant::Cn));
        assert!(!uses_three_endpoint_query(WbVariant::Ai));
    }
}
