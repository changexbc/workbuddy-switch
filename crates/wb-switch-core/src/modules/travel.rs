//! 派猫猫旅行：状态机、接口封装、每日缓存与自动派发/领取奖励。
//!
//! 对照 WorkDaddy `daemon.js` + `growth-travel.js` 的派猫猫旅行实现。
//! 状态机以服务端 `data.state` 为准：idle ->(depart)-> traveling ->(到点)-> arrived ->(claim)-> idle。
//! 官网 `idle` + `daily_limit_reached` 对应「累了，明天再来吧」，展示为已结束，不能再按 traveling 倒计时。

use chrono::Local;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::modules::account::{account_display_name, build_auth_headers, load_accounts};
use crate::modules::config::{
    http_request, load_checkin_config, load_travel_cache, load_travel_config, now_ms, now_secs,
    save_travel_cache, with_travel_cache_lock, RunFlagGuard, TRAVEL_API_PREFIX,
    WORKBUDDY_API_ENDPOINT,
};
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};

static TRAVEL_RUNNING: AtomicBool = AtomicBool::new(false);
static TRAVEL_CLAIM_RUNNING: AtomicBool = AtomicBool::new(false);

/// 派发周期：启动即派发，之后每 30 分钟补一轮（并重试 no-buddy / 瞬时错误）。
pub const TRAVEL_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 领取奖励检查周期：启动立刻查一轮，之后每隔 15 分钟检查并领取。
pub const TRAVEL_CLAIM_INTERVAL: Duration = Duration::from_secs(15 * 60);

const KNOWN_STATES: [&str; 3] = ["idle", "traveling", "arrived"];

/// 判定为「可重试」的跳过原因：这些情况下当日不算完成，后续轮次继续重试。
///
/// 针对参考项目 Bug：账号初始无 Buddy 时缓存了 no-buddy 且标记 completed，
/// 之后账号有了 Buddy 也不会再重试，导致一直显示「无 Buddy」。
fn is_retryable_skip(skip: Option<&str>) -> bool {
    matches!(
        skip,
        Some("no-buddy")
            | Some("error")
            | Some("config-error")
            | Some("no-location")
            | Some("location-unavailable")
            | Some("status-error")
    )
}

fn today_str() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn account_key(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .unwrap_or_else(|| account_display_name(account))
}

fn build_travel_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("x-client-platform".to_string(), "web".to_string());
    headers.insert("origin".to_string(), WORKBUDDY_API_ENDPOINT.to_string());
    headers.insert(
        "referer".to_string(),
        format!("{WORKBUDDY_API_ENDPOINT}/profile/growth-center"),
    );
    headers
}

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

/// 发旅行接口请求；遇到未授权且存在 refresh token 时刷新一次并重试。
async fn travel_request(path: &str, method: &str, body: Option<Value>, account: &Value) -> Value {
    let url = format!("{WORKBUDDY_API_ENDPOINT}{path}");
    let headers = build_travel_headers(account);
    let mut resp = http_request(&url, method, body.clone(), Some(&headers)).await;
    if is_unauthorized(&resp)
        && !account
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
    {
        let refreshed = refresh_account_token(account.clone()).await;
        let headers = build_travel_headers(&refreshed);
        resp = http_request(&url, method, body, Some(&headers)).await;
    }
    resp
}

fn resp_error(resp: &Value, fallback_code: i64) -> String {
    resp.get("message")
        .or_else(|| resp.get("msg"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("code={fallback_code}"))
}

fn parse_travel_state(raw: Option<&str>) -> Option<&'static str> {
    let normalized = raw.map(|s| s.trim().to_ascii_lowercase())?;
    KNOWN_STATES
        .iter()
        .copied()
        .find(|state| *state == normalized)
}

fn result_claimed(entry: &Value) -> bool {
    entry.get("claimed").and_then(Value::as_bool) == Some(true)
}

fn result_in_flight(entry: &Value) -> bool {
    entry.get("ok").and_then(Value::as_bool) == Some(true) && !result_claimed(entry)
}

fn cache_results(cache: &Value) -> Option<&Map<String, Value>> {
    cache.get("results").and_then(Value::as_object)
}

/// 跨日时丢掉已结束/失败记录，但保留仍在 traveling/arrived 的未领奖励。
fn roll_cache_to_today(cache: &mut Value, today: &str) {
    if cache.get("date").and_then(Value::as_str) == Some(today) {
        return;
    }
    let kept = cache_results(cache)
        .map(|results| {
            results
                .iter()
                .filter(|(_, entry)| result_in_flight(entry))
                .map(|(id, entry)| (id.clone(), entry.clone()))
                .collect::<Map<String, Value>>()
        })
        .unwrap_or_default();
    cache["date"] = json!(today);
    cache["completed"] = json!(false);
    cache["results"] = Value::Object(kept);
}

