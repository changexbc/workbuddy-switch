//! VS Code 内 CodeBuddy 扩展（`tencent-cloud.coding-copilot`）账号切换。
//!
//! 复用 WorkBuddy 账号库中的 CN token（www.codebuddy.cn），写入 VS Code 用户数据目录
//! （`%APPDATA%\Code` / `~/Library/Application Support/Code` / `$XDG_CONFIG_HOME/Code`）
//! 下 `User/globalStorage/state.vscdb` 的 Safe Storage secret：
//! `secret://{"extensionId":"tencent-cloud.coding-copilot","key":"Tencent-Cloud.coding-copilot.new.accessToken"}`。
//!
//! 与 CodeBuddy CN IDE（`codebuddy_cn_ide`）共用同一套 Safe Storage 加解密流程，
//! 仅目标描述符不同。写入前 VS Code 必须完全退出（主进程把 ItemTable 全量缓存在内存，
//! 运行中写入读不到也留不住）：`restart = true` 时由本模块负责
//! 「优雅退出 → 等待进程退出 → 注入 → 重新打开」，超时只报错、**绝不强杀**。
//! 扩展从未登录（`state.vscdb` 无会话 secret 行）时按新会话载荷写入，不再要求先登录一次。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
use std::time::Duration;
#[cfg(not(target_os = "windows"))]
use std::time::Instant;
#[cfg(target_os = "windows")]
use std::time::Instant;

use crate::modules::account::{self, get_str};
use crate::modules::auth_file::build_account_obj;
use crate::modules::codebuddy_cn_ide::{match_account_for_token, parse_token_from_secret};
use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::process;
use crate::modules::variant::codebuddy_domain_for;
use crate::modules::vscode_cn_inject::{
    has_secret_row_for, inject_secret_for, read_secret_for, state_db_path_for,
    VscodeSafeStorageTarget,
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
    if err.contains("Unexpected ciphertext prefix")
        || err.contains("Unsupported Linux ciphertext prefix")
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
/// 先以既有明文为底（保留 `accounts[]` / `auth` 等扩展私有字段），仅覆盖账号身份相关的
/// 顶层键与 `auth` 内的凭据键；读不到既有 secret（未登录）时退化为新建完整载荷
/// （`accounts` 单元素）。
pub fn build_ext_session_json(acc: &Value, existing: Option<&str>) -> String {
    let uid = get_str(acc, "uid").unwrap_or_default();
    let domain = codebuddy_domain_for(
        get_str(acc, "domain").unwrap_or_default().as_str(),
        account::variant_of(acc),
    );
    let refresh_token = get_str(acc, "refresh_token").unwrap_or_default();
    let access_token = get_str(acc, "access_token").unwrap_or_default();
    let token_type = get_str(acc, "token_type").unwrap_or_else(|| "Bearer".to_string());
    let expires_at = acc.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
    let refresh_expires_at = acc.get("refreshExpiresAt").and_then(|v| v.as_i64());

    let mut root: serde_json::Map<String, Value> = existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // 目标账号条目：显式 lastLogin=true，供顶层 account 与 accounts[] upsert 复用。
    let mut account_obj = build_account_obj(acc);
    if let Some(map) = account_obj.as_object_mut() {
        map.insert("lastLogin".to_string(), json!(true));
    }

    // auth 同样走 merge：以既有 auth 为底，只覆盖身份/凭据键，扩展私有键
    //（`scope` / `sessionState` / `notBeforePolicy` 等）原样保留。
    let mut auth: serde_json::Map<String, Value> = root
        .get("auth")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    auth.insert("accessToken".to_string(), json!(access_token));
    auth.insert("refreshToken".to_string(), json!(refresh_token));
    auth.insert("tokenType".to_string(), json!(token_type));
    auth.insert("domain".to_string(), json!(domain));
    auth.insert("expiresAt".to_string(), json!(expires_at));
    // 注：`expiresIn` / `refreshExpiresIn` 刻意沿用写绝对时间的既有写法，与线上已验证的
    // CN IDE 实现（`codebuddy_cn_ide::build_session_json`）保持一致，不在本次修正。
    auth.insert("expiresIn".to_string(), json!(expires_at));
    auth.insert("refreshExpiresIn".to_string(), json!(0));
    match refresh_expires_at {
        // 账号库优先
        Some(value) => {
            auth.insert("refreshExpiresAt".to_string(), json!(value));
        }
        // 账号库没有：保留既有值；连既有值也没有时维持旧的 0（不让键凭空消失）。
        None if !auth.contains_key("refreshExpiresAt") => {
            auth.insert("refreshExpiresAt".to_string(), json!(0));
        }
        None => {}
    }
    auth.insert("lastRefreshTime".to_string(), json!(now_ms()));

    root.insert("id".to_string(), json!(PAYLOAD_ID));
    root.insert("token".to_string(), json!(access_token));
    root.insert("refreshToken".to_string(), json!(refresh_token));
    root.insert("expiresAt".to_string(), json!(expires_at));
    root.insert("domain".to_string(), json!(domain));
    root.insert(
        "accessToken".to_string(),
        json!(format!("{uid}+{access_token}")),
    );
    root.insert("converted".to_string(), json!(true));
    root.insert("account".to_string(), account_obj.clone());
    root.insert("auth".to_string(), Value::Object(auth));

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
            let matched = target_key.as_deref().and_then(|key| {
                entries
                    .iter()
                    .position(|entry| account_entry_key(entry) == Some(key))
            });
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
    lower.contains("/code/code")
        || lower.contains("/bin/code")
        || lower.contains("visual studio code")
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

/// 枚举 Linux 上的 Code 进程：`(pid, 可执行文件路径)`。
///
/// 可执行文件路径用于关闭后按同一路径重新打开（不依赖 `code` CLI / PATH）；
/// 读不到 `/proc/<pid>/exe` 时该项为 `None`。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_code_processes() -> Vec<(u32, Option<PathBuf>)> {
    let self_pid = std::process::id();
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return rows;
    };
    for entry in entries.flatten() {
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == self_pid {
            continue;
        }
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
        if exe.as_deref().is_some_and(linux_exe_is_code) {
            rows.push((pid, exe));
            continue;
        }
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).replace('\0', " "),
            _ => continue,
        };
        if linux_cmdline_is_code(&cmdline) {
            rows.push((pid, exe));
        }
    }
    rows
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_code_pids() -> Vec<u32> {
    linux_code_processes()
        .into_iter()
        .map(|(pid, _)| pid)
        .collect()
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

/// 状态：是否安装（数据目录存在）、是否运行、是否登录（只读）、当前账号（本地状态文件 + 账号库）。
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
    // 登录态 = `state.vscdb` 里是否存在会话 secret 行：只查 key、不解密（macOS 解密会弹
    // 钥匙串授权，绝不能进这条轮询路径）。查询失败/文件不存在一律 false，仅用于文案。
    let logged_in = has_secret_row_for(&VSCODE_TARGET, data_dir.as_deref()).unwrap_or(false);

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
        "loggedIn": logged_in,
        "dataDir": data_dir.map(|p| p.to_string_lossy().to_string()),
        "dbPath": db_path.map(|p| p.to_string_lossy().to_string()),
        "dbExists": db_exists,
        "activeAccountId": active_account_id,
        "activeAccountName": active_account_name,
        "detectedFrom": "state",
        "statePath": state_path().to_string_lossy(),
    })
}

