//! 签到：状态查询 / 执行签到 / 自动签到调度。
//!
//! 对照 server.py `get_checkin_status` / `perform_checkin` /
//! `checkin_account` / `run_checkin_cycle` / `_generate_schedule_minute` /
//! `_checkin_request` / `_is_unauthorized`。

use chrono::{Local, Timelike};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};

use crate::modules::account::{account_display_name, build_auth_headers, load_accounts};
use crate::modules::config::{
    add_checkin_log, http_request, load_checkin_config, load_checkin_logs, now_ms, RunFlagGuard,
    CHECKIN_API_PREFIX, WORKBUDDY_API_ENDPOINT,
};
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};

static CHECKIN_RUNNING: AtomicBool = AtomicBool::new(false);
static SCHEDULES: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();

fn schedules() -> &'static Mutex<HashMap<String, Value>> {
    SCHEDULES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 判断是否因 token 失效被拒（用于触发刷新重试）。
fn is_unauthorized(resp: &Value) -> bool {
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 401 || code == 403 {
        return true;
    }
    let msg = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    ["unauthorized", "401", "登录", "失效", "过期", "token"]
        .iter()
        .any(|k| msg.contains(k))
}

/// 发签到相关请求；遇到未授权且存在 refresh token 时刷新一次并重试。
async fn checkin_request(path: &str, account: &Value) -> Value {
    let url = format!("{WORKBUDDY_API_ENDPOINT}{path}");
    let headers = build_auth_headers(account);
    let mut resp = http_request(&url, "POST", Some(json!({})), Some(&headers)).await;
    if is_unauthorized(&resp)
        && !account
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
    {
        let refreshed = refresh_account_token(account.clone()).await;
        let headers = build_auth_headers(&refreshed);
        resp = http_request(&url, "POST", Some(json!({})), Some(&headers)).await;
    }
    resp
}

/// 查询签到状态：新接口 checkin-activity-status，失败回退 checkin-status。
pub async fn get_checkin_status(account: &Value) -> Value {
    let resp = checkin_request(
        &format!("{CHECKIN_API_PREFIX}/checkin-activity-status"),
        account,
    )
    .await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
        return json!({
            "ok": true,
            "todayCheckedIn": data.get("today_checked_in")
                .or_else(|| data.get("todayCheckedIn"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "raw": data,
        });
    }
    let resp2 = checkin_request(&format!("{CHECKIN_API_PREFIX}/checkin-status"), account).await;
    let code2 = resp2.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code2 == 0 || code2 == 200 {
        let data = resp2.get("data").cloned().unwrap_or_else(|| json!({}));
        return json!({
            "ok": true,
            "todayCheckedIn": data.get("today_checked_in")
                .or_else(|| data.get("todayCheckedIn"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "raw": data,
        });
    }
    json!({
        "ok": false,
        "error": resp2.get("message")
            .or_else(|| resp2.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("code={code2}"))
            .to_string(),
    })
}

/// 执行签到（POST daily-checkin）；服务端返回已签到提示按成功处理。
pub async fn perform_checkin(account: &Value) -> Value {
    let resp = checkin_request(&format!("{CHECKIN_API_PREFIX}/daily-checkin"), account).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        return json!({"ok": true, "raw": resp.get("data").cloned().unwrap_or_else(|| json!({}))});
    }
    let msg = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(|v| v.as_str())
        .unwrap_or(&format!("code={code}"))
        .to_string();
    if msg.contains("已签到") || msg.to_lowercase().contains("repeat") {
        return json!({"ok": true, "already": true, "message": msg});
    }
    json!({"ok": false, "error": msg})
}

/// 对单个账号执行完整签到流程：惰性刷新 → 查状态 → 签到 → 写日志。
pub async fn checkin_account(account: &Value) -> Value {
    let cfg = load_checkin_config();
    let acc = ensure_fresh_token(account.clone(), &cfg).await;
    let entry = json!({
        "ts": now_ms(),
        "accountId": acc.get("id").cloned().unwrap_or_else(|| json!(null)),
        "email": account_display_name(&acc),
    });
    let status = get_checkin_status(&acc).await;
    if status.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let error = status
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("查询签到状态失败")
            .to_string();
        let entry = json!({"result": "error", "error": error, "ts": entry["ts"], "accountId": entry["accountId"], "email": entry["email"]});
        add_checkin_log(&entry);
        return json!({"result": "error", "error": error});
    }
    if status.get("todayCheckedIn").and_then(|v| v.as_bool()) == Some(true) {
        let entry = json!({"result": "already", "ts": entry["ts"], "accountId": entry["accountId"], "email": entry["email"]});
        add_checkin_log(&entry);
        return json!({"result": "already"});
    }
    let res = perform_checkin(&acc).await;
    let result = if res.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        if res.get("already").and_then(|v| v.as_bool()) == Some(true) {
            "already"
        } else {
            "success"
        }
    } else {
        "error"
    };
    let error = if result == "error" {
        res.get("error").and_then(|v| v.as_str()).map(|s| s.to_string())
    } else {
        None
    };
    let mut entry_map = json!({
        "result": result,
        "ts": entry["ts"],
        "accountId": entry["accountId"],
        "email": entry["email"],
    });
    if let Some(e) = error.clone() {
        entry_map["error"] = json!(e);
    }
    add_checkin_log(&entry_map);
    json!({"result": result, "error": error})
}