fn has_retryable_results(results: &Map<String, Value>) -> bool {
    results
        .values()
        .any(|entry| is_retryable_skip(entry.get("skip").and_then(Value::as_str)))
}

/// 读取旅行配置（地点列表等）。返回 `{ ok, enabled, locations: [{ id, name }] }`。
async fn fetch_travel_config(account: &Value) -> Value {
    let resp = travel_request(&format!("{TRAVEL_API_PREFIX}/config"), "GET", None, account).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
        let enabled_flag = data.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let locations: Vec<Value> = data
            .get("locations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|l| {
                json!({
                    "id": l.get("id").cloned().unwrap_or(Value::Null),
                    "name": l.get("name").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect();
        return json!({
            "ok": true,
            "enabled": enabled_flag && !locations.is_empty(),
            "locations": locations,
        });
    }
    json!({
        "ok": false,
        "error": resp_error(&resp, code),
    })
}

/// 读取当前旅行状态。缺 state / 未知 state 视为失败，避免被当成 idle 后误标已领取。
///
/// 官方以 `data.state` + `data.daily_limit_reached` 判断能否派出：
/// arrived → 领奖；traveling → 等待；idle 且未达每日上限 → 可 depart。
async fn fetch_travel_status(account: &Value) -> Value {
    let resp = travel_request(&format!("{TRAVEL_API_PREFIX}/status"), "GET", None, account).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
        let raw_state = data.get("state").and_then(Value::as_str);
        let Some(state) = parse_travel_state(raw_state) else {
            return json!({
                "ok": false,
                "error": match raw_state {
                    Some(value) if !value.trim().is_empty() => format!("unknown travel state: {value}"),
                    _ => "missing travel state".to_string(),
                },
            });
        };
        return json!({
            "ok": true,
            "state": state,
            "locationId": data
                .get("location")
                .and_then(|l| l.get("id"))
                .cloned()
                .unwrap_or(Value::Null),
            "departAt": data.get("depart_at").and_then(Value::as_i64).unwrap_or(0),
            "arriveAt": data.get("arrive_at").and_then(Value::as_i64).unwrap_or(0),
            "dailyLimitReached": data.get("daily_limit_reached").and_then(Value::as_bool).unwrap_or(false),
            "buddyId": data.get("buddy_id").and_then(Value::as_i64).unwrap_or(0),
            "recordId": data.get("record_id").and_then(Value::as_i64).unwrap_or(0),
            "serverNow": data.get("server_now").and_then(Value::as_i64).unwrap_or(0),
            "locationName": data
                .get("location")
                .and_then(|location| location.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(""),
            "rewardCredit": data.get("reward_credit").cloned().unwrap_or(Value::Null),
        });
    }
    json!({
        "ok": false,
        "error": resp_error(&resp, code),
    })
}

/// 派猫猫旅行（depart）。返回 `{ ok, state }` 或 `{ ok:false, message }`。
async fn depart_travel(account: &Value, location_id: &Value) -> Value {
    let resp = travel_request(
        &format!("{TRAVEL_API_PREFIX}/depart"),
        "POST",
        Some(json!({ "location_id": location_id })),
        account,
    )
    .await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
        return json!({
            "ok": true,
            "state": data.get("state").and_then(Value::as_str).unwrap_or("traveling"),
        });
    }
    json!({
        "ok": false,
        "code": code,
        "message": resp_error(&resp, code),
    })
}

/// 领取旅行奖励（state==='arrived' 时调用）。返回 `{ ok, rewardCredit }` 或 `{ ok:false, message }`。
async fn claim_travel(account: &Value, record_id: i64) -> Value {
    let payload = if record_id > 0 {
        json!({ "record_id": record_id })
    } else {
        json!({})
    };
    let resp = travel_request(
        &format!("{TRAVEL_API_PREFIX}/claim"),
        "POST",
        Some(payload),
        account,
    )
    .await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
        let reward_credit = data
            .get("reward_credit")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
        return json!({ "ok": true, "rewardCredit": reward_credit });
    }
    json!({
        "ok": false,
        "code": code,
        "message": resp_error(&resp, code),
    })
}

fn depart_result(
    account: &Value,
    uid: Option<&str>,
    ok: bool,
    already: bool,
    skip: Option<&str>,
    state: Option<&str>,
    message: &str,
) -> Value {
    json!({
        "accountId": account_key(account),
        "uid": uid,
        "ok": ok,
        "already": already,
        "skip": skip,
        "state": state,
        "message": message,
        "claimed": false,
        "rewardCredit": Value::Null,
        "claimedAt": 0,
        "at": now_ms(),
    })
}

