//! 签到：状态查询 / 执行签到 / 自动签到调度。
//!
//! 对照 server.py `get_checkin_status` / `perform_checkin` /
//! `checkin_account` / `run_checkin_cycle` / `_checkin_request` /
//! `_is_unauthorized`。
//!
//! 档位策略：签到仅国内版可用（`WbVariant::supports_checkin`）。国际版没有签到
//! 接口，自动周期、一键签到与单账号签到都在发请求前统一跳过，绝不发起任何请求；
//! 下游的 inactive / statusUnsupported 判定保留为防御，不依赖它们拦截国际版。
//!
//! 时间段（`checkin_start` / `checkin_end`，本地时区）：窗口生效时，每个账号每天在
//! 窗口内按 (本地日期, 账号 id) 确定性抽一个目标分钟；只有到点未办的账号才发请求，
//! 未到点整轮不发任何请求。窗口未生效（字段为空/非法）时节奏、payload 与日志
//! 行为与既有实现逐字一致。
//!
//! 账号页「刷新并签到」按 `respect_window=true` 调用 `run_checkin_all`：窗口生效且
//! 当前时刻不在 `[start, end)` 内时整轮跳过，逐账号返回 `skipped` /
//! `outside_checkin_window`，不查状态、不提交、不写日志。设置页 / 托盘 / 单账号
//! 手动入口不传该参数（默认 `false`），保持「立即签到」语义。

use chrono::{Local, TimeZone, Timelike};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::modules::account::{
    account_display_name, build_auth_headers, envelope_token_error, load_accounts, variant_of,
};
use crate::modules::config::{
    add_checkin_log, http_request, is_route_missing, load_checkin_config, load_checkin_logs,
    now_ms, parse_clock, RunFlagGuard, CHECKIN_API_PREFIX,
};
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};
use crate::modules::variant::WbVariant;

static CHECKIN_RUNNING: AtomicBool = AtomicBool::new(false);
static CHECKIN_ACCOUNTS_RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// 上一轮是否有账号提交/查询失败（进程内，不落盘）。
static LAST_CYCLE_HAD_ERRORS: AtomicBool = AtomicBool::new(false);
/// 窗口模式下的快速重试预算；干净轮次重置为满额。
static FAST_RETRY_BUDGET: AtomicU32 = AtomicU32::new(FAST_RETRY_MAX);

/// Automatic recovery cadence shared by every host.
pub const CHECKIN_RECOVERY_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 窗口模式下到点失败后的快速重试间隔。
const FAST_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// 快速重试次数上限（12 × 5 分钟 ≈ 1 小时）；耗尽后回落 30 分钟一轮。
const FAST_RETRY_MAX: u32 = 12;

/// 窗口模式下两轮之间的最小间隔（到点即办）。
const MIN_CYCLE_DELAY: Duration = Duration::from_secs(60);

/// 未显式配置签到时间段时使用的默认分散窗口（本地时区，`[start, end)`）。
///
/// 历史行为是「空串 = 不限制时段」，于是所有账号会在宿主启动的那一刻被连续签完
/// （实测 34 个账号 61 秒内完成，平均间隔 1.8 秒）。这种密集请求与人工使用特征
/// 差异极大。改为默认窗口后，账号会按 (本地日期, 账号) 哈希分散到窗口内的某一
/// 分钟，天然错峰；用户仍可把窗口设成 `00:00`~`23:59` 恢复「不限时段」。
const DEFAULT_CHECKIN_WINDOW: (u32, u32) = (8 * 60, 22 * 60);

/// 单轮最多提交的账号数（防御性上限）。
///
/// 窗口被配置得很窄、或大量账号的当日目标时刻已经过去（例如 23:00 才启动宿主）
/// 时，单轮 `due` 集合可能很大。这里截断，其余账号留到下一轮，避免又在几十秒内
/// 打完一批；被截断的数量写入返回值的 `deferred` 字段，便于用户发现并放宽窗口。
const MAX_CHECKIN_PER_CYCLE: usize = 8;

/// 同一轮内相邻账号之间的基础间隔，进一步拉开连续请求的节奏。
const CHECKIN_STAGGER: Duration = Duration::from_millis(1800);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckinCycleMode {
    /// Always verify every account against the server after a host starts.
    StartupVerify,
    /// Re-verify every account against the server during background recovery.
    PeriodicRecovery,
}

#[derive(Debug, Eq, PartialEq)]
enum StatusDecision {
    Already,
    Submit,
    Error(String),
}

struct AccountRunGuard {
    key: String,
}

impl AccountRunGuard {
    fn try_acquire(account: &Value) -> Option<Self> {
        let key = account
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(String::from)
            .unwrap_or_else(|| account_display_name(account));
        let mut running = CHECKIN_ACCOUNTS_RUNNING
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap();
        if !running.insert(key.clone()) {
            return None;
        }
        Some(Self { key })
    }
}

