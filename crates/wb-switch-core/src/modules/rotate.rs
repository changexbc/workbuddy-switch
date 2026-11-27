//! CodeBuddy CLI 账号自动轮换（积分到期与模型限流）。
//!
//! 后台周期检查所有账号的积分到期情况，把 CodeBuddy CLI 切到"最紧迫"的账号
//! （最早到期且仍有剩余积分），防止积分过期浪费。国内/国际是同一套 CLI，
//! 候选不按档位过滤；切号时由 `switch_active_account` 同步区域环境变量。
//! 仅切换 CodeBuddy CLI。当前账号遇到模型限流时，跳过积分紧迫度、到期差异和最小积分阈值。
//!
//! 防抖动约束（核心）：
//! - 冷却期：切换后 cooldown_minutes 内不重复切；
//! - 到期差异阈值：目标比当前账号早到期超过 min_gap_hours 才切，
//!   避免"三个账号都是明天到期"时来回切换。
//!
//! 存活门控与推迟提示（本轮起）：
//! - 只要存在**心跳新鲜**的 CLI 会话（`~/.codebuddy/sessions/*.json`）就跳过本次轮换：
//!   活进程持的是旧 key，切了也不生效，还会破坏「活进程 key == 当前账号」的不变式；
//!   旧版的「活跃保护」（按 transcript mtime 的 30 分钟窗口）已被它取代——那个判据
//!   管不住"开着但闲置"的会话，还会全量扫 `~/.codebuddy/projects`。
//! - 被门控拦下、但"除门控外本来会切换"时提示一次（受每日预算约束，见 `config`）。

use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::atomic::{AtomicI64, Ordering},
};

use crate::modules::account;
use crate::modules::codebuddy_cli;
use crate::modules::config::{
    add_rotate_log, atomic_write, load_auto_rotate_config, load_rotate_logs, now_ms, store_dir,
    try_consume_rotate_notify, RotateNotifyClaim,
};
use crate::modules::credits;
use crate::modules::limits::Resolved;
use crate::modules::rate_limit_events::{self, HookSource};
use crate::modules::session_link::{try_lock_file, LockError};
use crate::modules::variant::WbVariant;

static LAST_CHECK_AT: AtomicI64 = AtomicI64::new(0);
static LAST_SWITCH_AT: AtomicI64 = AtomicI64::new(0);
const LAST_SWITCH_FILE: &str = "rotate_last_switch.json";

fn load_last_switch_at(path: &Path) -> Option<i64> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<i64>(&text).ok().filter(|&ts| ts > 0)
}

fn save_last_switch_at(path: &Path, timestamp: i64) -> std::io::Result<()> {
    atomic_write(path, &timestamp.to_string())
}

fn last_switch_at(path: &Path) -> Option<i64> {
    let local = LAST_SWITCH_AT.load(Ordering::SeqCst);
    let saved = load_last_switch_at(path).or_else(|| {
        // 升级后首次读取时沿用旧版轮换日志里的成功切换时间。
        load_rotate_logs()
            .iter()
            .filter(|entry| entry["action"] == "switched")
            .filter_map(|entry| entry["ts"].as_i64().filter(|&ts| ts > 0))
            .max()
    });
    saved.max((local > 0).then_some(local))
}

/// 一天的毫秒数（新代码里用它代替裸写的 `24 * 3_600_000`）。
const DAY_MS: i64 = 24 * 3_600_000;

/// 存活门控跳过时的固定原因：轮换日志与"是否该提示"的判定都认这一条。
const SKIP_LIVE_SESSION_REASON: &str = "有 CodeBuddy CLI 会话在运行，暂不切换";

/// 推迟提示的通知标题（返回体里的 `notify.title` 与宿主投递用的是同一个值）。
const ROTATE_NOTIFY_TITLE: &str = "workbuddy-switch";

/// 单个账号的积分候选（从 get_credit_expiry 提取，不携带 token）。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub account_id: String,
    pub display_name: String,
    /// 剩余积分中最早到期时间（毫秒）；无剩余积分资源时为 None。
    pub soonest_expire_at: Option<i64>,
    pub total_remaining: f64,
    /// 查询成功、未过期、有剩余积分 → 可被选为目标。
    pub valid: bool,
    pub error: Option<String>,
}

impl Candidate {
    /// 紧迫度排序键：到期越早越紧迫；无到期时间排最后。
    fn urgency_key(&self) -> (i64, i64) {
        match self.soonest_expire_at {
            Some(ts) => (0, ts),
            None => (1, 0),
        }
    }
}

/// 从积分查询结果提取候选（失败时生成 invalid 候选，供日志/防抖动参照）。
fn to_candidate(account: &Value, credit: &Value) -> Candidate {
    let account_id = account
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let display_name = account_display_name_or(account);
    let ok = credit.get("ok").and_then(|v| v.as_bool()) == Some(true);
    let expired = credit.get("expired").and_then(|v| v.as_bool()) == Some(true);
    let total_remaining = credit
        .get("totalRemaining")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let soonest_expire_at = credit.get("soonestExpireAt").and_then(|v| v.as_i64());
    let error = credit
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Candidate {
        valid: ok && !expired && total_remaining > 0.0,
        account_id,
        display_name,
        soonest_expire_at,
        total_remaining,
        error,
    }
}