/// 等待 VS Code 退出的默认上限（秒）：超时只报错，不强杀（D2/D9）。
const VSCODE_CLOSE_TIMEOUT_SECS: i64 = 60;

/// 运行中 + 手动模式（`restart=false`）的报错文案：与改造前逐字一致。
const RUNNING_MANUAL_HINT: &str =
    "检测到 VS Code 正在运行，请先完全退出后再切换，否则写入会被 VS Code 覆盖。";

/// 关闭前采集的 VS Code 运行实例快照。
///
/// **必须在关闭前采集**：编辑器退出后进程表里已无可回溯的启动路径，重开就没依据了。
#[derive(Debug)]
struct RunningInstance {
    /// 命中的相关进程 PID（macOS 主进程层 / Windows 全部 `Code.exe` / Linux Code 进程）。
    pids: Vec<u32>,
    /// 主进程可执行文件路径（Windows 来自 CIM `ExecutablePath`，Linux 读 `/proc/<pid>/exe`）。
    main_exe: Option<PathBuf>,
    /// macOS 由主进程 args 回溯出的 `.app` bundle 路径。
    macos_bundle: Option<PathBuf>,
}

impl RunningInstance {
    fn running(&self) -> bool {
        !self.pids.is_empty()
    }

    /// 重开目标：macOS 优先 `.app` bundle，Windows / Linux 用可执行文件路径。
    fn launch_target(&self) -> Option<&Path> {
        self.macos_bundle.as_deref().or(self.main_exe.as_deref())
    }
}

#[cfg(target_os = "macos")]
fn running_instance() -> RunningInstance {
    let rows = process::macos_rows_by_patterns(&macos_code_main_patterns());
    let pids = rows.iter().map(|(pid, _)| *pid).collect();
    let macos_bundle = rows
        .iter()
        .find_map(|(_, args)| process::extract_app_bundle_from_args(args));
    RunningInstance {
        pids,
        main_exe: None,
        macos_bundle,
    }
}

