//! 本地积分观察快照与统计投影。
//!
//! WorkBuddy 只返回当前资源余额，没有可复用的历史账单序列。因此这里把
//! 成功查询到的余额保存为本地观察值，再用相邻快照的正向下降量推导“观察到
//! 的消耗”。首次快照和余额增加只建立新的基线，不产生负数消耗。

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::modules::account::{account_display_name, load_accounts};
use crate::modules::config::{
    atomic_write, credit_usage_snapshots_file, load_checkin_logs, now_ms, store_dir,
    CHECKIN_LOG_KEEP_DAYS,
};

pub const CREDIT_SNAPSHOT_RETENTION_DAYS: i64 = 90;
pub const CREDIT_SNAPSHOT_MAX_RECORDS: usize = 5_000;
pub const CREDIT_SNAPSHOT_DEDUPE_WINDOW_MS: i64 = 5 * 60 * 1000;
const CREDIT_STATS_MAX_EVENTS: usize = 200;

static SNAPSHOT_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
struct Snapshot {
    ts: i64,
    account_id: String,
    account_name: String,
    total: f64,
    remaining: f64,
}

#[derive(Clone, Debug)]
struct UsageEvent {
    ts: i64,
    date: String,
    account_id: String,
    amount: f64,
}

#[derive(Clone, Debug)]
struct CheckinEvent {
    ts: i64,
    date: String,
    account_id: Option<String>,
    account_name: String,
    result: String,
    error: Option<String>,
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(value)) => value.as_f64(),
        Some(Value::String(value)) => value.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn snapshot_from_value(value: &Value) -> Option<Snapshot> {
    let account_id = non_empty_string(value.get("accountId"))?;
    let ts = value.get("ts").and_then(Value::as_i64)?;
    let total = number(value.get("total"))?.max(0.0);
    let remaining = number(value.get("remaining"))?.max(0.0);
    Some(Snapshot {
        ts,
        account_id,
        account_name: non_empty_string(value.get("accountName"))
            .unwrap_or_else(|| "unknown".to_string()),
        total,
        remaining,
    })
}

fn snapshot_value(
    ts: i64,
    account_id: &str,
    account_name: &str,
    total: f64,
    remaining: f64,
) -> Value {
    json!({
        "ts": ts,
        "accountId": account_id,
        "accountName": account_name,
        "total": total.max(0.0),
        "remaining": remaining.max(0.0),
    })
}

/// 读取本地观察快照；文件缺失或损坏时返回空列表。
pub fn load_snapshots() -> Vec<Value> {
    let path = credit_usage_snapshots_file();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) {
            return items;
        }
    }
    vec![]
}

fn should_suppress_duplicate(
    snapshots: &[Value],
    account_id: &str,
    total: f64,
    remaining: f64,
    at_ms: i64,
) -> bool {
    let Some(latest) = snapshots
        .iter()
        .filter_map(snapshot_from_value)
        .filter(|snapshot| snapshot.account_id == account_id)
        .max_by_key(|snapshot| snapshot.ts)
    else {
        return false;
    };

    latest.ts <= at_ms
        && at_ms - latest.ts <= CREDIT_SNAPSHOT_DEDUPE_WINDOW_MS
        && (latest.total - total).abs() < f64::EPSILON
        && (latest.remaining - remaining).abs() < f64::EPSILON
}

fn normalize_snapshots(snapshots: &[Value], at_ms: i64) -> Vec<Value> {
    let cutoff = at_ms.saturating_sub(CREDIT_SNAPSHOT_RETENTION_DAYS * 24 * 3600 * 1000);
    let mut kept: Vec<Value> = snapshots
        .iter()
        .filter_map(|value| {
            let snapshot = snapshot_from_value(value)?;
            (snapshot.ts >= cutoff && snapshot.ts <= at_ms).then_some(value.clone())
        })
        .collect();
    if kept.len() > CREDIT_SNAPSHOT_MAX_RECORDS {
        kept.drain(..kept.len() - CREDIT_SNAPSHOT_MAX_RECORDS);
    }
    kept
}

