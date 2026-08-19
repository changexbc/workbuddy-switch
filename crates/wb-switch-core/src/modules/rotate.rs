//! CodeBuddy CLI 账号自动轮换（防积分过期浪费）。
//!
//! 后台周期检查所有账号的积分到期情况，把 CodeBuddy CLI 切到"最紧迫"的账号
//! （最早到期且仍有剩余积分），防止积分过期浪费。只写 `~/.codebuddy-rotate/state.json`
//! （复用 codebuddy_cli::set_active_account），不影响 WorkBuddy App。
//!
//! 防抖动约束（核心）：
//! - 冷却期：切换后 cooldown_minutes 内不重复切；
//! - 到期差异阈值：目标比当前账号早到期超过 min_gap_hours 才切，
//!   避免"三个账号都是明天到期"时来回切换。

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use crate::modules::account;
use crate::modules::codebuddy_cli;
use crate::modules::config::{
    add_rotate_log, load_auto_rotate_config, load_rotate_logs, now_ms, RunFlagGuard,
};
use crate::modules::credits;

static ROTATE_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_CHECK_AT: AtomicI64 = AtomicI64::new(0);
static LAST_SWITCH_AT: AtomicI64 = AtomicI64::new(0);

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
    let soonest_expire_at = credit
        .get("soonestExpireAt")
        .and_then(|v| v.as_i64());
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

/// 决策结果。
#[derive(Debug, PartialEq)]
pub enum Decision {
    /// 不切，附原因。
    Skip(String),
    /// 切到目标账号 id。
    Switch(String),
}

/// 纯策略函数：在候选里选目标并判断是否切换（可单测）。
///
/// - `current_account_id`：当前 CLI 账号（可能不在候选中）。
/// - `last_switch_at_ms`：上次切换时间，None 视为从未切换。
/// - `min_urgency_ms`：紧迫度阈值——目标到期剩余超过该值则不切（都还早，无需切）。
/// - `cli_recent_activity_ms`：CLI 会话最近写入时间，None 视为无会话。
/// - `active_guard_ms`：活跃保护窗口——最近活动距今 < 该值则不切（正在用）。
/// - `min_remaining`：目标最小剩余积分，低于则不切（默认 0 关闭）。
pub fn decide_target(
    candidates: &[Candidate],
    current_account_id: Option<&str>,
    last_switch_at_ms: Option<i64>,
    cooldown_ms: i64,
    min_gap_ms: i64,
    min_urgency_ms: i64,
    cli_recent_activity_ms: Option<i64>,
    active_guard_ms: i64,
    min_remaining: f64,
) -> Decision {
    // 1) 有效候选（可被选为目标）
    let mut valid: Vec<&Candidate> = candidates.iter().filter(|c| c.valid).collect();
    if valid.is_empty() {
        return Decision::Skip("没有可用账号（查询失败/已过期/无剩余积分）".to_string());
    }
    // 2) 目标 = 紧迫度最高（到期最早）
    valid.sort_by_key(|c| c.urgency_key());
    let target = valid[0];
    // 3) 紧迫度检查：目标到期还早（> 阈值）→ 无需切换
    if let Some(target_ts) = target.soonest_expire_at {
        let remaining_ms = target_ts - now_ms();
        if remaining_ms > min_urgency_ms {
            return Decision::Skip(format!(
                "所有账号到期都还早（最紧迫的还剩 {} 天），无需切换",
                remaining_ms / (24 * 3600_000)
            ));
        }
    }
    // 4) 已是目标 → 不切
    if current_account_id == Some(target.account_id.as_str()) {
        return Decision::Skip("当前账号已是最紧迫账号".to_string());
    }
    // 5) 冷却期
    if let Some(ts) = last_switch_at_ms {
        if ts + cooldown_ms > now_ms() {
            return Decision::Skip("处于切换冷却期".to_string());
        }
    }
    // 6) 活跃保护：CLI 会话最近有写入 → 不切（正在用）
    if let Some(activity) = cli_recent_activity_ms {
        if activity + active_guard_ms > now_ms() {
            return Decision::Skip("CLI 正在使用中，暂不切换".to_string());
        }
    }
    // 7) 价值过滤：目标剩余积分太少，切过去不值得
    if min_remaining > 0.0 && target.total_remaining < min_remaining {
        return Decision::Skip(format!(
            "目标账号剩余积分不足（{}，阈值 {}），不值得切换",
            target.total_remaining, min_remaining
        ));
    }
    // 8) 防抖动：目标比当前早到期，但差异 < 阈值 → 不切
    if let Some(current) = candidates
        .iter()
        .find(|c| c.account_id == current_account_id.unwrap_or(""))
    {
        if let (Some(cur_ts), Some(target_ts)) = (current.soonest_expire_at, target.soonest_expire_at)
        {
            if cur_ts > 0 && target_ts < cur_ts && cur_ts - target_ts < min_gap_ms {
                return Decision::Skip(format!(
                    "目标到期仅早 {} 小时，未达切换阈值（{} 小时）",
                    (cur_ts - target_ts) / 3600_000,
                    min_gap_ms / 3600_000
                ));
            }
        }
    }
    Decision::Switch(target.account_id.clone())
}

