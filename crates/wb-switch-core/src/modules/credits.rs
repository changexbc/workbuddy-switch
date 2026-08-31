//! WorkBuddy 积分资源查询。
//!
//! WorkBuddy 套餐页使用 summary/paid/free 三个资源接口；旧的
//! `POST /v2/billing/meter/get-user-resource` 仍作为兼容回退。
//! 这里仅返回脱敏后的资源摘要，不把 token 或完整响应交给前端。

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::modules::account::{account_display_name, build_auth_headers};
use crate::modules::config::{http_request, load_checkin_config, now_ms, WORKBUDDY_API_ENDPOINT};
use crate::modules::credit_usage;
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};

const USER_RESOURCE_PATH: &str = "/v2/billing/meter/get-user-resource";
const WORKBUDDY_WEB_ENDPOINT: &str = "https://www.workbuddy.cn";
const RESOURCE_SUMMARY_PATH: &str = "/billing/meter/get-user-resource-summary";
const RESOURCE_PAID_PACKAGES_PATH: &str = "/billing/meter/get-user-resource-paid-packages";
const RESOURCE_FREE_PACKAGES_PATH: &str = "/billing/meter/get-user-resource-free-packages";
const PRODUCT_CODE: &str = "p_tcaca";
const EXPIRING_SOON_DAYS: i64 = 7;

