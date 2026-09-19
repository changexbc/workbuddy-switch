//! 账号切换：备份 → 关进程 → 复制会话 / 数据对齐（可选）→ 写认证 → 启动。
//!
//! 对照 server.py `switch_account`。切换过程中通过进度回调向前端推送实时进度，
//! 避免界面长时间无反馈被误认为卡死。core 不依赖 Tauri，进度回调由宿主适配
//! （桌面端转发为 `switch-progress` 事件，HTTP 端写入轮询/SSE）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::account;
use crate::modules::auth_file;
use crate::modules::automations;
use crate::modules::oplog;
use crate::modules::process::{close_workbuddy, launch_workbuddy};
use crate::modules::session;
use crate::modules::variant::WbVariant;

/// 切换进度回调（宿主注入，如 Tauri `app.emit` 或 HTTP 进度缓存）。
pub type ProgressFn = Box<dyn Fn(&str) + Send + Sync>;

/// 切换选项。
///
/// serde 必须用 camelCase：HTTP api（api_switch）直接把前端扁平 JSON 反序列化成
/// 本结构，字段名对不上会被当未知字段忽略、静默落回 default（勾选静默失效）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SwitchOptions {
    #[serde(default = "default_true")]
    pub restart: bool,
    #[serde(default)]
    pub share_sessions: bool,
    #[serde(default)]
    pub copy_session_ids: Vec<String>,
    /// 带走定时任务（multi_sync L3）：把 `automations` / `automation_delivery_outbox`
    /// 的归属改成目标账号，让目标账号也能看到、继续管这些定时任务。
    #[serde(default = "default_true")]
    pub align_automations: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SwitchOptions {
    fn default() -> Self {
        Self {
            restart: true,
            share_sessions: false,
            copy_session_ids: Vec::new(),
            align_automations: true,
        }
    }
}

/// 切换账号。
///
/// 薄包装：调用 [`switch_account_inner`] 做实际切换，并把结果留痕到
/// `~/.wb-switch/switch_logs.json`（见 `oplog` 模块）。留痕失败不影响切换结果。
pub fn switch_account(
    progress_fn: Option<&ProgressFn>,
    account_id: &str,
    opts: &SwitchOptions,
) -> Result<Value, String> {
    let from_uid = session::current_user_uid(WbVariant::Cn);
    let to_uid = account::find_account(account_id)
        .map(|acc| account_uid(&acc))
        .unwrap_or_default();
    let outcome = switch_account_inner(progress_fn, account_id, opts);
    let log_options = json!({
        "copySessions": opts.copy_session_ids.len(),
        "alignAutomations": opts.align_automations,
        "restart": opts.restart,
    });
    match &outcome {
        Ok(result) => oplog::add_switch_log(&oplog::switch_log_entry(
            "switch",
            from_uid.as_deref(),
            &to_uid,
            &log_options,
            result,
        )),
        Err(err) => oplog::add_switch_log(&oplog::switch_log_entry(
            "error",
            from_uid.as_deref(),
            &to_uid,
            &log_options,
            &json!({ "ok": false, "error": err }),
        )),
    }
    outcome
}

/// 从账号记录里取 uid（空串 = 该账号缺 uid）。
fn account_uid(acc: &Value) -> String {
    acc.get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn switch_account_inner(
    progress_fn: Option<&ProgressFn>,
    account_id: &str,
    opts: &SwitchOptions,
) -> Result<Value, String> {
    let progress = |message: &str| {
        eprintln!("[switch] progress: {message}");
        if let Some(p) = progress_fn {
            p(message);
        }
    };

    progress("开始切换账号…");
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    // 档位以账号自身为准：签名里的参数无法表达「用 A 档位操作 B 档位账号」。
    let variant = account::variant_of(&acc);
    let backup = auth_file::backup_auth_file(variant);

    let mut copy_report: Option<Value> = None;
    let mut session_report: Option<Value> = None;
    let mut align_report: Option<Value> = None;
    if opts.restart {
        progress("正在关闭 WorkBuddy…");
        close_workbuddy(variant, 20)?;
        // 只有重启场景才做会话/数据操作（数据库在运行中不宜写入）
        // 能力探测只对国际版生效：国内版数据根与改造前同构，探测会把「从未用过
        // 会话」的国内版机器判成不支持并整段跳过（A1 零回归）。
        let copy_available = variant != WbVariant::Ai || variant.supports_session_copy();
        if !opts.copy_session_ids.is_empty() && copy_available {
            progress("正在复制会话到目标账号…");
            // 复制失败不阻断切换：报告里带上错误，切换本身仍然继续。
            copy_report = Some(
                match session::copy_sessions_for_switch(&acc, &opts.copy_session_ids) {
                    Ok(report) => report,
                    Err(error) => json!({"error": error}),
                },
            );
        }

        if opts.align_automations {
            // 带走定时任务：改归属而不是复制，天然没有双跑。
            // 目标账号缺 uid / 库不存在 ⇒ 跳过并在报告里说明，不阻断切换。
            let target_uid = account_uid(&acc);
            if target_uid.is_empty() {
                align_report = Some(json!({ "error": "该账号缺少 uid，无法对齐" }));
            } else {
                progress("正在把定时任务带到目标账号…");
                let db_path = session::workbuddy_db_path(variant);
                match automations::align_automations_owner_in_db(&db_path, &target_uid, false) {
                    Ok((updated, outbox)) => {
                        align_report = Some(json!({
                            "automations": { "updated": updated, "outbox": outbox }
                        }));
                    }
                    Err(error) => align_report = Some(json!({ "error": error })),
                }
            }
        }

        if opts.share_sessions {
            // 旧的「全体转移」兼容路径（默认关闭），Rust 版暂未实现
            session_report = Some(json!({"error": "share_sessions 兼容路径暂未在 Rust 版实现"}));
        }
    }
    progress("正在写入认证文件…");
    auth_file::write_account_to_auth_file(&acc, variant)?;
    if opts.restart {
        progress("正在启动 WorkBuddy…");
        launch_workbuddy(variant, Some(&progress))?;
    }
    progress("切换完成");

    let mut result = json!({
        "ok": true,
        "account": account::account_display_name(&acc),
        "variant": variant.as_str(),
        "backup": backup.map(|p| p.to_string_lossy().to_string()),
    });
    if let Some(c) = copy_report {
        result["sessionCopy"] = c;
    }
    if let Some(s) = session_report {
        result["sessionShare"] = s;
    }
    if let Some(a) = align_report {
        result["alignData"] = a;
    }
    Ok(result)
}