/// 最近 CLI 会话活动时间：递归扫 `~/.codebuddy/projects/**/*.jsonl`
/// （含 subagents/ 子目录），取最新 mtime（毫秒）；无会话文件返回 None。
pub fn cli_recent_activity() -> Option<i64> {
    let projects = crate::modules::config::home_dir().join(".codebuddy").join("projects");
    if !projects.is_dir() {
        return None;
    }
    let mut newest: Option<i64> = None;
    let mut stack = vec![projects];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                            let ms = dur.as_millis() as i64;
                            newest = Some(newest.map_or(ms, |n| n.max(ms)));
                        }
                    }
                }
            }
        }
    }
    newest
}

/// 一次轮换周期（后台定时任务 / 手动触发共用）。
pub async fn run_rotate_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&ROTATE_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };

    let cfg = load_auto_rotate_config();
    if cfg.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return json!({"status": "disabled"});
    }

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
    let active_guard_minutes = cfg
        .get("active_guard_minutes")
        .and_then(|v| v.as_i64())
        .unwrap_or(30)
        .max(0);
    let min_remaining_credits = cfg
        .get("min_remaining_credits")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .max(0.0);

    // 拉取所有账号积分
    let accounts = account::load_accounts();
    let mut candidates: Vec<Candidate> = Vec::with_capacity(accounts.len());
    for acc in &accounts {
        let credit = credits::get_credit_expiry(acc).await;
        candidates.push(to_candidate(acc, &credit));
    }

    // 当前 CLI 账号
    let cli_status = codebuddy_cli::status();
    let current_id = cli_status
        .get("activeAccountId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let last_switch = LAST_SWITCH_AT.load(Ordering::SeqCst);
    let last_switch_opt = (last_switch > 0).then_some(last_switch);
    let decision = decide_target(
        &candidates,
        current_id.as_deref(),
        last_switch_opt,
        cooldown_minutes * 60_000,
        min_gap_hours * 3600_000,
        min_urgency_hours * 3600_000,
        cli_recent_activity(),
        active_guard_minutes * 60_000,
        min_remaining_credits,
    );

    let now = now_ms();
    LAST_CHECK_AT.store(now, Ordering::SeqCst);

    let mut log = json!({
        "ts": now,
        "action": "noop",
        "reason": Value::Null,
        "from": Value::Null,
        "to": Value::Null,
    });
    // 每次检查记录各账号积分快照（供观察后调整 min_remaining_credits）
    log["detail"] = json!(candidates.iter().map(|c| json!({
        "name": c.display_name,
        "remaining": c.total_remaining,
        "soonestExpireAt": c.soonest_expire_at,
        "valid": c.valid,
    })).collect::<Vec<_>>());
    if let Some(id) = &current_id {
        log["from"] = json!({"id": id, "name": cli_status.get("activeAccountName").cloned().unwrap_or_else(|| json!(null))});
    }

    let result = match decision {
        Decision::Skip(reason) => {
            log["action"] = json!("skipped");
            log["reason"] = json!(reason);
            json!({"status": "skipped", "reason": reason})
        }
        Decision::Switch(target_id) => {
            log["to"] = json!({"id": target_id, "name": candidates.iter().find(|c| c.account_id == target_id).map(|c| c.display_name.clone()).unwrap_or_default()});
            match codebuddy_cli::set_active_account(&target_id) {
                Ok(res) => {
                    LAST_SWITCH_AT.store(now, Ordering::SeqCst);
                    log["action"] = json!("switched");
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
    let last_switch = LAST_SWITCH_AT.load(Ordering::SeqCst);
    json!({
        "config": cfg,
        "cliConfigured": cli_status.get("configured").cloned().unwrap_or_else(|| json!(false)),
        "activeAccountId": cli_status.get("activeAccountId").cloned().unwrap_or_else(|| json!(null)),
        "activeAccountName": cli_status.get("activeAccountName").cloned().unwrap_or_else(|| json!(null)),
        "lastCheckAt": (last_check > 0).then_some(last_check),
        "lastSwitchAt": (last_switch > 0).then_some(last_switch),
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

    const GAP: i64 = 24 * 3600_000; // 防抖动差异阈值
    const URG: i64 = 72 * 3600_000; // 紧迫度阈值
    const GUARD: i64 = 30 * 60_000; // 活跃保护窗口

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

    /// 默认参数调用：无冷却、无活跃会话、无价值过滤。
    fn dt(candidates: &[Candidate], current: Option<&str>) -> Decision {
        decide_target(candidates, current, None, 0, GAP, URG, None, GUARD, 0.0)
    }

    #[test]
    fn switches_to_most_urgent_account() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3600_000), 100.0, true),
            cand("b", Some(now + 1 * 24 * 3600_000), 50.0, true),
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
            cand("a", Some(now + 1 * 24 * 3600_000), 50.0, true),
            cand("b", Some(now + 30 * 24 * 3600_000), 100.0, true),
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
            cand("a", Some(now + 24 * 3600_000), 50.0, true),
            cand("b", Some(now + 22 * 3600_000), 60.0, true),
            cand("c", Some(now + 21 * 3600_000), 70.0, true),
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
            cand("a", Some(now + 30 * 24 * 3600_000), 100.0, true),
            cand("b", Some(now + 1 * 24 * 3600_000), 50.0, true),
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
            cand("a", Some(now + 30 * 24 * 3600_000), 100.0, true),
            cand("b", Some(now + 1 * 24 * 3600_000), 50.0, true),
        ];
        // 刚切过（10 分钟前），冷却 30 分钟 → 不切
        assert_eq!(
            decide_target(&candidates, Some("a"), Some(now - 10 * 60_000), 30 * 60_000, GAP, URG, None, GUARD, 0.0),
            Decision::Skip("处于切换冷却期".to_string())
        );
        // 冷却结束 → 切
        assert_eq!(
            decide_target(&candidates, Some("a"), Some(now - 40 * 60_000), 30 * 60_000, GAP, URG, None, GUARD, 0.0),
            Decision::Switch("b".to_string())
        );
    }

    #[test]
    fn expired_and_failed_accounts_excluded() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 2 * 24 * 3600_000), 100.0, true),
            cand("expired", Some(now - 1 * 3600_000), 0.0, false),
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
        // 所有账号 5 天后才过期：最紧迫剩余 > 72h → 不切
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 6 * 24 * 3600_000), 100.0, true),
            cand("b", Some(now + 5 * 24 * 3600_000), 50.0, true),
        ];
        assert_eq!(
            dt(&candidates, Some("a")),
            Decision::Skip("所有账号到期都还早（最紧迫的还剩 5 天），无需切换".to_string())
        );
    }

    #[test]
    fn skips_when_cli_active() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3600_000), 100.0, true),
            cand("b", Some(now + 1 * 24 * 3600_000), 50.0, true),
        ];
        // 10 分钟前有会话写入 → 活跃保护，不切
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, Some(now - 10 * 60_000), GUARD, 0.0),
            Decision::Skip("CLI 正在使用中，暂不切换".to_string())
        );
        // 1 小时前有会话写入 → 已过窗口，正常切
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, Some(now - 60 * 60_000), GUARD, 0.0),
            Decision::Switch("b".to_string())
        );
        // 无会话文件（None）→ 正常切
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, None, GUARD, 0.0),
            Decision::Switch("b".to_string())
        );
    }

    #[test]
    fn skips_when_target_low_remaining() {
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 30 * 24 * 3600_000), 100.0, true),
            cand("b", Some(now + 1 * 24 * 3600_000), 10.0, true),
        ];
        // 目标剩余 10 < 阈值 30 → 不值得切
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, None, GUARD, 30.0),
            Decision::Skip("目标账号剩余积分不足（10，阈值 30），不值得切换".to_string())
        );
        // 阈值 5：目标剩余 10 达标 → 切
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, None, GUARD, 5.0),
            Decision::Switch("b".to_string())
        );
    }
}
