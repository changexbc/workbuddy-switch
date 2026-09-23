//! CodeBuddy 国际版 IDE（桌面客户端）账号切换。
//!
//! 复用 WorkBuddy AI 账号库中的 token，写入
//! `~/Library/Application Support/CodeBuddy/.../state.vscdb` 的 Safe Storage
//! secret，并可选重启 CodeBuddy。与 CodeBuddy CN、CodeBuddy CLI 完全独立。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::modules::account::{self, get_str};
#[cfg(target_os = "macos")]
use crate::modules::config::home_dir;
use crate::modules::config::{
    atomic_write, clear_codebuddy_ide_app_cache, load_codebuddy_ide_app_cache, now_ms,
    save_codebuddy_ide_app_cache, store_dir,
};
use crate::modules::variant::{codebuddy_domain_for, WbVariant};
// 复用 process 模块带并发管道读取的正确实现；本地轮询版会在子进程输出
// 超过 64KB（如 `ps -axo pid=,args=`）时因管道写满而死锁到超时。
use crate::modules::process;
use crate::modules::process::run_cmd_timeout as run_cmd;
use crate::modules::vscode_cn_inject::{
    codebuddy_ide_data_dir, inject_codebuddy_ide_secret, read_codebuddy_ide_secret,
    CodeBuddyIdeFlavor,
};

fn intl_data_dir() -> Option<PathBuf> {
    codebuddy_ide_data_dir(CodeBuddyIdeFlavor::Intl)
}

fn intl_state_db_path() -> Option<PathBuf> {
    intl_data_dir().map(|d| d.join("User").join("globalStorage").join("state.vscdb"))
}

fn inject_intl_secret(plaintext: &str, user_data_dir: Option<&Path>) -> Result<PathBuf, String> {
    inject_codebuddy_ide_secret(CodeBuddyIdeFlavor::Intl, plaintext, user_data_dir)
}

fn read_intl_secret(user_data_dir: Option<&Path>) -> Result<Option<String>, String> {
    read_codebuddy_ide_secret(CodeBuddyIdeFlavor::Intl, user_data_dir)
}

const STATE_FILE: &str = "codebuddy_ide.json";
#[cfg(target_os = "macos")]
const MACOS_BUNDLE_ID: &str = "com.tencent.codebuddy";
#[cfg(target_os = "macos")]
const MACOS_APP_NAME: &str = "CodeBuddy.app";

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

/// 构造注入到 CN IDE 的会话 JSON（与 CN 客户端登录态写入结构一致）。
pub fn build_session_json(acc: &Value) -> String {
    let uid = get_str(acc, "uid").unwrap_or_default();
    let nickname = get_str(acc, "nickname").unwrap_or_default();
    let enterprise_id = get_str(acc, "enterpriseId")
        .or_else(|| get_str(acc, "enterprise_id"))
        .unwrap_or_default();
    let enterprise_name = get_str(acc, "enterpriseName")
        .or_else(|| get_str(acc, "enterprise_name"))
        .unwrap_or_default();
    let domain = codebuddy_domain_for(
        get_str(acc, "domain").unwrap_or_default().as_str(),
        account::variant_of(acc),
    );
    let refresh_token = get_str(acc, "refresh_token").unwrap_or_default();
    let access_token = get_str(acc, "access_token").unwrap_or_default();
    let token_type = get_str(acc, "token_type").unwrap_or_else(|| "Bearer".to_string());
    let expires_at = acc.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);

    json!({
        "id": "Tencent-Cloud.genie-ide",
        "token": access_token,
        "refreshToken": refresh_token,
        "expiresAt": expires_at,
        "domain": domain,
        "accessToken": format!("{uid}+{access_token}"),
        "converted": true,
        "account": {
            "id": uid,
            "uid": uid,
            "label": nickname,
            "nickname": nickname,
            "enterpriseId": enterprise_id,
            "enterpriseName": enterprise_name,
            "pluginEnabled": true,
            "lastLogin": true,
        },
        "auth": {
            "accessToken": access_token,
            "refreshToken": refresh_token,
            "tokenType": token_type,
            "domain": domain,
            "expiresAt": expires_at,
            "expiresIn": expires_at,
            "refreshExpiresIn": 0,
            "refreshExpiresAt": 0,
            "lastRefreshTime": now_ms(),
        }
    })
    .to_string()
}

fn parse_token_from_secret(secret: &str) -> Option<(Option<String>, String)> {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let token = value
            .get("token")
            .or_else(|| value.get("access_token"))
            .or_else(|| value.get("accessToken"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                value
                    .get("auth")
                    .and_then(|a| a.get("accessToken").or_else(|| a.get("access_token")))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })?;
        let uid = value
            .get("uid")
            .or_else(|| value.pointer("/account/uid"))
            .or_else(|| value.pointer("/account/id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        // accessToken 可能是 `uid+token`
        if let Some((prefix, suffix)) = token.split_once('+') {
            let suffix = suffix.trim();
            if !suffix.is_empty() {
                let uid = uid.or_else(|| {
                    let p = prefix.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.to_string())
                    }
                });
                return Some((uid, suffix.to_string()));
            }
        }
        return Some((uid, token));
    }
    if let Some((prefix, suffix)) = trimmed.split_once('+') {
        let suffix = suffix.trim();
        if !suffix.is_empty() {
            let uid = {
                let p = prefix.trim();
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                }
            };
            return Some((uid, suffix.to_string()));
        }
    }
    Some((None, trimmed.to_string()))
}