fn account_display_name_or(account: &Value) -> String {
    account::account_display_name(account)
}

/// 测试辅助：按档位切分账号。生产轮换不再过滤，因为同一套 CLI 跨档位共用。
#[cfg(test)]
fn candidates_for_variant(accounts: &[Value], variant: WbVariant) -> Vec<Value> {
    accounts
        .iter()
        .filter(|account| WbVariant::from_account(account) == variant)
        .cloned()
        .collect()
}

/// 决策结果。
#[derive(Debug, PartialEq)]
pub enum Decision {
    /// 不切，附原因。
    Skip(String),
    /// 切到目标账号 id。
    Switch(String),
}

pub struct RotationInputs<'a> {
    pub current_account_id: Option<&'a str>,
    pub now_ms: i64,
    pub last_switch_at_ms: Option<i64>,
    pub cooldown_ms: i64,
    pub min_gap_ms: i64,
    pub min_urgency_ms: i64,
    pub has_live_session: bool,
    pub min_remaining: f64,
    pub rate_limited: HashSet<&'a str>,
}

/// 用同一时刻评估候选、到期阈值和冷却期。限流时仍要求目标有剩余积分。
pub fn decide_target(candidates: &[Candidate], inputs: &RotationInputs<'_>) -> Decision {
    let current_rate_limited = inputs
        .current_account_id
        .is_some_and(|id| inputs.rate_limited.contains(id));
    let target = candidates
        .iter()
        .filter(|c| c.valid && !inputs.rate_limited.contains(c.account_id.as_str()))
        .min_by_key(|c| c.urgency_key());
    let Some(target) = target else {
        let reason = if candidates.iter().any(|c| c.valid) {
            "没有可用账号（有剩余积分的账号均处于模型限流）"
        } else if current_rate_limited {
            "当前账号模型限流，且没有可用账号（查询失败/已过期/无剩余积分）"
        } else {
            "没有可用账号（查询失败/已过期/无剩余积分）"
        };
        return Decision::Skip(reason.to_string());
    };

    if let Some(target_ts) = target.soonest_expire_at.filter(|_| !current_rate_limited) {
        let remaining_ms = target_ts - inputs.now_ms;
        if remaining_ms > inputs.min_urgency_ms {
            return Decision::Skip(format!(
                "所有账号到期都还早（最紧迫的还剩 {} 天），无需切换",
                remaining_ms / DAY_MS
            ));
        }
    }
    if inputs.current_account_id == Some(target.account_id.as_str()) {
        return Decision::Skip("当前账号已是最紧迫账号".to_string());
    }
    if let Some(ts) = inputs.last_switch_at_ms {
        if ts.saturating_add(inputs.cooldown_ms) > inputs.now_ms {
            return Decision::Skip("处于切换冷却期".to_string());
        }
    }
    // CLI 活进程持旧 key，切号会破坏「活进程 key == 当前账号」。
    if inputs.has_live_session {
        return Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string());
    }
    if current_rate_limited {
        return Decision::Switch(target.account_id.clone());
    }
    // 只为节约积分而轮换时，目标须达到最低剩余积分和到期差异阈值。
    if inputs.min_remaining > 0.0 && target.total_remaining < inputs.min_remaining {
        return Decision::Skip(format!(
            "目标账号剩余积分不足（{}，阈值 {}），不值得切换",
            target.total_remaining, inputs.min_remaining
        ));
    }
    if let Some(current) = inputs
        .current_account_id
        .and_then(|id| candidates.iter().find(|c| c.account_id == id))
    {
        if let (Some(cur_ts), Some(target_ts)) =
            (current.soonest_expire_at, target.soonest_expire_at)
        {
            if cur_ts > 0 && target_ts < cur_ts && cur_ts - target_ts < inputs.min_gap_ms {
                return Decision::Skip(format!(
                    "目标到期仅早 {} 小时，未达切换阈值（{} 小时）",
                    (cur_ts - target_ts) / 3_600_000,
                    inputs.min_gap_ms / 3_600_000
                ));
            }
        }
    }
    Decision::Switch(target.account_id.clone())
}

/// 目标账号剩余到期天数（向上取整，至少 1）：提示文案里的「N 天后到期」。
fn remaining_days_until(expire_at: i64, now: i64) -> i64 {
    let remaining = (expire_at - now).max(0);
    ((remaining + DAY_MS - 1) / DAY_MS).max(1)
}