impl Drop for AccountRunGuard {
    fn drop(&mut self) {
        if let Some(running) = CHECKIN_ACCOUNTS_RUNNING.get() {
            running.lock().unwrap().remove(&self.key);
        }
    }
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

/// 该响应是否应在刷新/重试判定**之前**原样返回（国际版「未开启 / 未开放 / 已过期」类业务码）。
///
/// 这类提示是业务结果，不是鉴权失败；而 `is_unauthorized` 的弱关键字含「过期」，
/// 若不先短路就会白刷一次 token 并重发一次（刷新失败还会写 `needs_relogin`）。
/// 国内版不做任何短路，语义逐字不变。
fn skips_refresh_before_retry(variant: WbVariant, resp: &Value) -> bool {
    if variant != WbVariant::Ai {
        return false;
    }
    let msg = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    is_inactive_message(msg)
}

/// 发单次签到请求；遇到未授权且存在 refresh token 时刷新一次并重试。
async fn checkin_request_once(path: &str, account: &Value, variant: WbVariant) -> Value {
    // 加密信封凭据短路：不发空 Bearer，直接给出可读错误（issue #94）。
    if let Some(err) = envelope_token_error(account) {
        return json!({"code": -2, "message": err});
    }
    let url = format!("{}{path}", variant.api_endpoint());
    let headers = build_auth_headers(account);
    let mut resp = http_request(&url, "POST", Some(json!({})), Some(&headers)).await;
    // 国际版「未开放 / 已过期」类业务码：直接返回，绝不刷新、绝不重试。
    if skips_refresh_before_retry(variant, &resp) {
        return resp;
    }
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

/// 发签到相关请求：路径候选按档位生成，**只有 404 才回落**到下一个候选。
///
/// 401/403（鉴权）、10085（网关指纹拦截）、`code=-1`（传输错误）都必须原样返回：
/// 它们不是路径问题，回落只会掩盖真因并多打一次无意义的请求。
async fn checkin_request(suffix: &str, account: &Value) -> Value {
    let variant = variant_of(account);
    let paths = variant.billing_paths(&format!("{CHECKIN_API_PREFIX}{suffix}"));
    let mut last = json!({"code": -1, "message": "无可用签到路径"});
    for (index, path) in paths.iter().enumerate() {
        let resp = checkin_request_once(path, account, variant).await;
        if index + 1 == paths.len() || !is_route_missing(&resp) {
            return resp;
        }
        last = resp;
    }
    last
}

/// 成功的状态响应 → 结果对象；非成功返回 None。
fn status_from_response(resp: &Value) -> Option<Value> {
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 && code != 200 {
        return None;
    }
    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    Some(json!({
        "ok": true,
        "todayCheckedIn": data.get("today_checked_in")
            .or_else(|| data.get("todayCheckedIn"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "raw": data,
    }))
}

/// 查询签到状态：新接口 checkin-activity-status，失败回退 checkin-status。
///
/// `checkin-activity-status` / `checkin-status` 是**国内版专有**接口；国际版没有
/// 对应实现（也没有签到本身）。因此国际版不发起任何请求，直接返回
/// `statusUnsupported: true`；签到链路的其它入口在 `checkin_account` 处统一跳过。
pub async fn get_checkin_status(account: &Value) -> Value {
    if variant_of(account) == WbVariant::Cn {
        let resp = checkin_request("/checkin-activity-status", account).await;
        if let Some(status) = status_from_response(&resp) {
            return status;
        }
        let resp2 = checkin_request("/checkin-status", account).await;
        if let Some(status) = status_from_response(&resp2) {
            return status;
        }
        let code2 = resp2.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        return json!({
            "ok": false,
            "error": resp2.get("message")
                .or_else(|| resp2.get("msg"))
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("code={code2}"))
                .to_string(),
        });
    }
    json!({
        "ok": false,
        "statusUnsupported": true,
        "error": "该档位暂无签到状态接口",
    })
}

/// 页面展示触发的状态查询遵守单账号开关；手动签到内部仍直接查询服务端。
pub async fn get_checkin_status_for_display(account: &Value) -> Value {
    with_auto_checkin_preference(account, &load_checkin_config(), get_checkin_status(account)).await
}

/// 国际版「功能不可用」类业务提示：签到活动未开启 / 未开放 / 已过期。
fn is_inactive_message(message: &str) -> bool {
    let raw = message.to_lowercase();
    [
        "未开启",
        "未开放",
        "已过期",
        "inactive",
        "not enabled",
        "not available",
    ]
    .iter()
    .any(|keyword| raw.contains(keyword))
}

/// 执行签到（POST daily-checkin）；服务端返回已签到提示按成功处理。
pub async fn perform_checkin(account: &Value) -> Value {
    let variant = variant_of(account);
    let resp = checkin_request("/daily-checkin", account).await;
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
    // 幂等业务码（已签到）保持既有 `already` 语义，两档位一致。
    if msg.contains("已签到") || msg.to_lowercase().contains("repeat") {
        return json!({"ok": true, "already": true, "message": msg});
    }
    // 国际版「功能未开启 / 未开放 / 已过期 / inactive」= 该档位未开放签到，
    // 归类为新增结果 `inactive`：绝不伪造成 success。仅对国际版生效，
    // 国内版保持既有 error 归类（零回归）。
    if variant == WbVariant::Ai && is_inactive_message(&msg) {
        return json!({"ok": false, "inactive": true, "message": msg});
    }
    json!({"ok": false, "error": msg})
}

fn decide_from_status(status: &Value) -> StatusDecision {
    // 没有状态查询接口的档位允许直接提交一次 daily-checkin，而不是把账号判成失败
    // 并让调度反复重试。安全性来自 daily-checkin 自身的幂等性——重复提交会返回
    // 「已签到」，因此这里不可能产生重复签到；结果也不会被伪造成 success。
    // 国际版当前在 `checkin_account` 入口即被跳过，此分支保留为防御。
    if status.get("statusUnsupported").and_then(Value::as_bool) == Some(true) {
        return StatusDecision::Submit;
    }
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        return StatusDecision::Error(
            status
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("查询签到状态失败")
                .to_string(),
        );
    }
    if status.get("todayCheckedIn").and_then(Value::as_bool) == Some(true) {
        StatusDecision::Already
    } else {
        StatusDecision::Submit
    }
}

/// 对单个账号执行完整签到流程：惰性刷新 → 查状态 → 未签到时提交 → 写提交日志。
pub async fn checkin_account(account: &Value) -> Value {
    let Some(_account_guard) = AccountRunGuard::try_acquire(account) else {
        return json!({"result": "error", "error": "该账号正在签到，请稍后再试"});
    };
    // 国际版没有签到接口：自动周期、一键签到与单账号签到的公开入口都在这里汇聚，
    // 统一短路以确保不向任何签到接口发起请求。
    if !variant_of(account).supports_checkin() {
        return json!({"result": "skipped", "reason": "unsupported_variant"});
    }
    let cfg = load_checkin_config();
    let acc = ensure_fresh_token(account.clone(), &cfg).await;
    let variant = variant_of(&acc).as_str();
    let status = get_checkin_status(&acc).await;
    match decide_from_status(&status) {
        StatusDecision::Already => return json!({"result": "already"}),
        StatusDecision::Error(error) => {
            return json!({"result": "error", "error": error});
        }
        StatusDecision::Submit => {}
    }

    // Only this branch submits daily-checkin, so only its outcome is eligible
    // for the sign-in log.
    let entry = json!({
        "ts": now_ms(),
        "accountId": acc.get("id").cloned().unwrap_or(Value::Null),
        "email": account_display_name(&acc),
        "variant": variant,
    });
    let res = perform_checkin(&acc).await;
    if res.get("inactive").and_then(|v| v.as_bool()) == Some(true) {
        // inactive 语义（新增）：既不能记为成功，也不应让统计页计为失败，
        // 因此**不写签到日志**（写了必然被算作 success 或 failed 之一），
        // 也不做任何重试；结果对象显式带 inactive: true 供上层区分。
        return json!({
            "result": "inactive",
            "inactive": true,
            "message": res.get("message").cloned().unwrap_or(Value::Null),
        });
    }
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
        res.get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    let mut entry_map = json!({
        "result": result,
        "ts": entry["ts"],
        "accountId": entry["accountId"],
        "email": entry["email"],
        "variant": entry["variant"],
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
        let secs = ms / 1000;
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| d.with_timezone(&Local).format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| dt.format("%Y-%m-%d").to_string())
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

/// 只匹配稳定 id，不以昵称或邮箱排除其它账号。
fn auto_checkin_excluded(account: &Value, cfg: &Value) -> bool {
    account.get("id").and_then(Value::as_str).is_some_and(|id| {
        cfg.get("excluded_account_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(id)))
    })
}

fn allows_auto_checkin(account: &Value, cfg: &Value) -> bool {
    variant_of(account).supports_checkin() && !auto_checkin_excluded(account, cfg)
}

/// 在执行异步操作前检查偏好：被排除时不 poll future，因此不会查询、刷新 token 或提交。
async fn with_auto_checkin_preference(
    account: &Value,
    cfg: &Value,
    operation: impl Future<Output = Value>,
) -> Value {
    if auto_checkin_excluded(account, cfg) {
        return json!({"ok": false, "result": "skipped", "reason": "auto_checkin_disabled"});
    }
    operation.await
}

/// 自动调度、时间段计划与批量签到共用的账号集合；单账号手动签到不经过这里。
fn auto_checkin_accounts(accounts: Vec<Value>, cfg: &Value) -> Vec<Value> {
    accounts
        .into_iter()
        .filter(|acc| allows_auto_checkin(acc, cfg))
        .collect()
}

/// 账号标识：稳定 id，缺失时回落展示名（与 `AccountRunGuard` 同口径）。
fn account_key(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .unwrap_or_else(|| account_display_name(account))
}

/// 窗口生效条件：两个字段都能解析成 `"HH:MM"` 且 `start < end`（不支持跨午夜）。
///
/// 返回当日窗口的 `[start_minute, end_minute)`；否则 `None`（= 不限制，行为与改动前一致）。
fn window_minutes(cfg: &Value) -> Option<(u32, u32)> {
    let (start_hour, start_minute) = cfg
        .get("checkin_start")
        .and_then(Value::as_str)
        .and_then(parse_clock)?;
    let (end_hour, end_minute) = cfg
        .get("checkin_end")
        .and_then(Value::as_str)
        .and_then(parse_clock)?;
    let start = start_hour * 60 + start_minute;
    let end = end_hour * 60 + end_minute;
    (start < end).then_some((start, end))
}

/// `now`（ms）的本地分钟是否落在窗口 `[start, end)` 内。
///
/// 与抽签 target 同口径：`end` 为开区间。时间戳无法解析成本地时刻时保守判为不在
/// 窗口内（真实调用方传入 `now_ms()`，不会走到这一支）。
fn now_in_window(now: i64, win: (u32, u32)) -> bool {
    Local
        .timestamp_millis_opt(now)
        .single()
        .is_some_and(|dt| (win.0..win.1).contains(&(dt.hour() * 60 + dt.minute())))
}

/// 手动批量签到的窗口跳过行：仅在 `respect_window` 且窗口生效且当前在窗口外时返回
/// `Some`（逐账号 `skipped` / `outside_checkin_window`）；否则返回 `None`，调用方走
/// 原有「立即签」路径。
///
/// 返回 `Some` 时整轮不查询状态、不提交、不写日志：行内不携带任何请求结果。
fn manual_window_skip_rows(
    accounts: &[Value],
    cfg: &Value,
    respect_window: bool,
    now: i64,
) -> Option<Vec<Value>> {
    if !respect_window {
        return None;
    }
    let win = window_minutes(cfg)?;
    if now_in_window(now, win) {
        return None;
    }
    Some(
        accounts
            .iter()
            .map(|acc| {
                json!({
                    "accountId": acc.get("id").cloned().unwrap_or(Value::Null),
                    "email": account_display_name(acc),
                    "result": "skipped",
                    "error": Value::Null,
                    "reason": "outside_checkin_window",
                })
            })
            .collect(),
    )
}

/// 本地日期 `day` 的相邻日（`delta` 为 ±1 天）。
fn shift_day(day: &str, delta: i64) -> Option<String> {
    let date = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let days = chrono::Days::new(delta.unsigned_abs());
    let shifted = if delta < 0 {
        date.checked_sub_days(days)?
    } else {
        date.checked_add_days(days)?
    };
    Some(shifted.format("%Y-%m-%d").to_string())
}

/// 某账号在 `day` 的目标分钟：窗口内按 (本地日期, 账号) 确定性抽签。
///
/// 同一天重启不变、不同账号/用户天然错峰；不引入新依赖（`rand` 已移除）。
fn target_minute(day: &str, account_key: &str, start: u32, end: u32) -> u32 {
    let mut hasher = DefaultHasher::new();
    day.hash(&mut hasher);
    account_key.hash(&mut hasher);
    start + (hasher.finish() % u64::from(end - start)) as u32
}

/// `day` 当天第 `minute` 分钟的本地时刻（ms）。
fn target_at(day: &str, minute: u32) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let naive = date.and_hms_opt(minute / 60, minute % 60, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp_millis())
}

/// 某账号在 `day` 的目标时刻（ms）。
fn target_ms_for_day(day: &str, account: &Value, win: (u32, u32)) -> Option<i64> {
    target_at(day, target_minute(day, &account_key(account), win.0, win.1))
}

/// 最近一个已应到的目标时刻：未到今日目标时取昨日目标。
fn latest_target_ms(now: i64, account: &Value, win: (u32, u32)) -> Option<i64> {
    let today = date_str(Some(now));
    let today_target = target_ms_for_day(&today, account, win)?;
    if now >= today_target {
        return Some(today_target);
    }
    target_ms_for_day(&shift_day(&today, -1)?, account, win)
}

/// 下一个目标时刻（今日未到则今日，已过则明日），用于计算睡眠时长。
fn next_target_ms(now: i64, account: &Value, win: (u32, u32)) -> Option<i64> {
    let today = date_str(Some(now));
    let today_target = target_ms_for_day(&today, account, win)?;
    if now < today_target {
        return Some(today_target);
    }
    target_ms_for_day(&shift_day(&today, 1)?, account, win)
}

/// 该账号在 `since` 之后是否已有 `success` / `already` 日志（即该窗口是否已被回应）。
fn logs_answered_window(logs: &[Value], account: &Value, since: i64) -> bool {
    let stable_id = account
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let display = account_display_name(account);
    logs.iter().any(|entry| {
        if entry
            .get("ts")
            .and_then(Value::as_i64)
            .is_none_or(|ts| ts < since)
        {
            return false;
        }
        if !matches!(
            entry.get("result").and_then(Value::as_str),
            Some("success" | "already")
        ) {
            return false;
        }
        match stable_id {
            // 稳定 id 是真源：只认 accountId，避免同一展示名互相误判。
            Some(id) => entry.get("accountId").and_then(Value::as_str) == Some(id),
            // 无 id 的账号写日志时 accountId 为空（null / 缺字段 / 空串），只能按展示名匹配。
            None => {
                entry
                    .get("accountId")
                    .and_then(Value::as_str)
                    .is_none_or(|id| id.is_empty())
                    && entry.get("email").and_then(Value::as_str) == Some(display.as_str())
            }
        }
    })
}

/// 窗口模式下该账号是否「到点未办」：最近一次目标时刻之后没有任何 success/already。
fn account_due(now: i64, account: &Value, win: (u32, u32), logs: &[Value]) -> bool {
    match latest_target_ms(now, account, win) {
        Some(latest) => !logs_answered_window(logs, account, latest),
        // 日期无法解释（极端时区 / DST 缺口）：保守当作到点未办。
        None => true,
    }
}

/// 窗口生效时只保留「到点未办」的账号；`win` 为 `None`（未生效）时原样返回全部账号。
fn due_accounts(
    accounts: Vec<Value>,
    now: i64,
    win: Option<(u32, u32)>,
    logs: &[Value],
) -> Vec<Value> {
    match win {
        Some(win) => accounts
            .into_iter()
            .filter(|acc| account_due(now, acc, win, logs))
            .collect(),
        None => accounts,
    }
}

/// 记录本轮健康度：干净轮次把快速重试预算重置满。
fn record_cycle_health(had_errors: bool) {
    LAST_CYCLE_HAD_ERRORS.store(had_errors, Ordering::Relaxed);
    if !had_errors {
        FAST_RETRY_BUDGET.store(FAST_RETRY_MAX, Ordering::Relaxed);
    }
}

/// 下一轮延迟的纯决策，返回 `(延迟, 剩余快速重试预算)`。
///
/// - 窗口未生效：既有 30 分钟节奏，预算不参与、不消耗（与改动前逐字一致）。
/// - 上一轮有 error 且预算未耗尽：5 分钟，并消耗一次预算。
/// - 否则：到最近一个目标时刻的剩余时长，夹在 `[1 分钟, 30 分钟]`；
///   上限同时充当窗口内的本地复核周期（只读本地状态，不发请求）。
fn plan_next_delay(
    had_errors: bool,
    budget: u32,
    now: i64,
    accounts: &[Value],
    win: Option<(u32, u32)>,
) -> (Duration, u32) {
    let Some(win) = win else {
        return (CHECKIN_RECOVERY_INTERVAL, budget);
    };
    if had_errors && budget > 0 {
        return (FAST_RETRY_INTERVAL, budget - 1);
    }
    let nearest = accounts
        .iter()
        .filter_map(|acc| next_target_ms(now, acc, win))
        .min();
    let delay = match nearest {
        Some(target) => Duration::from_millis((target - now).max(0) as u64)
            .clamp(MIN_CYCLE_DELAY, CHECKIN_RECOVERY_INTERVAL),
        None => CHECKIN_RECOVERY_INTERVAL,
    };
    (delay, budget)
}

/// 宿主下一轮睡眠时长。策略全在 core，宿主只读这个时长，不解释 payload。
pub fn next_cycle_delay() -> Duration {
    let cfg = load_checkin_config();
    let window = window_minutes(&cfg);
    // 窗口未生效时不必读账号库/日志：直接走既有 30 分钟节奏。
    let accounts = if window.is_some() {
        auto_checkin_accounts(load_accounts(), &cfg)
    } else {
        Vec::new()
    };
    let (delay, remaining) = plan_next_delay(
        LAST_CYCLE_HAD_ERRORS.load(Ordering::Relaxed),
        FAST_RETRY_BUDGET.load(Ordering::Relaxed),
        now_ms(),
        &accounts,
        window,
    );
    FAST_RETRY_BUDGET.store(remaining, Ordering::Relaxed);
    delay
}

/// 窗口模式下到点复核发现服务端「今日已签」→ 补一条 `already` 回执。
///
/// 回执让「本窗口已回应」跨重启成立，避免次日 00:0x 被误判为「窗口未回应」而提前签。
/// 只在该窗口尚无记录时补写（每账号每目标时刻至多一条），且仅窗口模式补写；
/// `checkin_account` / `run_checkin_all` 的公开 payload 不变。
fn write_window_receipt(win: (u32, u32), account: &Value) {
    let now = now_ms();
    let Some(latest) = latest_target_ms(now, account, win) else {
        return;
    };
    if logs_answered_window(&load_checkin_logs(), account, latest) {
        return;
    }
    add_checkin_log(&json!({
        "ts": now,
        "accountId": account.get("id").cloned().unwrap_or(Value::Null),
        "email": account_display_name(account),
        "variant": variant_of(account).as_str(),
        "result": "already",
    }));
}

/// 执行一轮自动签到。启动与周期轮次均逐账号查询服务端状态。
///
/// 只遍历支持签到且允许自动签到的账号。并发锁防止与
/// 手动签到/上一轮重复运行。
///
/// 每轮只处理「到点未办」的账号：未到点整轮不发任何请求，返回
/// `{"status":"skipped","reason":"before_checkin_time","accounts":[]}`。
/// 未配置时间段时使用 `DEFAULT_CHECKIN_WINDOW` 分散，而不是「不限时段全量签」。
pub async fn run_checkin_cycle(_mode: CheckinCycleMode) -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&CHECKIN_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let cfg = load_checkin_config();
    if cfg.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return json!({"status": "disabled"});
    }
    let accounts = auto_checkin_accounts(load_accounts(), &cfg);
    if accounts.is_empty() {
        return json!({"status": "no_accounts"});
    }
    // 未显式配置时间段时退化为默认分散窗口：避免「一启动就把全部账号打完」。
    // 用户仍可把窗口设成 00:00~23:59 来恢复「不限时段」的旧行为。
    let window = Some(window_minutes(&cfg).unwrap_or(DEFAULT_CHECKIN_WINDOW));
    let mut accounts = due_accounts(accounts, now_ms(), window, &load_checkin_logs());
    if accounts.is_empty() {
        // 整轮没有任何账号到点，不发任何请求。
        record_cycle_health(false);
        return json!({"status": "skipped", "reason": "before_checkin_time", "accounts": []});
    }
    // 单轮限流：超出上限的账号留到下一轮，避免瞬间打出一大批请求。
    let deferred = accounts.len().saturating_sub(MAX_CHECKIN_PER_CYCLE);
    if deferred > 0 {
        accounts.truncate(MAX_CHECKIN_PER_CYCLE);
    }
    let mut summary = json!({"status": "ok", "accounts": []});
    if deferred > 0 {
        summary["deferred"] = json!(deferred);
    }
    let mut had_errors = false;
    for (idx, acc) in accounts.into_iter().enumerate() {
        // 相邻账号之间拉开间隔，进一步降低连续请求的密度。
        if idx > 0 {
            tokio::time::sleep(CHECKIN_STAGGER).await;
        }
        let result = checkin_account(&acc).await;
        let outcome = result.get("result").and_then(Value::as_str);
        if outcome == Some("error") {
            had_errors = true;
        }
        if outcome == Some("already") {
            // 到点复核发现服务端已签（例如用户白天手动签过）→ 补一条窗口回执。
            if let Some(win) = window {
                write_window_receipt(win, &acc);
            }
        }
        let mut row = json!({
            "email": account_display_name(&acc),
            "result": result.get("result").cloned().unwrap_or(Value::Null),
            "error": result.get("error").cloned().unwrap_or(Value::Null),
        });
        // 只在 inactive 时追加标记：国内版结果行结构与改造前逐字一致。
        if result.get("inactive").and_then(Value::as_bool) == Some(true) {
            row["inactive"] = json!(true);
        }
        summary["accounts"].as_array_mut().unwrap().push(row);
    }
    record_cycle_health(had_errors);
    summary
}

/// True when every stored account has a today's log of `success` or `already`.
///
/// Empty account list is false so the tray keeps offering 一键签到.
pub fn all_accounts_checked_in_today() -> bool {
    accounts_checked_in_today(
        &load_accounts(),
        &load_checkin_logs(),
        &date_str(None),
        &load_checkin_config(),
    )
}

/// 判定「今天是否所有应签到的账号都已签到」。
///
/// 只有允许自动签到的账号（见 `allows_auto_checkin`）参与判定：国际版没有签到接口，
/// 关闭自动签到的账号也不会产生签到日志，若把它们算进来，托盘会永远显示「可签到」。
/// 有账号但没有任何账号需要签到（例如只装了国际版、或全部关闭自动签到）时视为无需
/// 签到，返回 true；账号库为空仍返回 false，保留「一键签到」入口。
pub fn accounts_checked_in_today(
    accounts: &[Value],
    logs: &[Value],
    today: &str,
    cfg: &Value,
) -> bool {
    if accounts.is_empty() {
        return false;
    }
    let pending: Vec<&Value> = accounts
        .iter()
        .filter(|account| allows_auto_checkin(account, cfg))
        .collect();
    if pending.is_empty() {
        return true;
    }
    pending.iter().all(|account| {
        let Some(id) = account.get("id").and_then(Value::as_str) else {
            return false;
        };
        latest_today_result(logs, id, today)
            .map(|result| result == "success" || result == "already")
            .unwrap_or(false)
    })
}

/// 给签到日志行补齐档位（宿主按档位过滤用）。
///
/// 新写入的日志自带 `variant`；历史行按当前账号库回填，账号已删除或缺失时按
/// 国内版解释（缺省即 cn，见 design D2）。纯函数，便于单测。
pub fn checkin_logs_with_variant(logs: &[Value], accounts: &[Value]) -> Vec<Value> {
    let mut known: HashMap<String, &'static str> = HashMap::new();
    for account in accounts {
        if let Some(id) = account.get("id").and_then(Value::as_str) {
            known.insert(id.to_string(), variant_of(account).as_str());
        }
    }
    logs.iter()
        .map(|entry| {
            let mut row = entry.clone();
            if row.get("variant").and_then(Value::as_str).is_none() {
                let fallback = row
                    .get("accountId")
                    .and_then(Value::as_str)
                    .and_then(|id| known.get(id).copied())
                    .unwrap_or_else(|| WbVariant::parse(None).as_str());
                row["variant"] = json!(fallback);
            }
            row
        })
        .collect()
}

/// 读取签到日志并补齐档位字段。
pub fn load_checkin_logs_with_variant() -> Vec<Value> {
    checkin_logs_with_variant(&load_checkin_logs(), &load_accounts())
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

/// 对全部账号批量签到（前端「刷新并签到」/ 一键签到）。
///
/// `variant = None` 覆盖全部档位（设置页与托盘「立即签到」语义）；
/// 显式传入时只处理该档位（账号页按当前档位触发，避免跨档位误签到）。
/// 无论哪种取值，都只处理支持签到的档位：国际版没有签到接口，绝不发起请求；
/// 关闭自动签到的账号同样跳过，并逐账号返回 skipped 原因。
///
/// `respect_window` 只由账号页「刷新并签到」传 `true`：窗口生效且当前不在
/// `[start, end)` 内时整轮跳过（逐账号 `skipped` / `outside_checkin_window`，
/// 返回 `{"status":"skipped","reason":"outside_checkin_window"}`），窗口外不发
/// 任何签到请求。设置页 / 托盘 / 单账号手动入口传 `false`，保持立即签到语义。
pub async fn run_checkin_all(variant: Option<WbVariant>, respect_window: bool) -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&CHECKIN_RUNNING) else {
        return json!({"accounts": [], "status": "skipped", "reason": "already_running"});
    };
    let cfg = load_checkin_config();
    let accounts: Vec<Value> = load_accounts()
        .into_iter()
        .filter(|acc| {
            let acc_variant = variant_of(acc);
            acc_variant.supports_checkin() && variant.is_none_or(|target| acc_variant == target)
        })
        .collect();
    if let Some(rows) = manual_window_skip_rows(&accounts, &cfg, respect_window, now_ms()) {
        return json!({"accounts": rows, "status": "skipped", "reason": "outside_checkin_window"});
    }
    json!({"accounts": checkin_all_rows(accounts, &cfg).await})
}

/// 批量签到的逐账号结果行；被排除账号不 poll 操作，因此不会发起任何请求。
async fn checkin_all_rows(accounts: Vec<Value>, cfg: &Value) -> Vec<Value> {
    let mut results: Vec<Value> = Vec::new();
    for acc in accounts {
        let r = with_auto_checkin_preference(&acc, cfg, checkin_account(&acc)).await;
        let mut row = json!({
            "accountId": acc.get("id").cloned().unwrap_or(Value::Null),
            "email": account_display_name(&acc),
            "result": r.get("result").cloned().unwrap_or(Value::Null),
            "error": r.get("error").cloned().unwrap_or(Value::Null),
        });
        if r.get("inactive").and_then(Value::as_bool) == Some(true) {
            row["inactive"] = json!(true);
        }
        if let Some(reason) = r.get("reason") {
            row["reason"] = reason.clone();
        }
        results.push(row);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归 issue #94：信封凭据的签到请求应在入口短路并返回可读错误，
    /// 不发出空 Bearer（此前会被网关 401 后把 HTML 原样回显）。
    #[tokio::test]
    async fn envelope_credentials_short_circuit_before_request() {
        let account = json!({
            "id": "envelope-only",
            "variant": "cn",
            "access_token": {"$wbEncrypted": true, "envelope": "…"},
            "refresh_token": {"$wbEncrypted": true, "envelope": "…"},
        });
        let resp = checkin_request_once("/whatever", &account, WbVariant::Cn).await;
        assert_eq!(resp["code"], -2);
        let msg = resp["message"].as_str().expect("message 应为字符串");
        assert!(msg.contains("信封"), "错误文案应可读：{msg}");
    }

    #[tokio::test]
    async fn excluded_account_never_starts_passive_operation() {
        let account = json!({"id": "excluded", "variant": "cn"});
        let cfg = json!({"excluded_account_ids": ["excluded"]});
        let result = with_auto_checkin_preference(&account, &cfg, async {
            panic!("excluded account must not query status, refresh credentials or submit checkin")
        })
        .await;
        assert_eq!(result["result"], "skipped");
        assert_eq!(result["reason"], "auto_checkin_disabled");
        assert_eq!(result["ok"], false);
        assert!(result.get("todayCheckedIn").is_none());
    }

    #[tokio::test]
    async fn reenabled_account_still_runs() {
        let account = json!({"id": "excluded", "variant": "cn"});
        for cfg in [json!({"excluded_account_ids": []}), json!({})] {
            let mut calls = 0;
            let result = with_auto_checkin_preference(&account, &cfg, async {
                calls += 1;
                json!({"result": "success"})
            })
            .await;
            assert_eq!(calls, 1);
            assert_eq!(result["result"], "success");
        }
    }

    #[tokio::test]
    async fn checkin_all_reports_skips_without_running_excluded_accounts() {
        // 全部账号都在名单里：批量路径不发起任何请求，逐账号返回 skipped 原因。
        let accounts = vec![
            json!({"id": "a", "variant": "cn"}),
            json!({"id": "c", "variant": "cn"}),
        ];
        let cfg = json!({"excluded_account_ids": ["a", "c", "unknown"]});
        let rows = checkin_all_rows(accounts, &cfg).await;
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row["result"], "skipped");
            assert_eq!(row["reason"], "auto_checkin_disabled");
        }
        assert_eq!(rows[0]["accountId"], "a");
        assert_eq!(rows[1]["accountId"], "c");
    }

