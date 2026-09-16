//! VS Code 内 CodeBuddy 扩展（`tencent-cloud.coding-copilot`）账号切换。
//!
//! 复用 WorkBuddy 账号库中的 CN token（www.codebuddy.cn），写入 VS Code 用户数据目录
//! （`%APPDATA%\Code` / `~/Library/Application Support/Code` / `$XDG_CONFIG_HOME/Code`）
//! 下 `User/globalStorage/state.vscdb` 的 Safe Storage secret：
//! `secret://{"extensionId":"tencent-cloud.coding-copilot","key":"Tencent-Cloud.coding-copilot.new.accessToken"}`。
//!
//! 与 CodeBuddy CN IDE（`codebuddy_cn_ide`）共用同一套 Safe Storage 加解密流程，
//! 仅目标描述符不同。写入前必须完全退出 VS Code（运行中写入会被覆盖），
//! 因此本模块**不主动重启**编辑器，只提示用户手动重载窗口。

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::modules::account::{self, get_str};
use crate::modules::auth_file::build_account_obj;
use crate::modules::codebuddy_cn_ide::{match_account_for_token, parse_token_from_secret};
use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::process;
use crate::modules::vscode_cn_inject::{
    inject_secret_for, read_secret_for, state_db_path_for, VscodeSafeStorageTarget,
};

const STATE_FILE: &str = "vscode_ext.json";
/// VS Code 扩展的 marketplace id（= secret key 中的 `extensionId`）。
const EXTENSION_ID: &str = "tencent-cloud.coding-copilot";
/// 扩展写入 globalState 的 id 前缀（= 载荷顶层 `id`）。
const PAYLOAD_ID: &str = "Tencent-Cloud.coding-copilot";

/// VS Code CodeBuddy 扩展目标描述符。
///
/// 目录解析是**唯一扩展点**：目前仅支持官方 `Code`，暂不处理 Insiders / Cursor /
/// 便携版 `--user-data-dir`，如需扩展只需替换 `data_dir_resolver`。
const VSCODE_TARGET: VscodeSafeStorageTarget = VscodeSafeStorageTarget {
    data_dir_resolver: vscode_data_dir,
    display_name: "VS Code",
    secret_item_prefix_extension_id: EXTENSION_ID,
    secret_key: "Tencent-Cloud.coding-copilot.new.accessToken",
    macos_keychain_service: "Code Safe Storage",
    linux_secret_tool_app_names: &["Code", "code"],
};

/// 解析 VS Code 官方版用户数据目录（单点，可扩展）。
///
/// - Windows: `%APPDATA%\Code`（`dirs::data_dir()` 即 Roaming）。
/// - macOS: `~/Library/Application Support/Code`。
/// - Linux: `$XDG_CONFIG_HOME/Code`（缺省 `~/.config/Code`）。
fn vscode_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(crate::modules::config::home_dir().join("Library/Application Support/Code"))
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir().map(|d| d.join("Code"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir().map(|d| d.join("Code"))
    }
}

/// VS Code state.vscdb 路径。
pub fn vscode_ext_state_db_path() -> Option<PathBuf> {
    state_db_path_for(&VSCODE_TARGET)
}

fn state_path() -> PathBuf {
    store_dir().join(STATE_FILE)
}

fn load_state() -> Value {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}))
}

fn save_state(state: &Value) -> Result<(), String> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    atomic_write(&path, &content).map_err(|e| e.to_string())
}

fn set_active_account_id(account_id: &str) -> Result<(), String> {
    let mut state = load_state();
    if let Some(obj) = state.as_object_mut() {
        obj.insert("activeAccountId".to_string(), json!(account_id));
        obj.insert("updatedAt".to_string(), json!(now_ms()));
    }
    save_state(&state)
}