/// 轮换推迟提示的通知内容（**唯一**构造点：标题与正文都在这里，措辞改动只应发生在此处）。
///
/// 口径说明：会话可能是"开着但闲置"，所以只说"检测到有会话在运行"，
/// 不能写"CLI 正在使用中"。
fn rotate_deferred_notify(
    target_name: &str,
    remaining_days: Option<i64>,
    current_limit: Option<(&str, &Resolved)>,
) -> Value {
    let body = if let Some((current_name, limit)) = current_limit {
        let model = limit.model.as_deref().unwrap_or("未知模型");
        let reset = Local
            .timestamp_millis_opt(limit.reset_at)
            .single()
            .map(|date| date.format("%Y-%m-%d %H:%M:%S %:z").to_string())
            .unwrap_or_else(|| format!("Unix 毫秒 {}", limit.reset_at));
        format!("账号「{current_name}」的模型「{model}」受限（恢复于 {reset}），建议换到「{target_name}」。检测到有 CodeBuddy CLI 会话在运行，暂不切换。")
    } else {
        let expiry = match remaining_days {
            Some(days) => format!("的积分 {days} 天后到期"),
            None => "的积分即将到期".to_string(),
        };
        format!("「{target_name}」{expiry}，但检测到有 CodeBuddy CLI 会话在运行；重启 CLI 后新账号才会生效。")
    };
    json!({
        "title": ROTATE_NOTIFY_TITLE,
        "body": body,
    })
}

/// 「要提示谁」的判定：只有**真实原因就是存活门控**、并且把门控强制放开后决策链给出
/// `Switch` 时，才返回该目标账号；任何其它 Skip（紧迫度/已是目标/冷却/价值/防抖动）
/// 都说明"不是仅被会话拦下"，不值得打扰用户。
///
/// `decide_without_gate` 只有在前半段成立时才会被调用（重跑决策链不是免费操作）。
fn deferred_prompt_target(
    real_reason: &str,
    decide_without_gate: impl FnOnce() -> Decision,
) -> Option<String> {
    if real_reason != SKIP_LIVE_SESSION_REASON {
        return None;
    }
    match decide_without_gate() {
        Decision::Switch(target_id) => Some(target_id),
        Decision::Skip(_) => None,
    }
}