fn match_account_for_token(uid: Option<&str>, token: &str) -> Option<Value> {
    let accounts = account::load_accounts();
    if let Some(uid) = uid.filter(|s| !s.is_empty()) {
        if let Some(acc) = accounts
            .iter()
            .find(|a| get_str(a, "uid").as_deref() == Some(uid))
        {
            return Some(acc.clone());
        }
    }
    accounts
        .into_iter()
        .find(|a| get_str(a, "access_token").as_deref() == Some(token))
}

fn windows_image_stem(name: &str) -> &str {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name).trim();
    if file.len() >= 4 && file[file.len() - 4..].eq_ignore_ascii_case(".exe") {
        file[..file.len() - 4].trim()
    } else {
        file
    }
}

/// 精确映像名：`CodeBuddy`（忽略 .exe / 路径 / 大小写）。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn is_codebuddy_ide_image_name(name: &str) -> bool {
    windows_image_stem(name).eq_ignore_ascii_case("CodeBuddy")
}

fn is_plain_codebuddy_image_name(name: &str) -> bool {
    windows_image_stem(name).eq_ignore_ascii_case("CodeBuddy")
}

/// 路径是否含独立目录分量 `CodeBuddy CN`（国内 IDE，不是国际版 `CodeBuddy`）。
fn path_contains_codebuddy_cn_dir(path: &str) -> bool {
    path.split(['\\', '/'])
        .any(|part| part.eq_ignore_ascii_case("CodeBuddy CN"))
}

/// Windows 国际 IDE：`CodeBuddy.exe`，且路径不含 `CodeBuddy CN`。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn is_codebuddy_ide_windows_exe(path: &str) -> bool {
    if path_contains_codebuddy_cn_dir(path) {
        return false;
    }
    is_codebuddy_ide_image_name(path) || is_plain_codebuddy_image_name(path)
}

fn persist_ide_app_cache(path: &Path) {
    if load_codebuddy_ide_app_cache().as_deref() == Some(path) {
        return;
    }
    let _ = save_codebuddy_ide_app_cache(path);
}

#[cfg(target_os = "macos")]
fn macos_ide_app_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Applications").join(MACOS_APP_NAME),
        home.join("Applications").join(MACOS_APP_NAME),
    ]
}

#[cfg(target_os = "macos")]
fn macos_ide_main_patterns(resolved_app: Option<&Path>) -> Vec<String> {
    match resolved_app {
        Some(app) => vec![format!("{}/Contents/MacOS", app.display())],
        None => vec!["CodeBuddy.app/Contents/MacOS".to_string()],
    }
}

#[cfg(target_os = "macos")]
fn macos_ide_bundle_patterns(resolved_app: Option<&Path>) -> Vec<String> {
    match resolved_app {
        Some(app) => vec![app.display().to_string()],
        None => vec!["CodeBuddy.app".to_string()],
    }
}

