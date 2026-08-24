//! WorkBuddy 官方请求用量投影。
//!
//! 该接口返回的是官方请求账单明细，不写入本地持久化文件。这里负责请求
//! 最近 31 个自然日、处理分页、校验和归一化明细，并只向上层暴露统计字段
//! 与有限的请求摘要；上游可能携带的 prompt/input 等字段永远不会被复制。

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::modules::account::{account_display_name, get_str};
use crate::modules::credits::authenticated_post;

pub const OFFICIAL_USAGE_URL: &str =
    "https://www.workbuddy.cn/billing/meter/get-user-request-usage";
pub const OFFICIAL_USAGE_PAGE_SIZE: usize = 3_000;
pub const OFFICIAL_USAGE_DETAIL_LIMIT: usize = 100;
const OFFICIAL_USAGE_MAX_PAGES: usize = 100;

#[derive(Clone, Debug)]
struct RequestRow {
    request_id: String,
    credit: f64,
    model: String,
    client: String,
    request_time: String,
    request_ts: i64,
    date: NaiveDate,
}

#[derive(Clone, Debug)]
struct OfficialPage {
    total: usize,
    raw_len: usize,
    rows: Vec<RequestRow>,
}

#[derive(Clone, Debug)]
struct AccountFetch {
    rows: Vec<RequestRow>,
    reported_total: usize,
    fetched_raw: usize,
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

fn parse_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(value)) => value.as_f64(),
        Some(Value::String(value)) => value.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn response_code(response: &Value) -> Option<i64> {
    response.get("code").and_then(|value| {
        value.as_i64().or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
    })
}

fn error_message(response: &Value) -> String {
    let upstream_message = response
        .get("message")
        .or_else(|| response.get("msg"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(|message| message.chars().take(160).collect::<String>());
    match response_code(response) {
        Some(code) if code == -1 => "官方请求失败（网络或服务不可达）".to_string(),
        Some(code) => upstream_message
            .map(|message| format!("官方请求失败（code={code}）：{message}"))
            .unwrap_or_else(|| format!("官方请求失败（code={code}）")),
        None => "官方响应格式无效".to_string(),
    }
}

fn local_datetime_to_parts(value: NaiveDateTime) -> Option<(NaiveDate, i64)> {
    Local
        .from_local_datetime(&value)
        .single()
        .map(|date| (date.date_naive(), date.timestamp_millis()))
}

fn timestamp_to_parts(ts: i64) -> Option<(NaiveDate, i64)> {
    Local
        .timestamp_millis_opt(ts)
        .single()
        .map(|date| (date.date_naive(), ts))
}

fn parse_request_time(value: Option<&Value>) -> Option<(NaiveDate, i64, String)> {
    let value = value?;
    if let Some(number) = parse_number(Some(value)) {
        if !number.is_finite() {
            return None;
        }
        let ts = if number.abs() < 10_000_000_000.0 {
            (number * 1000.0).round() as i64
        } else {
            number.round() as i64
        };
        let (date, ts) = timestamp_to_parts(ts)?;
        return Some((date, ts, ts.to_string()));
    }

    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        let ts = parsed.timestamp_millis();
        let date = Local.timestamp_millis_opt(ts).single()?.date_naive();
        return Some((date, ts, text.to_string()));
    }
    for pattern in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(text, pattern) {
            let (date, ts) = local_datetime_to_parts(parsed)?;
            return Some((date, ts, text.to_string()));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let parsed = date.and_hms_opt(12, 0, 0)?;
        let (date, ts) = local_datetime_to_parts(parsed)?;
        return Some((date, ts, text.to_string()));
    }
    None
}

fn compact_string(value: Option<&Value>, fallback: &str) -> String {
    let text = non_empty(value).unwrap_or_else(|| fallback.to_string());
    text.chars().take(160).collect()
}

fn normalize_row(value: &Value) -> Option<RequestRow> {
    let credit = parse_number(value.get("credit"))?;
    if !credit.is_finite() || credit < 0.0 {
        return None;
    }
    let (date, request_ts, request_time) = parse_request_time(
        value
            .get("requestTime")
            .or_else(|| value.get("request_time")),
    )?;
    Some(RequestRow {
        request_id: compact_string(
            value.get("requestId").or_else(|| value.get("request_id")),
            "unknown",
        ),
        credit,
        model: compact_string(value.get("model"), "—"),
        client: compact_string(value.get("client"), "—"),
        request_time,
        request_ts,
        date,
    })
}

fn parse_page(response: &Value) -> Result<OfficialPage, String> {
    if response_code(response) != Some(0) && response_code(response) != Some(200) {
        return Err(error_message(response));
    }
    let Some(data) = response.get("data").and_then(Value::as_object) else {
        return Err("官方响应格式无效".to_string());
    };
    let Some(raw_rows) = data.get("data").and_then(Value::as_array) else {
        return Err("官方响应格式无效".to_string());
    };
    let total = data
        .get("total")
        .and_then(|value| {
            value.as_u64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<u64>().ok())
            })
        })
        .unwrap_or(raw_rows.len() as u64)
        .min(usize::MAX as u64) as usize;
    Ok(OfficialPage {
        total,
        raw_len: raw_rows.len(),
        rows: raw_rows.iter().filter_map(normalize_row).collect(),
    })
}