pub fn date_str(ts_ms: Option<i64>) -> String {
    let dt = Local::now();
    if let Some(ms) = ts_ms {
        let secs = (ms / 1000) as i64;
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| d.with_timezone(&Local).format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| dt.format("%Y-%m-%d").to_string())
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

fn minute_of_day() -> i64 {
    let dt = Local::now();
    dt.hour() as i64 * 60 + dt.minute() as i64
}

/// 在配置时间段 [start_hour, end_hour) 内生成一个随机分钟。
fn generate_schedule_minute(cfg: &Value) -> i64 {
    let start_h = (cfg.get("start_hour").and_then(|v| v.as_i64()).unwrap_or(6) as i64).clamp(0, 23);
    let mut end_h = (cfg.get("end_hour").and_then(|v| v.as_i64()).unwrap_or(12) as i64).clamp(1, 24);
    if end_h <= start_h {
        end_h = start_h + 1;
    }
    let start_m = start_h * 60;
    let end_m = end_h * 60;
    use rand::Rng;
    let mut rng = rand::thread_rng();
    rng.gen_range(start_m..end_m)
}

/// 执行一轮自动签到：为每个账号在时间段内生成随机签到分钟，到点且未签则执行。
///
/// 并发锁防止与手动签到/上一轮重复运行。
pub async fn run_checkin_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&CHECKIN_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let cfg = load_checkin_config();
    if cfg.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return json!({"status": "disabled"});
    }
    let accounts = load_accounts();
    if accounts.is_empty() {
        return json!({"status": "no_accounts"});
    }
    let today = date_str(None);
    let now_min = minute_of_day();
    let mut summary = json!({"status": "ok", "accounts": []});
    for acc in accounts {
        let sid = acc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        // 锁与借用收在 block 内，避免跨 await 持有 MutexGuard
        let minute = {
            let mut sched_map = schedules().lock().unwrap();
            let sched = sched_map
                .entry(sid.clone())
                .or_insert_with(|| json!({"date": "", "minute": 0}));
            if sched.get("date").and_then(|v| v.as_str()) != Some(today.as_str()) {
                *sched = json!({"date": today, "minute": generate_schedule_minute(&cfg)});
            }
            sched.get("minute").and_then(|v| v.as_i64()).unwrap_or(0)
        };
        if now_min < minute {
            continue;
        }
        let result = checkin_account(&acc).await;
        summary["accounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "email": account_display_name(&acc),
                "result": result.get("result").cloned().unwrap_or_else(|| json!(null)),
                "error": result.get("error").cloned().unwrap_or_else(|| json!(null)),
            }));
    }
    summary
}