#[derive(Debug, PartialEq, Eq)]
enum DepartClass {
    AlreadyTraveling,
    DailyLimit,
    NoBuddy,
    LocationUnavailable,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
enum TravelAction {
    Claim,
    WaitTraveling,
    SkipDailyLimit,
    Depart,
    StatusError,
}

/// 与官方网页同一套判断：先看 state，再看 daily_limit_reached。
/// idle + daily_limit_reached 即官网「累了，明天再来吧」。
fn decide_travel_action(state: &str, daily_limit_reached: bool) -> TravelAction {
    match state {
        "arrived" => TravelAction::Claim,
        "traveling" => TravelAction::WaitTraveling,
        "idle" if daily_limit_reached => TravelAction::SkipDailyLimit,
        "idle" => TravelAction::Depart,
        _ => TravelAction::StatusError,
    }
}

/// HTTP 429 是限流，不是「今日已派」。只有文案明确 daily limit 才算当日完成。
fn classify_depart_error(_code: i64, message: &str) -> DepartClass {
    let raw = message.to_lowercase();
    if raw.contains("already traveling") {
        DepartClass::AlreadyTraveling
    } else if raw.contains("daily limit") || raw.contains("daily_limit") {
        DepartClass::DailyLimit
    } else if raw.contains("no active buddy") {
        DepartClass::NoBuddy
    } else if raw.contains("location not available") {
        DepartClass::LocationUnavailable
    } else {
        DepartClass::Other
    }
}

/// 对单个账号执行派猫猫旅行：依次尝试地点列表，报错分类处理。
pub async fn depart_travel_for_account(account: &Value) -> Value {
    let cfg = load_checkin_config();
    let acc = ensure_fresh_token(account.clone(), &cfg).await;
    let uid = acc.get("uid").and_then(Value::as_str).map(String::from);
    let uid_ref = uid.as_deref();

    let config = fetch_travel_config(&acc).await;
    if config.get("ok").and_then(Value::as_bool) != Some(true) {
        return depart_result(
            &acc,
            uid_ref,
            false,
            false,
            Some("config-error"),
            None,
            config
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("读取旅行配置失败"),
        );
    }
    let locations = config
        .get("locations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if config.get("enabled").and_then(Value::as_bool) != Some(true) || locations.is_empty() {
        return depart_result(
            &acc,
            uid_ref,
            false,
            false,
            Some("no-location"),
            None,
            "无旅行地点",
        );
    }

    let mut last_unavailable = false;
    for location in &locations {
        let location_id = location.get("id").cloned().unwrap_or(Value::Null);
        let res = depart_travel(&acc, &location_id).await;
        if res.get("ok").and_then(Value::as_bool) == Some(true) {
            let mut result = depart_result(
                &acc,
                uid_ref,
                true,
                false,
                None,
                Some(
                    res.get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("traveling"),
                ),
                "",
            );
            if let Some(name) = nonempty_str(location.get("name").unwrap_or(&Value::Null)) {
                result["locationName"] = json!(name);
            }
            let status = fetch_travel_status(&acc).await;
            if status.get("ok").and_then(Value::as_bool) == Some(true) {
                apply_status_record(&mut result, &status);
            }
            return result;
        }

        let raw = res.get("message").and_then(Value::as_str).unwrap_or("");
        let code = res.get("code").and_then(Value::as_i64).unwrap_or(-1);
        match classify_depart_error(code, raw) {
            DepartClass::AlreadyTraveling => {
                return depart_result(
                    &acc,
                    uid_ref,
                    true,
                    true,
                    None,
                    Some("traveling"),
                    "已在旅行中",
                );
            }
            DepartClass::DailyLimit => {
                let mut result = depart_result(
                    &acc,
                    uid_ref,
                    true,
                    true,
                    Some("daily-limit"),
                    Some("idle"),
                    "今日已派",
                );
                result["claimed"] = json!(true);
                result["claimedAt"] = json!(now_ms());
                return result;
            }
            DepartClass::NoBuddy => {
                return depart_result(
                    &acc,
                    uid_ref,
                    false,
                    false,
                    Some("no-buddy"),
                    None,
                    "无 Buddy",
                );
            }
            DepartClass::LocationUnavailable => {
                last_unavailable = true;
            }
            DepartClass::Other => {
                return depart_result(
                    &acc,
                    uid_ref,
                    false,
                    false,
                    Some("error"),
                    None,
                    &truncate_message(&raw.to_lowercase()),
                );
            }
        }
    }

    if last_unavailable {
        depart_result(
            &acc,
            uid_ref,
            false,
            false,
            Some("location-unavailable"),
            None,
            "地点不可用",
        )
    } else {
        depart_result(&acc, uid_ref, false, false, Some("error"), None, "派发失败")
    }
}

fn truncate_message(msg: &str) -> String {
    msg.chars().take(80).collect()
}

fn nonempty_str(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn nonzero_credit(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Number(n) if n.as_i64() == Some(0) || n.as_f64() == Some(0.0) => None,
        other if !other.is_null() => Some(other.clone()),
        _ => None,
    }
}

fn apply_status_record(result: &mut Value, status: &Value) {
    if let Some(name) = nonempty_str(status.get("locationName").unwrap_or(&Value::Null)) {
        result["locationName"] = json!(name);
    }
    if let Some(credit) = nonzero_credit(status.get("rewardCredit").unwrap_or(&Value::Null)) {
        result["rewardCredit"] = credit;
    }
    let arrive_at = status.get("arriveAt").and_then(Value::as_i64).unwrap_or(0);
    if arrive_at > 0 {
        result["arriveAt"] = json!(arrive_at);
    }
}

fn arrive_at_secs(arrive_at: i64) -> i64 {
    if arrive_at > 1_000_000_000_000 {
        arrive_at / 1000
    } else {
        arrive_at
    }
}

/// 旅行中且已过到达时间（或缓存没有到达时间）时，需要再问一次官方 status。
fn in_flight_due(entry: &Value, now: i64) -> bool {
    if !result_in_flight(entry) {
        return false;
    }
    let arrive_at = entry.get("arriveAt").and_then(Value::as_i64).unwrap_or(0);
    let arrive_secs = arrive_at_secs(arrive_at);
    arrive_secs <= 0 || arrive_secs <= now
}

/// 官网 idle + daily_limit_reached：当日已完成，保留地点/积分只改完结标记。
fn apply_daily_limit_reached(result: &mut Value, status: &Value) {
    result["ok"] = json!(true);
    result["already"] = json!(true);
    result["skip"] = json!("daily-limit");
    result["claimed"] = json!(true);
    result["state"] = json!("idle");
    result["message"] = json!("今日已派");
    if result.get("claimedAt").and_then(Value::as_i64).unwrap_or(0) == 0 {
        result["claimedAt"] = json!(now_ms());
    }
    apply_status_record(result, status);
}

/// 合并领取状态：旧记录已领取则保留；积分按非空优先。
/// 官方状态显示新的 traveling/arrived 时，视为新行程，不把上一趟的 claimed 盖回去。
fn merge_claim_state(prior: &Value, new: &Value) -> Value {
    let mut merged = new.clone();
    if result_in_flight(&merged) {
        if nonempty_str(merged.get("locationName").unwrap_or(&Value::Null)).is_none() {
            if let Some(name) = nonempty_str(prior.get("locationName").unwrap_or(&Value::Null)) {
                merged["locationName"] = json!(name);
            }
        }
        // 上一趟已领取的积分不能带到新行程上当「预计奖励」。
        if result_in_flight(prior)
            && nonzero_credit(merged.get("rewardCredit").unwrap_or(&Value::Null)).is_none()
        {
            if let Some(credit) = nonzero_credit(prior.get("rewardCredit").unwrap_or(&Value::Null)) {
                merged["rewardCredit"] = credit;
            }
        }
        if result_in_flight(prior)
            && merged.get("arriveAt").and_then(Value::as_i64).unwrap_or(0) <= 0
        {
            if let Some(arrive_at) = prior.get("arriveAt").and_then(Value::as_i64).filter(|v| *v > 0)
            {
                merged["arriveAt"] = json!(arrive_at);
            }
        }
        return merged;
    }
    if !result_claimed(prior) {
        return merged;
    }
    let new_has_credit = nonzero_credit(merged.get("rewardCredit").unwrap_or(&Value::Null)).is_some();
    merged["claimed"] = json!(true);
    if !new_has_credit {
        merged["rewardCredit"] = prior.get("rewardCredit").cloned().unwrap_or(Value::Null);
        merged["claimedAt"] = prior.get("claimedAt").cloned().unwrap_or(json!(0));
    }
    if nonempty_str(merged.get("locationName").unwrap_or(&Value::Null)).is_none() {
        if let Some(name) = nonempty_str(prior.get("locationName").unwrap_or(&Value::Null)) {
            merged["locationName"] = json!(name);
        }
    }
    merged["state"] = json!("idle");
    merged
}

fn persist_travel_cache(overlay: &Value) {
    with_travel_cache_lock(|| {
        let mut disk = load_travel_cache();
        let today = today_str();
        let overlay_date = overlay.get("date").and_then(Value::as_str);
        if overlay_date == Some(today.as_str()) {
            roll_cache_to_today(&mut disk, &today);
            disk["date"] = json!(today);
        }
        let overlay_results = overlay
            .get("results")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut results = cache_results(&disk).cloned().unwrap_or_default();
        for (id, entry) in overlay_results {
            let merged = if let Some(prior) = results.get(&id) {
                merge_claim_state(prior, &entry)
            } else {
                entry
            };
            results.insert(id, merged);
        }
        if overlay_date == Some(today.as_str()) {
            disk["date"] = json!(today);
        } else if disk.get("date").and_then(Value::as_str).is_none() {
            if let Some(date) = overlay_date {
                disk["date"] = json!(date);
            }
        }
        disk["completed"] = json!(!has_retryable_results(&results));
        disk["results"] = Value::Object(results);
        let _ = save_travel_cache(&disk);
    });
}

fn mark_claimed(result: &mut Value, reward_credit: Option<Value>, message: Option<&str>) {
    result["claimed"] = json!(true);
    if let Some(credit) = reward_credit {
        if !credit.is_null() {
            result["rewardCredit"] = credit;
        }
    }
    result["claimedAt"] = json!(now_ms());
    result["state"] = json!("idle");
    if let Some(message) = message {
        result["message"] = json!(message);
    }
}

/// 领取单账号旅行奖励。未知/缺 state 保持未领取；idle 不默认当成已领。
async fn claim_travel_for_account(account: &Value, prior: &Value) -> Value {
    let mut result = prior.clone();
    let cfg = load_checkin_config();
    let acc = ensure_fresh_token(account.clone(), &cfg).await;

    let status = fetch_travel_status(&acc).await;
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        result["skip"] = json!("status-error");
        result["message"] = json!(status
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("查询旅行状态失败"));
        return result;
    }

