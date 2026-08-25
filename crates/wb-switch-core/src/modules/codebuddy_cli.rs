//! CodeBuddy CLI 账号轮换桥接。
//!
//! CodeBuddy CLI 的 `apiKeyHelper` 会周期性读取 stdout 作为 Bearer token。
//! CLI 本身的配置和会话仍由 CLI 管理；这里仅维护 helper 读取的当前账号，
//! 并复用 wb-switch 的 WorkBuddy 账号库。
//!
//! helper 用 Node 实现（CodeBuddy CLI 是 npm 安装的 Node 应用，运行 helper 时
//! node 必然可用），用户无需安装 Python 等额外环境。

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::modules::account;
use crate::modules::config::{atomic_write, home_dir, now_ms};

const ROTATE_DIR: &str = ".codebuddy-rotate";
const STATE_FILE: &str = "state.json";
/// 配置到 apiKeyHelper 的文件：mac/Linux 直接是 Node 脚本；
/// Windows 上 CodeBuddy CLI 通过 PowerShell/cmd 执行 helper，只认 .cmd，
/// 所以配一个启动跳板（内部 exec node 跑 helper.cjs）。
#[cfg(windows)]
const HELPER_FILE: &str = "helper.cmd";
#[cfg(not(windows))]
const HELPER_FILE: &str = "helper.cjs";
const LOGIC_FILE: &str = "helper.cjs";
const LEGACY_HELPER_FILE: &str = "helper.sh";
const SETTINGS_DIR: &str = ".codebuddy";
const SETTINGS_FILE: &str = "settings.json";
const STANDARD_HELPER: &str = include_str!("../../../../scripts/codebuddy-cli-helper.cjs");
#[cfg(windows)]
const WINDOWS_HELPER_SHIM: &str = include_str!("../../../../scripts/codebuddy-cli-helper.cmd");

fn rotate_dir() -> PathBuf {
    home_dir().join(ROTATE_DIR)
}

fn state_path() -> PathBuf {
    rotate_dir().join(STATE_FILE)
}

fn settings_path() -> PathBuf {
    home_dir().join(SETTINGS_DIR).join(SETTINGS_FILE)
}

