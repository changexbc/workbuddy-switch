//! 账号切换：备份 → 关进程 → 复制会话（可选）→ 写认证 → 启动。
//!
//! 对照 server.py `switch_account`。切换过程中通过 `switch-progress` 事件向前端
//! 推送实时进度，避免界面长时间无反馈被误认为卡死。

use serde_json::{json, Value};
use tauri::Emitter;

use crate::modules::account;
use crate::modules::auth_file;
use crate::modules::process::{close_workbuddy, launch_workbuddy};
use crate::modules::session;

/// 切换账号。copy_session_ids 非空时按路径 B 复制勾选会话（新 id，云端可同步）。
pub fn switch_account(
    app: &tauri::AppHandle,
    account_id: &str,
    restart: bool,
    share_sessions: bool,
    copy_session_ids: &[String],
) -> Result<Value, String> {
    let progress = |message: &str| {
        eprintln!("[switch] progress: {message}");
        let _ = app.emit("switch-progress", json!({"message": message}));
    };

    progress("开始切换账号…");
    let acc = account::find_account(account_id)
        .ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let backup = auth_file::backup_auth_file();

    let mut copy_report: Option<Value> = None;
    let mut session_report: Option<Value> = None;
    if restart {
        progress("正在关闭 WorkBuddy…");
        close_workbuddy(20)?;
        // 只有重启场景才做会话操作（数据库在运行中不宜写入）
        if !copy_session_ids.is_empty() {
            progress("正在复制会话到目标账号…");
            copy_report = session::copy_sessions_for_switch(&acc, copy_session_ids);
        }
        if share_sessions {
            // 旧的「全体转移」兼容路径（默认关闭），Rust 版暂未实现
            session_report =
                Some(json!({"error": "share_sessions 兼容路径暂未在 Rust 版实现"}));
        }
    }
    progress("正在写入认证文件…");
    auth_file::write_account_to_auth_file(&acc)?;
    if restart {
        progress("正在启动 WorkBuddy…");
        launch_workbuddy();
    }
    progress("切换完成");

    let mut result = json!({
        "ok": true,
        "account": account::account_display_name(&acc),
        "backup": backup.map(|p| p.to_string_lossy().to_string()),
    });
    if let Some(c) = copy_report {
        result["sessionCopy"] = c;
    }
    if let Some(s) = session_report {
        result["sessionShare"] = s;
    }
    Ok(result)
}