    let state = status.get("state").and_then(Value::as_str).unwrap_or("");
    let record_id = status.get("recordId").and_then(Value::as_i64).unwrap_or(0);
    match state {
        "traveling" => {
            result["claimed"] = json!(false);
            result["ok"] = json!(true);
            result["state"] = json!("traveling");
            apply_status_record(&mut result, &status);
        }
        "arrived" => {
            apply_status_record(&mut result, &status);
            apply_claim_response(&acc, &mut result, record_id).await;
        }
        "idle" => {
            let daily_limit = status
                .get("dailyLimitReached")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if daily_limit {
                apply_daily_limit_reached(&mut result, &status);
            } else if result_claimed(&result) {
                result["state"] = json!("idle");
            } else if result.get("ok").and_then(Value::as_bool) == Some(true) {
                apply_claim_response(&acc, &mut result, record_id).await;
            } else {
                result["claimed"] = json!(false);
                result["state"] = json!("idle");
            }
        }
        _ => {
            result["skip"] = json!("status-error");
            result["claimed"] = json!(false);
            result["message"] = json!(format!("unknown travel state: {state}"));
        }
    }
    result
}

async fn apply_claim_response(account: &Value, result: &mut Value, record_id: i64) {
    let claim = claim_travel(account, record_id).await;
    if claim.get("ok").and_then(Value::as_bool) == Some(true) {
        mark_claimed(
            result,
            Some(claim.get("rewardCredit").cloned().unwrap_or(Value::Null)),
            None,
        );
        return;
    }
    let raw = claim
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if raw.contains("no unclaimed travel") || raw.contains("daily_limit") {
        mark_claimed(result, None, Some("已领取（网页端）"));
    } else if raw.contains("not arrived yet") {
        result["claimed"] = json!(false);
        result["ok"] = json!(true);
        result["state"] = json!("arrived");
    } else {
        result["skip"] = json!("claim-error");
        result["message"] = json!(truncate_message(&raw));
    }
}