    #[test]
    fn auto_checkin_excludes_only_matching_ids_and_preserves_legacy_accounts() {
        let accounts = vec![
            json!({"id": "cn-excluded", "email": "shared@example.com"}),
            json!({"id": "cn-allowed", "email": "shared@example.com"}),
            json!({"id": "ai-account", "variant": "ai"}),
            json!({"id": "legacy-account"}),
        ];
        let legacy = auto_checkin_accounts(accounts.clone(), &json!({}));
        assert_eq!(
            legacy,
            vec![
                accounts[0].clone(),
                accounts[1].clone(),
                accounts[3].clone()
            ]
        );

        let cfg = json!({"excluded_account_ids": ["cn-excluded", "deleted-account", "shared@example.com"]});
        let eligible = auto_checkin_accounts(accounts.clone(), &cfg);
        assert_eq!(eligible, vec![accounts[1].clone(), accounts[3].clone()]);
        // 排除设置不影响「是否支持签到」；单账号手动签到不经过名单过滤。
        assert!(variant_of(&accounts[0]).supports_checkin());
        assert!(!allows_auto_checkin(&accounts[0], &cfg));
        assert!(allows_auto_checkin(
            &accounts[0],
            &json!({"excluded_account_ids": []})
        ));
    }

    #[test]
    fn excluded_account_is_not_due_even_after_its_target_time() {
        let (account, target) = due_fixture();
        let cfg = json!({"excluded_account_ids": [account["id"]]});
        let accounts = auto_checkin_accounts(vec![account], &cfg);
        assert!(due_accounts(accounts.clone(), target, Some(TEST_WINDOW), &[]).is_empty());
        assert!(due_accounts(accounts.clone(), target, None, &[]).is_empty());
        // 全部排除时不再为它们的目标时刻安排唤醒。
        assert_eq!(
            plan_next_delay(
                false,
                FAST_RETRY_MAX,
                target - 60_000,
                &accounts,
                Some(TEST_WINDOW)
            ),
            (CHECKIN_RECOVERY_INTERVAL, FAST_RETRY_MAX)
        );
    }