/// True when every stored account has a today's log of `success` or `already`.
///
/// Empty account list is false so the tray keeps offering 一键签到.
pub fn all_accounts_checked_in_today() -> bool {
    accounts_checked_in_today(&load_accounts(), &load_checkin_logs(), &date_str(None))
}

pub fn accounts_checked_in_today(accounts: &[Value], logs: &[Value], today: &str) -> bool {
    if accounts.is_empty() {
        return false;
    }
    accounts.iter().all(|account| {
        let Some(id) = account.get("id").and_then(Value::as_str) else {
            return false;
        };
        latest_today_result(logs, id, today)
            .map(|result| result == "success" || result == "already")
            .unwrap_or(false)
    })
}

fn latest_today_result<'a>(logs: &'a [Value], account_id: &str, today: &str) -> Option<&'a str> {
    logs.iter()
        .rev()
        .find(|entry| {
            entry.get("accountId").and_then(Value::as_str) == Some(account_id)
                && date_str(entry.get("ts").and_then(Value::as_i64)) == today
        })
        .and_then(|entry| entry.get("result").and_then(Value::as_str))
}

/// 对全部账号立即签到（前端一键签到）。
pub async fn run_checkin_all() -> Value {
    let accounts = load_accounts();
    let mut results: Vec<Value> = Vec::new();
    for acc in accounts {
        let r = checkin_account(&acc).await;
        results.push(json!({
            "accountId": acc.get("id").cloned().unwrap_or_else(|| json!(null)),
            "email": account_display_name(&acc),
            "result": r.get("result").cloned().unwrap_or_else(|| json!(null)),
            "error": r.get("error").cloned().unwrap_or_else(|| json!(null)),
        }));
    }
    json!({"accounts": results})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_minute_within_range() {
        let cfg = json!({"start_hour": 6, "end_hour": 12});
        for _ in 0..100 {
            let m = generate_schedule_minute(&cfg);
            assert!((360..720).contains(&m), "minute {m} 超出 [6h,12h)");
        }
    }

    #[test]
    fn schedule_minute_end_after_start() {
        let cfg = json!({"start_hour": 12, "end_hour": 6});
        let m = generate_schedule_minute(&cfg);
        assert!(m >= 720 && m < 840, "end_hour<=start_hour 时强制 end=start+1");
    }

    #[test]
    fn is_unauthorized_detects_code() {
        assert!(is_unauthorized(&json!({"code": 401})));
        assert!(is_unauthorized(&json!({"code": 403})));
        assert!(!is_unauthorized(&json!({"code": 0})));
    }

    #[test]
    fn checked_in_today_requires_every_account() {
        let accounts = vec![json!({"id": "a"}), json!({"id": "b"})];
        let logs = vec![
            json!({"accountId": "a", "result": "success", "ts": 1_700_000_000_000_i64}),
            json!({"accountId": "b", "result": "already", "ts": 1_700_000_100_000_i64}),
        ];
        let today = date_str(Some(1_700_000_000_000));
        assert!(accounts_checked_in_today(&accounts, &logs, &today));
    }

    #[test]
    fn checked_in_today_false_when_one_failed_last() {
        let accounts = vec![json!({"id": "a"})];
        let logs = vec![
            json!({"accountId": "a", "result": "success", "ts": 1_700_000_000_000_i64}),
            json!({"accountId": "a", "result": "error", "ts": 1_700_000_200_000_i64}),
        ];
        let today = date_str(Some(1_700_000_200_000));
        assert!(!accounts_checked_in_today(&accounts, &logs, &today));
    }

    #[test]
    fn checked_in_today_false_when_empty_or_missing() {
        assert!(!accounts_checked_in_today(&[], &[], "2026-08-19"));
        let accounts = vec![json!({"id": "a"})];
        assert!(!accounts_checked_in_today(&accounts, &[], "2026-08-19"));
    }
}