#[cfg(target_os = "windows")]
fn running_instance() -> RunningInstance {
    let rows = windows_code_process_rows();
    let pids = rows.iter().map(|row| row.pid).collect();
    let main_exe = rows.iter().find_map(|row| row.exe_path.clone());
    RunningInstance {
        pids,
        main_exe,
        macos_bundle: None,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn running_instance() -> RunningInstance {
    let rows = linux_code_processes();
    let pids = rows.iter().map(|(pid, _)| *pid).collect();
    let main_exe = rows.iter().find_map(|(_, exe)| exe.clone());
    RunningInstance {
        pids,
        main_exe,
        macos_bundle: None,
    }
}

/// 关闭超时文案：列出剩余 PID 与可操作提示（**不做强杀**）。
///
/// Windows 走 [`close_timeout_error_windows`]（要点名窗口标题），这条只在 macOS / Linux
/// 与单测里用得到。
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn close_timeout_error(alive: &[u32]) -> String {
    let pids: Vec<String> = alive.iter().map(|pid| pid.to_string()).collect();
    format!(
        "等待 VS Code 退出超时（仍有进程运行: {}）。请在 VS Code 中处理保存提示；若你在等待期间重新打开过 VS Code，请退出后重试。",
        pids.join(", ")
    )
}

/// Windows 侧「按 PID 点关闭按钮」的最小 user32 封装。
///
/// 为什么不只用 `taskkill /PID <pid> /T`（不带 `/F`）：对**没有窗口**的进程它只会返回
/// 「只能强制终止」（本机实测退出码 255、进程原样存活），而快照里绝大多数 pid 都是这一类
/// （语言服务 / GPU / 扩展宿主）；真正要关的主窗口是否被它命中，取决于它自己的窗口选择，
/// 我们既看不见也控制不了 —— 实测两次「等待 VS Code 退出超时」时，主窗口都还开着，
/// 而手动点 ✕ 秒关。这里改为自己 `EnumWindows` 找出该 PID 的可见顶层窗口再发 `SC_CLOSE`：
/// 与用户点 ✕ 完全等价、同样**不强杀**，而且能读到窗口标题写进报错文案。
///
/// 与 `jetbrains::win_close` 同源（同样的 `extern "system"` 最小 API 面，不扩大依赖图），
/// 差异是本处需要窗口标题、需要能对同一批窗口重复发消息。
#[cfg(target_os = "windows")]
mod win_close {
    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(lpEnumFunc: WndEnumProc, lParam: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, lpdwProcessId: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn GetWindowTextW(hwnd: isize, lpString: *mut u16, nMaxCount: i32) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }
    type WndEnumProc = Option<unsafe extern "system" fn(isize, isize) -> i32>;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(handle: isize, exit_code: *mut u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    const WM_SYSCOMMAND: u32 = 0x0112;
    const SC_CLOSE: usize = 0xF060;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// `GetExitCodeProcess` 的「仍在运行」哨兵值。
    const STILL_ACTIVE: u32 = 259;
    /// 窗口标题缓冲上限（UTF-16 字符数）：只用于报错文案，超长截断可接受。
    const TITLE_MAX: usize = 256;

    /// 一个可见顶层窗口。
    pub(super) struct VisibleWindow {
        pub(super) hwnd: isize,
        pub(super) pid: u32,
        pub(super) title: String,
    }

    unsafe extern "system" fn enum_cb(hwnd: isize, lparam: isize) -> i32 {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return 1;
        }
        let mut buf = [0u16; TITLE_MAX];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), TITLE_MAX as i32);
        let title = if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let out = &mut *(lparam as *mut Vec<VisibleWindow>);
        out.push(VisibleWindow { hwnd, pid, title });
        1
    }

    /// 当前全部可见顶层窗口。
    pub(super) fn visible_windows() -> Vec<VisibleWindow> {
        let mut rows: Vec<VisibleWindow> = Vec::new();
        unsafe {
            EnumWindows(Some(enum_cb), &mut rows as *mut _ as isize);
        }
        rows
    }

    /// 对指定 PID 的全部可见窗口发 `SC_CLOSE`（= 点 ✕）；返回命中的窗口数。
    pub(super) fn post_close(pid: u32) -> usize {
        let mut hit = 0;
        for window in visible_windows().iter().filter(|window| window.pid == pid) {
            unsafe {
                PostMessageW(window.hwnd, WM_SYSCOMMAND, SC_CLOSE, 0);
            }
            hit += 1;
        }
        hit
    }

    /// 进程是否仍存活：读退出码判定，不起子进程（`tasklist` 轮询在进程多的机器上很贵）。
    pub(super) fn pid_alive(pid: u32) -> bool {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle == 0 {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }
}

/// 关闭超时文案（Windows 专用）：点名「没关掉的窗口标题」+ 关闭请求的返回。
///
/// 与 [`close_timeout_error`] 分开是因为要回答的问题不同：这里要回答「到底是哪个窗口没关掉」，
/// 而不是列一串 PID 让用户自己对；关闭命令的返回也一并带上，避免再出现「发了但没人知道结果」。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn close_timeout_error_windows(
    alive: &[u32],
    windows: &[(u32, String)],
    notes: &[String],
) -> String {
    let mut message = if windows.is_empty() {
        let pids: Vec<String> = alive.iter().map(u32::to_string).collect();
        format!(
            "等待 VS Code 退出超时（仍有进程运行: {}）。",
            pids.join(", ")
        )
    } else {
        let named: Vec<String> = windows
            .iter()
            .map(|(pid, title)| {
                let title = title.trim();
                if title.is_empty() {
                    format!("PID {pid}")
                } else {
                    format!("『{title}』(PID {pid})")
                }
            })
            .collect();
        format!("VS Code 窗口 {} 没有关闭。", named.join("、"))
    };
    message
        .push_str("请在 VS Code 中处理保存提示；若你在等待期间重新打开过 VS Code，请退出后重试。");
    if !notes.is_empty() {
        message.push_str(&format!("（关闭请求返回：{}）", notes.join("；")));
    }
    message
}

