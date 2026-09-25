//! CodeBuddy CLI 账号自动轮换（防积分过期浪费）。
//!
//! 后台周期检查所有账号的积分到期情况，把 CodeBuddy CLI 切到"最紧迫"的账号
//! （最早到期且仍有剩余积分），防止积分过期浪费。国内/国际是同一套 CLI，
//! 候选不按档位过滤；切号时由 `switch_active_account` 同步区域环境变量。
//! 只写 `~/.codebuddy-rotate/state.json`，不影响 WorkBuddy App。
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

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use crate::modules::account;
use crate::modules::codebuddy_cli;
use crate::modules::config::{
    add_rotate_log, load_auto_rotate_config, load_rotate_logs, now_ms, try_consume_rotate_notify,
    RunFlagGuard,
};
use crate::modules::credits;
use crate::modules::variant::WbVariant;

static ROTATE_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_CHECK_AT: AtomicI64 = AtomicI64::new(0);
static LAST_SWITCH_AT: AtomicI64 = AtomicI64::new(0);

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

/// 纯策略函数：在候选里选目标并判断是否切换（可单测）。
///
/// - `current_account_id`：当前 CLI 账号（可能不在候选中）。
/// - `last_switch_at_ms`：上次切换时间，None 视为从未切换。
/// - `min_urgency_ms`：紧迫度阈值——目标到期剩余超过该值则不切（都还早，无需切）。
/// - `has_live_session`：是否存在心跳新鲜的 CLI 会话；true 则本次不切（存活门控）。
/// - `min_remaining`：目标最小剩余积分，低于则不切（默认 0 关闭）。
///
/// 参数都是纯策略输入，调用方（轮换周期与用例）需要逐项显式传入，故刻意放行
/// `too_many_arguments`；继续往这个签名里加参数前，先考虑是否该引入参数结构体。
#[allow(clippy::too_many_arguments)]
pub fn decide_target(
    candidates: &[Candidate],
    current_account_id: Option<&str>,
    last_switch_at_ms: Option<i64>,
    cooldown_ms: i64,
    min_gap_ms: i64,
    min_urgency_ms: i64,
    has_live_session: bool,
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
                remaining_ms / (24 * 3_600_000)
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
    // 6) 存活门控：有 CLI 会话在跑 → 不切（活进程持旧 key，切了不生效还会破坏不变式）
    if has_live_session {
        return Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string());
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
        if let (Some(cur_ts), Some(target_ts)) =
            (current.soonest_expire_at, target.soonest_expire_at)
        {
            if cur_ts > 0 && target_ts < cur_ts && cur_ts - target_ts < min_gap_ms {
                return Decision::Skip(format!(
                    "目标到期仅早 {} 小时，未达切换阈值（{} 小时）",
                    (cur_ts - target_ts) / 3_600_000,
                    min_gap_ms / 3_600_000
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
fn rotate_deferred_notify(target_name: &str, remaining_days: Option<i64>) -> Value {
    let expiry = match remaining_days {
        Some(days) => format!("的积分 {days} 天后到期"),
        None => "的积分即将到期".to_string(),
    };
    json!({
        "title": ROTATE_NOTIFY_TITLE,
        "body": format!(
            "「{target_name}」{expiry}，但检测到有 CodeBuddy CLI 会话在运行；重启 CLI 后新账号才会生效。"
        ),
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
    let current_id = cli_status
        .get("activeAccountId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let now = now_ms();
    let last_switch = LAST_SWITCH_AT.load(Ordering::SeqCst);
    let last_switch_opt = (last_switch > 0).then_some(last_switch);
    // 存活门控：读 `~/.codebuddy/sessions/*.json` 的心跳（单次检查只读这里与 state.json，
    // 不再遍历 `~/.codebuddy/projects` 下的 transcript）。
    let has_live_session = codebuddy_cli::has_live_session(now);
    // 同一决策链跑两次：真实门控值 + 强制放开（只有"除门控外本会切换"时才提示）。
    let decide = |has_live_session: bool| {
        decide_target(
            &candidates,
            current_id.as_deref(),
            last_switch_opt,
            cooldown_minutes * 60_000,
            min_gap_hours * 3_600_000,
            min_urgency_hours * 3_600_000,
            has_live_session,
            min_remaining_credits,
        )
    };
    let decision = decide(has_live_session);

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
        }))
        .collect::<Vec<_>>());
    if let Some(id) = &current_id {
        log["from"] = json!({"id": id, "name": cli_status.get("activeAccountName").cloned().unwrap_or_default()});
    }

    let mut notify: Option<Value> = None;
    let result = match decision {
        Decision::Skip(reason) => {
            log["action"] = json!("skipped");
            let mut reason = reason;
            // 门控跳过时再问一句：把门控强制放开会不会切？会 → 说明只是被会话拦住，
            // 值得提示一次（否则用户永远不知道积分快过期了）。
            let deferred_target = deferred_prompt_target(&reason, || decide(false));
            if let Some(target_id) = deferred_target {
                if try_consume_rotate_notify(now) {
                    let target = candidates.iter().find(|c| c.account_id == target_id);
                    let name = target
                        .map(|c| c.display_name.clone())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| target_id.clone());
                    let days = target
                        .and_then(|c| c.soonest_expire_at)
                        .map(|expire_at| remaining_days_until(expire_at, now));
                    notify = Some(rotate_deferred_notify(&name, days));
                    reason = format!("{reason}（已提示）");
                } else {
                    reason = format!("{reason}（已达今日提示上限）");
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
        "cliConfigured": cli_status.get("configured").cloned().unwrap_or(json!(false)),
        "activeAccountId": cli_status.get("activeAccountId").cloned().unwrap_or_default(),
        "activeAccountName": cli_status.get("activeAccountName").cloned().unwrap_or_default(),
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

    const GAP: i64 = 24 * 3_600_000; // 防抖动差异阈值
    const URG: i64 = 72 * 3_600_000; // 紧迫度阈值
    const HOUR_MS: i64 = 3_600_000;

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

    /// 默认参数调用：无冷却、无存活会话、无价值过滤。
    fn dt(candidates: &[Candidate], current: Option<&str>) -> Decision {
        decide_target(candidates, current, None, 0, GAP, URG, false, 0.0)
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
                Some("a"),
                Some(now - 10 * 60_000),
                30 * 60_000,
                GAP,
                URG,
                false,
                0.0
            ),
            Decision::Skip("处于切换冷却期".to_string())
        );
        // 冷却结束 → 切
        assert_eq!(
            decide_target(
                &candidates,
                Some("a"),
                Some(now - 40 * 60_000),
                30 * 60_000,
                GAP,
                URG,
                false,
                0.0
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
        // 所有账号 5 天后才过期：最紧迫剩余 > 72h → 不切
        let now = now_ms();
        let candidates = vec![
            cand("a", Some(now + 6 * 24 * 3_600_000), 100.0, true),
            cand("b", Some(now + 5 * 24 * 3_600_000), 50.0, true),
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
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, true, 0.0),
            Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string()),
            "门控命中时 reason 必须固定，提示判定依赖它"
        );
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, false, 0.0),
            Decision::Switch("b".to_string()),
            "放开门控后其余条件满足 → 会切"
        );
    }

    /// 门控被拦下但"放开也不会切"时不产生提示（这里覆盖两种典型原因）。
    #[test]
    fn gate_skip_without_a_real_target_does_not_prompt() {
        let now = now_ms();
        // ① 两个账号到期差 < min_gap_hours：放开也只会 Skip（防抖动）。
        let close_expiry = vec![
            cand("a", Some(now + DAY_MS), 50.0, true),
            cand("b", Some(now + 21 * HOUR_MS), 70.0, true),
        ];
        assert_eq!(
            decide_target(&close_expiry, Some("a"), None, 0, GAP, URG, true, 0.0),
            Decision::Skip(SKIP_LIVE_SESSION_REASON.to_string())
        );
        assert!(
            !matches!(
                decide_target(&close_expiry, Some("a"), None, 0, GAP, URG, false, 0.0),
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
            decide_target(&already_target, Some("a"), None, 0, GAP, URG, true, 0.0),
            Decision::Skip("当前账号已是最紧迫账号".to_string()),
            "已是目标时不会走到门控，也就不会按门控文案提示"
        );
        assert_eq!(
            decide_target(&already_target, Some("a"), None, 0, GAP, URG, false, 0.0),
            Decision::Skip("当前账号已是最紧迫账号".to_string())
        );
    }

    /// AC7 的判定本身：用真实决策链验证「放开门控后得 Switch 才提示」。
    #[test]
    fn prompt_only_when_releasing_the_gate_would_switch() {
        let now = now_ms();
        let run = |candidates: &[Candidate], gated: bool| {
            decide_target(candidates, Some("a"), None, 0, GAP, URG, gated, 0.0)
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
        let notify = rotate_deferred_notify("账号B", Some(1));
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
            rotate_deferred_notify("B", None)["body"],
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
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, false, 30.0),
            Decision::Skip("目标账号剩余积分不足（10，阈值 30），不值得切换".to_string())
        );
        // 阈值 5：目标剩余 10 达标 → 切
        assert_eq!(
            decide_target(&candidates, Some("a"), None, 0, GAP, URG, false, 5.0),
            Decision::Switch("b".to_string())
        );
    }
}