/// 写入一个成功的资源观察值。
///
/// 同一账号同一资源值在短时间内只保留一条；资源值发生变化时立即保留，
/// 这样余额下降可以归因到新快照。返回值表示本次是否实际写入。
pub fn record_snapshot(account_id: &str, account_name: &str, total: f64, remaining: f64) -> bool {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return false;
    }

    let at_ms = now_ms();
    let _guard = SNAPSHOT_WRITE_LOCK.lock().unwrap();
    let snapshots = load_snapshots();
    if should_suppress_duplicate(&snapshots, account_id, total, remaining, at_ms) {
        return false;
    }

    let mut kept = normalize_snapshots(&snapshots, at_ms);
    kept.push(snapshot_value(
        at_ms,
        account_id,
        account_name.trim(),
        total,
        remaining,
    ));
    if kept.len() > CREDIT_SNAPSHOT_MAX_RECORDS {
        kept.drain(..kept.len() - CREDIT_SNAPSHOT_MAX_RECORDS);
    }

    if let Err(error) = std::fs::create_dir_all(store_dir()).and_then(|_| {
        let content = serde_json::to_string_pretty(&kept).unwrap_or_default();
        atomic_write(&credit_usage_snapshots_file(), &content)
    }) {
        eprintln!("[积分统计] 保存积分快照失败: {error}");
        return false;
    }
    true
}

fn local_date(ts: i64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(ts)
        .single()
        .map(|date| date.date_naive())
}

fn local_date_string(ts: i64) -> Option<String> {
    local_date(ts).map(|date| date.format("%Y-%m-%d").to_string())
}

fn parse_checkin_event(value: &Value) -> Option<CheckinEvent> {
    let ts = value
        .get("ts")
        .and_then(Value::as_i64)
        .or_else(|| crate::modules::config::norm_ts(value.get("ts")))?;
    Some(CheckinEvent {
        ts,
        date: local_date_string(ts)?,
        account_id: non_empty_string(value.get("accountId")),
        account_name: non_empty_string(value.get("email")).unwrap_or_else(|| "unknown".to_string()),
        result: non_empty_string(value.get("result")).unwrap_or_else(|| "error".to_string()),
        error: non_empty_string(value.get("error")),
    })
}

fn parse_snapshots(values: &[Value], at_ms: i64) -> Vec<Snapshot> {
    let cutoff = at_ms.saturating_sub(CREDIT_SNAPSHOT_RETENTION_DAYS * 24 * 3600 * 1000);
    let mut snapshots: Vec<Snapshot> = values
        .iter()
        .filter_map(snapshot_from_value)
        .filter(|snapshot| snapshot.ts >= cutoff && snapshot.ts <= at_ms)
        .collect();
    snapshots.sort_by_key(|snapshot| snapshot.ts);
    snapshots
}

fn usage_in_windows(date: &str, today: NaiveDate) -> (bool, bool, bool) {
    let Some(date) = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok() else {
        return (false, false, false);
    };
    let distance = (today - date).num_days();
    (
        distance == 0,
        (0..7).contains(&distance),
        date.year() == today.year() && date.month() == today.month(),
    )
}

fn add_account_name(
    account_ids: &mut Vec<String>,
    account_names: &mut HashMap<String, String>,
    account_id: String,
    account_name: String,
) {
    if !account_ids.contains(&account_id) {
        account_ids.push(account_id.clone());
    }
    let should_replace = account_names
        .get(&account_id)
        .map(|name| name == "unknown" && account_name != "unknown")
        .unwrap_or(true);
    if should_replace {
        account_names.insert(account_id, account_name);
    }
}