/// 优雅关闭 VS Code → 等待进程消失；超时返回可读错误，**绝不强杀**。
///
/// `pids` 取自**关闭前**采集的 [`RunningInstance`] 快照：Windows / Linux 按这些 PID 精确
/// 关闭，不再二次探测（二次探测会多花一次 CIM/PowerShell 探测，且两次探测之间编辑器自己
/// 退出时会拿到空集合、被误判成「我们关掉了它」而在写入后又把它拉起来）。
///
/// 契约（对齐 `.trellis/spec/wb-switch-core/backend/codebuddy-cn-process.md`）：
/// 禁止 `pkill -f` / `pgrep -f` / `taskkill /IM`，一律精确 PID；**绝不 `/F` 强杀**。
///
/// 等待判据按平台不同（都不是「主进程不存在」这种早退）：
/// - macOS：按主进程模式命中的进程集合为空；
/// - Windows：快照里**带可见窗口**的进程（即用户看得见的那个实例）全部退出；一个可见窗口都
///   没有时退回「全部快照 pid」。收窄的理由：库写入由主进程负责，只剩语言服务 / GPU /
///   扩展宿主时写入已经安全（它们经主进程 IPC 写库，主进程没了就写不进去），按原判据等
///   它们只会平白超时 —— 实测一次超时的 18 个残留里 17 个属于这一类；
/// - 其它平台：全部快照 pid 退出。
fn close_vscode(pids: &[u32], timeout_secs: i64) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // macOS 无需 PID 列表：`osascript quit app id` 走原生退出，等待用的是「主进程模式
        // 命中的全部进程」（含 Helper 辅助进程，见 close_vscode 的调用方契约）。
        let _ = pids;
        let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
        let started = Instant::now();
        let patterns = macos_code_main_patterns();
        // 走原生退出流程：保存提示 / 热退出保护由 VS Code 自己负责。
        let quit_script = r#"quit app id "com.microsoft.VSCode""#;
        match process::run_cmd_timeout("osascript", &["-e", quit_script], 10) {
            Some(out) if !out.status.success() => eprintln!(
                "[vscode] osascript quit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
            None => eprintln!("[vscode] osascript quit timed out"),
            _ => {}
        }
        let remaining = timeout
            .saturating_sub(started.elapsed())
            .max(Duration::from_millis(100));
        let alive = if process::wait_macos_main_gone(&patterns, remaining) {
            Vec::new()
        } else {
            // 超时：把仍存活的 PID 报给用户，不做清杀。
            process::macos_pids_by_patterns(&patterns)
        };
        if alive.is_empty() {
            Ok(())
        } else {
            Err(close_timeout_error(&alive))
        }
    }
    #[cfg(target_os = "windows")]
    {
        if pids.is_empty() {
            return Ok(());
        }
        // ① 关闭前先看清「快照里哪些 pid 带着可见窗口」：既用于报错文案，也用于决定必须等到谁退出。
        let blocked: Vec<(u32, String)> = {
            let mut rows: Vec<(u32, String)> = win_close::visible_windows()
                .into_iter()
                .filter(|window| pids.contains(&window.pid))
                .map(|window| (window.pid, window.title))
                .collect();
            rows.sort();
            rows.dedup();
            rows
        };
        // ② 必须等到「带可见窗口的进程」全部退出（见函数头契约）。没有可见窗口时退回等全部快照 pid。
        let must_wait: Vec<u32> = if blocked.is_empty() {
            pids.to_vec()
        } else {
            blocked.iter().map(|(pid, _)| *pid).collect()
        };
        // ③ 第一轮关闭：`taskkill /PID /T` **不带 `/F`**（等价 WM_CLOSE，会走保存提示），
        //    外加自己向可见窗口发 SC_CLOSE。taskkill 的返回全部收下来：它对无窗口进程只会报
        //    「只能强制终止」，那属于预期噪声；其余失败要能进报错文案，不再是一条 `let _ =`。
        let mut notes: Vec<String> = Vec::new();
        for pid in pids {
            let pid_arg = pid.to_string();
            match process::run_cmd_timeout("taskkill", &["/PID", &pid_arg, "/T"], 10) {
                Some(out) => {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let text = text.trim();
                    if !out.status.success()
                        && !text.is_empty()
                        && !text.to_ascii_lowercase().contains("forcefully")
                    {
                        notes.push(format!("taskkill {pid}: {text}"));
                    }
                }
                None => notes.push(format!("taskkill {pid}: 未能在超时内执行")),
            }
        }
        let window_hits: usize = must_wait
            .iter()
            .map(|pid| win_close::post_close(*pid))
            .sum();
        notes.push(format!("已向 {window_hits} 个可见窗口发出关闭请求"));
        // ④ 等待退出；每 10s 对仍可见的窗口重发一次 SC_CLOSE（有些 Electron 应用会吞掉第一次）。
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1) as u64);
        let mut last_nudge = Instant::now();
        loop {
            let alive: Vec<u32> = must_wait
                .iter()
                .copied()
                .filter(|pid| win_close::pid_alive(*pid))
                .collect();
            if alive.is_empty() {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                // 超时：把「还开着的窗口标题」与关闭请求的返回一并报出，并落一份诊断日志
                // （这一路以前完全没有留痕，两次线上超时都只能靠猜）。
                let remaining: Vec<(u32, String)> = win_close::visible_windows()
                    .into_iter()
                    .filter(|window| alive.contains(&window.pid))
                    .map(|window| (window.pid, window.title))
                    .collect();
                crate::modules::error_log::record(
                    "backend",
                    "VS Code 关闭超时",
                    &format!("alive={alive:?} windows={remaining:?} notes={notes:?}"),
                );
                return Err(close_timeout_error_windows(&alive, &remaining, &notes));
            }
            if now.duration_since(last_nudge) >= Duration::from_secs(10) {
                for pid in &alive {
                    win_close::post_close(*pid);
                }
                last_nudge = now;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if pids.is_empty() {
            Ok(())
        } else {
            let args: Vec<String> = std::iter::once("-TERM".to_string())
                .chain(pids.iter().map(|pid| pid.to_string()))
                .collect();
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let _ = process::run_cmd_timeout("kill", &arg_refs, 10);
            let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1) as u64);
            loop {
                let alive: Vec<u32> = pids
                    .iter()
                    .copied()
                    .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
                    .collect();
                if alive.is_empty() {
                    break Ok(());
                }
                if Instant::now() >= deadline {
                    break Err(close_timeout_error(&alive));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// `open` 失败原因：优先 stderr，为空时回退出码。
#[cfg(target_os = "macos")]
fn open_error_reason(out: &std::process::Output) -> String {
    let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if reason.is_empty() {
        format!("open 退出码 {}", out.status.code().unwrap_or(-1))
    } else {
        reason
    }
}

/// 重新打开 VS Code：只用关闭前记录的路径（D8），不依赖 `code` CLI / PATH。
fn launch_vscode(inst: &RunningInstance) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let bundle = inst
            .launch_target()
            .filter(|path| process::is_app_bundle(path))
            .map(Path::to_path_buf);
        match bundle {
            Some(bundle) => {
                let app = bundle.to_string_lossy().into_owned();
                match process::run_cmd_timeout("open", &[app.as_str()], 10) {
                    Some(out) if out.status.success() => Ok(()),
                    Some(out) => Err(format!(
                        "重新打开 VS Code 失败: {}（路径: {}）",
                        open_error_reason(&out),
                        bundle.display()
                    )),
                    None => Err(format!(
                        "重新打开 VS Code 失败: open 超时（路径: {}）",
                        bundle.display()
                    )),
                }
            }
            // 退化：记录不到 bundle 路径时按 bundle id 打开。
            None => match process::run_cmd_timeout("open", &["-b", "com.microsoft.VSCode"], 10) {
                Some(out) if out.status.success() => Ok(()),
                Some(out) => Err(format!(
                    "重新打开 VS Code 失败: {}",
                    open_error_reason(&out)
                )),
                None => Err("重新打开 VS Code 失败: open 超时".to_string()),
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let candidates = launch_candidates(inst.launch_target());
        if candidates.is_empty() {
            return Err("未能记录 VS Code 可执行文件路径，请手动打开 VS Code".to_string());
        }
        let mut last = String::new();
        for exe in &candidates {
            match process::cmd_builder(exe)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => return Ok(()),
                Err(e) => last = format!("{e}（路径: {}）", exe.display()),
            }
        }
        Err(format!("重新打开 VS Code 失败: {last}"))
    }
}

/// 重开候选，按优先级排列。
///
/// 先试关闭前记录的路径；它可能不可用——旧版本按 UTF-8 解码 PowerShell 输出，非 ASCII
/// 用户目录会被写成替换字符（`C:\Users\???\…`），拿它 spawn 只会得到「系统找不到指定的
/// 路径」。那就退到 VS Code 系 CLI：CLI 走自己的安装信息，不受这个问题影响，而且无参启动
/// 会恢复上次的窗口。
///
/// 只在非 macOS 的重开分支里用到（macOS 走 `open -a`），故 macOS 下允许未使用。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn launch_candidates(main_exe: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !out.contains(&path) {
            out.push(path);
        }
    };
    if let Some(path) = main_exe.filter(|p| !p.to_string_lossy().contains('\u{fffd}')) {
        push(path.to_path_buf());
    }
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    for name in vscode_cli_names() {
        if let Some(found) = path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
        {
            push(found);
        }
    }
    for dir in vscode_cli_dirs() {
        for name in vscode_cli_names() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                push(candidate);
            }
        }
    }
    out
}