// WorkBuddy CN UserCenter 的商品码（来自其公开套餐配置）。解析器不会依赖
// 这些常量，因此上游新增商品时仍可通过 summary/明细返回资源。
const PAID_PACKAGE_CODES: &[&str] = &[
    "TCACA_code_002_AkiJS3ZHF5",
    "TCACA_code_023_4xbGhMrE6q",
    "TCACA_code_026_BaESVICNoi",
    "TCACA_code_027_0FCGVA6vSa",
    "TCACA_code_009_0XmEQc2xOf",
    "TCACA_code_038_OhvqZtiPKr",
];
const FREE_PACKAGE_CODES: &[&str] = &[
    "TCACA_code_008_cfWoLwvjU4",
    "TCACA_code_007_nzdH5h4Nl0",
    "TCACA_code_028_NtpWi0jzXs",
    "TCACA_code_029_6wCGEWquYy",
    "TCACA_code_030_BjSt89qTvr",
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
    keys.iter()
        .find_map(|key| parse_number(value.get(*key)))
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
    paths
        .iter()
        .any(|path| value_at_path(response, path).and_then(Value::as_array).is_some())
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
    paths
        .iter()
        .any(|path| value_at_path(response, path).and_then(Value::as_array).is_some())
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
        .or_else(|| raw_remaining.zip(raw_used).map(|(remaining, used)| remaining + used))
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
    let expire_at = parse_timestamp_ms(first_value(
        raw,
        &[
            "DeductionEndTime",
            "deductionEndTime",
            "ExpiredTime",
            "expiredTime",
            "CycleEndTime",
            "cycleEndTime",
        ],
    ));
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

/// 发起需要账号身份的 JSON POST 请求。
///
/// 资源查询和官方用量查询必须共用这条链路：先按现有惰性策略保证 token
/// 新鲜，遇到未授权时使用 refresh token 重试一次。调用方只拿到上游 JSON，
/// 不会把认证字段拼进返回值。
pub async fn authenticated_post(account: &Value, url: &str, body: Value) -> Value {
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
    let headers = resource_auth_headers(account);
    http_request(url, "POST", Some(body), Some(&headers)).await
}

fn resource_auth_headers(account: &Value) -> std::collections::HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    // WorkBuddy 用户中心的 Axios 拦截器始终携带该头。桌面端使用同一组
    // billing 接口时也保持一致，避免网关把请求当成未知客户端。
    headers.insert("X-Client-Platform".to_string(), "web".to_string());
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
    let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap_or(now.naive_local());
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

fn new_resource_endpoint(account: &Value) -> &'static str {
    // 官网脚本使用相对路径，实际请求的是当前登录 origin。账号库中的 CN
    // OAuth token 默认签发给 www.codebuddy.cn；若把它固定发往
    // www.workbuddy.cn，令牌域和 X-Domain 会不一致并被网关拒绝。
    // 这里只在两个已知官方 origin 间选择，不允许账号数据拼出任意主机。
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

fn new_resource_url(account: &Value, path: &str) -> String {
    format!("{}{path}", new_resource_endpoint(account))
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
    url: &str,
    body: Value,
) -> Value {
    if is_unauthorized(&response) {
        post_with_account(account, url, body).await
    } else {
        response
    }
}

/// 统一惰性刷新后并行请求三类新资源接口；若任一路返回未授权，只刷新一次，
/// 然后仅重试该分支，避免三个 future 同时刷新并覆盖账号库中的 token。
async fn fetch_new_resource_responses(account: &Value) -> NewResourceResponses {
    let config = load_checkin_config();
    let working_account = ensure_fresh_token(account.clone(), &config).await;
    let summary_url = new_resource_url(&working_account, RESOURCE_SUMMARY_PATH);
    let paid_url = new_resource_url(&working_account, RESOURCE_PAID_PACKAGES_PATH);
    let free_url = new_resource_url(&working_account, RESOURCE_FREE_PACKAGES_PATH);
    let summary_body = json!({});
    let paid_body = paid_packages_body();
    let free_body = free_packages_body();
    let (summary, paid, free) = tokio::join!(
        post_with_account(&working_account, &summary_url, summary_body.clone()),
        post_with_account(&working_account, &paid_url, paid_body.clone()),
        post_with_account(&working_account, &free_url, free_body.clone()),
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
        retry_new_response_if_unauthorized(&refreshed, summary, &summary_url, summary_body),
        retry_new_response_if_unauthorized(&refreshed, paid, &paid_url, paid_body),
        retry_new_response_if_unauthorized(&refreshed, free, &free_url, free_body),
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
    let url = format!("{WORKBUDDY_API_ENDPOINT}{USER_RESOURCE_PATH}");
    // 新接口编排已经统一执行过惰性刷新，并在任一路未授权时只刷新一次。
    // 旧接口回退必须直接复用该账号，不能重新进入 authenticated_post，
    // 否则可能重复刷新并用旧 refresh token 覆盖刚落盘的新 token。
    post_with_account(account, &url, body).await
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

/// 查询单账号的积分资源及到期时间。
pub async fn get_credit_expiry(account: &Value) -> Value {
    let account_id = account.get("id").cloned().unwrap_or(Value::Null);
    let now = now_ms();
    let responses = fetch_new_resource_responses(account).await;
    if let Some(resources) = normalized_new_resources(
        &responses.summary,
        &responses.paid,
        &responses.free,
        now,
    ) {
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
    if is_success(&response) && has_resource_accounts(&response) {
        let resources = resource_accounts(&response)
            .into_iter()
            .map(|resource| resource_summary(resource, now))
            .collect();
        return credit_result(account, resources, now);
    }
    json!({
        "ok": false,
        "accountId": account_id,
        "accountName": account_display_name(account),
        "error": response_error(&response),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_eq!(
            new_resource_url(&codebuddy, RESOURCE_SUMMARY_PATH),
            "https://www.codebuddy.cn/billing/meter/get-user-resource-summary"
        );
        assert_eq!(
            new_resource_url(&workbuddy, RESOURCE_SUMMARY_PATH),
            "https://www.workbuddy.cn/billing/meter/get-user-resource-summary"
        );
        assert_eq!(
            new_resource_url(&unknown, RESOURCE_SUMMARY_PATH),
            "https://www.codebuddy.cn/billing/meter/get-user-resource-summary"
        );

        let headers = resource_auth_headers(&codebuddy);
        assert_eq!(headers.get("X-Client-Platform").map(String::as_str), Some("web"));
        assert_eq!(
            headers.get("X-Domain").map(String::as_str),
            Some("www.codebuddy.cn")
        );
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
}