async fn sync_account_for_dispatch(account: &Value, prior: Option<&Value>) -> Value {
    let cfg = load_checkin_config();
    let acc = ensure_fresh_token(account.clone(), &cfg).await;
    let uid = acc.get("uid").and_then(Value::as_str).map(String::from);
    let uid_ref = uid.as_deref();

    let status = fetch_travel_status(&acc).await;
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        return depart_result(
            &acc,
            uid_ref,
            false,
            false,
            Some("status-error"),
            None,
            status
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("查询旅行状态失败"),
        );
    }

    let state = status.get("state").and_then(Value::as_str).unwrap_or("");
    let daily_limit = status
        .get("dailyLimitReached")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match decide_travel_action(state, daily_limit) {
        TravelAction::WaitTraveling => {
            let mut result = depart_result(
                &acc,
                uid_ref,
                true,
                true,
                None,
                Some("traveling"),
                "已在旅行中",
            );
            apply_status_record(&mut result, &status);
            result
        }
        TravelAction::Claim => {
            let stub = prior.cloned().unwrap_or_else(|| {
                depart_result(&acc, uid_ref, true, true, None, Some("arrived"), "已到达")
            });
            claim_travel_for_account(account, &stub).await
        }
        TravelAction::SkipDailyLimit => {
            let mut result = prior.cloned().unwrap_or_else(|| {
                depart_result(
                    &acc,
                    uid_ref,
                    true,
                    true,
                    Some("daily-limit"),
                    Some("idle"),
                    "今日已派",
                )
            });
            apply_daily_limit_reached(&mut result, &status);
            result
        }
        TravelAction::Depart => depart_travel_for_account(account).await,
        TravelAction::StatusError => depart_result(
            &acc,
            uid_ref,
            false,
            false,
            Some("status-error"),
            None,
            &format!("unknown travel state: {state}"),
        ),
    }
}