    #[test]
    fn checked_in_status_returns_already_without_submission() {
        assert_eq!(
            decide_from_status(&json!({"ok": true, "todayCheckedIn": true})),
            StatusDecision::Already
        );
    }

    #[test]
    fn failed_status_returns_error_without_submission() {
        assert_eq!(
            decide_from_status(&json!({"ok": false, "error": "offline"})),
            StatusDecision::Error("offline".to_string())
        );
    }

    #[test]
    fn unchecked_status_submits() {
        assert_eq!(
            decide_from_status(&json!({"ok": true, "todayCheckedIn": false})),
            StatusDecision::Submit
        );
    }

    /// 有意扩展：该档位没有状态接口时允许直接提交一次（daily-checkin 自身幂等）。
    #[test]
    fn status_unsupported_variant_submits_once() {
        assert_eq!(
            decide_from_status(&json!({
                "ok": false,
                "statusUnsupported": true,
                "error": "该档位暂无签到状态接口"
            })),
            StatusDecision::Submit
        );
        // 其它失败仍然不进提交流程。
        assert_eq!(
            decide_from_status(&json!({"ok": false, "statusUnsupported": false, "error": "x"})),
            StatusDecision::Error("x".to_string())
        );
    }

    #[test]
    fn inactive_messages_are_recognized() {
        for message in [
            "功能未开启",
            "签到未开放",
            "活动已过期",
            "inactive",
            "Feature not enabled",
            "not available",
        ] {
            assert!(is_inactive_message(message), "{message}");
        }
        assert!(!is_inactive_message("签到成功"));
        assert!(!is_inactive_message("系统繁忙，请稍后重试"));
        assert!(!is_inactive_message(""));
    }