/// VS Code 系客户端的命令行入口名。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn vscode_cli_names() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        vec![
            "code.cmd",
            "code-insiders.cmd",
            "codium.cmd",
            "cursor.cmd",
            "windsurf.cmd",
        ]
    } else {
        vec!["code", "code-insiders", "codium", "cursor", "windsurf"]
    }
}

/// 常见安装位置下的 CLI 目录。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn vscode_cli_dirs() -> Vec<PathBuf> {
    if !cfg!(target_os = "windows") {
        return vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")];
    }
    let mut out = Vec::new();
    for var in ["LOCALAPPDATA", "PROGRAMFILES", "ProgramFiles(x86)"] {
        let Some(base) = std::env::var_os(var) else {
            continue;
        };
        let base = PathBuf::from(base);
        for suffix in [
            "Programs/Microsoft VS Code/bin",
            "Microsoft VS Code/bin",
            "Programs/VSCodium/bin",
            "VSCodium/bin",
            "Programs/cursor/resources/app/bin",
            "Programs/Windsurf/bin",
        ] {
            out.push(base.join(suffix));
        }
    }
    out
}

/// 注入失败的文案映射（D3）：钥匙串 / Safe Storage 缺失时补一条可操作提示。
fn describe_inject_error(err: String) -> String {
    if err.contains("Safe Storage") || err.contains("Keychain") {
        format!(
            "注入登录状态失败：{err}\n\n请先手动打开 VS Code 并登录一次 CodeBuddy 插件，确保系统凭据存储中存在「Code Safe Storage」条目后再试。"
        )
    } else {
        err
    }
}

/// 切换成功文案（design §5）：区分「已重新打开」「重开失败」「本来没运行」。
fn switch_message(name: &str, restarted: bool, relaunch_error: Option<&str>) -> String {
    match (restarted, relaunch_error) {
        (true, _) => {
            format!("已写入 VS Code CodeBuddy 插件凭证（{name}），VS Code 已重新打开。")
        }
        (false, Some(err)) => format!(
            "已写入 VS Code CodeBuddy 插件凭证（{name}），但自动重新打开 VS Code 失败：{err}；请手动打开 VS Code 生效。"
        ),
        (false, None) => {
            format!("已写入 VS Code CodeBuddy 插件凭证（{name}）；请打开 VS Code 生效。")
        }
    }
}

/// 切换前置校验（账号 / `access_token` / 数据目录 / `state.vscdb`）。
///
/// 必须在关闭编辑器**之前**执行：账号不存在、`access_token` 为空、目录或 DB 缺失这类
/// 「无论怎么关都注定失败」的目标，不该让用户的编辑器被关掉。返回目标账号条目与用户数据目录；
/// 幂等且廉价，[`switch_account_after_close`] 会再校验一次。
pub(crate) fn validate_switch_target(account_id: &str) -> Result<(Value, PathBuf), String> {
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let token = get_str(&acc, "access_token")
        .ok_or_else(|| "账号缺少 access_token，无法注入 VS Code CodeBuddy 插件".to_string())?;
    if token.is_empty() {
        return Err("账号 access_token 为空".to_string());
    }

    let data_dir = vscode_data_dir().ok_or_else(|| "无法定位 VS Code 数据目录".to_string())?;
    if !data_dir.exists() {
        return Err(format!(
            "未找到 VS Code 用户数据目录（{}）。请先手动打开 VS Code 并安装 CodeBuddy 插件后重试。",
            data_dir.display()
        ));
    }

    let db_path =
        vscode_ext_state_db_path().ok_or_else(|| "无法定位 VS Code 数据目录".to_string())?;
    if !db_path.exists() {
        return Err(format!(
            "未找到 VS Code 状态数据库（{}）。请先手动打开一次 VS Code（无需先登录插件）后重试。",
            db_path.display()
        ));
    }

    Ok((acc, data_dir))
}

/// 关闭前采集的实例快照（不透明句柄）：交给调用方持有，供关闭后的重开复用（D8）。
///
/// 关掉编辑器后再去探测进程已经拿不到启动路径，所以快照必须由本模块在关闭前采集。
#[derive(Debug)]
pub struct ClosedEditor(RunningInstance);

/// 关闭决策 + 执行：未运行 → `Ok(None)`（不主动拉起）；运行中且 `restart=false` → `Err`
/// （手动模式保持旧行为，编辑器未被触碰）；运行中且 `restart=true` → 优雅退出后返回关闭前快照。
///
/// 关闭失败 / 超时一律 `Err`（D6）：不吞错、不返回 `Ok`。
pub fn close_vscode_for_switch(restart: bool) -> Result<Option<ClosedEditor>, String> {
    close_vscode_for_switch_with(running_instance(), restart)
}

/// [`close_vscode_for_switch`] 的可测实现：运行实例由调用方注入，便于单测关闭决策而不碰真实进程。
fn close_vscode_for_switch_with(
    inst: RunningInstance,
    restart: bool,
) -> Result<Option<ClosedEditor>, String> {
    if !inst.running() {
        return Ok(None);
    }
    if !restart {
        return Err(RUNNING_MANUAL_HINT.to_string());
    }
    close_vscode(&inst.pids, VSCODE_CLOSE_TIMEOUT_SECS)?;
    Ok(Some(ClosedEditor(inst)))
}