/// 对所有账号依次派猫猫旅行（每日缓存幂等；存在可重试项时不标记当日完成）。
pub async fn run_travel_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&TRAVEL_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let cfg = load_travel_config();
    if cfg.get("enabled").and_then(Value::as_bool) != Some(true) {
        return json!({"status": "disabled"});
    }
    let accounts = load_accounts();
    if accounts.is_empty() {
        return json!({"status": "no_accounts"});
    }

    let today = today_str();
    let mut cache = load_travel_cache();
    roll_cache_to_today(&mut cache, &today);

    let prior_results = cache_results(&cache).cloned().unwrap_or_default();
    let mut results = Map::new();
    let mut summary_accounts = Vec::new();
    for acc in &accounts {
        let id = account_key(acc);
        let prior = prior_results.get(&id);
        let r = sync_account_for_dispatch(acc, prior).await;
        let r = if let Some(prior) = prior {
            merge_claim_state(prior, &r)
        } else {
            r
        };
        let skip = r.get("skip").and_then(Value::as_str);
        let result = if r.get("ok").and_then(Value::as_bool) == Some(true) {
            "success"
        } else {
            "error"
        };
        summary_accounts.push(json!({
            "accountId": id,
            "email": account_display_name(acc),
            "result": result,
            "skip": skip,
            "message": r.get("message").cloned().unwrap_or(Value::Null),
        }));
        results.insert(id, r);
    }

    let overlay = json!({
        "date": today,
        "results": Value::Object(results.clone()),
    });
    persist_travel_cache(&overlay);

    json!({
        "status": "ok",
        "completed": !has_retryable_results(&results),
        "accounts": summary_accounts,
    })
}

/// 检查并领取未领奖励。不绑死「今天」的 cache.date，跨日仍处理 traveling/arrived。
pub async fn run_travel_claim_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&TRAVEL_CLAIM_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let mut cache = load_travel_cache();
    let Some(results) = cache.get_mut("results").and_then(Value::as_object_mut) else {
        return json!({"status": "skipped", "reason": "nothing-to-claim"});
    };

    let ids: Vec<String> = results
        .iter()
        .filter(|(_, r)| result_in_flight(r))
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() {
        return json!({"status": "skipped", "reason": "nothing-to-claim"});
    }

    let accounts = load_accounts();
    let mut claimed = 0;
    let total = ids.len();
    let mut overlay_results = Map::new();
    for id in &ids {
        let Some(account) = accounts
            .iter()
            .find(|a| account_key(a).as_str() == id.as_str())
        else {
            continue;
        };
        let prior = results.get(id).cloned().unwrap_or_else(|| json!({}));
        let updated = claim_travel_for_account(account, &prior).await;
        if result_claimed(&updated) {
            claimed += 1;
        }
        overlay_results.insert(id.clone(), updated);
    }
    let overlay = json!({
        "date": cache.get("date").cloned().unwrap_or(Value::Null),
        "results": Value::Object(overlay_results),
    });
    persist_travel_cache(&overlay);

    json!({ "status": "ok", "total": total, "claimed": claimed })
}

fn display_record(label: &str, result: &Value) -> Value {
    let arrive_at = result.get("arriveAt").and_then(Value::as_i64).unwrap_or(0);
    json!({
        "label": label,
        "rewardCredit": nonzero_credit(result.get("rewardCredit").unwrap_or(&Value::Null))
            .unwrap_or(Value::Null),
        "locationName": nonempty_str(result.get("locationName").unwrap_or(&Value::Null))
            .map(str::to_string),
        "arriveAt": if arrive_at > 0 { json!(arrive_at) } else { Value::Null },
    })
}

fn display_label(same_day: bool, result: &Value) -> &'static str {
    if result_in_flight(result) {
        "traveling"
    } else if same_day && result.get("skip").and_then(Value::as_str) == Some("no-buddy") {
        "no-buddy"
    } else if same_day && result_claimed(result) {
        "finished"
    } else {
        "untraveled"
    }
}