fn checkin_identity(event: &CheckinEvent) -> Option<String> {
    if let Some(account_id) = event.account_id.as_ref() {
        return Some(format!("account:{account_id}"));
    }
    (event.account_name != "unknown").then(|| format!("legacy:{}", event.account_name))
}

fn build_statistics(
    snapshot_values: &[Value],
    checkin_values: &[Value],
    accounts: &[Value],
    at_ms: i64,
) -> Value {
    let snapshots = parse_snapshots(snapshot_values, at_ms);
    let today = local_date(at_ms).unwrap_or_else(|| Local::now().date_naive());
    let checkin_cutoff = at_ms.saturating_sub(CHECKIN_LOG_KEEP_DAYS * 24 * 3600 * 1000);
    let checkins: Vec<CheckinEvent> = checkin_values
        .iter()
        .filter_map(parse_checkin_event)
        .filter(|event| event.ts >= checkin_cutoff && event.ts <= at_ms)
        .collect();

    let mut account_ids = Vec::new();
    let mut current_account_ids = Vec::new();
    let mut account_names = HashMap::new();
    for account in accounts {
        if let Some(id) = non_empty_string(account.get("id")) {
            if !current_account_ids.contains(&id) {
                current_account_ids.push(id.clone());
            }
            add_account_name(
                &mut account_ids,
                &mut account_names,
                id,
                account_display_name(account),
            );
        }
    }
    for snapshot in &snapshots {
        add_account_name(
            &mut account_ids,
            &mut account_names,
            snapshot.account_id.clone(),
            snapshot.account_name.clone(),
        );
    }
    for event in &checkins {
        if let Some(account_id) = &event.account_id {
            add_account_name(
                &mut account_ids,
                &mut account_names,
                account_id.clone(),
                event.account_name.clone(),
            );
        }
    }

    let mut by_account: HashMap<String, Vec<Snapshot>> = HashMap::new();
    for snapshot in snapshots {
        by_account
            .entry(snapshot.account_id.clone())
            .or_default()
            .push(snapshot);
    }
    let coverage_start_at = by_account
        .values()
        .flat_map(|snapshots| snapshots.iter().map(|snapshot| snapshot.ts))
        .min();

    let mut usage_events = Vec::new();
    let mut usage_totals: HashMap<String, (f64, f64, f64)> = HashMap::new();
    let mut daily_usage: HashMap<String, f64> = HashMap::new();
    let mut latest_snapshots = HashMap::new();
    for (account_id, mut snapshots) in by_account {
        snapshots.sort_by_key(|snapshot: &Snapshot| snapshot.ts);
        if let Some(latest) = snapshots.last() {
            latest_snapshots.insert(account_id.clone(), latest.clone());
        }
        for pair in snapshots.windows(2) {
            let previous = &pair[0];
            let current = &pair[1];
            let amount = previous.remaining - current.remaining;
            if amount <= f64::EPSILON {
                continue;
            }
            let Some(date) = local_date_string(current.ts) else {
                continue;
            };
            let entry = usage_totals.entry(account_id.clone()).or_default();
            let (today_usage, week_usage, month_usage) = usage_in_windows(&date, today);
            if today_usage {
                entry.0 += amount;
            }
            if week_usage {
                entry.1 += amount;
            }
            if month_usage {
                entry.2 += amount;
            }
            *daily_usage.entry(date.clone()).or_default() += amount;
            usage_events.push(UsageEvent {
                ts: current.ts,
                date,
                account_id: account_id.clone(),
                amount,
            });
        }
    }

    let mut checkin_today_latest: HashMap<String, (i64, String)> = HashMap::new();
    let mut today_success = 0;
    let mut today_already = 0;
    let mut today_failed = 0;
    for event in &checkins {
        if event.date != today.format("%Y-%m-%d").to_string() {
            continue;
        }
        match event.result.as_str() {
            "success" => today_success += 1,
            "already" => today_already += 1,
            _ => today_failed += 1,
        }
        if let Some(identity) = checkin_identity(event) {
            if checkin_today_latest
                .get(&identity)
                .map(|(ts, _)| *ts <= event.ts)
                .unwrap_or(true)
            {
                checkin_today_latest.insert(identity, (event.ts, event.result.clone()));
            }
        }
    }

    let today_checked_in_accounts = checkin_today_latest
        .values()
        .filter(|(_, result)| result == "success" || result == "already")
        .count();
    let today_key = today.format("%Y-%m-%d").to_string();

    let mut current_remaining = 0.0;
    let mut current_capacity = 0.0;
    for account_id in &current_account_ids {
        if let Some(snapshot) = latest_snapshots.get(account_id) {
            current_remaining += snapshot.remaining;
            current_capacity += snapshot.total;
        }
    }

    let mut last_checkins: HashMap<String, CheckinEvent> = HashMap::new();
    for event in &checkins {
        let Some(account_id) = &event.account_id else {
            continue;
        };
        if last_checkins
            .get(account_id)
            .map(|current: &CheckinEvent| current.ts <= event.ts)
            .unwrap_or(true)
        {
            last_checkins.insert(account_id.clone(), event.clone());
        }
    }

    let account_summaries: Vec<Value> = account_ids
        .iter()
        .map(|account_id| {
            let latest = latest_snapshots.get(account_id);
            let is_current = current_account_ids.contains(account_id);
            let (usage_today, usage_week, usage_month) = usage_totals
                .get(account_id)
                .copied()
                .unwrap_or_default();
            let today_checkin = checkins
                .iter()
                .filter(|event| {
                    event.account_id.as_deref() == Some(account_id.as_str())
                        && event.date == today_key
                })
                .max_by_key(|event| event.ts);
            let last_checkin = last_checkins.get(account_id);
            json!({
                "accountId": account_id,
                "accountName": account_names.get(account_id).cloned().unwrap_or_else(|| "unknown".to_string()),
                "isCurrent": is_current,
                "currentRemaining": is_current.then(|| latest.map(|snapshot| snapshot.remaining)).flatten(),
                "totalCapacity": is_current.then(|| latest.map(|snapshot| snapshot.total)).flatten(),
                "lastSnapshotAt": latest.map(|snapshot| snapshot.ts),
                "usageToday": usage_today,
                "usage7Days": usage_week,
                "usageThisMonth": usage_month,
                "checkedInToday": today_checkin.map(|event| event.result == "success" || event.result == "already"),
                "checkinStatusToday": today_checkin.map(|event| event.result.clone()),
                "lastCheckinAt": last_checkin.map(|event| event.ts),
                "lastCheckinResult": last_checkin.map(|event| event.result.clone()),
            })
        })
        .collect();

    let daily = coverage_start_at
        .and_then(local_date)
        .map(|coverage_date| {
            let earliest = today - ChronoDuration::days(CREDIT_SNAPSHOT_RETENTION_DAYS - 1);
            let mut date = coverage_date.max(earliest);
            let mut points = Vec::new();
            while date <= today {
                let key = date.format("%Y-%m-%d").to_string();
                points.push(json!({
                    "date": key,
                    "usage": daily_usage.get(&key).copied().unwrap_or(0.0),
                }));
                date += ChronoDuration::days(1);
            }
            points
        })
        .unwrap_or_default();

    let mut events: Vec<(i64, Value)> = usage_events
        .iter()
        .map(|event| {
            (
                event.ts,
                json!({
                    "kind": "usage",
                    "ts": event.ts,
                    "date": event.date,
                    "accountId": event.account_id,
                    "accountName": account_names.get(&event.account_id).cloned().unwrap_or_else(|| "unknown".to_string()),
                    "amount": event.amount,
                }),
            )
        })
        .collect();
    events.extend(checkins.iter().map(|event| {
        (
            event.ts,
            json!({
                "kind": "checkin",
                "ts": event.ts,
                "date": event.date,
                "accountId": event.account_id,
                "accountName": event.account_id.as_ref().and_then(|id| account_names.get(id)).cloned().unwrap_or_else(|| event.account_name.clone()),
                "result": event.result,
                "error": event.error,
            }),
        )
    }));
    events.sort_by_key(|event| Reverse(event.0));
    let recent_events: Vec<Value> = events
        .into_iter()
        .take(CREDIT_STATS_MAX_EVENTS)
        .map(|(_, event)| event)
        .collect();

    json!({
        "generatedAt": at_ms,
        "retentionDays": CREDIT_SNAPSHOT_RETENTION_DAYS,
        "coverageStartAt": coverage_start_at,
        "summary": {
            "currentRemaining": current_remaining,
            "currentCapacity": current_capacity,
            "usageToday": usage_totals.values().map(|value| value.0).sum::<f64>(),
            "usage7Days": usage_totals.values().map(|value| value.1).sum::<f64>(),
            "usageThisMonth": usage_totals.values().map(|value| value.2).sum::<f64>(),
            "todayCheckedInAccounts": today_checked_in_accounts,
            "todaySuccess": today_success,
            "todayAlready": today_already,
            "todayFailed": today_failed,
        },
        "daily": daily,
        "accounts": account_summaries,
        "events": recent_events,
    })
}