/// 一次轮换周期（后台定时任务 / 手动触发共用）。
pub async fn run_rotate_cycle() -> Value {
    let cfg = load_auto_rotate_config();
    if cfg.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return json!({"status": "disabled"});
    }
    // 手动触发和后台周期可以来自不同进程，共用冷却状态与这把锁。
    let store = store_dir();
    let _guard = match try_lock_file(&store.join("locks").join("rotate-cycle.lock")) {
        Ok(guard) => guard,
        Err(LockError::Busy) => return json!({"status": "skipped", "reason": "already_running"}),
        Err(error) => return json!({"status": "error", "error": error.message("轮换操作")}),
    };
    let last_switch_path = store.join(LAST_SWITCH_FILE);

    let cooldown_minutes = cfg
        .get("cooldown_minutes")
        .and_then(|v| v.as_i64())
        .unwrap_or(120)
        .max(1);
    let min_gap_hours = cfg
        .get("min_gap_hours")
        .and_then(|v| v.as_i64())
        .unwrap_or(24)
        .max(0);
    let min_urgency_hours = cfg
        .get("min_urgency_hours")
        .and_then(|v| v.as_i64())
        .unwrap_or(72)
        .max(0);
    let min_remaining_credits = cfg
        .get("min_remaining_credits")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .max(0.0);

    // 同一套 CLI：国内/国际账号一起参与轮换，切号时带上对应区域环境。
    let accounts = account::load_accounts();
    let mut candidates: Vec<Candidate> = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let credit = credits::get_credit_expiry(acc).await;
        candidates.push(to_candidate(acc, &credit));
    }

    // 当前 CLI 账号
    let cli_status = codebuddy_cli::status();
    let current_id = cli_status["activeAccountId"].as_str();
    let current_name = cli_status["activeAccountName"]
        .as_str()
        .filter(|name| !name.is_empty())
        .or(current_id)
        .unwrap_or_default();

    let now = now_ms();
    let limits = rate_limit_events::saved_hook_entries(
        &rate_limit_events::state_path(),
        now,
        HookSource::Cli,
    );
    // 下一个 CLI 进程使用的模型可能变化，因此保守地避开任一模型仍受限的账号。
    // 多账号同模型全局限流可能导致反复换号，仍由持久化冷却期限制切换频率。
    let rate_limited = limits
        .iter()
        .map(|entry| entry.account_id.as_str())
        .collect();
    // 多个模型受限时提示恢复最晚的一项，对应账号重新进入候选池的时间。
    let current_limit = limits
        .iter()
        .filter(|entry| Some(entry.account_id.as_str()) == current_id)
        .max_by_key(|entry| (entry.reset_at, entry.model.is_some()));
    let mut inputs = RotationInputs {
        current_account_id: current_id,
        now_ms: now,
        last_switch_at_ms: last_switch_at(&last_switch_path),
        cooldown_ms: cooldown_minutes.saturating_mul(60_000),
        min_gap_ms: min_gap_hours.saturating_mul(3_600_000),
        min_urgency_ms: min_urgency_hours.saturating_mul(3_600_000),
        has_live_session: codebuddy_cli::has_live_session(now),
        min_remaining: min_remaining_credits,
        rate_limited,
    };
    let decision = decide_target(&candidates, &inputs);

    LAST_CHECK_AT.store(now, Ordering::SeqCst);

    let mut log = json!({
        "ts": now,
        "action": "noop",
        "reason": Value::Null,
        "from": Value::Null,
        "to": Value::Null,
    });
    // 每次检查记录各账号积分快照（供观察后调整 min_remaining_credits）
    log["detail"] = json!(candidates
        .iter()
        .map(|c| json!({
            "name": c.display_name,
            "remaining": c.total_remaining,
            "soonestExpireAt": c.soonest_expire_at,
            "valid": c.valid,
            "rateLimited": inputs.rate_limited.contains(c.account_id.as_str()),
        }))
        .collect::<Vec<_>>());
    if let Some(id) = current_id {
        log["from"] = json!({"id": id, "name": cli_status.get("activeAccountName").cloned().unwrap_or_default()});
    }

    let mut notify: Option<Value> = None;
    let result = match decision {
        Decision::Skip(reason) => {
            log["action"] = json!("skipped");
            let mut reason = reason;
            // 门控跳过时再问一句：把门控强制放开会不会切？会 → 说明只是被会话拦住，
            // 值得提示一次（否则用户永远不知道积分快过期了）。
            let deferred_target = deferred_prompt_target(&reason, || {
                inputs.has_live_session = false;
                decide_target(&candidates, &inputs)
            });
            if let Some(target_id) = deferred_target {
                let limit_event = current_limit.map(|entry| {
                    let key = json!([entry.account_id, entry.model, entry.reset_at]).to_string();
                    (key, entry.reset_at)
                });
                match try_consume_rotate_notify(now, limit_event) {
                    RotateNotifyClaim::Granted => {
                        let target = candidates.iter().find(|c| c.account_id == target_id);
                        let name = target
                            .map(|c| c.display_name.as_str())
                            .filter(|name| !name.is_empty())
                            .unwrap_or(&target_id);
                        let days = target
                            .and_then(|c| c.soonest_expire_at)
                            .map(|expire_at| remaining_days_until(expire_at, now));
                        let notice = rotate_deferred_notify(
                            name,
                            days,
                            current_limit.map(|limit| (current_name, limit)),
                        );
                        reason = if current_limit.is_some() {
                            // HTTP 服务的后台任务没有推送通道，轮换日志也保留提示内容。
                            notice["body"].as_str().unwrap_or(&reason).to_string()
                        } else {
                            format!("{reason}（已提示）")
                        };
                        notify = Some(notice);
                    }
                    RotateNotifyClaim::AlreadyNotified => {
                        reason = format!("{reason}（该模型限流已提示）")
                    }
                    RotateNotifyClaim::DailyLimit => {
                        reason = format!("{reason}（已达今日提示上限）")
                    }
                    RotateNotifyClaim::Unavailable => {
                        reason = format!("{reason}（无法保存提示记录）")
                    }
                }
            }
            log["reason"] = json!(reason);
            let mut payload = json!({"status": "skipped", "reason": reason});
            if let Some(notify) = notify {
                payload["notify"] = notify;
            }
            payload
        }
        Decision::Switch(target_id) => {
            let target_name = candidates
                .iter()
                .find(|c| c.account_id == target_id)
                .map(|c| c.display_name.clone())
                .unwrap_or_default();
            let target_variant = accounts
                .iter()
                .find(|account| {
                    account.get("id").and_then(Value::as_str) == Some(target_id.as_str())
                })
                .map(WbVariant::from_account)
                .unwrap_or(WbVariant::Cn);
            log["to"] = json!({
                "id": target_id,
                "name": target_name,
                "variant": target_variant.as_str(),
            });
            match codebuddy_cli::switch_active_account(&target_id) {
                Ok(res) => {
                    let switched_at = now_ms();
                    LAST_SWITCH_AT.store(switched_at, Ordering::SeqCst);
                    if let Err(error) = save_last_switch_at(&last_switch_path, switched_at) {
                        eprintln!("[轮换] 保存上次切换时间失败: {error}");
                    }
                    log["ts"] = json!(switched_at);
                    log["action"] = json!("switched");
                    if current_limit.is_some() {
                        log["reason"] = json!("模型限流");
                    }
                    json!({"status": "switched", "to": target_id, "detail": res})
                }
                Err(e) => {
                    log["action"] = json!("error");
                    log["reason"] = json!(e);
                    json!({"status": "error", "error": e})
                }
            }
        }
    };
    add_rotate_log(&log);
    result
}

/// 轮换状态（配置 + 上次检查/切换 + 当前 CLI 账号），供前端展示。
pub fn rotate_status() -> Value {
    let cfg = load_auto_rotate_config();
    let cli_status = codebuddy_cli::status();
    let last_check = LAST_CHECK_AT.load(Ordering::SeqCst);
    let last_switch = last_switch_at(&store_dir().join(LAST_SWITCH_FILE));
    json!({
        "config": cfg,
        "cliConfigured": cli_status.get("configured").cloned().unwrap_or(json!(false)),
        "activeAccountId": cli_status.get("activeAccountId").cloned().unwrap_or_default(),
        "activeAccountName": cli_status.get("activeAccountName").cloned().unwrap_or_default(),
        "lastCheckAt": (last_check > 0).then_some(last_check),
        "lastSwitchAt": last_switch,
    })
}