/// best-effort 重新打开本次由我们关闭的编辑器（复用关闭前记录的路径，D8）。
pub fn relaunch_closed_editor(closed: &ClosedEditor) -> Result<(), String> {
    launch_vscode(&closed.0)
}

/// 「编辑器已按需关闭后」的切换主体：读既有会话 → 注入 → 记录当前账号 → 重开（仅本次我们关闭时）。
///
/// `closed = Some(..)` 表示编辑器已由 [`close_vscode_for_switch`] 优雅退出；`None` 表示本来没运行。
/// **任何失败路径**（含校验早退、读既有 session 失败、注入失败、写状态失败）在编辑器是我们关的
/// 时候都会先 best-effort 开回来再报错（D4）；重开失败只降级为警告文案，写入本身已生效，
/// 整体仍算成功。
pub fn switch_account_after_close(
    account_id: &str,
    closed: Option<ClosedEditor>,
) -> Result<Value, String> {
    match switch_account_after_close_inner(account_id, closed.as_ref()) {
        Ok(value) => Ok(value),
        Err(error) => Err(relaunch_closed_on_error(
            closed.as_ref(),
            relaunch_closed_editor,
            error,
        )),
    }
}

/// 失败兜底（D4）：`closed.is_some()` 时先尽力把编辑器开回来，再回报原错误。
///
/// `relaunch` 由调用方注入——真实实现会**真的启动编辑器**，因此单测必须在不碰进程的前提下
/// 验证「关了的要开回来 / 本来没关的不许动」这两条分支。
fn relaunch_closed_on_error(
    closed: Option<&ClosedEditor>,
    relaunch: impl FnOnce(&ClosedEditor) -> Result<(), String>,
    error: String,
) -> String {
    match closed {
        None => error,
        Some(closed) => match relaunch(closed) {
            Ok(()) => error,
            // 两条信息都要给用户：否则他不知道编辑器还关着，也不知道为什么。
            Err(launch_error) => format!("{error}\n\n{launch_error}"),
        },
    }
}

/// [`switch_account_after_close`] 的主体：**不负责失败兜底**（由外层统一处理，才能覆盖
/// 「校验就失败」这类还没开始切换的早退）。
fn switch_account_after_close_inner(
    account_id: &str,
    closed: Option<&ClosedEditor>,
) -> Result<Value, String> {
    let (acc, data_dir) = validate_switch_target(account_id)?;

    // 未登录（无既有会话行）→ 走新建完整载荷，不再要求用户先登录一次。
    let existing_secret =
        read_secret_for(&VSCODE_TARGET, Some(&data_dir)).map_err(describe_secret_error)?;
    let existing_session = existing_secret.is_some();
    let session = build_ext_session_json(&acc, existing_secret.as_deref());

    let db_path = inject_secret_for(&VSCODE_TARGET, &session, Some(&data_dir))
        .map_err(describe_inject_error)?;

    set_active_account_id(account_id)?;

    let mut restarted = false;
    let mut relaunch_error: Option<String> = None;
    if let Some(closed) = closed {
        match relaunch_closed_editor(closed) {
            Ok(()) => restarted = true,
            Err(err) => relaunch_error = Some(err),
        }
    }

    let closed_by_us = closed.is_some();
    let name = account::account_display_name(&acc);
    let message = switch_message(&name, restarted, relaunch_error.as_deref());

    Ok(json!({
        "ok": true,
        "account": name,
        "accountId": account_id,
        "dbPath": db_path.to_string_lossy(),
        "restarted": restarted,
        "closedByUs": closed_by_us,
        "existingSession": existing_session,
        "message": message,
    }))
}

/// 切换 VS Code CodeBuddy 扩展账号。
///
/// 时序（design §3）：校验账号 / 目录 / db → 关闭决策（[`close_vscode_for_switch`]：
/// 运行中且 `restart=false` 报错、`restart=true` 先优雅退出，失败即报错且**不写入**）
/// → [`switch_account_after_close`]。切换前未运行时行为与改造前一致（只写入，不主动拉起编辑器）。
pub fn switch_account(account_id: &str, restart: bool) -> Result<Value, String> {
    // 先校验再关闭：账号不存在时不关编辑器（design §3.1）。
    validate_switch_target(account_id)?;
    let closed = close_vscode_for_switch(restart)?;
    switch_account_after_close(account_id, closed)
}