fn read_json_file(path: &PathBuf) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn helper_path() -> Option<String> {
    read_json_file(&settings_path())
        .and_then(|settings| {
            settings
                .get("apiKeyHelper")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

fn helper_is_configured() -> bool {
    helper_path()
        .map(|path| PathBuf::from(path).is_file())
        .unwrap_or(false)
}

fn helper_supports_account_ids() -> bool {
    // Windows 上配置的是 .cmd 跳板（不含 activeAccountId），实际逻辑在
    // helper.cjs，所以要同时检查两个文件。
    let configured = helper_path().and_then(|path| std::fs::read_to_string(path).ok());
    let logic = std::fs::read_to_string(rotate_dir().join(LOGIC_FILE)).ok();
    configured
        .into_iter()
        .chain(logic)
        .any(|source| source.contains("activeAccountId"))
}

fn load_state() -> Value {
    read_json_file(&state_path())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn account_index(accounts: &[Value], account_id: &str) -> Option<(usize, String)> {
    accounts.iter().enumerate().find_map(|(index, account)| {
        let matches = account.get("id").and_then(Value::as_str) == Some(account_id)
            || account.get("uid").and_then(Value::as_str) == Some(account_id);
        if !matches {
            return None;
        }
        let canonical_id = account
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| account_id.to_string());
        Some((index, canonical_id))
    })
}

fn state_account_index(state: &Value, accounts: &[Value]) -> Option<(usize, String)> {
    if accounts.is_empty() {
        return None;
    }
    if let Some(active_id) = state.get("activeAccountId").and_then(Value::as_str) {
        if let Some(found) = account_index(accounts, active_id) {
            return Some(found);
        }
    }

    let index = state
        .get("active")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .rem_euclid(accounts.len() as i64) as usize;
    let account = accounts.get(index)?;
    let id = account.get("id").and_then(Value::as_str)?.to_string();
    Some((index, id))
}

/// 返回脱敏的 CLI 轮换状态，不返回 token 或 helper 内容。
pub fn status() -> Value {
    let accounts = account::load_accounts();
    let state = load_state();
    let active = state_account_index(&state, &accounts);
    let configured = helper_is_configured();
    json!({
        "configured": configured,
        "settingsPresent": settings_path().is_file(),
        "helperPresent": helper_path().map(|path| PathBuf::from(path).is_file()).unwrap_or(false),
        "helperSupportsAccountIds": helper_supports_account_ids(),
        "activeIndex": active.as_ref().map(|(index, _)| *index),
        "activeAccountId": active.as_ref().map(|(_, id)| id),
        "activeAccountName": active.and_then(|(_, id)| account::find_account(&id).map(|account| account::account_display_name(&account))),
        "accountCount": accounts.len(),
        "statePath": state_path().to_string_lossy(),
    })
}

/// 安装/升级项目提供的 helper。只有用户显式调用这个命令时才会修改用户级配置。
///
/// 兼容旧版：早期 helper 是 `helper.sh`（bash + python3），若当前配置的是
/// wb-switch 的旧 helper，允许直接原地升级，并清理旧文件。
pub fn install_helper() -> Result<Value, String> {
    let target = rotate_dir().join(HELPER_FILE);
    let logic_target = rotate_dir().join(LOGIC_FILE);
    let legacy_target = rotate_dir().join(LEGACY_HELPER_FILE);
    if let Some(current) = helper_path() {
        let current = PathBuf::from(current);
        if current != target && current != legacy_target {
            return Err(format!(
                "已有其他 CodeBuddy CLI helper：{}；请先确认后再替换",
                current.display()
            ));
        }
    }

    let settings = settings_path();
    let mut settings_value = if settings.is_file() {
        read_json_file(&settings).ok_or("CodeBuddy settings.json 不是有效 JSON")?
    } else {
        json!({})
    };
    if !settings_value.is_object() {
        return Err("CodeBuddy settings.json 顶层不是对象".to_string());
    }

    std::fs::create_dir_all(rotate_dir()).map_err(|error| error.to_string())?;
    // 核心逻辑：helper.cjs（mac/Linux 直接配置它，Windows 由 .cmd 跳板调用）
    atomic_write(&logic_target, STANDARD_HELPER).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&logic_target, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    // Windows：额外写 .cmd 启动跳板，并把它配置给 apiKeyHelper
    #[cfg(windows)]
    {
        atomic_write(&target, WINDOWS_HELPER_SHIM).map_err(|error| error.to_string())?;
    }
    // 清理旧版 helper.sh（如果存在且已不再被引用）
    if legacy_target.exists() && legacy_target != target {
        let _ = std::fs::remove_file(&legacy_target);
    }

    settings_value["apiKeyHelper"] = json!(target.to_string_lossy().to_string());
    let content =
        serde_json::to_string_pretty(&settings_value).map_err(|error| error.to_string())?;
    if let Some(parent) = settings.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    atomic_write(&settings, &content).map_err(|error| error.to_string())?;

    Ok(json!({
        "ok": true,
        "configured": true,
        "helperPresent": true,
        "helperSupportsAccountIds": true,
        "message": "CodeBuddy CLI helper 已安装/升级（Node 实现，无需 Python）；新启动的 CLI 会话将按账号 ID 生效",
    }))
}

/// 将 CodeBuddy CLI 的当前账号设置为 WorkBuddy 账号库中的目标账号。
pub fn set_active_account(account_id: &str) -> Result<Value, String> {
    if !helper_is_configured() {
        return Err(
            "未检测到 CodeBuddy CLI apiKeyHelper；请先在 ~/.codebuddy/settings.json 配置轮换 helper"
                .to_string(),
        );
    }

    let accounts = account::load_accounts();
    let Some((index, canonical_id)) = account_index(&accounts, account_id) else {
        return Err(format!("账号不存在: {account_id}"));
    };

    let mut state = load_state();
    state["active"] = json!(index);
    state["activeAccountId"] = json!(canonical_id);
    state["updatedAt"] = json!(now_ms());
    std::fs::create_dir_all(rotate_dir()).map_err(|error| error.to_string())?;
    let content = serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?;
    atomic_write(&state_path(), &content).map_err(|error| error.to_string())?;

    Ok(json!({
        "ok": true,
        "configured": true,
        "synced": true,
        "activeIndex": index,
        "activeAccountId": canonical_id,
        "message": "CodeBuddy CLI 已切换；新请求会在 helper 缓存刷新后使用目标账号",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_account_by_id_or_uid_and_returns_canonical_id() {
        let accounts = vec![
            json!({"id": "a1", "uid": "u1"}),
            json!({"id": "a2", "uid": "u2"}),
        ];
        assert_eq!(account_index(&accounts, "a2"), Some((1, "a2".to_string())));
        assert_eq!(account_index(&accounts, "u1"), Some((0, "a1".to_string())));
        assert_eq!(account_index(&accounts, "missing"), None);
    }

    #[test]
    fn state_prefers_account_id_over_legacy_index() {
        let accounts = vec![
            json!({"id": "a1", "uid": "u1"}),
            json!({"id": "a2", "uid": "u2"}),
        ];
        let state = json!({"active": 0, "activeAccountId": "a2"});
        assert_eq!(
            state_account_index(&state, &accounts),
            Some((1, "a2".to_string()))
        );
    }

    #[test]
    fn legacy_index_wraps_without_panicking() {
        let accounts = vec![json!({"id": "a1"}), json!({"id": "a2"})];
        let state = json!({"active": 5});
        assert_eq!(
            state_account_index(&state, &accounts),
            Some((1, "a2".to_string()))
        );
    }

    #[test]
    fn empty_accounts_have_no_active_account() {
        assert_eq!(state_account_index(&json!({"active": 0}), &[]), None);
    }
}