fn active_account_id_from_state() -> Option<String> {
    load_state()
        .get("activeAccountId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 把底层解密错误翻译成更明确的用户文案（尤其非 `v10` 前缀的场景）。
fn describe_secret_error(err: String) -> String {
    if err.contains("Unexpected ciphertext prefix") || err.contains("Unsupported Linux ciphertext prefix")
    {
        format!(
            "{err}\n\n检测到 VS Code 使用了当前版本不支持的 Safe Storage 加密前缀（可能是 Chromium 127+ 的 v20 app-bound 加密）。目前仅支持 v10，请反馈该问题。"
        )
    } else {
        err
    }
}

/// 账号条目的匹配键：优先 `uid`，缺失时回退 `id`（均取非空字符串）。
fn account_entry_key(entry: &Value) -> Option<&str> {
    entry
        .get("uid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
}

/// 构造写入 VS Code 扩展 secret 的会话 JSON（merge 策略）。
///
/// 先以既有明文为底（保留 `accounts[]` 等扩展私有字段），仅覆盖账号身份相关的
/// 顶层键；读不到既有 secret（未登录）时退化为新建完整载荷（`accounts` 单元素）。
pub fn build_ext_session_json(acc: &Value, existing: Option<&str>) -> String {
    let uid = get_str(acc, "uid").unwrap_or_default();
    let domain = get_str(acc, "domain").unwrap_or_default();
    let refresh_token = get_str(acc, "refresh_token").unwrap_or_default();
    let access_token = get_str(acc, "access_token").unwrap_or_default();
    let token_type = get_str(acc, "token_type").unwrap_or_else(|| "Bearer".to_string());
    let expires_at = acc.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);

    let mut root: serde_json::Map<String, Value> = existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // 目标账号条目：显式 lastLogin=true，供顶层 account 与 accounts[] upsert 复用。
    let mut account_obj = build_account_obj(acc);
    if let Some(map) = account_obj.as_object_mut() {
        map.insert("lastLogin".to_string(), json!(true));
    }
    let auth = json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "tokenType": token_type,
        "domain": domain,
        "expiresAt": expires_at,
        "expiresIn": expires_at,
        "refreshExpiresIn": 0,
        "refreshExpiresAt": 0,
        "lastRefreshTime": now_ms(),
    });

    root.insert("id".to_string(), json!(PAYLOAD_ID));
    root.insert("token".to_string(), json!(access_token));
    root.insert("refreshToken".to_string(), json!(refresh_token));
    root.insert("expiresAt".to_string(), json!(expires_at));
    root.insert("domain".to_string(), json!(domain));
    root.insert("accessToken".to_string(), json!(format!("{uid}+{access_token}")));
    root.insert("converted".to_string(), json!(true));
    root.insert("account".to_string(), account_obj.clone());
    root.insert("auth".to_string(), auth);

    // accounts[] upsert：保留其他账号条目、数组顺序稳定，并把当前账号标记为 lastLogin。
    // 命中（按 uid，缺失回退 id）原地替换，未命中追加；数组不存在或非数组则新建单元素数组。
    let target_key = account_entry_key(&account_obj).map(str::to_string);
    let has_accounts_array = root.get("accounts").map(Value::is_array).unwrap_or(false);
    if has_accounts_array {
        if let Some(entries) = root.get_mut("accounts").and_then(Value::as_array_mut) {
            for entry in entries.iter_mut() {
                if let Some(map) = entry.as_object_mut() {
                    map.insert("lastLogin".to_string(), json!(false));
                }
            }
            let matched = target_key
                .as_deref()
                .and_then(|key| entries.iter().position(|entry| account_entry_key(entry) == Some(key)));
            match matched {
                Some(index) => entries[index] = account_obj,
                None => entries.push(account_obj),
            }
        }
    } else {
        root.insert("accounts".to_string(), json!([account_obj]));
    }

    Value::Object(root).to_string()
}

fn windows_image_stem(name: &str) -> &str {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name).trim();
    if file.len() >= 4 && file[file.len() - 4..].eq_ignore_ascii_case(".exe") {
        file[..file.len() - 4].trim()
    } else {
        file
    }
}

/// 精确映像名：`Code`（忽略 .exe / 路径 / 大小写）。
///
/// 严格排除 `Code - Insiders`、`CodeBuddy CN`、`CodeBuddy` 等其它变体。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn is_code_image_name(name: &str) -> bool {
    windows_image_stem(name).eq_ignore_ascii_case("Code")
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn keep_windows_code_row(row: &process::WindowsProcessRow) -> bool {
    let path_s = row
        .exe_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file_name = path_s.rsplit(['\\', '/']).next().unwrap_or("").trim();
    if process::is_self_image_name(&row.name) || process::is_self_image_name(file_name) {
        return false;
    }
    if process::is_crashpad_helper_name(&row.name) || process::is_crashpad_helper_name(file_name) {
        return false;
    }
    is_code_image_name(&row.name) || is_code_image_name(file_name)
}

