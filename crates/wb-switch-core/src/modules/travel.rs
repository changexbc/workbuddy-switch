//! 派猫猫旅行：状态机、接口封装、每日缓存与自动派发/领取奖励。
//!
//! 对照 WorkDaddy `daemon.js` + `growth-travel.js` 的派猫猫旅行实现。
//! 状态机：idle ->(depart)-> traveling ->(到点)-> arrived ->(claim)-> idle。

use chrono::Local;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::modules::account::{account_display_name, build_auth_headers, load_accounts};
use crate::modules::config::{
    http_request, load_checkin_config, load_travel_cache, load_travel_config, now_ms,
    save_travel_cache, RunFlagGuard, TRAVEL_API_PREFIX, WORKBUDDY_API_ENDPOINT,
};
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};

static TRAVEL_RUNNING: AtomicBool = AtomicBool::new(false);
static TRAVEL_CLAIM_RUNNING: AtomicBool = AtomicBool::new(false);

/// 派发周期：启动即派发，之后每 30 分钟补一轮（并重试 no-buddy / 瞬时错误）。
pub const TRAVEL_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 领取奖励检查周期：旅行到点后每隔 15 分钟检查并领取。
pub const TRAVEL_CLAIM_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// 判定为「可重试」的跳过原因：这些情况下当日不算完成，后续轮次继续重试。
///
/// 针对参考项目 Bug：账号初始无 Buddy 时缓存了 no-buddy 且标记 completed，
/// 之后账号有了 Buddy 也不会再重试，导致一直显示「无 Buddy」。
fn is_retryable_skip(skip: Option<&str>) -> bool {
    matches!(skip, Some("no-buddy") | Some("error") | Some("config-error"))
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
async fn travel_request(
    path: &str,
    method: &str,
    body: Option<Value>,
    account: &Value,
) -> Value {
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

/// 读取旅行配置（地点列表等）。返回 `{ ok, enabled, locations: [{ id, name }] }`。
async fn fetch_travel_config(account: &Value) -> Value {
    let resp = travel_request(
        &format!("{TRAVEL_API_PREFIX}/config"),
        "GET",
        None,
        account,
    )
    .await;
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

/// 读取当前旅行状态。返回 `{ ok, state, locationId, departAt, arriveAt }`。
async fn fetch_travel_status(account: &Value) -> Value {
    let resp = travel_request(
        &format!("{TRAVEL_API_PREFIX}/status"),
        "GET",
        None,
        account,
    )
    .await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 || code == 200 {
        let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
        return json!({
            "ok": true,
            "state": data.get("state").and_then(Value::as_str).unwrap_or("idle"),
            "locationId": data
                .get("location")
                .and_then(|l| l.get("id"))
                .cloned()
                .unwrap_or(Value::Null),
            "departAt": data.get("depart_at").and_then(Value::as_i64).unwrap_or(0),
            "arriveAt": data.get("arrive_at").and_then(Value::as_i64).unwrap_or(0),
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
async fn claim_travel(account: &Value) -> Value {
    let resp = travel_request(
        &format!("{TRAVEL_API_PREFIX}/claim"),
        "POST",
        Some(json!({})),
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

fn depart_result(account: &Value, uid: Option<&str>, ok: bool, already: bool, skip: Option<&str>, state: Option<&str>, message: &str) -> Value {
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

/// 对单个账号执行派猫猫旅行：取 config 第一个地点 -> depart，报错分类处理。
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
            config.get("error").and_then(Value::as_str).unwrap_or("读取旅行配置失败"),
        );
    }
    if config.get("enabled").and_then(Value::as_bool) != Some(true) {
        return depart_result(&acc, uid_ref, false, false, Some("no-location"), None, "无旅行地点");
    }
    let location_id = config
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|locations| locations.first())
        .and_then(|location| location.get("id"))
        .cloned()
        .unwrap_or(Value::Null);

    let res = depart_travel(&acc, &location_id).await;
    if res.get("ok").and_then(Value::as_bool) == Some(true) {
        return depart_result(
            &acc,
            uid_ref,
            true,
            false,
            None,
            Some(res.get("state").and_then(Value::as_str).unwrap_or("traveling")),
            "",
        );
    }

    let raw = res
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let code = res.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if raw.contains("already traveling") {
        depart_result(&acc, uid_ref, true, true, None, Some("traveling"), "已在旅行中")
    } else if raw.contains("daily limit") || code == 429 {
        depart_result(&acc, uid_ref, true, true, Some("daily-limit"), Some("traveling"), "今日已派")
    } else if raw.contains("no active buddy") {
        depart_result(&acc, uid_ref, false, false, Some("no-buddy"), None, "无 Buddy")
    } else if raw.contains("location not available") {
        depart_result(&acc, uid_ref, false, false, Some("location-unavailable"), None, "地点不可用")
    } else {
        depart_result(&acc, uid_ref, false, false, Some("error"), None, &truncate_message(&raw))
    }
}

fn truncate_message(msg: &str) -> String {
    msg.chars().take(80).collect()
}

/// 合并领取状态：旧记录已领取且有积分、新记录未领取或积分为空时，继承旧的领取结果。
///
/// 两类场景：
/// - 同日重试轮重建缓存：新 depart 结果 `claimed=false`，不能丢当日已领取状态；
/// - 并发领取（桌面端与 server CLI 同时运行，另一进程先领了）：新结果
///   `claimed=true` 但响应为“no unclaimed travel”，拿不到积分数额。
fn merge_claim_state(prior: &Value, new: &Value) -> Value {
    let mut merged = new.clone();
    let prior_has_credit = prior.get("claimed").and_then(Value::as_bool) == Some(true)
        && prior.get("rewardCredit").map(|v| !v.is_null()).unwrap_or(false);
    if !prior_has_credit {
        return merged;
    }
    let new_claimed = merged.get("claimed").and_then(Value::as_bool) == Some(true);
    let new_has_credit = merged
        .get("rewardCredit")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    if !new_claimed || !new_has_credit {
        merged["claimed"] = json!(true);
        merged["rewardCredit"] = prior.get("rewardCredit").cloned().unwrap_or(Value::Null);
        merged["claimedAt"] = prior.get("claimedAt").cloned().unwrap_or(json!(0));
        merged["state"] = json!("idle");
    }
    merged
}

/// 保存前重读磁盘并逐账号合并领取状态。
///
/// 桌面端（Tauri）与 server CLI 可能同时运行，各自“整份读-改-写”travel_cache.json；
/// 直接覆盖会把另一方刚写入的 claimed/rewardCredit 擦掉，表现为部分账号
/// 标签“已结束”但积分丢失。
fn merge_disk_claim_state(cache: &mut Value, today: &str) {
    let disk = load_travel_cache();
    if disk.get("date").and_then(Value::as_str) != Some(today) {
        return;
    }
    let Some(disk_results) = disk
        .get("results")
        .and_then(Value::as_object)
        .cloned()
    else {
        return;
    };
    let Some(results) = cache.get_mut("results").and_then(Value::as_object_mut) else {
        return;
    };
    for (id, entry) in results.iter_mut() {
        if let Some(prior) = disk_results.get(id.as_str()) {
            *entry = merge_claim_state(prior, entry);
        }
    }
}

/// 领取单账号旅行奖励。状态机：traveling=跳过；arrived=调 claim；idle=奖励已领取。
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

    let state = status.get("state").and_then(Value::as_str).unwrap_or("idle");
    match state {
        "traveling" => {
            result["claimed"] = json!(false);
            result["state"] = json!("traveling");
        }
        "arrived" => {
            let claim = claim_travel(&acc).await;
            if claim.get("ok").and_then(Value::as_bool) == Some(true) {
                result["claimed"] = json!(true);
                result["rewardCredit"] = claim.get("rewardCredit").cloned().unwrap_or(Value::Null);
                result["claimedAt"] = json!(now_ms());
                result["state"] = json!("idle");
            } else {
                let raw = claim
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                if raw.contains("no unclaimed travel") {
                    result["claimed"] = json!(true);
                    result["claimedAt"] = json!(now_ms());
                    result["state"] = json!("idle");
                    result["message"] = json!("已领取（网页端）");
                } else if raw.contains("not arrived yet") {
                    result["claimed"] = json!(false);
                    result["state"] = json!("arrived");
                } else {
                    result["skip"] = json!("claim-error");
                    result["message"] = json!(truncate_message(&raw));
                }
            }
        }
        _ => {
            // idle：今日确有派出记录但状态已复位 => 旅行已结束且奖励已领取
            result["claimed"] = json!(true);
            result["claimedAt"] = json!(now_ms());
            result["state"] = json!("idle");
        }
    }
    result
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
    let cache = load_travel_cache();
    if cache.get("date").and_then(Value::as_str) == Some(today.as_str())
        && cache.get("completed").and_then(Value::as_bool) == Some(true)
    {
        return json!({"status": "ok", "completed": true});
    }

    // 新的一天从头开始；同一天内重试时覆盖旧结果，
    // 但保留各账号当日已领取的奖励状态（claimed/rewardCredit）。
    let same_day = cache.get("date").and_then(Value::as_str) == Some(today.as_str());
    let mut results = serde_json::Map::new();
    let mut has_retryable = false;
    let mut summary_accounts = Vec::new();
    for acc in &accounts {
        let mut r = depart_travel_for_account(acc).await;
        let id = account_key(acc);
        if same_day {
            if let Some(prior) = cache.get("results").and_then(|m| m.get(id.as_str())) {
                r = merge_claim_state(prior, &r);
            }
        }
        let skip = r.get("skip").and_then(Value::as_str);
        if is_retryable_skip(skip) {
            has_retryable = true;
        }
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

    let mut cache = json!({
        "date": today,
        "completed": !has_retryable,
        "results": Value::Object(results),
    });
    merge_disk_claim_state(&mut cache, &today);
    let _ = save_travel_cache(&cache);

    json!({
        "status": "ok",
        "completed": !has_retryable,
        "accounts": summary_accounts,
    })
}

/// 对所有今天派出过旅行的账号检查并领取奖励（只查待领取的，报错跳过）。
pub async fn run_travel_claim_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&TRAVEL_CLAIM_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let today = today_str();
    let mut cache = load_travel_cache();
    if cache.get("date").and_then(Value::as_str) != Some(today.as_str()) {
        return json!({"status": "skipped", "reason": "nothing-to-claim"});
    }
    let Some(results) = cache.get_mut("results").and_then(Value::as_object_mut) else {
        return json!({"status": "skipped", "reason": "nothing-to-claim"});
    };

    // 收集待领取账号（今天派过且尚未领取）。
    let ids: Vec<String> = results
        .iter()
        .filter(|(_, r)| r.get("ok").and_then(Value::as_bool) == Some(true)
            && r.get("claimed").and_then(Value::as_bool) != Some(true))
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() {
        return json!({"status": "skipped", "reason": "nothing-to-claim"});
    }

    let accounts = load_accounts();
    let mut claimed = 0;
    let total = ids.len();
    for id in &ids {
        let Some(account) = accounts.iter().find(|a| account_key(a).as_str() == id.as_str()) else {
            continue;
        };
        let prior = results.get(id).cloned().unwrap_or_else(|| json!({}));
        let updated = claim_travel_for_account(account, &prior).await;
        if updated.get("claimed").and_then(Value::as_bool) == Some(true) {
            claimed += 1;
        }
        results.insert(id.clone(), updated);
    }
    merge_disk_claim_state(&mut cache, &today);
    let _ = save_travel_cache(&cache);

    json!({ "status": "ok", "total": total, "claimed": claimed })
}

/// 某账号今日旅行状态的展示值：`{ label, rewardCredit }`。
///
/// label 取值：`untraveled`（未旅行）、`no-buddy`、`traveling`（旅行中）、
/// `finished`（已结束+获得积分）。前端据此渲染四个状态标签。
pub fn travel_display(account_id: &str) -> Value {
    let today = today_str();
    let cache = load_travel_cache();
    if cache.get("date").and_then(Value::as_str) != Some(today.as_str()) {
        return json!({ "label": "untraveled", "rewardCredit": Value::Null });
    }
    let Some(r) = cache.get("results").and_then(|m| m.get(account_id)) else {
        return json!({ "label": "untraveled", "rewardCredit": Value::Null });
    };
    if r.get("skip").and_then(Value::as_str) == Some("no-buddy") {
        return json!({ "label": "no-buddy", "rewardCredit": Value::Null });
    }
    if r.get("claimed").and_then(Value::as_bool) == Some(true) {
        return json!({ "label": "finished", "rewardCredit": r.get("rewardCredit").cloned().unwrap_or(Value::Null) });
    }
    if r.get("ok").and_then(Value::as_bool) == Some(true) {
        return json!({ "label": "traveling", "rewardCredit": Value::Null });
    }
    json!({ "label": "untraveled", "rewardCredit": Value::Null })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_skips_are_not_terminal() {
        assert!(is_retryable_skip(Some("no-buddy")));
        assert!(is_retryable_skip(Some("error")));
        assert!(is_retryable_skip(Some("config-error")));
        assert!(!is_retryable_skip(Some("no-location")));
        assert!(!is_retryable_skip(Some("daily-limit")));
        assert!(!is_retryable_skip(Some("location-unavailable")));
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
    fn merge_restores_claim_state_on_rebuild() {
        // 重试轮重建：新 depart 结果 claimed=false，不能丢当日已领取状态。
        let prior = json!({
            "claimed": true, "rewardCredit": 8, "claimedAt": 100, "state": "idle",
        });
        let new = json!({
            "ok": true, "already": true, "claimed": false, "rewardCredit": null,
            "claimedAt": 0, "state": "traveling", "message": "已在旅行中",
        });
        let merged = merge_claim_state(&prior, &new);
        assert_eq!(merged["claimed"], true);
        assert_eq!(merged["rewardCredit"], 8);
        assert_eq!(merged["state"], "idle");
        // depart 侧信息保留
        assert_eq!(merged["already"], true);
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
}