    /// 国际版「未开放 / 已过期」类业务码不得触发 token 刷新与重试（P1-1）。
    ///
    /// `checkin_request_once` 的刷新/重试是唯一会产生副作用的分支，这里直接对
    /// 该分支的前置判定（纯函数）做断言：命中即原样返回，不会走到
    /// `refresh_account_token` + 重发。
    #[test]
    fn ai_inactive_response_skips_token_refresh_and_retry() {
        for resp in [
            json!({"code": 10011, "message": "签到活动已过期"}),
            json!({"code": 1, "msg": "签到未开放"}),
            json!({"code": 1, "message": "Feature not enabled"}),
        ] {
            assert!(
                skips_refresh_before_retry(WbVariant::Ai, &resp),
                "国际版 inactive 响应必须先短路: {resp}"
            );
            // 同一响应确实会被归类为 inactive（两个判定词表一致）。
            let msg = resp
                .get("message")
                .or_else(|| resp.get("msg"))
                .and_then(|v| v.as_str())
                .unwrap();
            assert!(is_inactive_message(msg), "{msg}");
        }

        // 国内版语义逐字不变：「已过期」仍走既有 is_unauthorized 分支（允许刷新重试）。
        let cn_resp = json!({"code": 10011, "message": "签到活动已过期"});
        assert!(!skips_refresh_before_retry(WbVariant::Cn, &cn_resp));
        assert!(is_unauthorized(&cn_resp));

        // 国际版真正的鉴权失败仍然保留「一次刷新 + 一次重试」。
        assert!(!skips_refresh_before_retry(
            WbVariant::Ai,
            &json!({"code": 401, "message": "token 失效"})
        ));
        // 成功响应不短路（幂等「已签到」照常返回）。
        assert!(!skips_refresh_before_retry(
            WbVariant::Ai,
            &json!({"code": 0, "message": "签到成功"})
        ));
    }

    /// 国际版不调用国内版专有的状态接口；国内版保持两次尝试的顺序。
    #[tokio::test]
    async fn ai_account_does_not_call_cn_status_endpoints() {
        let ai = json!({"id": "ai-1", "uid": "u-1", "variant": "ai"});
        let status = get_checkin_status(&ai).await;
        assert_eq!(status["ok"], false);
        assert_eq!(status["statusUnsupported"], true);
        assert!(status.get("raw").is_none());
        // 没有产生任何状态查询响应（未发请求）。
        assert!(status.get("todayCheckedIn").is_none());
    }

    /// 国际版没有签到接口：入口即跳过，绝不发起任何请求。
    ///
    /// 守卫位于 `load_checkin_config` / `ensure_fresh_token` / 状态查询之前，
    /// 因此这里既不会触发 token 刷新，也不会触碰任何签到接口。
    #[tokio::test]
    async fn ai_account_checkin_is_skipped_without_requests() {
        let ai = json!({"id": "ai-skip-checkin", "uid": "u-ai", "variant": "ai"});
        let result = checkin_account(&ai).await;
        assert_eq!(result["result"], "skipped");
        assert_eq!(result["reason"], "unsupported_variant");
        // 不伪装成成功、失败或 inactive。
        assert!(result.get("error").is_none());
        assert!(result.get("inactive").is_none());
    }

    /// 状态成功响应解析保持原有结构（国内版零回归）。
    #[test]
    fn status_from_response_matches_legacy_shape() {
        assert_eq!(status_from_response(&json!({"code": 500})), None);
        let ok = status_from_response(&json!({
            "code": 0,
            "data": {"today_checked_in": true, "extra": 1}
        }))
        .expect("成功响应应解析");
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["todayCheckedIn"], true);
        assert_eq!(ok["raw"]["extra"], 1);