#[cfg(target_os = "windows")]
fn windows_code_cim_process_script() -> &'static str {
    "Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
         Where-Object { $_.Name -eq 'Code.exe' } | \
         ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.ExecutablePath }"
}

#[cfg(target_os = "windows")]
fn windows_code_process_rows() -> Vec<process::WindowsProcessRow> {
    let self_pid = std::process::id();
    if let Some(stdout) = process::ps_output(windows_code_cim_process_script(), 5) {
        let rows: Vec<_> = process::parse_windows_process_rows(&stdout)
            .into_iter()
            .filter(|row| row.pid != self_pid && keep_windows_code_row(row))
            .collect();
        if !rows.is_empty() {
            return rows;
        }
    }
    process::windows_tasklist_image_rows("Code.exe")
        .into_iter()
        .filter(|row| row.pid != self_pid && keep_windows_code_row(row))
        .collect()
}

#[cfg(target_os = "macos")]
fn macos_code_main_patterns() -> Vec<String> {
    vec!["Visual Studio Code.app/Contents/MacOS".to_string()]
}

#[cfg_attr(
    not(any(test, not(any(target_os = "macos", target_os = "windows")))),
    allow(dead_code)
)]
fn linux_cmdline_is_code(cmdline: &str) -> bool {
    let lower = cmdline.to_ascii_lowercase();
    if lower.contains("wb-switch") || lower.contains("workbuddy-switch") {
        return false;
    }
    if lower.contains("crashpad") || lower.contains("--type=") {
        return false;
    }
    // 严格排除 Insiders / VSCodium / Cursor / Windsurf 等变体。
    if lower.contains("code-insiders")
        || lower.contains("vscodium")
        || lower.contains("codium")
        || lower.contains("cursor")
        || lower.contains("windsurf")
        || lower.contains("codebuddy")
    {
        return false;
    }
    // 主进程命令行形如 `/usr/share/code/code …`；辅助进程命令行不含主程序路径。
    lower.contains("/code/code") || lower.contains("/bin/code") || lower.contains("visual studio code")
}

#[cfg_attr(
    not(any(test, not(any(target_os = "macos", target_os = "windows")))),
    allow(dead_code)
)]
fn linux_exe_is_code(exe: &std::path::Path) -> bool {
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim();
    name.eq_ignore_ascii_case("code")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_code_pids() -> Vec<u32> {
    let self_pid = std::process::id();
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == self_pid {
            continue;
        }
        if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
            if linux_exe_is_code(&exe) {
                pids.push(pid);
                continue;
            }
        }
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).replace('\0', " "),
            _ => continue,
        };
        if linux_cmdline_is_code(&cmdline) {
            pids.push(pid);
        }
    }
    pids
}

/// VS Code 是否在运行（footer 语义 = GUI 主进程）。
pub fn is_vscode_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        let patterns = macos_code_main_patterns();
        !process::macos_pids_by_patterns(&patterns).is_empty()
    }
    #[cfg(target_os = "windows")]
    {
        !windows_code_process_rows().is_empty()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        !linux_code_pids().is_empty()
    }
}

/// 状态：是否安装（数据目录存在）、是否运行、当前账号（来自本地状态文件 + 账号库）。
pub fn status() -> Value {
    let data_dir = vscode_data_dir();
    let db_path = vscode_ext_state_db_path();
    let installed = data_dir.as_ref().map(|p| p.exists()).unwrap_or(false);
    // 扩展是否已安装：VS Code 数据目录下 globalStorage/<extensionId> 存在。
    let extension_installed = data_dir
        .as_ref()
        .map(|d| {
            d.join("User")
                .join("globalStorage")
                .join(EXTENSION_ID)
                .exists()
        })
        .unwrap_or(false);
    let db_exists = db_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    let running = is_vscode_running();

    let mut active_account_id = active_account_id_from_state();
    let mut active_account_name: Option<String> = None;

    if let Some(id) = active_account_id.clone() {
        if let Some(acc) = account::find_account(&id) {
            active_account_name = Some(account::account_display_name(&acc));
        } else {
            // 状态文件有记录但账号库已无此账号：视为未检测到，不回退读取 secret。
            active_account_id = None;
        }
    }

    json!({
        "installed": installed,
        "extensionInstalled": extension_installed,
        "running": running,
        "dataDir": data_dir.map(|p| p.to_string_lossy().to_string()),
        "dbPath": db_path.map(|p| p.to_string_lossy().to_string()),
        "dbExists": db_exists,
        "activeAccountId": active_account_id,
        "activeAccountName": active_account_name,
        "detectedFrom": "state",
        "statePath": state_path().to_string_lossy(),
    })
}

