//! 本地专属 Tauri 命令（上游无此文件 → 与上游合并零冲突）。
//!
//! 只放本项目新增的命令：账号发现 / 补录。`commands.rs` 保持上游原貌，
//! 新命令在 `lib.rs` 的 `invoke_handler` 里以 `commands_local::xxx` 注册。

use serde_json::{json, Value};

use wb_switch_core::modules::discover;

/// 识别本机曾登录/留有数据的账号（对照在册）。
#[tauri::command]
pub fn discover_known_accounts() -> Value {
    discover::discover_known_accounts()
}

/// 用最新 auth 历史备份补录指定 uid 进账号库。
#[tauri::command(rename_all = "camelCase")]
pub fn adopt_account(uid: String) -> Result<Value, String> {
    if uid.trim().is_empty() {
        return Err("缺少 uid".to_string());
    }
    discover::adopt_account(&uid).map(|meta| json!({ "ok": true, "account": meta }))
}