        let snake = status_from_response(&json!({
            "code": 200,
            "data": {"todayCheckedIn": false}
        }))
        .expect("camelCase 也应解析");
        assert_eq!(snake["todayCheckedIn"], false);
    }

    /// 路径候选：国际版先 /billing/meter/... 再回落 /v2/billing/meter/...。
    #[test]
    fn checkin_path_candidates_are_variant_specific() {
        assert_eq!(
            variant_of(&json!({"variant": "ai"}))
                .billing_paths(&format!("{CHECKIN_API_PREFIX}/daily-checkin")),
            vec![
                "/billing/meter/daily-checkin",
                "/v2/billing/meter/daily-checkin"
            ]
        );
        assert_eq!(
            variant_of(&json!({})).billing_paths(&format!("{CHECKIN_API_PREFIX}/daily-checkin")),
            vec!["/v2/billing/meter/daily-checkin"]
        );
    }

    #[test]
    fn same_account_cannot_acquire_two_operation_guards() {
        let account = json!({"id": "checkin-guard-test-account"});
        let first = AccountRunGuard::try_acquire(&account).expect("first operation acquires guard");
        assert!(AccountRunGuard::try_acquire(&account).is_none());
        drop(first);
        assert!(AccountRunGuard::try_acquire(&account).is_some());
    }

    #[tokio::test]
    async fn manual_all_reports_busy_when_cycle_is_running() {
        let _cycle_guard =
            RunFlagGuard::try_acquire(&CHECKIN_RUNNING).expect("test acquires cycle guard");
        let result = run_checkin_all(None, false).await;

        assert_eq!(result["accounts"], json!([]));
        assert_eq!(result["status"], "skipped");
        assert_eq!(result["reason"], "already_running");
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
        assert!(accounts_checked_in_today(
            &accounts,
            &logs,
            &today,
            &json!({})
        ));
    }

    #[test]
    fn checked_in_today_false_when_one_failed_last() {
        let accounts = vec![json!({"id": "a"})];
        let logs = vec![
            json!({"accountId": "a", "result": "success", "ts": 1_700_000_000_000_i64}),
            json!({"accountId": "a", "result": "error", "ts": 1_700_000_200_000_i64}),
        ];
        let today = date_str(Some(1_700_000_200_000));
        assert!(!accounts_checked_in_today(
            &accounts,
            &logs,
            &today,
            &json!({})
        ));
    }

    #[test]
    fn checked_in_today_false_when_empty_or_missing() {
        assert!(!accounts_checked_in_today(
            &[],
            &[],
            "2026-08-19",
            &json!({})
        ));
        let accounts = vec![json!({"id": "a"})];
        assert!(!accounts_checked_in_today(
            &accounts,
            &[],
            "2026-08-19",
            &json!({})
        ));
    }

    /// 国际版账号不会有签到日志，不得让托盘永远显示「可签到」。
    #[test]
    fn checked_in_today_ignores_variants_without_checkin() {
        let today = date_str(Some(1_700_000_000_000));
        let logs = vec![json!({
            "accountId": "cn-1",
            "result": "success",
            "ts": 1_700_000_000_000_i64
        })];

        // 仅国际版账号：没有待签到项，不再提示「可签到」。
        let ai_only = vec![json!({"id": "ai-1", "variant": "ai"})];
        assert!(accounts_checked_in_today(&ai_only, &[], &today, &json!({})));

        // 国内版已签 + 国际版无日志：国际版不拖累判定。
        let mixed = vec![
            json!({"id": "cn-1", "variant": "cn"}),
            json!({"id": "ai-1", "variant": "ai"}),
        ];
        assert!(accounts_checked_in_today(&mixed, &logs, &today, &json!({})));

        // 国内版未签 + 国际版无日志：仍需签到。
        let pending_cn = vec![
            json!({"id": "cn-2", "variant": "cn"}),
            json!({"id": "ai-1", "variant": "ai"}),
        ];
        assert!(!accounts_checked_in_today(
            &pending_cn,
            &logs,
            &today,
            &json!({})
        ));
    }

    /// 关闭自动签到的账号同样不参与判定；全部关闭时视为无需签到。
    #[test]
    fn checked_in_today_ignores_excluded_accounts() {
        let today = date_str(Some(1_700_000_000_000));
        let logs = vec![json!({
            "accountId": "cn-1",
            "result": "success",
            "ts": 1_700_000_000_000_i64
        })];

        // 全部关闭：没有待签到项，托盘显示「已签到」。
        let excluded_only = vec![json!({"id": "cn-1", "variant": "cn"})];
        assert!(accounts_checked_in_today(
            &excluded_only,
            &[],
            &today,
            &json!({"excluded_account_ids": ["cn-1"]})
        ));

        // 关闭的账号不参与判定，未关闭的账号未签时仍提示「可签到」。
        let mixed_excluded = vec![
            json!({"id": "cn-1", "variant": "cn"}),
            json!({"id": "cn-2", "variant": "cn"}),
        ];
        assert!(!accounts_checked_in_today(
            &mixed_excluded,
            &logs,
            &today,
            &json!({"excluded_account_ids": ["cn-1"]})
        ));
    }

    #[test]
    fn logs_are_tagged_with_variant_and_legacy_rows_fall_back_to_cn() {
        let accounts = vec![
            json!({"id": "cn-1"}),
            json!({"id": "ai-1", "variant": "ai"}),
        ];
        let logs = vec![
            // 新日志自带档位。
            json!({"accountId": "ai-1", "result": "success", "variant": "ai"}),
            // 历史日志按账号库回填。
            json!({"accountId": "ai-1", "result": "success"}),
            json!({"accountId": "cn-1", "result": "success"}),
            // 账号已删除：按缺省国内版解释。
            json!({"accountId": "gone", "result": "success"}),
            json!({"email": "legacy@example.com", "result": "success"}),
        ];

        let rows = checkin_logs_with_variant(&logs, &accounts);
        assert_eq!(rows.len(), logs.len());
        assert_eq!(rows[0]["variant"], "ai");
        assert_eq!(rows[1]["variant"], "ai");
        assert_eq!(rows[2]["variant"], "cn");
        assert_eq!(rows[3]["variant"], "cn");
        assert_eq!(rows[4]["variant"], "cn");
    }

    // -----------------------------------------------------------------------
    // 签到时间段：抽签 / 窗口 / due 判定 / 延迟
    // -----------------------------------------------------------------------

    /// 设计口径样例窗口：22:00–23:30。
    const TEST_WINDOW: (u32, u32) = (22 * 60, 23 * 60 + 30);

    fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("test timestamp must be unambiguous")
            .timestamp_millis()
    }

    fn log_row(account_id: &str, result: &str, ts: i64) -> Value {
        json!({"accountId": account_id, "email": account_id, "result": result, "ts": ts})
    }

    /// 某天某账号在样例窗口内的目标时刻（ms）。
    fn day_target_ms(day: &str, key: &str) -> i64 {
        target_at(day, target_minute(day, key, TEST_WINDOW.0, TEST_WINDOW.1))
            .expect("目标时刻必须可解析")
    }

    /// 样例窗口的账号与今日目标时刻。
    fn due_fixture() -> (Value, i64) {
        (json!({"id": "due-a"}), day_target_ms("2026-09-21", "due-a"))
    }

    /// `LAST_CYCLE_HAD_ERRORS` / `FAST_RETRY_BUDGET` 是进程内全局状态，
    /// 只有本文件的健康度用例会写它，串行化即可。
    static RETRY_STATE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn target_minute_is_stable_inside_window_and_spread_across_accounts_and_days() {
        let (start, end) = TEST_WINDOW;
        let day = "2026-09-21";
        let anchor = target_minute(day, "acc-a", start, end);
        assert!(
            (start..end).contains(&anchor),
            "抽签必须落在窗口内: {anchor}"
        );
        // 同一天跨重启不变（确定性抽签，与进程内状态无关）。
        assert_eq!(anchor, target_minute(day, "acc-a", start, end));
        // 单分钟窗口只能落在该分钟（不 panic、不越界）。
        assert_eq!(target_minute(day, "acc-a", 60, 61), 60);
        assert_eq!(target_minute(day, "acc-a", 1439, 1440), 1439);

        let accounts: HashSet<u32> = ["a", "b", "c", "d", "e", "f", "g", "h"]
            .iter()
            .map(|key| target_minute(day, key, start, end))
            .collect();
        assert!(accounts.len() > 1, "不同账号必须错峰: {accounts:?}");
        assert!(accounts.iter().all(|minute| (start..end).contains(minute)));

        let days: HashSet<u32> = ["2026-09-21", "2026-09-22", "2026-09-23", "2026-09-24"]
            .iter()
            .map(|day| target_minute(day, "acc-a", start, end))
            .collect();
        assert!(days.len() > 1, "不同日期必须重新抽签: {days:?}");
    }

    #[test]
    fn window_minutes_requires_both_bounds_and_start_before_end() {
        // 未设置 / 缺失 / 只填一个 / 非法 / start >= end 一律按不限制处理。
        for cfg in [
            json!({}),
            json!({"checkin_start": "", "checkin_end": ""}),
            json!({"checkin_start": "22:00", "checkin_end": ""}),
            json!({"checkin_start": "", "checkin_end": "23:30"}),
            json!({"checkin_start": "22:00"}),
            json!({"checkin_end": "23:30"}),
            json!({"checkin_start": "22:00", "checkin_end": "25:00"}),
            json!({"checkin_start": "22:00", "checkin_end": "abc"}),
            json!({"checkin_start": 2200, "checkin_end": 2330}),
            json!({"checkin_start": "23:30", "checkin_end": "22:00"}),
            json!({"checkin_start": "22:00", "checkin_end": "22:00"}),
        ] {
            assert_eq!(window_minutes(&cfg), None, "必须按不限制处理: {cfg}");
        }

        assert_eq!(
            window_minutes(&json!({"checkin_start": "22:00", "checkin_end": "23:30"})),
            Some((22 * 60, 23 * 60 + 30))
        );
        // 1–2 位写法同样生效；想签在 0 点后填 00:00–01:00（不支持跨午夜）。
        assert_eq!(
            window_minutes(&json!({"checkin_start": "9:5", "checkin_end": "10:00"})),
            Some((9 * 60 + 5, 10 * 60))
        );
        assert_eq!(
            window_minutes(&json!({"checkin_start": "0:0", "checkin_end": "1:0"})),
            Some((0, 60))
        );
    }

    /// 手动入口窗口判定边界：`[start, end)`，end 整点已在窗口外。
    #[test]
    fn now_in_window_covers_window_boundaries() {
        let win = TEST_WINDOW;
        assert!(
            !now_in_window(local_ms(2026, 9, 21, 21, 59), win),
            "start 前必须判为窗口外"
        );
        assert!(
            now_in_window(local_ms(2026, 9, 21, 22, 0), win),
            "start 整点必须判为窗口内"
        );
        assert!(
            now_in_window(local_ms(2026, 9, 21, 22, 45), win),
            "窗口内必须判为窗口内"
        );
        assert!(
            now_in_window(local_ms(2026, 9, 21, 23, 29), win),
            "end-1 分必须判为窗口内"
        );
        assert!(
            !now_in_window(local_ms(2026, 9, 21, 23, 30), win),
            "end 整点是开区间，必须判为窗口外"
        );
        assert!(
            !now_in_window(local_ms(2026, 9, 21, 23, 31), win),
            "end 后必须判为窗口外"
        );
    }

    /// 手动批量签到的窗口跳过行：仅 `respect_window + 窗口生效 + 窗口外` 才整轮跳过。
    #[test]
    fn manual_window_skip_rows_only_when_respecting_effective_window_outside() {
        let cfg = json!({"checkin_start": "22:00", "checkin_end": "23:30"});
        let accounts = vec![
            json!({"id": "cn-a", "email": "a@example.com"}),
            json!({"id": "cn-b"}),
        ];
        let outside = local_ms(2026, 9, 21, 12, 0);
        let inside = local_ms(2026, 9, 21, 22, 30);

        // respect_window=false：任何时刻都走立即签路径。
        assert!(manual_window_skip_rows(&accounts, &cfg, false, outside).is_none());
        // 窗口未生效（未设置）：不限制，照常立即签。
        assert!(manual_window_skip_rows(&accounts, &json!({}), true, outside).is_none());
        // 窗口生效且在窗口内：照常立即签。
        assert!(manual_window_skip_rows(&accounts, &cfg, true, inside).is_none());

        // 窗口生效且在窗口外：逐账号 skipped 行，整轮不发任何签到请求。
        let rows = manual_window_skip_rows(&accounts, &cfg, true, outside).expect("窗口外必须跳过");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["accountId"], "cn-a");
        assert_eq!(rows[0]["email"], "a@example.com");
        assert_eq!(rows[1]["accountId"], "cn-b");
        for row in &rows {
            assert_eq!(row["result"], "skipped");
            assert_eq!(row["reason"], "outside_checkin_window");
            assert_eq!(row["error"], Value::Null);
        }

        // 空账号列表同样按跳过返回空行集。
        assert_eq!(
            manual_window_skip_rows(&[], &cfg, true, outside),
            Some(vec![])
        );
    }

    #[test]
    fn target_boundaries_are_stable_across_restarts() {
        let (account, target) = due_fixture();
        let yesterday = day_target_ms("2026-09-20", "due-a");
        let tomorrow = day_target_ms("2026-09-22", "due-a");

        // 未到今日目标 → 最近目标是昨日，下一个目标是今日。
        assert_eq!(
            latest_target_ms(target - 1, &account, TEST_WINDOW),
            Some(yesterday)
        );
        assert_eq!(
            next_target_ms(target - 1, &account, TEST_WINDOW),
            Some(target)
        );
        // 已过今日目标 → 最近目标是今日，下一个目标是明日。
        assert_eq!(
            latest_target_ms(target + 1, &account, TEST_WINDOW),
            Some(target)
        );
        assert_eq!(
            next_target_ms(target + 1, &account, TEST_WINDOW),
            Some(tomorrow)
        );
        // 目标本身 = 到点时刻。
        assert_eq!(
            latest_target_ms(target, &account, TEST_WINDOW),
            Some(target)
        );
    }

    /// 常驻场景：窗口内到点前不 due（不发请求）；到点未办即 due。
    #[test]
    fn account_due_waits_for_today_target_then_fires() {
        let (account, target) = due_fixture();
        let answered_yesterday = vec![log_row(
            "due-a",
            "success",
            day_target_ms("2026-09-20", "due-a") + 60_000,
        )];

        assert!(
            !account_due(target - 60_000, &account, TEST_WINDOW, &answered_yesterday),
            "到点前不得发请求"
        );
        assert!(account_due(
            target + 60_000,
            &account,
            TEST_WINDOW,
            &answered_yesterday
        ));

        let mut answered = answered_yesterday;
        answered.push(log_row("due-a", "success", target + 30_000));
        assert!(!account_due(
            target + 60_000,
            &account,
            TEST_WINDOW,
            &answered
        ));
    }

    /// 整夜未运行 / 每天仅在窗口外开机 / 目标已过的新账号 → 立即补一轮。
    #[test]
    fn missed_window_and_new_accounts_are_due_immediately() {
        let (account, target) = due_fixture();
        assert!(account_due(
            local_ms(2026, 9, 22, 8, 0),
            &account,
            TEST_WINDOW,
            &[]
        ));
        assert!(account_due(
            local_ms(2026, 9, 22, 10, 0),
            &account,
            TEST_WINDOW,
            &[]
        ));
        assert!(account_due(target + 60_000, &account, TEST_WINDOW, &[]));

        // 昨日已回应不顶替今日目标：次日凌晨仍要先补昨日那一轮。
        let answered_yesterday = vec![log_row(
            "due-a",
            "success",
            day_target_ms("2026-09-20", "due-a") + 60_000,
        )];
        assert!(account_due(
            local_ms(2026, 9, 22, 8, 0),
            &account,
            TEST_WINDOW,
            &answered_yesterday
        ));
    }

    /// 目标前手动签到：到点仍 due（复核后写 already 回执），回执使窗口跨重启成立。
    #[test]
    fn manual_checkin_before_target_is_answered_by_window_receipt() {
        let (account, target) = due_fixture();
        let manual = vec![log_row("due-a", "success", local_ms(2026, 9, 21, 14, 0))];
        assert!(account_due(target + 60_000, &account, TEST_WINDOW, &manual));

        let mut with_receipt = manual;
        with_receipt.push(log_row("due-a", "already", target + 60_000));
        assert!(!account_due(
            target + 60_000,
            &account,
            TEST_WINDOW,
            &with_receipt
        ));
        // 跨重启后（次日同一目标时刻之前）仍不 due，不会被拉回 00:0x。
        assert!(!account_due(
            local_ms(2026, 9, 22, 0, 30),
            &account,
            TEST_WINDOW,
            &with_receipt
        ));

        // error 行与其它账号的行都不算回应。
        assert!(account_due(
            target + 60_000,
            &account,
            TEST_WINDOW,
            &[log_row("due-a", "error", target + 60_000)]
        ));
        assert!(account_due(
            target + 60_000,
            &account,
            TEST_WINDOW,
            &[log_row("other", "success", target + 60_000)]
        ));
    }

    /// 窗口回执判定：只认该账号在该窗口之后的 success/already，不跨账号串味。
    #[test]
    fn window_receipt_predicate_matches_same_account_after_target_only() {
        let (account, target) = due_fixture();
        assert!(!logs_answered_window(&[], &account, target));
        assert!(
            !logs_answered_window(
                &[log_row("due-a", "success", target - 60_000)],
                &account,
                target
            ),
            "目标之前的记录不算本窗口回应"
        );
        assert!(logs_answered_window(
            &[log_row("due-a", "already", target + 60_000)],
            &account,
            target
        ));

        // 无稳定 id 的账号：日志行 accountId 为空，按展示名匹配。
        let legacy = json!({"email": "legacy@example.com"});
        let legacy_rows = vec![json!({
            "accountId": null,
            "email": "legacy@example.com",
            "result": "success",
            "ts": target + 1
        })];
        assert!(logs_answered_window(&legacy_rows, &legacy, target));
        assert!(!logs_answered_window(
            &legacy_rows,
            &json!({"email": "other@example.com"}),
            target
        ));

        // 空串 accountId 与缺字段 / null 同口径（无稳定 id），按展示名匹配。
        let empty_id_account = json!({"id": "", "email": "legacy@example.com"});
        let empty_id_rows = vec![json!({
            "accountId": "",
            "email": "legacy@example.com",
            "result": "already",
            "ts": target + 1
        })];
        assert!(logs_answered_window(
            &empty_id_rows,
            &empty_id_account,
            target
        ));
        assert!(logs_answered_window(&empty_id_rows, &legacy, target));
        assert!(!logs_answered_window(
            &empty_id_rows,
            &json!({"id": "due-a", "email": "legacy@example.com"}),
            target
        ));
    }

    #[test]
    fn unresolvable_target_is_treated_as_due() {
        assert_eq!(target_at("not-a-date", 0), None);
        assert_eq!(target_at("2026-09-21", 24 * 60), None);

        // minute=1440 无法表示为本地 HMS（hour=24），与 DST 缺口一样让 target_at 返回 None。
        let win = (24 * 60, 24 * 60 + 1);
        let account = json!({"id": "gap"});
        let now = local_ms(2026, 9, 21, 12, 0);
        assert_eq!(latest_target_ms(now, &account, win), None);
        assert!(
            account_due(now, &account, win, &[]),
            "target_at 失败必须保守当作到点未办，避免漏天"
        );
        assert_eq!(
            plan_next_delay(false, FAST_RETRY_MAX, now, &[account], Some(win)),
            (CHECKIN_RECOVERY_INTERVAL, FAST_RETRY_MAX),
            "无未来目标时回落 30 分钟，不得退化成更短空转"
        );
    }

    /// 未设置（或非法）窗口时不得过滤任何账号：与改动前「每轮处理全部账号」一致。
    #[test]
    fn unset_or_invalid_window_never_filters_accounts() {
        let accounts = vec![json!({"id": "due-a"}), json!({"id": "due-b"})];
        let (account, target) = due_fixture();
        let answered = vec![log_row("due-a", "success", target + 60_000)];
        let now = target + 120_000;

        for cfg in [
            json!({}),
            json!({"checkin_start": "", "checkin_end": ""}),
            json!({"checkin_start": "22:00"}),
            json!({"checkin_start": "23:30", "checkin_end": "22:00"}),
            json!({"checkin_start": "22:00", "checkin_end": "25:00"}),
        ] {
            assert_eq!(
                due_accounts(accounts.clone(), now, window_minutes(&cfg), &answered),
                accounts,
                "未设置窗口必须原样返回全部账号: {cfg}"
            );
        }

        // 对照：窗口生效时，已回应的账号被剔除，其余保留。
        let due = due_accounts(accounts, now, Some(TEST_WINDOW), &answered);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0]["id"], json!("due-b"));
        assert!(!account_due(now, &account, TEST_WINDOW, &answered));
    }

    /// 延迟规则：30 分钟基线、5 分钟快速重试（预算递减）、耗尽回落、1 分钟下限。
    #[test]
    fn plan_next_delay_covers_window_and_retry_rules() {
        let (account, target) = due_fixture();
        let accounts = vec![account];

        // 窗口未生效：既有 30 分钟节奏，快速重试预算不参与也不消耗。
        assert_eq!(
            plan_next_delay(true, 3, target, &accounts, None),
            (CHECKIN_RECOVERY_INTERVAL, 3)
        );
        assert_eq!(
            plan_next_delay(true, 3, target, &[], None),
            (CHECKIN_RECOVERY_INTERVAL, 3)
        );

        // 上一轮有 error 且预算未耗尽：5 分钟，并消耗一次预算。
        assert_eq!(
            plan_next_delay(true, FAST_RETRY_MAX, target, &accounts, Some(TEST_WINDOW)),
            (FAST_RETRY_INTERVAL, FAST_RETRY_MAX - 1)
        );
        assert_eq!(
            plan_next_delay(true, 1, target, &accounts, Some(TEST_WINDOW)),
            (FAST_RETRY_INTERVAL, 0)
        );

        // 预算耗尽 → 回落；此时最近目标是明日 → 上限 30 分钟（当天继续尝试）。
        assert_eq!(
            plan_next_delay(true, 0, target + 60_000, &accounts, Some(TEST_WINDOW)),
            (CHECKIN_RECOVERY_INTERVAL, 0)
        );

        // 无待办（最近目标是明日的 22:00–23:30）→ 30 分钟上限，本地复核周期。
        assert_eq!(
            plan_next_delay(
                false,
                FAST_RETRY_MAX,
                target + 60_000,
                &accounts,
                Some(TEST_WINDOW)
            ),
            (CHECKIN_RECOVERY_INTERVAL, FAST_RETRY_MAX)
        );

        // 到点未办：目标已在 1 分钟内 → 1 分钟下限，立刻处理。
        assert_eq!(
            plan_next_delay(
                false,
                FAST_RETRY_MAX,
                target - 30_000,
                &accounts,
                Some(TEST_WINDOW)
            ),
            (MIN_CYCLE_DELAY, FAST_RETRY_MAX)
        );
        assert_eq!(
            plan_next_delay(false, FAST_RETRY_MAX, target, &accounts, Some(TEST_WINDOW)),
            (CHECKIN_RECOVERY_INTERVAL, FAST_RETRY_MAX)
        );

        // 中间值：目标在 10 分钟后 → 正好睡到目标时刻。
        assert_eq!(
            plan_next_delay(
                false,
                FAST_RETRY_MAX,
                target - 10 * 60_000,
                &accounts,
                Some(TEST_WINDOW)
            ),
            (Duration::from_secs(10 * 60), FAST_RETRY_MAX)
        );

        // 无账号 / 窗口生效但无账号 → 回落到 30 分钟，不 panic。
        assert_eq!(
            plan_next_delay(false, FAST_RETRY_MAX, target, &[], Some(TEST_WINDOW)),
            (CHECKIN_RECOVERY_INTERVAL, FAST_RETRY_MAX)
        );
    }

    /// 干净轮次重置预算；有错误时保留（由 `next_cycle_delay` 逐次消耗）。
    #[test]
    fn cycle_health_resets_fast_retry_budget_only_when_clean() {
        let _serial = RETRY_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        record_cycle_health(true);
        assert!(LAST_CYCLE_HAD_ERRORS.load(Ordering::Relaxed));
        assert_eq!(FAST_RETRY_BUDGET.load(Ordering::Relaxed), FAST_RETRY_MAX);

        FAST_RETRY_BUDGET.store(2, Ordering::Relaxed);
        record_cycle_health(true);
        assert_eq!(
            FAST_RETRY_BUDGET.load(Ordering::Relaxed),
            2,
            "有 error 的轮次不得重置预算"
        );

        record_cycle_health(false);
        assert!(!LAST_CYCLE_HAD_ERRORS.load(Ordering::Relaxed));
        assert_eq!(FAST_RETRY_BUDGET.load(Ordering::Relaxed), FAST_RETRY_MAX);
    }
}