/// 切换 VS Code CodeBuddy 扩展账号：校验 → 注入 secret（仅写入，不重启编辑器）。
///
/// `restart` 默认 false；即便传 true 也**不主动关闭/重启** VS Code（避免丢失未保存
/// 内容），仅在返回文案里提示用户手动重载窗口。
pub fn switch_account(account_id: &str, restart: bool) -> Result<Value, String> {
    let acc = account::find_account(account_id)
        .ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let token = get_str(&acc, "access_token")
        .ok_or_else(|| "账号缺少 access_token，无法注入 VS Code CodeBuddy 扩展".to_string())?;
    if token.is_empty() {
        return Err("账号 access_token 为空".to_string());
    }

    let data_dir = vscode_data_dir()
        .ok_or_else(|| "无法定位 VS Code 数据目录".to_string())?;
    if !data_dir.exists() {
        return Err(format!(
            "未找到 VS Code 用户数据目录（{}）。请先手动打开 VS Code 并安装 CodeBuddy 扩展后重试。",
            data_dir.display()
        ));
    }

    let db_path = vscode_ext_state_db_path()
        .ok_or_else(|| "无法定位 VS Code 数据目录".to_string())?;
    if !db_path.exists() {
        return Err(format!(
            "未找到 VS Code 状态数据库（{}）。请先手动打开 VS Code 并安装、登录 CodeBuddy 扩展后重试。",
            db_path.display()
        ));
    }

    // 写入前必须先完全退出 VS Code，否则会被运行中的编辑器覆盖。
    if is_vscode_running() {
        return Err(
            "检测到 VS Code 正在运行，请先完全退出后再切换，否则写入会被 VS Code 覆盖。"
                .to_string(),
        );
    }

    // 扩展未登录（无 secret 行）→ 明确报错，不静默失败。
    let existing_secret = read_secret_for(&VSCODE_TARGET, Some(&data_dir)).map_err(describe_secret_error)?;
    let Some(existing_secret) = existing_secret else {
        return Err(
            "未在 VS Code 中找到 CodeBuddy 扩展的登录状态。请先在 VS Code 中安装并登录 CodeBuddy 扩展（腾讯云 AI 代码助手）后重试。"
                .to_string(),
        );
    };

    let session = build_ext_session_json(&acc, Some(&existing_secret));
    let db_path = inject_secret_for(&VSCODE_TARGET, &session, Some(&data_dir)).map_err(|err| {
        if err.contains("Safe Storage") || err.contains("Keychain") {
            format!(
                "注入登录状态失败：{err}\n\n请先手动打开 VS Code 并登录一次 CodeBuddy 扩展，确保系统凭据存储中存在「Code Safe Storage」条目后再试。"
            )
        } else {
            err
        }
    })?;

    set_active_account_id(account_id)?;

    let name = account::account_display_name(&acc);
    let message = if restart {
        format!(
            "已写入 VS Code CodeBuddy 扩展凭证（{name}）；为避免丢失未保存内容，未自动重启 VS Code，请手动执行「开发人员: 重新加载窗口」或在扩展中重新登录以生效。"
        )
    } else {
        format!("已写入 VS Code CodeBuddy 扩展凭证（{name}）；请手动重载 VS Code 窗口生效。")
    };

    Ok(json!({
        "ok": true,
        "account": name,
        "accountId": account_id,
        "dbPath": db_path.to_string_lossy(),
        "restarted": false,
        "message": message,
    }))
}