/// 返回当前本地快照、账号列表和签到日志的统一统计投影。
pub fn get_statistics() -> Value {
    build_statistics(
        &load_snapshots(),
        &load_checkin_logs(),
        &load_accounts(),
        now_ms(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_local_date(days_ago: i64, hour: u32) -> i64 {
        let today = Local::now().date_naive() - ChronoDuration::days(days_ago);
        Local
            .with_ymd_and_hms(today.year(), today.month(), today.day(), hour, 0, 0)
            .single()
            .expect("valid local test date")
            .timestamp_millis()
    }

    fn snap(ts: i64, remaining: f64) -> Value {
        snapshot_value(ts, "account-1", "one@example.com", 100.0, remaining)
    }

    #[test]
    fn first_observation_does_not_create_usage() {
        let now = at_local_date(0, 12);
        let stats = build_statistics(&[snap(now, 100.0)], &[], &[], now);

        assert_eq!(stats["summary"]["usageToday"], 0.0);
        assert_eq!(stats["daily"][0]["usage"], 0.0);
    }

    #[test]
    fn positive_decreases_are_assigned_to_the_newer_local_day() {
        let now = at_local_date(0, 12);
        let yesterday = at_local_date(1, 12);
        let stats = build_statistics(&[snap(yesterday, 100.0), snap(now, 70.0)], &[], &[], now);

        assert_eq!(stats["summary"]["usageToday"], 30.0);
        assert_eq!(stats["summary"]["usage7Days"], 30.0);
        assert_eq!(
            stats["daily"].as_array().unwrap().last().unwrap()["usage"],
            30.0
        );
        assert_eq!(stats["events"][0]["kind"], "usage");
        assert_eq!(stats["events"][0]["amount"], 30.0);
    }

    #[test]
    fn increases_reset_the_baseline_without_negative_usage() {
        let now = at_local_date(0, 12);
        let earlier = at_local_date(2, 12);
        let yesterday = at_local_date(1, 12);
        let stats = build_statistics(
            &[
                snap(earlier, 100.0),
                snap(yesterday, 125.0),
                snap(now, 115.0),
            ],
            &[],
            &[],
            now,
        );

        assert_eq!(stats["summary"]["usageToday"], 10.0);
        assert_eq!(stats["summary"]["usage7Days"], 10.0);
        assert_eq!(stats["events"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn retention_and_month_windows_use_local_calendar_dates() {
        let now = at_local_date(0, 12);
        let old = at_local_date(CREDIT_SNAPSHOT_RETENTION_DAYS + 1, 12);
        let stats = build_statistics(&[snap(old, 100.0), snap(now, 80.0)], &[], &[], now);

        assert_eq!(stats["summary"]["usage7Days"], 0.0);
        assert_eq!(stats["summary"]["usageThisMonth"], 0.0);
        assert_eq!(stats["coverageStartAt"], now);
    }

    #[test]
    fn checkin_events_are_separate_from_credit_usage() {
        let now = at_local_date(0, 12);
        let logs = vec![json!({
            "ts": now,
            "accountId": "account-1",
            "email": "one@example.com",
            "result": "success",
        })];
        let stats = build_statistics(
            &[snap(now - 60_000, 100.0), snap(now, 90.0)],
            &logs,
            &[],
            now,
        );

        assert_eq!(stats["summary"]["usageToday"], 10.0);
        assert_eq!(stats["summary"]["todayCheckedInAccounts"], 1);
        assert_eq!(stats["events"].as_array().unwrap().len(), 2);
        let event_kinds: Vec<&str> = stats["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["kind"].as_str())
            .collect();
        assert!(event_kinds.contains(&"checkin"));
        assert!(event_kinds.contains(&"usage"));
    }

    #[test]
    fn checkins_without_identity_are_kept_as_events_but_not_counted_as_accounts() {
        let now = at_local_date(0, 12);
        let logs = vec![json!({
            "ts": now,
            "result": "success",
        })];
        let stats = build_statistics(&[], &logs, &[], now);

        assert_eq!(stats["summary"]["todayCheckedInAccounts"], 0);
        assert_eq!(stats["events"][0]["kind"], "checkin");
    }

    #[test]
    fn historical_only_accounts_do_not_look_current() {
        let now = at_local_date(0, 12);
        let stats = build_statistics(
            &[snap(now - 60_000, 100.0), snap(now, 80.0)],
            &[],
            &[json!({"id": "current-account"})],
            now,
        );

        assert_eq!(stats["summary"]["currentRemaining"], 0.0);
        let historical = stats["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["accountId"] == "account-1")
            .expect("historical account summary");
        assert_eq!(historical["isCurrent"], false);
        assert!(historical["currentRemaining"].is_null());
        assert_eq!(historical["lastSnapshotAt"], now);
    }

    #[test]
    fn snapshot_normalization_filters_old_values_and_caps_records() {
        let now = at_local_date(0, 12);
        let old = snap(
            now - (CREDIT_SNAPSHOT_RETENTION_DAYS + 1) * 24 * 3600 * 1000,
            100.0,
        );
        let mut values = vec![old];
        values.extend((0..(CREDIT_SNAPSHOT_MAX_RECORDS + 1)).map(|index| {
            snap(
                now - (CREDIT_SNAPSHOT_MAX_RECORDS as i64 - index as i64) * 1_000,
                100.0,
            )
        }));

        let normalized = normalize_snapshots(&values, now);

        assert_eq!(normalized.len(), CREDIT_SNAPSHOT_MAX_RECORDS);
        assert!(normalized.iter().all(|value| {
            snapshot_from_value(value)
                .map(|snapshot| {
                    snapshot.ts >= now - CREDIT_SNAPSHOT_RETENTION_DAYS * 24 * 3600 * 1000
                })
                .unwrap_or(false)
        }));
    }

    #[test]
    fn same_value_is_suppressed_only_inside_the_short_window() {
        let now = at_local_date(0, 12);
        let previous = snap(now - CREDIT_SNAPSHOT_DEDUPE_WINDOW_MS - 1, 100.0);
        assert!(!should_suppress_duplicate(
            std::slice::from_ref(&previous),
            "account-1",
            100.0,
            100.0,
            now,
        ));
        assert!(should_suppress_duplicate(
            &[snap(now - CREDIT_SNAPSHOT_DEDUPE_WINDOW_MS + 1, 100.0)],
            "account-1",
            100.0,
            100.0,
            now,
        ));
    }
}
