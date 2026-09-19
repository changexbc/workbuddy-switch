//! 本地专属 Tauri 命令（上游无此文件 → 与上游合并零冲突）。
//!
//! 只放本项目新增的命令：定时任务归属对齐。`commands.rs` 保持上游原貌，
//! 新命令在 `lib.rs` 的 `invoke_handler` 里以 `commands_local::xxx` 注册。

use serde_json::{json, Value};

use wb_switch_core::modules::{account, automations};

/// 从账号记录里取 uid（空串 = 该账号缺 uid）。
fn account_uid(acc: &Value) -> String {
    acc.get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 不切号，把当前定时任务归属立即对齐到指定账号（适用于先用旧版切完号、补做对齐的场景）。
/// 需先完全退出 WorkBuddy。
#[tauri::command(rename_all = "camelCase")]
pub async fn align_automations(account_id: String) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&account_id).ok_or("账号不存在")?;
        let uid = account_uid(&target);
        if uid.is_empty() {
            return Err("该账号缺少 uid，无法对齐".to_string());
        }
        automations::align_automations_owner(&uid).ok_or_else(|| "workbuddy.db 不存在".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