/// 到点仍卡在 traveling 的缓存，按官方 status 再对一次。
/// 官网 idle + daily_limit_reached 会落成已结束，避免卡片一直显示「即将到达」。
pub async fn reconcile_due_travel(account_id: Option<&str>) {
    let cache = load_travel_cache();
    let Some(results) = cache_results(&cache) else {
        return;
    };
    let now = now_secs();
    let due: Vec<String> = results
        .iter()
        .filter(|(id, entry)| {
            if let Some(filter) = account_id {
                if filter != id.as_str() {
                    return false;
                }
            }
            in_flight_due(entry, now)
        })
        .map(|(id, _)| id.clone())
        .collect();
    if due.is_empty() {
        return;
    }

    let accounts = load_accounts();
    let mut overlay_results = Map::new();
    for id in due {
        let Some(account) = accounts
            .iter()
            .find(|a| account_key(a).as_str() == id.as_str())
        else {
            continue;
        };
        let prior = results.get(&id).cloned().unwrap_or_else(|| json!({}));
        let updated = sync_account_for_dispatch(account, Some(&prior)).await;
        let updated = merge_claim_state(&prior, &updated);
        overlay_results.insert(id, updated);
    }
    if overlay_results.is_empty() {
        return;
    }
    persist_travel_cache(&json!({
        "date": cache.get("date").cloned().unwrap_or(Value::Null),
        "results": Value::Object(overlay_results),
    }));
}