fn should_fetch_next_page(
    page_number: usize,
    total: usize,
    fetched_raw: usize,
    page_len: usize,
) -> bool {
    page_len > 0 && fetched_raw < total && page_number < OFFICIAL_USAGE_MAX_PAGES
}

fn local_date_at(ts: i64) -> NaiveDate {
    Local
        .timestamp_millis_opt(ts)
        .single()
        .map(|date| date.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

async fn fetch_account_usage(
    account: &Value,
    range_start: NaiveDate,
    range_end: NaiveDate,
) -> Result<AccountFetch, String> {
    let start_time = format!("{range_start} 00:00:00");
    let end_time = format!("{range_end} 23:59:59");
    let mut page_number = 1;
    let mut fetched_raw = 0;
    let mut reported_total = 0;
    let mut rows = Vec::new();
    let mut seen_request_ids = HashSet::new();

    loop {
        let response = authenticated_post(
            account,
            OFFICIAL_USAGE_URL,
            json!({
                "startTime": start_time,
                "endTime": end_time,
                "pageNum": page_number,
                "pageSize": OFFICIAL_USAGE_PAGE_SIZE,
            }),
        )
        .await;
        let page = parse_page(&response)?;
        reported_total = reported_total.max(page.total);
        fetched_raw += page.raw_len;
        for row in page
            .rows
            .into_iter()
            .filter(|row| row.date >= range_start && row.date <= range_end)
        {
            if seen_request_ids.insert((row.request_id.clone(), row.request_ts)) {
                rows.push(row);
            }
        }

        if !should_fetch_next_page(page_number, reported_total, fetched_raw, page.raw_len) {
            if page.raw_len == 0 && fetched_raw < reported_total {
                return Err("官方用量分页数据不完整".to_string());
            }
            if page_number >= OFFICIAL_USAGE_MAX_PAGES && fetched_raw < reported_total {
                return Err("官方用量分页超过安全上限".to_string());
            }
            break;
        }
        page_number += 1;
    }

    Ok(AccountFetch {
        rows,
        reported_total,
        fetched_raw,
    })
}

fn aggregate_rows(
    rows: &[RequestRow],
    today: NaiveDate,
) -> (f64, f64, f64, HashMap<NaiveDate, f64>) {
    let mut today_usage = 0.0;
    let mut week_usage = 0.0;
    let mut month_usage = 0.0;
    let mut daily = HashMap::new();
    for row in rows {
        let distance = (today - row.date).num_days();
        if distance == 0 {
            today_usage += row.credit;
        }
        if (0..7).contains(&distance) {
            week_usage += row.credit;
        }
        if row.date.year() == today.year() && row.date.month() == today.month() {
            month_usage += row.credit;
        }
        *daily.entry(row.date).or_insert(0.0) += row.credit;
    }
    (today_usage, week_usage, month_usage, daily)
}

fn request_value(account_id: &str, account_name: &str, row: &RequestRow) -> Value {
    json!({
        "accountId": account_id,
        "accountName": account_name,
        "requestId": row.request_id,
        "credit": row.credit,
        "model": row.model,
        "client": row.client,
        "requestTime": row.request_time,
    })
}

/// 查询全部当前账号并生成官方请求用量投影。
pub async fn collect_official_usage(accounts: &[Value], at_ms: i64) -> Value {
    let today = local_date_at(at_ms);
    let range_start = today - Duration::days(30);
    let range_end = today;
    let range_start_text = range_start.format("%Y-%m-%d").to_string();
    let range_end_text = range_end.format("%Y-%m-%d").to_string();

    let mut successful_accounts = 0usize;
    let mut account_rows = Vec::new();
    let mut errors = Vec::new();
    let mut daily_totals: HashMap<NaiveDate, f64> = HashMap::new();
    let mut total_today = 0.0;
    let mut total_week = 0.0;
    let mut total_month = 0.0;
    let mut recent_requests: Vec<(i64, Value)> = Vec::new();

    for (index, account) in accounts.iter().enumerate() {
        let account_id = get_str(account, "id").unwrap_or_else(|| format!("unknown-{index}"));
        let account_name = account_display_name(account);
        match fetch_account_usage(account, range_start, range_end).await {
            Ok(result) => {
                successful_accounts += 1;
                let (usage_today, usage_week, usage_month, daily) =
                    aggregate_rows(&result.rows, today);
                total_today += usage_today;
                total_week += usage_week;
                total_month += usage_month;
                for (date, amount) in daily {
                    *daily_totals.entry(date).or_insert(0.0) += amount;
                }

                let mut sorted_rows = result.rows.clone();
                sorted_rows.sort_by_key(|row| Reverse(row.request_ts));
                for row in sorted_rows.iter().take(OFFICIAL_USAGE_DETAIL_LIMIT) {
                    recent_requests.push((
                        row.request_ts,
                        request_value(&account_id, &account_name, row),
                    ));
                }
                account_rows.push(json!({
                    "accountId": account_id,
                    "accountName": account_name,
                    "ok": true,
                    "requestCount": result.reported_total,
                    "detailTruncated": result.reported_total > OFFICIAL_USAGE_DETAIL_LIMIT,
                    "usageToday": usage_today,
                    "usage7Days": usage_week,
                    "usageThisMonth": usage_month,
                    "error": Value::Null,
                    "reportedTotal": result.reported_total,
                    "fetchedCount": result.fetched_raw,
                }));
            }
            Err(error) => {
                errors.push(json!({
                    "accountId": account_id,
                    "accountName": account_name,
                    "error": error,
                }));
                account_rows.push(json!({
                    "accountId": account_id,
                    "accountName": account_name,
                    "ok": false,
                    "requestCount": 0,
                    "detailTruncated": false,
                    "usageToday": Value::Null,
                    "usage7Days": Value::Null,
                    "usageThisMonth": Value::Null,
                    "error": errors.last().and_then(|item| item.get("error")).cloned().unwrap_or(Value::Null),
                    "reportedTotal": Value::Null,
                    "fetchedCount": 0,
                }));
            }
        }
    }

    recent_requests.sort_by_key(|(ts, _)| Reverse(*ts));
    let requests: Vec<Value> = recent_requests
        .into_iter()
        .map(|(_, value)| value)
        .collect();

    let mut daily = Vec::new();
    let mut date = range_start;
    while date <= range_end {
        daily.push(json!({
            "date": date.format("%Y-%m-%d").to_string(),
            "usage": daily_totals.get(&date).copied().unwrap_or(0.0),
        }));
        date += Duration::days(1);
    }

    let status = if accounts.is_empty() {
        "unavailable"
    } else if successful_accounts == accounts.len() {
        "complete"
    } else if successful_accounts > 0 {
        "partial"
    } else {
        "unavailable"
    };

    json!({
        "status": status,
        "rangeStart": range_start_text,
        "rangeEnd": range_end_text,
        "summary": {
            "usageToday": total_today,
            "usage7Days": total_week,
            "usageThisMonth": total_month,
        },
        "daily": daily,
        "accounts": account_rows,
        "requests": requests,
        "detailLimitPerAccount": OFFICIAL_USAGE_DETAIL_LIMIT,
        "errors": errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_date(days_ago: i64, hour: u32) -> (NaiveDate, i64) {
        let date = Local::now().date_naive() - Duration::days(days_ago);
        let timestamp = Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), hour, 0, 0)
            .single()
            .expect("valid local test date")
            .timestamp_millis();
        (date, timestamp)
    }

    fn row(date: NaiveDate, ts: i64, credit: f64) -> RequestRow {
        RequestRow {
            request_id: "request-1".to_string(),
            credit,
            model: "model-a".to_string(),
            client: "client-a".to_string(),
            request_time: format!("{date} 12:00:00"),
            request_ts: ts,
            date,
        }
    }

    #[test]
    fn parses_success_page_and_does_not_copy_prompt_fields() {
        let page = parse_page(&json!({
            "code": 0,
            "data": {
                "total": "1",
                "data": [{
                    "requestId": "req-1",
                    "credit": "1.25",
                    "model": "model-a",
                    "client": "cli",
                    "requestTime": "2026-08-24 12:34:56",
                    "input": "do not expose this prompt",
                    "inputTrunc": "also secret"
                }]
            }
        }))
        .expect("valid official page");

        assert_eq!(page.total, 1);
        assert_eq!(page.raw_len, 1);
        assert_eq!(page.rows[0].credit, 1.25);
        let output = request_value("account-1", "one@example.com", &page.rows[0]);
        let text = output.to_string();
        assert!(!text.contains("do not expose"));
        assert!(!text.contains("inputTrunc"));
        assert_eq!(output["requestId"], "req-1");
    }

    #[test]
    fn malformed_rows_are_ignored_without_turning_into_zero_usage() {
        let page = parse_page(&json!({
            "code": 200,
            "data": {
                "total": 3,
                "data": [
                    {"requestId": "valid", "credit": 2, "requestTime": "2026-08-24 12:00:00"},
                    {"requestId": "negative", "credit": -1, "requestTime": "2026-08-24 12:00:00"},
                    {"requestId": "bad-time", "credit": 4, "requestTime": "not-a-date"}
                ]
            }
        }))
        .expect("page shape is valid");

        assert_eq!(page.raw_len, 3);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].credit, 2.0);
    }

    #[test]
    fn pagination_stops_on_empty_short_total_and_page_limit() {
        assert!(!should_fetch_next_page(1, 10, 0, 0));
        assert!(should_fetch_next_page(1, 10, 2, 2));
        assert!(!should_fetch_next_page(
            1,
            2_000,
            2_000,
            OFFICIAL_USAGE_PAGE_SIZE
        ));
        assert!(should_fetch_next_page(
            1,
            6_000,
            OFFICIAL_USAGE_PAGE_SIZE,
            OFFICIAL_USAGE_PAGE_SIZE
        ));
        assert!(!should_fetch_next_page(
            OFFICIAL_USAGE_MAX_PAGES,
            usize::MAX,
            OFFICIAL_USAGE_PAGE_SIZE * OFFICIAL_USAGE_MAX_PAGES,
            OFFICIAL_USAGE_PAGE_SIZE,
        ));
    }

    #[test]
    fn aggregation_uses_positive_credits_and_local_today_windows() {
        let (today, today_ts) = local_date(0, 12);
        let (yesterday, yesterday_ts) = local_date(1, 12);
        let (last_month, last_month_ts) = local_date(40, 12);
        let rows = vec![
            row(today, today_ts, 1.5),
            row(yesterday, yesterday_ts, 2.0),
            row(last_month, last_month_ts, 5.0),
        ];

        let (today_usage, week_usage, month_usage, daily) = aggregate_rows(&rows, today);
        assert_eq!(today_usage, 1.5);
        assert_eq!(week_usage, 3.5);
        assert_eq!(month_usage, 3.5);
        assert_eq!(daily[&today], 1.5);
        assert_eq!(daily[&yesterday], 2.0);
    }

    #[test]
    fn rejects_error_and_missing_data_shapes() {
        assert!(matches!(
            parse_page(&json!({"code": 500, "msg": "bad"})),
            Err(error) if error == "官方请求失败（code=500）：bad"
        ));
        assert!(matches!(
            parse_page(&json!({"code": 0, "data": {"total": 0}})),
            Err(error) if error == "官方响应格式无效"
        ));
        assert_eq!(
            error_message(&json!({"code": -1, "message": "secret token"})),
            "官方请求失败（网络或服务不可达）"
        );
    }
}