/// 从本机 VS Code 扩展读取当前 token；若能匹配账号库则返回匹配信息（不新建账号）。
pub fn detect_current_account() -> Result<Value, String> {
    let secret = read_secret_for(&VSCODE_TARGET, None).map_err(describe_secret_error)?;
    let Some(secret) = secret else {
        return Ok(json!({
            "ok": true,
            "found": false,
            "message": "本机 VS Code 未找到 CodeBuddy 扩展登录 secret",
        }));
    };
    let Some((uid, token)) = parse_token_from_secret(&secret) else {
        return Err("本地 VS Code CodeBuddy 扩展登录信息解析失败".to_string());
    };
    if let Some(acc) = match_account_for_token(uid.as_deref(), &token) {
        let id = get_str(&acc, "id").unwrap_or_default();
        let _ = set_active_account_id(&id);
        return Ok(json!({
            "ok": true,
            "found": true,
            "matched": true,
            "accountId": id,
            "account": account::account_meta(&acc),
            "uid": uid,
        }));
    }
    Ok(json!({
        "ok": true,
        "found": true,
        "matched": false,
        "uid": uid,
        "message": "本机 VS Code 已登录 CodeBuddy 扩展，但账号库中无匹配账号；可先用「从本机导入」或扫码登录同步账号后再切换。",
    }))
}