/// 从本机 VS Code 扩展读取当前 token；若能匹配账号库则返回匹配信息（不新建账号）。
pub fn detect_current_account() -> Result<Value, String> {
    let secret = read_secret_for(&VSCODE_TARGET, None).map_err(describe_secret_error)?;
    let Some(secret) = secret else {
        return Ok(json!({
            "ok": true,
            "found": false,
            "message": "本机 VS Code 未找到 CodeBuddy 插件登录 secret",
        }));
    };
    let Some((uid, token)) = parse_token_from_secret(&secret) else {
        return Err("本地 VS Code CodeBuddy 插件登录信息解析失败".to_string());
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
        "message": "本机 VS Code 已登录 CodeBuddy 插件，但账号库中无匹配账号；可先用「从本机导入」或扫码登录同步账号后再切换。",
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

    /// WorkBuddy 产品域注入 CodeBuddy 扩展前必须规范化：否则扩展把域归为 `selfhosted`
    /// （该分支会读「企业版端点」设置）；企业自建域保持原样。
    #[test]
    fn ext_session_json_normalizes_workbuddy_domain() {
        let acc = json!({
            "uid": "u-9",
            "nickname": "张佳",
            "access_token": "tok-9",
            "domain": "www.workbuddy.cn",
        });
        let v: Value = serde_json::from_str(&build_ext_session_json(&acc, None)).unwrap();
        assert_eq!(v["domain"], "www.codebuddy.cn");
        assert_eq!(v["auth"]["domain"], "www.codebuddy.cn");

        let corp = json!({
            "uid": "u-c",
            "access_token": "tok-c",
            "domain": "corp.example.com",
        });
        let vc: Value = serde_json::from_str(&build_ext_session_json(&corp, None)).unwrap();
        assert_eq!(vc["domain"], "corp.example.com");
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
            "refreshExpiresAt": 1_700_000_000_000_i64,
        });
        let existing = r#"{"accounts":[{"uid":"old","lastLogin":true}],"id":"Tencent-Cloud.coding-copilot","craftSettings":{"x":1},"converted":true,"auth":{"accessToken":"tok-old","scope":"all","sessionState":"logged_in","notBeforePolicy":0,"refreshExpiresAt":111,"expiresIn":5184000}}"#;
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
        // auth 的扩展私有键原样保留（F4）
        assert_eq!(v["auth"]["scope"], "all");
        assert_eq!(v["auth"]["sessionState"], "logged_in");
        assert_eq!(v["auth"]["notBeforePolicy"], 0);
        // auth 的身份/凭据键被覆盖
        assert_eq!(v["auth"]["accessToken"], "tok-new");
        assert_eq!(v["auth"]["refreshToken"], "rt-new");
        assert_eq!(v["auth"]["tokenType"], "Bearer");
        assert_eq!(v["auth"]["domain"], "www.codebuddy.cn");
        assert_eq!(v["auth"]["expiresAt"], 42);
        // refreshExpiresAt 取账号库的值（不再写 0）
        assert_eq!(v["auth"]["refreshExpiresAt"], 1_700_000_000_000_i64);
        // expiresIn 刻意沿用「写绝对时间」的既有写法（与 CN IDE 已验证实现一致）
        assert_eq!(v["auth"]["expiresIn"], 42);
        assert_eq!(v["auth"]["refreshExpiresIn"], 0);
    }

    /// F4：账号库没有 `refreshExpiresAt` 时保留既有 auth 里的值（而非写 0）。
    #[test]
    fn ext_session_json_keeps_existing_auth_refresh_expires_at_without_account_value() {
        let acc = json!({
            "uid": "u-7",
            "nickname": "无 refreshExpiresAt",
            "access_token": "tok-7",
            "domain": "www.codebuddy.cn",
            "expiresAt": 7_i64,
        });
        let existing = r#"{"auth":{"refreshExpiresAt":777,"scope":"all"}}"#;
        let s = build_ext_session_json(&acc, Some(existing));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["auth"]["refreshExpiresAt"], 777);
        assert_eq!(v["auth"]["scope"], "all");
        assert_eq!(v["auth"]["accessToken"], "tok-7");
    }

    /// F4：既无账号库值也无既有 auth（未登录）时维持旧载荷形状（键在、值为 0）。
    #[test]
    fn ext_session_json_without_existing_auth_keeps_zero_refresh_expires_at() {
        let acc = json!({
            "uid": "u-8",
            "access_token": "tok-8",
            "domain": "www.codebuddy.cn",
            "expiresAt": 8_i64,
        });
        let s = build_ext_session_json(&acc, None);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["auth"]["refreshExpiresAt"], 0);
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
        assert!(is_code_image_name(
            r"C:\Users\Zhou\AppData\Local\Programs\Microsoft VS Code\Code.exe"
        ));
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
        assert!(linux_exe_is_code(std::path::Path::new(
            "/usr/share/code/code"
        )));
        assert!(!linux_cmdline_is_code("/usr/bin/workbuddy-switch"));
        assert!(!linux_cmdline_is_code(
            "/usr/share/code/code --type=gpu-process"
        ));
        assert!(!linux_cmdline_is_code("/usr/bin/code-insiders"));
        assert!(!linux_cmdline_is_code("/opt/codebuddy-cn/codebuddy-cn"));
        assert!(!linux_exe_is_code(std::path::Path::new(
            "/usr/bin/code-insiders"
        )));
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

    /// macOS 重开依赖关闭前从主进程 args 回溯的 `.app` bundle 路径。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_code_bundle_is_recovered_from_main_args() {
        assert_eq!(
            process::extract_app_bundle_from_args(
                "/Applications/Visual Studio Code.app/Contents/MacOS/Electron -psn_0_123"
            ),
            Some(PathBuf::from("/Applications/Visual Studio Code.app"))
        );
        assert_eq!(
            process::extract_app_bundle_from_args(
                "/Users/zhou/Applications/Visual Studio Code.app/Contents/MacOS/Electron \
                 --type=renderer"
            ),
            Some(PathBuf::from(
                "/Users/zhou/Applications/Visual Studio Code.app"
            ))
        );
        // 非 VS Code 进程不得被当成目标
        assert_eq!(
            process::extract_app_bundle_from_args("/usr/bin/ssh host"),
            None
        );
    }

    /// 主进程模式必须只命中官方 VS Code（不含 Insiders / CodeBuddy 系）。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_code_main_patterns_match_code_not_variants() {
        let patterns = macos_code_main_patterns();
        let hits = |args: &str| patterns.iter().any(|p| args.contains(p.as_str()));
        assert!(hits(
            "/Applications/Visual Studio Code.app/Contents/MacOS/Electron -psn_0_1"
        ));
        assert!(!hits(
            "/Applications/Visual Studio Code - Insiders.app/Contents/MacOS/Electron"
        ));
        assert!(!hits(
            "/Applications/CodeBuddy CN.app/Contents/MacOS/CodeBuddy CN"
        ));
        assert!(!hits(
            "/Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy"
        ));
    }

    /// Windows 超时文案要点名「没关掉的窗口标题」，并附上关闭请求的返回。
    #[test]
    fn close_timeout_error_windows_names_the_window_and_command_notes() {
        let windows = vec![(
            60680u32,
            "check_columns.sql - newmes - Visual Studio Code".to_string(),
        )];
        let notes = vec!["已向 1 个可见窗口发出关闭请求".to_string()];
        let message = close_timeout_error_windows(&[60680], &windows, &notes);
        assert!(
            message.contains("check_columns.sql - newmes - Visual Studio Code"),
            "{message}"
        );
        assert!(message.contains("PID 60680"), "{message}");
        assert!(
            message.contains("已向 1 个可见窗口发出关闭请求"),
            "{message}"
        );
        // 没有可见窗口残留（例如只剩语言服务）时退回 PID 列表，不编造窗口名。
        let message = close_timeout_error_windows(&[1, 2], &[], &[]);
        assert!(message.contains("1, 2"), "{message}");
        assert!(!message.contains("窗口"), "{message}");
    }

    #[test]
    fn close_timeout_error_lists_pids_and_manual_hint() {
        let message = close_timeout_error(&[4242, 4243]);
        assert!(message.contains("4242, 4243"), "{message}");
        assert!(message.contains("请在 VS Code 中处理保存提示"), "{message}");
        // 等待期间被重新打开也会表现为「仍有进程」→ 文案要点出这条排查方向。
        assert!(message.contains("重新打开过 VS Code"), "{message}");
    }

    /// 记录到的路径可能被编码问题写坏（含替换字符）；这种候选必须被跳过，另走 CLI 兜底。
    #[test]
    fn launch_candidates_skip_broken_recorded_path() {
        let broken = PathBuf::from(
            "C:\\Users\\\u{fffd}\u{fffd}\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe",
        );
        assert!(!launch_candidates(Some(&broken)).contains(&broken));

        let recorded = PathBuf::from("/Applications/Visual Studio Code.app");
        assert_eq!(
            launch_candidates(Some(&recorded)).first(),
            Some(&recorded),
            "记录到的正常路径必须排在最前"
        );
    }

    fn instance_with_pids(pids: Vec<u32>) -> RunningInstance {
        RunningInstance {
            pids,
            main_exe: None,
            macos_bundle: None,
        }
    }

    /// 关闭决策纯逻辑（注入实例，不碰真实进程）：
    /// 未运行 → `Ok(None)`（不主动拉起）；运行中 + 手动模式 → 报错且不关闭（AC10）。
    #[test]
    fn close_for_switch_decides_without_touching_processes() {
        assert!(
            close_vscode_for_switch_with(instance_with_pids(Vec::new()), false)
                .unwrap()
                .is_none()
        );
        assert!(
            close_vscode_for_switch_with(instance_with_pids(Vec::new()), true)
                .unwrap()
                .is_none()
        );

        let error =
            close_vscode_for_switch_with(instance_with_pids(vec![4242]), false).unwrap_err();
        assert_eq!(error, RUNNING_MANUAL_HINT);
    }

    /// 未运行时 [`close_vscode_for_switch`] 必须是空操作。本机可能真的开着 VS Code，
    /// 因此只在确认未运行时断言，其它情况跳过（**绝不在测试里关闭编辑器**）。
    #[test]
    fn close_for_switch_is_noop_when_vscode_not_running() {
        if is_vscode_running() {
            eprintln!("skip: 本机 VS Code 正在运行，无法安全断言 Ok(None)");
            return;
        }
        assert!(close_vscode_for_switch(false).unwrap().is_none());
    }

    /// 新入口 [`switch_account_after_close`] 的校验分支：账号不存在时返回可读错误，
    /// 且此时不会去读/写 state.vscdb（`None` 表示编辑器本来没运行）。
    #[test]
    fn switch_account_after_close_rejects_unknown_account() {
        let missing = format!("no-such-account-{}", uuid::Uuid::new_v4().simple());
        let error = switch_account_after_close(&missing, None).unwrap_err();
        assert!(error.contains("账号不存在"), "{error}");
    }

    /// 失败兜底（D4）：编辑器是本次我们关的 → 任何 Err 路径都要尽力开回来；本来没关的 → 一次都不许调。
    ///
    /// 重开动作由参数注入（真实实现会真的启动用户的 VS Code，测试里**绝不能**走到那一步）。
    #[test]
    fn error_paths_relaunch_only_editor_we_closed() {
        let closed = ClosedEditor(instance_with_pids(vec![4242]));
        let mut relaunch_calls = 0usize;

        // ① 编辑器是本次关的（如关闭后才发现的 access_token 为空 / 读 secret 失败）→ 必须开回来
        let error = relaunch_closed_on_error(
            Some(&closed),
            |_| {
                relaunch_calls += 1;
                Ok(())
            },
            "账号 access_token 为空".to_string(),
        );
        assert_eq!(relaunch_calls, 1, "关掉的编辑器必须被尽力开回来");
        assert_eq!(error, "账号 access_token 为空");

        // ② 编辑器本来没运行 → 不得尝试启动（否则会凭空拉起用户的 VS Code）
        let error = relaunch_closed_on_error(
            None,
            |_| {
                relaunch_calls += 1;
                Ok(())
            },
            "账号不存在".to_string(),
        );
        assert_eq!(relaunch_calls, 1, "未关闭编辑器时不得调用重开");
        assert_eq!(error, "账号不存在");

        // ③ 重开也失败 → 两条信息都要保留，用户才知道编辑器还关着
        let error = relaunch_closed_on_error(
            Some(&closed),
            |_| Err("重新打开 VS Code 失败: open 超时".to_string()),
            "注入登录状态失败".to_string(),
        );
        assert!(error.contains("注入登录状态失败"), "{error}");
        assert!(error.contains("open 超时"), "{error}");
    }

    /// 成功文案三态（design §5）：不再出现「重载窗口」这类与真实行为不符的提示。
    #[test]
    fn switch_message_matches_design_states() {
        let restarted = switch_message("测试号", true, None);
        assert!(restarted.contains("VS Code 已重新打开"), "{restarted}");
        assert!(!restarted.contains("重载"), "{restarted}");

        let relaunch_failed = switch_message("测试号", false, Some("open 超时"));
        assert!(
            relaunch_failed.contains("自动重新打开 VS Code 失败"),
            "{relaunch_failed}"
        );

        let not_running = switch_message("测试号", false, None);
        assert!(not_running.contains("请打开 VS Code 生效"), "{not_running}");
        assert!(!not_running.contains("重载"), "{not_running}");
    }
}