/// 某账号旅行状态的展示值：`{ label, rewardCredit, locationName }`。
///
/// label 取值：`untraveled`（未旅行）、`no-buddy`、`traveling`（旅行中）、
/// `finished`（已结束，含官网「累了，明天再来吧」）。跨日未领的 traveling/arrived 仍显示旅行中。
pub fn travel_display(account_id: &str) -> Value {
    let today = today_str();
    let cache = load_travel_cache();
    let Some(r) = cache_results(&cache).and_then(|results| results.get(account_id)) else {
        return display_record("untraveled", &json!({}));
    };
    let same_day = cache.get("date").and_then(Value::as_str) == Some(today.as_str());
    let label = display_label(same_day, r);
    if label == "untraveled" {
        display_record("untraveled", &json!({}))
    } else {
        display_record(label, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_skips_are_not_terminal() {
        assert!(is_retryable_skip(Some("no-buddy")));
        assert!(is_retryable_skip(Some("error")));
        assert!(is_retryable_skip(Some("config-error")));
        assert!(is_retryable_skip(Some("no-location")));
        assert!(is_retryable_skip(Some("location-unavailable")));
        assert!(is_retryable_skip(Some("status-error")));
        assert!(!is_retryable_skip(Some("daily-limit")));
        assert!(!is_retryable_skip(None));
    }

    #[test]
    fn display_defaults_to_untraveled_when_no_cache() {
        let value = travel_display("no-such-account");
        assert_eq!(value["label"], "untraveled");
    }

    #[test]
    fn truncate_message_caps_length() {
        assert_eq!(truncate_message("a"), "a");
        let long: String = "x".repeat(200);
        assert_eq!(truncate_message(&long).chars().count(), 80);
    }

    #[test]
    fn parse_travel_state_accepts_known_case_insensitive() {
        assert_eq!(parse_travel_state(Some("idle")), Some("idle"));
        assert_eq!(parse_travel_state(Some("Arrived")), Some("arrived"));
        assert_eq!(parse_travel_state(Some(" TRAVELING ")), Some("traveling"));
        assert_eq!(parse_travel_state(None), None);
        assert_eq!(parse_travel_state(Some("")), None);
        assert_eq!(parse_travel_state(Some("unknown")), None);
    }

    #[test]
    fn classify_depart_does_not_treat_http_429_as_daily_limit() {
        assert_eq!(
            classify_depart_error(429, "too many requests"),
            DepartClass::Other
        );
        assert_eq!(
            classify_depart_error(429, "daily limit reached"),
            DepartClass::DailyLimit
        );
        assert_eq!(
            classify_depart_error(0, "daily_limit_reached"),
            DepartClass::DailyLimit
        );
        assert_eq!(
            classify_depart_error(0, "already traveling"),
            DepartClass::AlreadyTraveling
        );
    }

    #[test]
    fn merge_keeps_prior_credit_when_new_claim_lacks_it() {
        let prior = json!({
            "claimed": true, "rewardCredit": 6, "claimedAt": 100, "state": "idle",
        });
        let new = json!({
            "claimed": true, "rewardCredit": null, "claimedAt": 200,
            "message": "已领取（网页端）",
        });
        let merged = merge_claim_state(&prior, &new);
        assert_eq!(merged["claimed"], true);
        assert_eq!(merged["rewardCredit"], 6);
        assert_eq!(merged["claimedAt"], 100);
        assert_eq!(merged["state"], "idle");
    }

    #[test]
    fn merge_does_not_cover_new_trip_with_old_claim() {
        let prior = json!({
            "claimed": true, "rewardCredit": 8, "claimedAt": 100, "state": "idle",
        });
        let new = json!({
            "ok": true, "already": true, "claimed": false, "rewardCredit": null,
            "claimedAt": 0, "state": "traveling", "message": "已在旅行中",
        });
        let merged = merge_claim_state(&prior, &new);
        assert_eq!(merged["claimed"], false);
        assert_eq!(merged["state"], "traveling");
        assert_eq!(merged["already"], true);
        assert_eq!(merged["rewardCredit"], Value::Null);
    }

    #[test]
    fn merge_keeps_claimed_without_credit() {
        let prior = json!({
            "claimed": true, "rewardCredit": null, "claimedAt": 100, "state": "idle",
        });
        let new = json!({
            "ok": false, "claimed": false, "rewardCredit": null, "state": "idle",
        });
        let merged = merge_claim_state(&prior, &new);
        assert_eq!(merged["claimed"], true);
        assert_eq!(merged["rewardCredit"], Value::Null);
        assert_eq!(merged["state"], "idle");
    }

    #[test]
    fn official_status_decides_whether_to_depart() {
        assert_eq!(
            decide_travel_action("idle", false),
            TravelAction::Depart
        );
        assert_eq!(
            decide_travel_action("idle", true),
            TravelAction::SkipDailyLimit
        );
        assert_eq!(
            decide_travel_action("traveling", true),
            TravelAction::WaitTraveling
        );
        assert_eq!(
            decide_travel_action("arrived", true),
            TravelAction::Claim
        );
    }

    #[test]
    fn merge_keeps_new_credit_over_prior_null() {
        let prior = json!({
            "claimed": true, "rewardCredit": null, "claimedAt": 100,
        });
        let new = json!({
            "claimed": true, "rewardCredit": 7, "claimedAt": 200,
        });
        let merged = merge_claim_state(&prior, &new);
        assert_eq!(merged["rewardCredit"], 7);
        assert_eq!(merged["claimedAt"], 200);
    }

    #[test]
    fn merge_noop_when_prior_unclaimed() {
        let prior = json!({ "claimed": false, "rewardCredit": null });
        let new = json!({ "claimed": false, "rewardCredit": null });
        let merged = merge_claim_state(&prior, &new);
        assert_eq!(merged["claimed"], false);
    }

    #[test]
    fn roll_cache_keeps_in_flight_and_drops_claimed() {
        let mut cache = json!({
            "date": "2026-09-08",
            "completed": true,
            "results": {
                "a": { "ok": true, "claimed": false, "state": "traveling" },
                "b": { "ok": true, "claimed": true, "rewardCredit": 4, "state": "idle" },
                "c": { "ok": false, "skip": "no-buddy" },
            }
        });
        roll_cache_to_today(&mut cache, "2026-09-09");
        assert_eq!(cache["date"], "2026-09-09");
        assert_eq!(cache["completed"], false);
        assert_eq!(cache["results"]["a"]["state"], "traveling");
        assert!(cache["results"].get("b").is_none());
        assert!(cache["results"].get("c").is_none());
    }

    #[test]
    fn in_flight_due_when_arrive_at_passed_or_missing() {
        let traveling = json!({ "ok": true, "claimed": false, "arriveAt": 100 });
        assert!(in_flight_due(&traveling, 100));
        assert!(in_flight_due(&traveling, 101));
        assert!(!in_flight_due(&traveling, 99));
        assert!(in_flight_due(
            &json!({ "ok": true, "claimed": false }),
            1
        ));
        assert!(!in_flight_due(
            &json!({ "ok": true, "claimed": true, "arriveAt": 1 }),
            100
        ));
        let millis = json!({
            "ok": true,
            "claimed": false,
            "arriveAt": 1_788_964_568_000_i64,
        });
        assert!(in_flight_due(&millis, 1_788_964_568));
        assert!(!in_flight_due(&millis, 1_788_964_567));
    }

    #[test]
    fn daily_limit_marks_finished_and_keeps_trip_details() {
        let mut result = json!({
            "ok": true,
            "claimed": false,
            "state": "traveling",
            "message": "已在旅行中",
            "locationName": "咖啡馆",
            "rewardCredit": 9,
            "arriveAt": 1788964568_i64,
        });
        apply_daily_limit_reached(
            &mut result,
            &json!({
                "locationName": "",
                "rewardCredit": 0,
                "arriveAt": 0,
            }),
        );
        assert_eq!(result["claimed"], true);
        assert_eq!(result["skip"], "daily-limit");
        assert_eq!(result["state"], "idle");
        assert_eq!(result["message"], "今日已派");
        assert_eq!(result["locationName"], "咖啡馆");
        assert_eq!(result["rewardCredit"], 9);
        assert_eq!(display_label(true, &result), "finished");
        assert_eq!(
            display_label(
                true,
                &json!({
                    "ok": true,
                    "claimed": false,
                    "state": "traveling",
                    "locationName": "咖啡馆",
                    "rewardCredit": 9,
                })
            ),
            "traveling"
        );
    }
}