/// 当前登录 VS Code CodeBuddy 扩展的账号 uid（用于定位可复制的会话目录）。
///
/// 优先从扩展登录 secret 解析 uid；不可用时回退到本地状态文件记录的账号 id → 账号库 uid。
/// 任一来源都拿不到时返回 `None`（调用方据此给出「未登录」空态）。
pub fn active_ext_uid() -> Option<String> {
    if let Ok(Some(secret)) = read_secret_for(&VSCODE_TARGET, None) {
        if let Some((Some(uid), _token)) = parse_token_from_secret(&secret) {
            let uid = uid.trim().to_string();
            if !uid.is_empty() {
                return Some(uid);
            }
        }
    }
    active_account_id_from_state()
        .and_then(|id| account::find_account(&id))
        .and_then(|acc| get_str(&acc, "uid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_session_json_uses_uid_plus_token_and_vscode_id() {
        let acc = json!({
            "uid": "u-42",
            "nickname": "测试",
            "access_token": "tok-abc",
            "refresh_token": "rt-1",
            "domain": "www.codebuddy.cn",
            "expiresAt": 1234567890_i64,
        });
        let s = build_ext_session_json(&acc, None);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["id"], "Tencent-Cloud.coding-copilot");
        assert_eq!(v["accessToken"], "u-42+tok-abc");
        assert_eq!(v["token"], "tok-abc");
        assert_eq!(v["auth"]["accessToken"], "tok-abc");
        assert_eq!(v["account"]["uid"], "u-42");
        assert_eq!(v["converted"], true);
        // 未登录既有 secret 时补 accounts 单元素数组
        assert_eq!(v["accounts"].as_array().map(|a| a.len()), Some(1));
    }

    #[test]
    fn ext_session_json_merge_upserts_accounts_and_preserves_fields() {
        let acc = json!({
            "uid": "u-99",
            "nickname": "新账号",
            "access_token": "tok-new",
            "refresh_token": "rt-new",
            "domain": "www.codebuddy.cn",
            "expiresAt": 42_i64,
        });
        let existing = r#"{"accounts":[{"uid":"old","lastLogin":true}],"id":"Tencent-Cloud.coding-copilot","craftSettings":{"x":1},"converted":true}"#;
        let s = build_ext_session_json(&acc, Some(existing));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["accessToken"], "u-99+tok-new");
        assert_eq!(v["account"]["uid"], "u-99");
        assert_eq!(v["account"]["lastLogin"], true);
        // 旧账号条目仍在，但被标记为非当前登录
        assert_eq!(v["accounts"][0]["uid"], "old");
        assert_eq!(v["accounts"][0]["lastLogin"], false);
        // 目标账号被追加并标为当前登录
        assert_eq!(v["accounts"][1]["uid"], "u-99");
        assert_eq!(v["accounts"][1]["lastLogin"], true);
        assert_eq!(v["accounts"].as_array().map(|a| a.len()), Some(2));
        // 扩展私有字段保留，不被覆盖
        assert_eq!(v["craftSettings"]["x"], 1);
    }

    #[test]
    fn ext_session_json_upsert_replaces_existing_entry_in_place() {
        let acc = json!({
            "uid": "u-99",
            "nickname": "新账号",
            "access_token": "tok-new",
            "domain": "www.codebuddy.cn",
            "expiresAt": 7_i64,
        });
        let existing =
            r#"{"accounts":[{"uid":"u-99","lastLogin":false,"label":"旧"},{"uid":"other"}]}"#;
        let s = build_ext_session_json(&acc, Some(existing));
        let v: Value = serde_json::from_str(&s).unwrap();
        // 命中 uid：原地替换，不产生重复条目
        assert_eq!(v["accounts"].as_array().map(|a| a.len()), Some(2));
        assert_eq!(v["accounts"][0]["uid"], "u-99");
        assert_eq!(v["accounts"][0]["lastLogin"], true);
        assert_eq!(v["accounts"][1]["uid"], "other");
        assert_eq!(v["accounts"][1]["lastLogin"], false);
    }

    #[test]
    fn code_image_name_is_exact_not_insiders_or_codebuddy() {
        assert!(is_code_image_name("Code.exe"));
        assert!(is_code_image_name("code"));
        assert!(is_code_image_name(r"C:\Users\Zhou\AppData\Local\Programs\Microsoft VS Code\Code.exe"));
        assert!(!is_code_image_name("Code - Insiders.exe"));
        assert!(!is_code_image_name("CodeBuddy CN.exe"));
        assert!(!is_code_image_name("CodeBuddy.exe"));
        assert!(!is_code_image_name("workbuddy-switch.exe"));
        assert!(!is_code_image_name("wb-switch"));
    }

    #[test]
    fn windows_code_rows_drop_self_insiders_and_codebuddy() {
        let stdout = "\
2001|workbuddy-switch|C:\\apps\\workbuddy-switch.exe
2002|Code - Insiders|C:\\Users\\Zhou\\AppData\\Local\\Programs\\Microsoft VS Code Insiders\\Code - Insiders.exe
2003|Code|C:\\Users\\Zhou\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe
2004|crashpad_handler|C:\\x\\crashpad_handler.exe
2005|CodeBuddy CN|D:\\Programs\\CodeBuddy CN\\CodeBuddy CN.exe
2006|Code|C:\\Users\\Zhou\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe
";
        let kept: Vec<u32> = process::parse_windows_process_rows(stdout)
            .into_iter()
            .filter(keep_windows_code_row)
            .map(|row| row.pid)
            .collect();
        assert_eq!(kept, vec![2003, 2006]);
    }

    #[test]
    fn linux_cmdline_matcher_accepts_code_not_variants() {
        assert!(linux_cmdline_is_code("/usr/share/code/code --unity-launch"));
        assert!(linux_cmdline_is_code("/usr/bin/code --no-sandbox"));
        assert!(linux_exe_is_code(std::path::Path::new("/usr/share/code/code")));
        assert!(!linux_cmdline_is_code("/usr/bin/workbuddy-switch"));
        assert!(!linux_cmdline_is_code("/usr/share/code/code --type=gpu-process"));
        assert!(!linux_cmdline_is_code("/usr/bin/code-insiders"));
        assert!(!linux_cmdline_is_code("/opt/codebuddy-cn/codebuddy-cn"));
        assert!(!linux_exe_is_code(std::path::Path::new("/usr/bin/code-insiders")));
    }

    #[test]
    fn state_db_path_ends_with_state_vscdb() {
        let Some(db) = vscode_ext_state_db_path() else {
            return;
        };
        assert!(db.ends_with("state.vscdb"));
        assert!(db.to_string_lossy().contains("globalStorage"));
    }

    #[test]
    fn target_secret_key_matches_vscode_extension() {
        use crate::modules::vscode_cn_inject::secret_storage_item_key_for;
        assert_eq!(
            secret_storage_item_key_for(&VSCODE_TARGET),
            r#"secret://{"extensionId":"tencent-cloud.coding-copilot","key":"Tencent-Cloud.coding-copilot.new.accessToken"}"#
        );
    }
}