/// 最近轮换日志（新→旧）。
pub fn rotate_logs() -> Vec<Value> {
    let mut logs = load_rotate_logs();
    logs.reverse();
    logs
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: i64 = 24 * 3_600_000; // 防抖动差异阈值
    const URG: i64 = 72 * 3_600_000; // 紧迫度阈值
    const HOUR_MS: i64 = 3_600_000;

    #[test]
    fn persisted_switch_time_blocks_rotation_after_takeover() {
        let store = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&store).unwrap();
        let path = store.join(LAST_SWITCH_FILE);
        let now = now_ms();
        let candidates = [cand("next", Some(now + HOUR_MS), 100.0, true)];
        let decide = || {
            decide_target(
                &candidates,
                &RotationInputs {
                    last_switch_at_ms: load_last_switch_at(&path),
                    cooldown_ms: HOUR_MS,
                    ..inputs(Some("current"), now)
                },
            )
        };

        assert_eq!(decide(), Decision::Switch("next".to_string()));
        save_last_switch_at(&path, now).unwrap();
        assert_eq!(decide(), Decision::Skip("处于切换冷却期".to_string()));
        save_last_switch_at(&path, now - 2 * HOUR_MS).unwrap();
        assert_eq!(decide(), Decision::Switch("next".to_string()));
        fs::remove_dir_all(store).unwrap();
    }

    fn cand(id: &str, expire_at: Option<i64>, remaining: f64, valid: bool) -> Candidate {
        Candidate {
            account_id: id.to_string(),
            display_name: id.to_string(),
            soonest_expire_at: expire_at,
            total_remaining: remaining,
            valid,
            error: None,
        }
    }

    fn inputs(current_account_id: Option<&str>, now_ms: i64) -> RotationInputs<'_> {
        RotationInputs {
            current_account_id,
            now_ms,
            last_switch_at_ms: None,
            cooldown_ms: 0,
            min_gap_ms: GAP,
            min_urgency_ms: URG,
            has_live_session: false,
            min_remaining: 0.0,
            rate_limited: HashSet::new(),
        }
    }

    fn dt(candidates: &[Candidate], current: Option<&str>) -> Decision {
        decide_target(candidates, &inputs(current, now_ms()))
    }

    #[test]
    fn rate_limited_account_is_excluded_even_when_most_urgent() {
        let now = 1_800_000_000_000;
        let candidates = [
            cand("current", Some(now + 3 * DAY_MS), 100.0, true),
            cand("limited", Some(now + HOUR_MS), 100.0, true),
            cand("available", Some(now + DAY_MS), 10.0, true),
        ];
        let inputs = RotationInputs {
            rate_limited: HashSet::from(["limited"]),
            ..inputs(Some("current"), now)
        };
        assert_eq!(
            decide_target(&candidates, &inputs),
            Decision::Switch("available".into())
        );
    }

    #[test]
    fn current_rate_limit_bypasses_credit_thresholds_but_keeps_persisted_cooldown() {
        let now = 1_800_000_000_000;
        let candidates = [
            cand("current", Some(now + 5 * DAY_MS), 100.0, true),
            cand("empty", Some(now + HOUR_MS), 0.0, false),
            cand("later", Some(now + 6 * DAY_MS), 100.0, true),
            cand("available", Some(now + 5 * DAY_MS - HOUR_MS), 1.0, true),
        ];
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&dir).unwrap();
        let path = dir.join(LAST_SWITCH_FILE);
        let mut inputs = RotationInputs {
            rate_limited: HashSet::from(["current"]),
            cooldown_ms: HOUR_MS,
            min_remaining: 10.0,
            ..inputs(Some("current"), now)
        };
        save_last_switch_at(&path, now - HOUR_MS + 1).unwrap();
        inputs.last_switch_at_ms = load_last_switch_at(&path);
        assert_eq!(
            decide_target(&candidates, &inputs),
            Decision::Skip("处于切换冷却期".into())
        );

        save_last_switch_at(&path, now - HOUR_MS).unwrap();
        inputs.last_switch_at_ms = load_last_switch_at(&path);
        assert_eq!(
            decide_target(&candidates, &inputs),
            Decision::Switch("available".into())
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn rate_limit_without_available_account_skips_with_reason() {
        let now = 1_800_000_000_000;
        let candidates = [
            cand("current", None, 10.0, true),
            cand("next", None, 10.0, true),
        ];
        let inputs = RotationInputs {
            rate_limited: HashSet::from(["current", "next"]),
            ..inputs(Some("current"), now)
        };
        assert_eq!(
            decide_target(&candidates, &inputs),
            Decision::Skip("没有可用账号（有剩余积分的账号均处于模型限流）".into())
        );
        assert_eq!(
            decide_target(&[cand("current", None, 0.0, false)], &inputs),
            Decision::Skip("当前账号模型限流，且没有可用账号（查询失败/已过期/无剩余积分）".into())
        );
    }

    #[test]
    fn live_session_defers_rate_limit_switch_and_recommends_the_target() {
        let now = 1_800_000_000_000;
        let candidates = [cand("next", Some(now + 5 * DAY_MS), 10.0, true)];
        let mut inputs = RotationInputs {
            rate_limited: HashSet::from(["current"]),
            has_live_session: true,
            ..inputs(Some("current"), now)
        };
        let Decision::Skip(reason) = decide_target(&candidates, &inputs) else {
            panic!("存活会话必须阻止限流换号");
        };
        let target = deferred_prompt_target(&reason, || {
            inputs.has_live_session = false;
            decide_target(&candidates, &inputs)
        });
        assert_eq!(target.as_deref(), Some("next"));

        let entry = Resolved {
            account_id: "current".into(),
            model: Some("hy3".into()),
            reset_at: now + HOUR_MS,
            first_seen_at: now,
            hit_count: 1,
        };
        let notify =
            rotate_deferred_notify(target.as_deref().unwrap(), None, Some(("账号 A", &entry)));
        let reset = Local
            .timestamp_millis_opt(entry.reset_at)
            .single()
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S %:z");
        assert_eq!(notify["title"], ROTATE_NOTIFY_TITLE);
        assert_eq!(notify["body"], format!("账号「账号 A」的模型「hy3」受限（恢复于 {reset}），建议换到「next」。检测到有 CodeBuddy CLI 会话在运行，暂不切换。"));
    }

    #[test]
    fn saved_ledger_restores_account_eligibility_at_reset_time() {
        let now = 1_800_000_000_000;
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&dir).unwrap();
        let path = dir.join("rate_limit_state.json");
        let state = json!({"events": [{
            "accountId": "next",
            "model": "hy3",
            "source": HookSource::Cli,
            "resetAt": now,
            "firstSeenAt": now - HOUR_MS,
            "hitCount": 1,
        }]});
        fs::write(&path, state.to_string()).unwrap();
        let candidates = [cand("next", Some(now + HOUR_MS), 10.0, true)];
        let decide_at = |at| {
            let entries = rate_limit_events::saved_hook_entries(&path, at, HookSource::Cli);
            decide_target(
                &candidates,
                &RotationInputs {
                    rate_limited: entries
                        .iter()
                        .map(|entry| entry.account_id.as_str())
                        .collect(),
                    ..inputs(Some("current"), at)
                },
            )
        };
        assert_eq!(
            decide_at(now - 1),
            Decision::Skip("没有可用账号（有剩余积分的账号均处于模型限流）".into())
        );
        assert_eq!(decide_at(now), Decision::Switch("next".into()));
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn only_cli_limits_affect_rotation_with_shared_account_ids() {
        let now = 1_800_000_000_000;
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&dir).unwrap();
        let path = dir.join("rate_limit_state.json");
        let candidates = [
            cand("current", Some(now + HOUR_MS), 10.0, true),
            cand("next", Some(now + 5 * DAY_MS), 10.0, true),
        ];
        let entry = |id: &str, source: HookSource| {
            json!({
                "accountId": id,
                "model": "hy3",
                "source": source,
                "resetAt": now + DAY_MS,
                "firstSeenAt": now,
                "hitCount": 1,
            })
        };
        let mut events = vec![
            entry("current", HookSource::WorkBuddy(WbVariant::Cn)),
            entry("next", HookSource::WorkBuddy(WbVariant::Ai)),
        ];
        let decide = |events: &[Value]| {
            fs::write(&path, json!({"events": events}).to_string()).unwrap();
            let entries = rate_limit_events::saved_hook_entries(&path, now, HookSource::Cli);
            decide_target(
                &candidates,
                &RotationInputs {
                    rate_limited: entries
                        .iter()
                        .map(|entry| entry.account_id.as_str())
                        .collect(),
                    ..inputs(Some("current"), now)
                },
            )
        };
        assert_eq!(
            decide(&events),
            Decision::Skip("当前账号已是最紧迫账号".into())
        );
        events.push(entry("current", HookSource::Cli));
        assert_eq!(decide(&events), Decision::Switch("next".into()));
        events.push(entry("next", HookSource::Cli));
        assert_eq!(
            decide(&events),
            Decision::Skip("没有可用账号（有剩余积分的账号均处于模型限流）".into())
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    /// 同一套 CLI：候选辅助仍可按档位切分，但轮换主路径不再只用国内账号。
    #[test]
    fn rotation_candidates_can_be_split_or_combined_by_variant() {
        let accounts = vec![
            json!({"id": "cn-1", "uid": "u-1"}),
            json!({"id": "ai-1", "uid": "u-2", "variant": "ai"}),
            json!({"id": "ai-2", "uid": "u-3", "domain": "www.workbuddy.ai"}),
        ];

        let cn = candidates_for_variant(&accounts, WbVariant::Cn);
        assert_eq!(cn.len(), 1);
        assert_eq!(cn[0]["id"], "cn-1");

        let ai = candidates_for_variant(&accounts, WbVariant::Ai);
        assert_eq!(ai.len(), 2);
        assert_eq!(accounts.len(), 3);
        assert!(ai
            .iter()
            .all(|a| a["variant"] == "ai" || a["domain"] == "www.workbuddy.ai"));
        assert!(ai.iter().all(|a| a["id"] != "cn-1"));
    }

    #[test]
    fn switches_to_most_urgent_account() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3_600_000), 100.0, true),
            cand("b", Some(now + 24 * 3_600_000), 50.0, true),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Switch("b".to_string())
        );
    }

    #[test]
    fn noop_when_current_is_most_urgent() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 24 * 3_600_000), 50.0, true),
            cand("b", Some(now + 30 * 24 * 3_600_000), 100.0, true),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Skip("当前账号已是最紧迫账号".to_string())
        );
    }

    #[test]
    fn skips_when_gap_below_threshold() {
        // 目标 c 比当前 a 早到期，但差异 < 24h → 不切（防抖动）
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 24 * 3_600_000), 50.0, true),
            cand("b", Some(now + 22 * 3_600_000), 60.0, true),
            cand("c", Some(now + 21 * 3_600_000), 70.0, true),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Skip("目标到期仅早 3 小时，未达切换阈值（24 小时）".to_string())
        );
    }

    #[test]
    fn switches_when_gap_above_threshold() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3_600_000), 100.0, true),
            cand("b", Some(now + 24 * 3_600_000), 50.0, true),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Switch("b".to_string())
        );
    }

    #[test]
    fn respects_cooldown() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3_600_000), 100.0, true),
            cand("b", Some(now + 24 * 3_600_000), 50.0, true),
        ];
        // 刚切过（10 分钟前），冷却 30 分钟 → 不切
        assert_eq!(
            decide_target(
                &candidates,
                &RotationInputs {
                    last_switch_at_ms: Some(now - 10 * 60_000),
                    cooldown_ms: 30 * 60_000,
                    ..inputs(Some("a"), now)
                }
            ),
            Decision::Skip("处于切换冷却期".to_string())
        );
        // 冷却结束 → 切
        assert_eq!(
            decide_target(
                &candidates,
                &RotationInputs {
                    last_switch_at_ms: Some(now - 40 * 60_000),
                    cooldown_ms: 30 * 60_000,
                    ..inputs(Some("a"), now)
                }
            ),
            Decision::Switch("b".to_string())
        );
    }

    #[test]
    fn expired_and_failed_accounts_excluded() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 2 * 24 * 3_600_000), 100.0, true),
            cand("expired", Some(now - 3_600_000), 0.0, false),
            cand("failed", None, 0.0, false),
        ];
        assert_eq!(
            dt(&candidates, Some("expired")),
            Decision::Switch("a".to_string())
        );
    }

    #[test]
    fn no_valid_candidates_skips() {
        let candidates = vec![
            cand("a", Some(now_ms()), 0.0, false),
            cand("b", None, 0.0, false),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Skip("没有可用账号（查询失败/已过期/无剩余积分）".to_string())
        );
    }

    #[test]
    fn skips_when_nothing_urgent() {
        // 所有账号 5 天后才过期：最紧迫剩余 > 72h → 不切。
        // 目标账号额外多给 1 分钟余量：判定函数会重新采样 now_ms()，
        // 若两次采样跨过 1 毫秒，截断除法会把「5 天」打成「4 天」。
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 6 * 24 * 3_600_000), 100.0, true),
            cand("b", Some(now + 5 * 24 * 3_600_000 + 60_000), 50.0, true),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Skip("所有账号到期都还早（最紧迫的还剩 5 天），无需切换".to_string())
        );
    }

    /// 存活门控：有 CLI 会话在跑 → 固定 reason 跳过；放开后同一条链会切。
    #[test]
    fn live_session_gate_skips_and_switches_once_the_session_is_gone() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * DAY_MS), 100.0, true),
            cand("b", Some(now + DAY_MS), 50.0, true),
        ];
        assert_eq!(
            decide_target(
                &candidates,
                &RotationInputs {
                    has_live_session: true,
                    ..inputs(Some("a"), now)
                }
            ),
            Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string()),
            "门控命中时 reason 必须固定，提示判定依赖它"
        );
        assert_eq!(
            decide_target(&candidates, &inputs(Some("a"), now)),
            Decision::Switch("b".to_string()),
            "放开门控后其余条件满足 → 会切"
        );
    }

    /// 门控被拦下但"放开也不会切"时不产生提示（这里覆盖两种典型原因）。
    #[test]
    fn gate_skip_without_a_real_target_does_not_prompt() {
        let now = now_ms();
        let live = RotationInputs {
            has_live_session: true,
            ..inputs(Some("a"), now)
        };
        // ① 两个账号到期差 < min_gap_hours：放开也只会 Skip（防抖动）。
        let close_expiry = vec![
            cand("a", Some(now + DAY_MS), 50.0, true),
            cand("b", Some(now + 21 * HOUR_MS), 70.0, true),
        ];
        assert_eq!(
            decide_target(&close_expiry, &live),
            Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string())
        );
        assert!(
            !matches!(
                decide_target(&close_expiry, &inputs(Some("a"), now)),
                Decision::Switch(_)
            ),
            "差异未达阈值 → 不该提示"
        );

        // ② 当前账号已是最紧迫账号：决策链在门控之前就拦下了，同样"不是仅因门控被拦"。
        let already_target = vec![
            cand("a", Some(now + DAY_MS), 50.0, true),
            cand("b", Some(now + 30 * DAY_MS), 100.0, true),
        ];
        assert_eq!(
            decide_target(&already_target, &live),
            Decision::Skip("当前账号已是最紧迫账号".to_string()),
            "已是目标时不会走到门控，也就不会按门控文案提示"
        );
        assert_eq!(
            decide_target(&already_target, &inputs(Some("a"), now)),
            Decision::Skip("当前账号已是最紧迫账号".to_string())
        );
    }

    /// AC7 的判定本身：用真实决策链验证「放开门控后得 Switch 才提示」。
    #[test]
    fn prompt_only_when_releasing_the_gate_would_switch() {
        let now = now_ms();
        let run = |candidates: &[Candidate], gated: bool| {
            decide_target(
                candidates,
                &RotationInputs {
                    has_live_session: gated,
                    ..inputs(Some("a"), now)
                },
            )
        };

        // 当前账号 3 天后到期、另一账号 1 天后到期 + 有存活会话 → 提示目标 b。
        let expiring = vec![
            cand("a", Some(now + 3 * DAY_MS), 100.0, true),
            cand("b", Some(now + DAY_MS), 50.0, true),
        ];
        assert_eq!(
            run(&expiring, true),
            Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string())
        );
        assert_eq!(
            deferred_prompt_target(SKIP_LIVE_SESSION_REASON, || run(&expiring, false)).as_deref(),
            Some("b"),
            "除门控外本来会切换 → 提示"
        );

        // 两个账号到期接近（差异未达 min_gap_hours）→ 真实原因仍是门控，放开也不切 → 不提示。
        let close_expiry = vec![
            cand("a", Some(now + DAY_MS), 50.0, true),
            cand("b", Some(now + 21 * HOUR_MS), 70.0, true),
        ];
        assert_eq!(
            run(&close_expiry, true),
            Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string())
        );
        assert_eq!(
            deferred_prompt_target(SKIP_LIVE_SESSION_REASON, || run(&close_expiry, false)),
            None,
            "差异未达阈值 → 不提示"
        );

        // 真实原因不是门控时不该重跑决策链（重跑不免费，而且结论无关）。
        let mut replayed = false;
        assert_eq!(
            deferred_prompt_target("处于切换冷却期", || {
                replayed = true;
                Decision::Switch("b".to_string())
            }),
            None
        );
        assert!(!replayed, "非门控原因不得重跑决策链");
    }

    /// 提示文案：唯一构造点，标题固定，且不得写成"正在使用中"（会话可能是闲置但存活）。
    #[test]
    fn deferred_notify_text_is_built_in_one_place() {
        let notify = rotate_deferred_notify("账号B", Some(1), None);
        assert_eq!(notify["title"], json!("workbuddy-switch"));
        assert_eq!(
            notify["body"],
            json!("「账号B」的积分 1 天后到期，但检测到有 CodeBuddy CLI 会话在运行；重启 CLI 后新账号才会生效。")
        );
        assert!(
            !notify["body"].as_str().unwrap().contains("正在使用中"),
            "会话可能是闲置但存活"
        );
        // 到期时刻未知（目标没有 soonestExpireAt）时不谎报天数。
        assert_eq!(
            rotate_deferred_notify("B", None, None)["body"],
            json!("「B」的积分即将到期，但检测到有 CodeBuddy CLI 会话在运行；重启 CLI 后新账号才会生效。")
        );
    }

    #[test]
    fn remaining_days_round_up_and_never_report_zero() {
        let now = 1_800_000_000_000;
        assert_eq!(remaining_days_until(now + DAY_MS, now), 1);
        assert_eq!(remaining_days_until(now + DAY_MS + 1, now), 2);
        assert_eq!(remaining_days_until(now + 1, now), 1, "不足一天按一天算");
        assert_eq!(
            remaining_days_until(now - 5 * DAY_MS, now),
            1,
            "已过期也不报 0 天"
        );
    }

    #[test]
    fn skips_when_target_low_remaining() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3_600_000), 100.0, true),
            cand("b", Some(now + 24 * 3_600_000), 10.0, true),
        ];
        // 目标剩余 10 < 阈值 30 → 不值得切
        assert_eq!(
            decide_target(
                &candidates,
                &RotationInputs {
                    min_remaining: 30.0,
                    ..inputs(Some("a"), now)
                }
            ),
            Decision::Skip("目标账号剩余积分不足（10，阈值 30），不值得切换".to_string())
        );
        // 阈值 5：目标剩余 10 达标 → 切
        assert_eq!(
            decide_target(
                &candidates,
                &RotationInputs {
                    min_remaining: 5.0,
                    ..inputs(Some("a"), now)
                }
            ),
            Decision::Switch("b".to_string())
        );
    }
}