#[cfg(target_os = "macos")]
fn macos_ide_running_app_path() -> Option<PathBuf> {
    let patterns = macos_ide_main_patterns(None);
    for (_pid, args) in process::macos_rows_by_patterns(&patterns) {
        if let Some(p) = process::extract_app_bundle_from_args(&args) {
            if process::is_app_bundle(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn macos_ide_mdfind_app_path() -> Option<PathBuf> {
    let query = format!("kMDItemCFBundleIdentifier == '{MACOS_BUNDLE_ID}'c");
    let out = run_cmd("mdfind", &[query.as_str()], 5)?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn macos_ide_app_path_resolved() -> Option<PathBuf> {
    if let Some(p) = macos_ide_running_app_path() {
        persist_ide_app_cache(&p);
        return Some(p);
    }
    if let Some(cached) = load_codebuddy_ide_app_cache() {
        if process::is_app_bundle(&cached) {
            return Some(cached);
        }
        clear_codebuddy_ide_app_cache();
    }
    for p in macos_ide_app_candidates(&home_dir()) {
        if process::is_app_bundle(&p) {
            persist_ide_app_cache(&p);
            return Some(p);
        }
    }
    if let Some(p) = macos_ide_mdfind_app_path() {
        if process::is_app_bundle(&p) {
            persist_ide_app_cache(&p);
            return Some(p);
        }
    }
    None
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn windows_ide_fallback_exe_candidates(
    local_appdata: Option<&str>,
    program_files: Option<&str>,
    program_files_x86: Option<&str>,
    username: Option<&str>,
    drives: &[char],
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.iter().any(|e| e == &p) {
            out.push(p);
        }
    };
    let folders = ["CodeBuddy"];
    let exe_names = ["CodeBuddy.exe", "CodeBuddy.exe"];
    let mut push_install = |base: PathBuf| {
        for folder in folders {
            for exe in exe_names {
                push(base.join(folder).join(exe));
            }
        }
    };
    if let Some(local) = local_appdata.map(str::trim).filter(|s| !s.is_empty()) {
        push_install(PathBuf::from(local).join("Programs"));
    }
    if let Some(pf) = program_files.map(str::trim).filter(|s| !s.is_empty()) {
        push_install(PathBuf::from(pf));
    }
    if let Some(pf86) = program_files_x86.map(str::trim).filter(|s| !s.is_empty()) {
        push_install(PathBuf::from(pf86));
    }
    let user = username.map(str::trim).filter(|s| !s.is_empty());
    for drive in drives {
        let letter = drive.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() {
            continue;
        }
        let root = format!("{letter}:");
        if let Some(user) = user {
            push_install(
                PathBuf::from(&root)
                    .join("Users")
                    .join(user)
                    .join("AppData")
                    .join("Local")
                    .join("Programs"),
            );
        }
        push_install(PathBuf::from(&root).join("Program Files"));
    }
    out
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn keep_windows_ide_row(row: &process::WindowsProcessRow) -> bool {
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
    if path_contains_codebuddy_cn_dir(&path_s) {
        return false;
    }
    is_codebuddy_ide_windows_exe(&row.name)
        || is_codebuddy_ide_windows_exe(file_name)
        || (!path_s.is_empty() && is_codebuddy_ide_windows_exe(&path_s))
}

#[cfg(target_os = "windows")]
fn is_existing_ide_exe(path: &Path) -> bool {
    path.is_file() && is_codebuddy_ide_windows_exe(&path.to_string_lossy())
}

#[cfg(target_os = "windows")]
fn windows_ide_cim_process_script() -> &'static str {
    "Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
         Where-Object { $_.Name -eq 'CodeBuddy.exe' -or $_.Name -eq 'CodeBuddy.exe' } | \
         ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.ExecutablePath }"
}

#[cfg(target_os = "windows")]
fn windows_ide_process_rows() -> Vec<process::WindowsProcessRow> {
    let self_pid = std::process::id();
    if let Some(stdout) = process::ps_output(windows_ide_cim_process_script(), 5) {
        let rows: Vec<_> = process::parse_windows_process_rows(&stdout)
            .into_iter()
            .filter(|row| row.pid != self_pid && keep_windows_ide_row(row))
            .collect();
        if !rows.is_empty() {
            return rows;
        }
    }
    let mut rows = Vec::new();
    rows.extend(process::windows_tasklist_image_rows("CodeBuddy.exe"));
    rows.extend(process::windows_tasklist_image_rows("CodeBuddy.exe"));
    rows.into_iter()
        .filter(|row| row.pid != self_pid && keep_windows_ide_row(row))
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_ide_running_exe() -> Option<PathBuf> {
    let stdout = process::ps_output(windows_ide_cim_process_script(), 5)?;
    for row in process::parse_windows_process_rows(&stdout) {
        if !keep_windows_ide_row(&row) {
            continue;
        }
        if let Some(p) = row.exe_path {
            if is_existing_ide_exe(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn windows_ide_registry_exe_candidates() -> Vec<PathBuf> {
    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$out = @()
$names = @('CodeBuddy.exe', 'CodeBuddy.exe')
$appHives = @(
  'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths',
  'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths',
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths'
)
foreach ($hive in $appHives) {
  foreach ($n in $names) {
    $key = Join-Path $hive $n
    $props = Get-ItemProperty -LiteralPath $key
    if ($props) {
      $def = $props.'(default)'
      if ($def) { $out += [string]$def }
      if ($props.Path) { $out += [string](Join-Path $props.Path $n) }
    }
  }
}
$unHives = @(
  'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
  'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
)
foreach ($hive in $unHives) {
  Get-ChildItem -LiteralPath $hive | ForEach-Object {
    $dn = $_.GetValue('DisplayName')
    if (-not $dn) { return }
    $dnl = [string]$dn
    if ($dnl -match 'workbuddy-switch|wb-switch') { return }
    if ($dnl -match 'CodeBuddy CN') { return }
    if ($dnl -notmatch 'CodeBuddy') { return }
    $icon = $_.GetValue('DisplayIcon')
    if ($icon) { $out += [string]$icon }
    $loc = $_.GetValue('InstallLocation')
    if ($loc) {
      $out += [string](Join-Path $loc 'CodeBuddy.exe')
      $out += [string](Join-Path $loc 'CodeBuddy.exe')
    }
  }
}
$out | ForEach-Object { $_ }
"#;
    let Some(stdout) = process::ps_output(script, 8) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(parsed) = process::parse_windows_display_icon(line) else {
            continue;
        };
        if process::is_self_image_name(&parsed) {
            continue;
        }
        if !is_codebuddy_ide_windows_exe(&parsed) {
            continue;
        }
        let pb = PathBuf::from(parsed);
        if !out.iter().any(|e| e == &pb) {
            out.push(pb);
        }
    }
    out
}

#[cfg(target_os = "windows")]
fn windows_ide_exe_path_resolved() -> Option<PathBuf> {
    if let Some(p) = windows_ide_running_exe() {
        persist_ide_app_cache(&p);
        return Some(p);
    }
    if let Some(cached) = load_codebuddy_ide_app_cache() {
        if is_existing_ide_exe(&cached) {
            return Some(cached);
        }
        clear_codebuddy_ide_app_cache();
    }
    for p in windows_ide_registry_exe_candidates() {
        if is_existing_ide_exe(&p) {
            persist_ide_app_cache(&p);
            return Some(p);
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok();
    let pf = std::env::var("PROGRAMFILES").ok();
    let pf86 = std::env::var("PROGRAMFILES(X86)").ok();
    let user = std::env::var("USERNAME").ok();
    let drives = process::existing_windows_drives();
    for p in windows_ide_fallback_exe_candidates(
        local.as_deref(),
        pf.as_deref(),
        pf86.as_deref(),
        user.as_deref(),
        &drives,
    ) {
        if is_existing_ide_exe(&p) {
            persist_ide_app_cache(&p);
            return Some(p);
        }
    }
    None
}

#[cfg_attr(
    not(any(test, not(any(target_os = "macos", target_os = "windows")))),
    allow(dead_code)
)]
fn linux_cmdline_is_codebuddy_ide(cmdline: &str) -> bool {
    let lower = cmdline.to_ascii_lowercase();
    if lower.contains("wb-switch") || lower.contains("workbuddy-switch") {
        return false;
    }
    if lower.contains("crashpad") || lower.contains("--type=") {
        return false;
    }
    if cmdline.contains("CodeBuddy CN")
        || lower.contains("codebuddy-cn")
        || lower.contains("codebuddycn")
        || lower.contains("workbuddy")
    {
        return false;
    }
    cmdline.contains("CodeBuddy") || lower.contains("codebuddy")
}

#[cfg_attr(
    not(any(test, not(any(target_os = "macos", target_os = "windows")))),
    allow(dead_code)
)]
fn linux_exe_is_codebuddy_ide(exe: &Path) -> bool {
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim();
    let lower = name.to_ascii_lowercase();
    if lower.contains("codebuddy-cn")
        || lower.contains("codebuddycn")
        || lower.contains("workbuddy")
    {
        return false;
    }
    name.eq_ignore_ascii_case("codebuddy") || is_codebuddy_ide_image_name(name)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_codebuddy_ide_pids() -> Vec<u32> {
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
            if linux_exe_is_codebuddy_ide(&exe) {
                pids.push(pid);
                continue;
            }
        }
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).replace('\0', " "),
            _ => continue,
        };
        if linux_cmdline_is_codebuddy_ide(&cmdline) {
            pids.push(pid);
        }
    }
    pids
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn wait_linux_pids_gone(pids: &[u32], timeout: Duration) -> Vec<u32> {
    if pids.is_empty() {
        return Vec::new();
    }
    let deadline = Instant::now() + timeout;
    loop {
        let alive: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
            .collect();
        if alive.is_empty() || Instant::now() >= deadline {
            return alive;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn kill_linux_pids(pids: &[u32], signal: &str) {
    if pids.is_empty() {
        return;
    }
    let owned: Vec<String> = std::iter::once(signal.to_string())
        .chain(pids.iter().map(|pid| pid.to_string()))
        .collect();
    let args: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    let _ = run_cmd("kill", &args, 10);
}

/// 解析 CodeBuddy 应用路径（macOS: .app bundle；Windows: exe）。
pub fn codebuddy_ide_app_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos_ide_app_path_resolved()
    }
    #[cfg(target_os = "windows")]
    {
        windows_ide_exe_path_resolved()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(cached) = load_codebuddy_ide_app_cache() {
            if cached.is_file() && linux_exe_is_codebuddy_ide(&cached) {
                return Some(cached);
            }
            clear_codebuddy_ide_app_cache();
        }
        let candidates = [
            "/usr/bin/codebuddy",
            "/usr/local/bin/codebuddy",
            "/opt/codebuddy/codebuddy",
        ];
        for p in candidates {
            let path = PathBuf::from(p);
            if path.is_file() {
                persist_ide_app_cache(&path);
                return Some(path);
            }
        }
        None
    }
}

/// CodeBuddy 是否在运行（footer 语义 = GUI 主进程）。
pub fn is_codebuddy_ide_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        let resolved = macos_ide_app_path_resolved();
        let patterns = macos_ide_main_patterns(resolved.as_deref());
        !process::macos_pids_by_patterns(&patterns).is_empty()
    }
    #[cfg(target_os = "windows")]
    {
        !windows_ide_process_rows().is_empty()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        !linux_codebuddy_ide_pids().is_empty()
    }
}

#[cfg(target_os = "macos")]
fn close_codebuddy_ide_macos(timeout_secs: i64) -> Result<(), String> {
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
    let resolved = macos_ide_app_path_resolved();
    let main_patterns = macos_ide_main_patterns(resolved.as_deref());
    let bundle_patterns = macos_ide_bundle_patterns(resolved.as_deref());
    let remaining = || {
        timeout
            .saturating_sub(started.elapsed())
            .max(Duration::from_millis(100))
    };

    let quit_script = format!("quit app id \"{MACOS_BUNDLE_ID}\"");
    let quit = run_cmd("osascript", &["-e", quit_script.as_str()], 10);
    match quit {
        Some(out) if !out.status.success() => {
            eprintln!(
                "[codebuddy-ide] osascript quit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        None => eprintln!("[codebuddy-ide] osascript quit timed out"),
        _ => {}
    }

    let graceful = Duration::from_secs(8).min(remaining());
    let _ = process::wait_macos_main_gone(&main_patterns, graceful);

    let bundle_pids = process::macos_pids_by_patterns(&bundle_patterns);
    if !bundle_pids.is_empty() {
        eprintln!(
            "[codebuddy-ide] killing {} bundle process(es)…",
            bundle_pids.len()
        );
        process::kill_macos_pids(&bundle_pids);
    }

    let leftover = process::wait_macos_patterns_empty(&bundle_patterns, remaining());
    if leftover.is_empty() {
        return Ok(());
    }
    let pids: Vec<String> = leftover.iter().map(|pid| pid.to_string()).collect();
    Err(format!(
        "CodeBuddy 进程无法完全关闭（残留进程: {}）。请手动执行: kill -9 {}",
        pids.join(", "),
        pids.join(" ")
    ))
}

#[cfg(target_os = "windows")]
fn close_codebuddy_ide_windows(timeout_secs: i64) -> Result<(), String> {
    let rows = windows_ide_process_rows();
    if rows.is_empty() {
        return Ok(());
    }
    let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
    for pid in &pids {
        let pid_s = pid.to_string();
        let _ = run_cmd("taskkill", &["/PID", &pid_s, "/T"], 10);
    }

    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
    let graceful_budget = Duration::from_secs(8).min(timeout);
    let remaining = process::wait_windows_pids_gone(&pids, graceful_budget);
    if remaining.is_empty() {
        return Ok(());
    }

    for pid in &remaining {
        let pid_s = pid.to_string();
        let _ = run_cmd("taskkill", &["/PID", &pid_s, "/T", "/F"], 10);
    }
    let rest = timeout
        .saturating_sub(started.elapsed())
        .max(Duration::from_secs(1));
    let leftover = process::wait_windows_pids_gone(&remaining, rest);
    if leftover.is_empty() {
        return Ok(());
    }
    Err("CodeBuddy 进程无法关闭，请手动结束 CodeBuddy 进程".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn close_codebuddy_ide_linux(timeout_secs: i64) -> Result<(), String> {
    let pids = linux_codebuddy_ide_pids();
    if pids.is_empty() {
        return Ok(());
    }
    kill_linux_pids(&pids, "-15");
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
    let graceful = Duration::from_secs(8).min(timeout);
    let remaining = wait_linux_pids_gone(&pids, graceful);
    if remaining.is_empty() {
        return Ok(());
    }
    kill_linux_pids(&remaining, "-9");
    let rest = timeout.saturating_sub(graceful).max(Duration::from_secs(1));
    let leftover = wait_linux_pids_gone(&remaining, rest);
    if leftover.is_empty() {
        return Ok(());
    }
    Err(format!(
        "CodeBuddy 进程无法关闭（残留进程: {}）。请手动执行: kill -9 {}",
        leftover
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        leftover
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

pub fn close_codebuddy_ide(timeout_secs: i64) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        close_codebuddy_ide_macos(timeout_secs)
    }
    #[cfg(target_os = "windows")]
    {
        close_codebuddy_ide_windows(timeout_secs)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        close_codebuddy_ide_linux(timeout_secs)
    }
}

#[cfg(target_os = "macos")]
fn validate_macos_ide_startup(app: &Path) -> Result<(), String> {
    let main_patterns = macos_ide_main_patterns(Some(app));
    let deadline = Instant::now() + Duration::from_secs(30);
    let sustain = Duration::from_secs(10);
    let mut seen_at: Option<Instant> = None;
    while Instant::now() < deadline {
        let now = Instant::now();
        let alive = !process::macos_pids_by_patterns(&main_patterns).is_empty();
        if alive {
            match seen_at {
                None => seen_at = Some(now),
                Some(start) => {
                    if now.duration_since(start) >= sustain {
                        return Ok(());
                    }
                }
            }
        } else if seen_at.is_some() {
            return Err(format!(
                "CodeBuddy 启动后立即退出（疑似残留单例锁）。请先手动打开一次 CodeBuddy（路径: {}）",
                app.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "启动 CodeBuddy 超时，未能确认运行（路径: {}）。请先手动打开一次 CodeBuddy。",
        app.display()
    ))
}

#[cfg(target_os = "macos")]
fn launch_codebuddy_ide_macos() -> Result<(), String> {
    let app = macos_ide_app_path_resolved().ok_or_else(|| {
        "未找到 CodeBuddy 应用（尝试路径: /Applications/CodeBuddy.app）。请先手动打开一次 CodeBuddy 后重试。".to_string()
    })?;
    if !process::is_app_bundle(&app) {
        return Err(format!(
            "未找到 CodeBuddy 应用（尝试路径: {}）。请先手动打开一次 CodeBuddy 后重试。",
            app.display()
        ));
    }

    let bundle_patterns = macos_ide_bundle_patterns(Some(&app));
    let bundle_pids = process::macos_pids_by_patterns(&bundle_patterns);
    if !bundle_pids.is_empty() {
        process::kill_macos_pids(&bundle_pids);
        let _ = process::wait_macos_patterns_empty(&bundle_patterns, Duration::from_secs(5));
    }

    let app_lossy = app.to_string_lossy();
    let open = run_cmd(
        "open",
        &["-n", "-a", app_lossy.as_ref(), "--args", "--new-window"],
        10,
    );
    match open {
        Some(out) if out.status.success() => {}
        Some(out) => {
            let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let reason = if reason.is_empty() {
                format!("open 退出码 {}", out.status.code().unwrap_or(-1))
            } else {
                reason
            };
            return Err(format!(
                "启动 CodeBuddy 失败: {reason}（路径: {}）",
                app.display()
            ));
        }
        None => {
            return Err(format!(
                "启动 CodeBuddy 失败: open 超时（路径: {}）",
                app.display()
            ));
        }
    }

    validate_macos_ide_startup(&app)
}

pub fn launch_codebuddy_ide() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        launch_codebuddy_ide_macos()
    }
    #[cfg(target_os = "windows")]
    {
        let exe = windows_ide_exe_path_resolved().ok_or_else(|| {
            "未找到 CodeBuddy 程序（尝试路径: %LOCALAPPDATA%\\Programs\\CodeBuddy\\CodeBuddy.exe）。请在 Windows 上打开 CodeBuddy 后重试。".to_string()
        })?;
        if !is_existing_ide_exe(&exe) {
            return Err(format!(
                "未找到 CodeBuddy 程序（尝试路径: {}）。请在 Windows 上打开 CodeBuddy 后重试。",
                exe.display()
            ));
        }
        persist_ide_app_cache(&exe);
        process::cmd_builder(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 CodeBuddy 失败: {e}（路径: {}）", exe.display()))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let exe = codebuddy_ide_app_path().ok_or_else(|| {
            "未找到 CodeBuddy 可执行文件（尝试路径: /usr/bin/codebuddy）。请先手动打开一次。"
                .to_string()
        })?;
        process::cmd_builder(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 CodeBuddy 失败: {e}（路径: {}）", exe.display()))?;
        Ok(())
    }
}

/// 状态：是否安装、是否运行、当前账号（仅来自本地状态文件 + 账号库，不读取钥匙串）。
pub fn status() -> Value {
    let data_dir = intl_data_dir();
    let db_path = intl_state_db_path();
    // `installed` 只表示"存在可运行的客户端"。数据目录存在不能算已安装——
    // 只读探测不再建目录（issue #91），残留空目录也不应误报「已接入」；
    // 是否有残留数据由 dataDir / dbExists 表达。
    let installed = codebuddy_ide_app_path().is_some();
    let db_exists = db_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    let running = is_codebuddy_ide_running();

    let mut active_account_id = active_account_id_from_state();
    let mut active_account_name: Option<String> = None;

    if let Some(id) = active_account_id.clone() {
        if let Some(acc) = account::find_account(&id) {
            active_account_name = Some(account::account_display_name(&acc));
        } else {
            // 状态文件有记录但账号库已无此账号：视为未检测到，不回退读取钥匙串
            active_account_id = None;
        }
    }

    json!({
        "installed": installed,
        "running": running,
        "dataDir": data_dir.map(|p| p.to_string_lossy().to_string()),
        "dbPath": db_path.map(|p| p.to_string_lossy().to_string()),
        "dbExists": db_exists,
        "appPath": codebuddy_ide_app_path().map(|p| p.to_string_lossy().to_string()),
        "activeAccountId": active_account_id,
        "activeAccountName": active_account_name,
        "detectedFrom": "state",
        "statePath": state_path().to_string_lossy(),
    })
}

/// 切换 CodeBuddy IDE 账号：关进程 → 注入 secret → 启动。
pub fn switch_account(account_id: &str, restart: bool) -> Result<Value, String> {
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    if WbVariant::from_account(&acc) != WbVariant::Ai {
        return Err("国际版 CodeBuddy IDE 只能切换国际版（WorkBuddy AI）账号".to_string());
    }
    let token = get_str(&acc, "access_token")
        .ok_or_else(|| "账号缺少 access_token，无法注入 CodeBuddy".to_string())?;
    if token.is_empty() {
        return Err("账号 access_token 为空".to_string());
    }

    let data_dir = intl_data_dir().ok_or_else(|| "无法定位 CodeBuddy 数据目录".to_string())?;
    if !data_dir.exists() {
        return Err(format!(
            "未找到 CodeBuddy 用户数据目录（{}）。请先手动打开 CodeBuddy 并登录一次。",
            data_dir.display()
        ));
    }

    if restart {
        eprintln!("[codebuddy-ide] closing CodeBuddy…");
        close_codebuddy_ide(20)?;
    }

    let session = build_session_json(&acc);
    eprintln!("[codebuddy-ide] injecting secret…");
    let db_path = inject_intl_secret(&session, Some(&data_dir)).map_err(|err| {
        if err.contains("Safe Storage") || err.contains("Keychain") {
            format!(
                "注入登录状态失败：{err}\n\n请先手动打开 CodeBuddy 并登录一次，确保 Keychain 中存在「CodeBuddy Safe Storage」条目后再试。"
            )
        } else {
            err
        }
    })?;

    set_active_account_id(account_id)?;

    if restart {
        eprintln!("[codebuddy-ide] launching CodeBuddy…");
        launch_codebuddy_ide()?;
    }

    Ok(json!({
        "ok": true,
        "account": account::account_display_name(&acc),
        "accountId": account_id,
        "dbPath": db_path.to_string_lossy(),
        "restarted": restart,
        "message": if restart {
            format!("已切换 CodeBuddy IDE 到 {} 并重启", account::account_display_name(&acc))
        } else {
            format!("已写入 CodeBuddy IDE 凭证（{}）；请手动重启 CodeBuddy 生效", account::account_display_name(&acc))
        },
    }))
}

/// 从本机 CN IDE 读取当前 token；若能匹配账号库则返回匹配信息（不新建账号）。
pub fn detect_current_account() -> Result<Value, String> {
    let secret = read_intl_secret(None)?;
    let Some(secret) = secret else {
        return Ok(json!({
            "ok": true,
            "found": false,
            "message": "本机 CodeBuddy 未找到登录 secret",
        }));
    };
    let Some((uid, token)) = parse_token_from_secret(&secret) else {
        return Err("本地 CodeBuddy 登录信息解析失败".to_string());
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
        "message": "本机已登录 CodeBuddy，但账号库中无匹配账号；可先用「从本机导入」或扫码登录同步账号后再切换。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_json_includes_uid_plus_token() {
        let acc = json!({
            "uid": "u-42",
            "nickname": "测试",
            "access_token": "tok-abc",
            "refresh_token": "rt-1",
            "domain": "www.codebuddy.cn",
            "expiresAt": 1234567890_i64,
        });
        let s = build_session_json(&acc);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["accessToken"], "u-42+tok-abc");
        assert_eq!(v["token"], "tok-abc");
        assert_eq!(v["auth"]["accessToken"], "tok-abc");
        assert_eq!(v["account"]["uid"], "u-42");
        assert_eq!(v["id"], "Tencent-Cloud.genie-ide");
    }

    #[test]
    fn parse_token_from_uid_plus_form() {
        let (uid, token) = parse_token_from_secret("uid-1+ACCESS").unwrap();
        assert_eq!(uid.as_deref(), Some("uid-1"));
        assert_eq!(token, "ACCESS");
    }

    #[test]
    fn parse_token_from_session_json() {
        let secret = r#"{"token":"T1","accessToken":"u9+T1","account":{"uid":"u9"}}"#;
        let (uid, token) = parse_token_from_secret(secret).unwrap();
        assert_eq!(uid.as_deref(), Some("u9"));
        assert_eq!(token, "T1");
    }

    #[test]
    fn secret_key_helper_reexported_path() {
        let key = crate::modules::vscode_cn_inject::secret_storage_item_key_for(
            &crate::modules::vscode_cn_inject::CODEBUDDY_INTL_TARGET,
        );
        assert!(key.contains("planning-genie.new.accessToken"));
        assert!(!key.contains("accessTokencn"));
        assert!(key.starts_with("secret://"));
    }

    #[test]
    fn ide_image_name_is_codebuddy_not_cn() {
        assert!(is_codebuddy_ide_image_name("CodeBuddy.exe"));
        assert!(is_codebuddy_ide_image_name("CodeBuddy"));
        assert!(!is_codebuddy_ide_image_name("CodeBuddy CN.exe"));
        assert!(!is_codebuddy_ide_image_name("WorkBuddy.exe"));
        assert!(!is_codebuddy_ide_image_name("workbuddy-switch.exe"));
        assert!(is_codebuddy_ide_windows_exe(
            r"C:\Users\Zhou\AppData\Local\Programs\CodeBuddy\CodeBuddy.exe"
        ));
        assert!(!is_codebuddy_ide_windows_exe(
            r"C:\Users\Zhou\AppData\Local\Programs\CodeBuddy CN\CodeBuddy.exe"
        ));
        assert!(!is_codebuddy_ide_windows_exe(
            r"C:\Users\Zhou\AppData\Local\Programs\CodeBuddy CN\CodeBuddy CN.exe"
        ));
    }

    #[test]
    fn windows_ide_rows_drop_self_cn_and_crashpad() {
        let stdout = "\
1001|workbuddy-switch|C:\\apps\\workbuddy-switch.exe
1002|CodeBuddy CN|D:\\Programs\\CodeBuddy CN\\CodeBuddy CN.exe
1003|CodeBuddy|D:\\Users\\Zhou\\AppData\\Local\\Programs\\CodeBuddy\\CodeBuddy.exe
1004|crashpad_handler|C:\\x\\crashpad_handler.exe
1005|wb-switch|
1006|CodeBuddy|C:\\Users\\Zhou\\AppData\\Local\\Programs\\CodeBuddy\\CodeBuddy.exe
1007|CodeBuddy|C:\\Users\\Zhou\\AppData\\Local\\Programs\\CodeBuddy CN\\CodeBuddy.exe
";
        let kept: Vec<u32> = process::parse_windows_process_rows(stdout)
            .into_iter()
            .filter(keep_windows_ide_row)
            .map(|row| row.pid)
            .collect();
        assert_eq!(kept, vec![1003, 1006]);
    }

    #[test]
    fn windows_ide_fallback_candidates_include_local_and_program_files() {
        let cands = windows_ide_fallback_exe_candidates(
            Some(r"C:\Users\Zhou\AppData\Local"),
            Some(r"C:\Program Files"),
            Some(r"C:\Program Files (x86)"),
            Some("Zhou"),
            &['C'],
        );
        let s: Vec<String> = cands
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(s
            .iter()
            .any(|p| p.contains("Programs") && p.contains("CodeBuddy.exe")));
        assert!(s
            .iter()
            .any(|p| p.contains("CodeBuddy") && p.contains("CodeBuddy.exe")));
        assert!(s
            .iter()
            .any(|p| p.contains("Program Files") && p.contains("CodeBuddy.exe")));
        assert!(!s.iter().any(|p| path_contains_codebuddy_cn_dir(p)));
    }

    #[test]
    fn linux_cmdline_matcher_accepts_intl_not_cn_or_switcher() {
        assert!(linux_cmdline_is_codebuddy_ide(
            "/opt/codebuddy/codebuddy --foo"
        ));
        assert!(linux_cmdline_is_codebuddy_ide("/usr/bin/CodeBuddy"));
        assert!(linux_exe_is_codebuddy_ide(Path::new("/usr/bin/codebuddy")));
        assert!(!linux_cmdline_is_codebuddy_ide("/usr/bin/workbuddy-switch"));
        assert!(!linux_cmdline_is_codebuddy_ide(
            "/opt/CodeBuddy CN/codebuddy-cn"
        ));
        assert!(!linux_cmdline_is_codebuddy_ide(
            "/opt/codebuddy/codebuddy --type=gpu-process"
        ));
        assert!(!linux_exe_is_codebuddy_ide(Path::new(
            "/usr/bin/codebuddy-cn"
        )));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_ide_pattern_fallback_does_not_match_international_codebuddy() {
        assert_eq!(
            macos_ide_main_patterns(None),
            vec!["CodeBuddy.app/Contents/MacOS".to_string()]
        );
        assert_eq!(
            macos_ide_bundle_patterns(None),
            vec!["CodeBuddy.app".to_string()]
        );
        assert_eq!(
            macos_ide_main_patterns(Some(Path::new("/Applications/CodeBuddy.app"))),
            vec!["/Applications/CodeBuddy.app/Contents/MacOS".to_string()]
        );
        let cands = macos_ide_app_candidates(Path::new("/Users/tester"));
        let s: Vec<String> = cands
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            s,
            vec![
                "/Applications/CodeBuddy.app".to_string(),
                "/Users/tester/Applications/CodeBuddy.app".to_string(),
            ]
        );
        assert!(s.iter().all(|p| p.ends_with("CodeBuddy.app")));
        assert!(!s.iter().any(|p| p.contains("CodeBuddy CN")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_ide_ps_filter_excludes_self_and_cn() {
        let self_pid = std::process::id();
        let stdout = format!(
            "{self_pid} /Applications/workbuddy-switch.app/Contents/MacOS/wb-switch\n\
             6001 /Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy --foo\n\
             6002 /Applications/CodeBuddy CN.app/Contents/MacOS/CodeBuddy CN\n\
             6003 /bin/zsh -c 'echo CodeBuddy.app mention via wb-switch'\n\
             6004 /Applications/CodeBuddy.app/Contents/Resources/helper\n"
        );
        let main_kept = process::filter_ps_rows(&stdout, &macos_ide_main_patterns(None), self_pid);
        let main_pids: Vec<u32> = main_kept.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(main_pids, vec![6001]);

        let bundle_kept =
            process::filter_ps_rows(&stdout, &macos_ide_bundle_patterns(None), self_pid);
        let bundle_pids: Vec<u32> = bundle_kept.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(bundle_pids, vec![6001, 6004]);
    }
}
