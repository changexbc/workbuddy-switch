//! CodeBuddy CN IDE（桌面客户端）账号切换。
//!
//! 复用 WorkBuddy 账号库中的 CN token（www.codebuddy.cn），写入
//! `~/Library/Application Support/CodeBuddy CN/.../state.vscdb` 的 Safe Storage
//! secret，并可选重启 CodeBuddy CN。与 CodeBuddy CLI（`~/.codebuddy`）完全独立。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::modules::account::{self, get_str};
use crate::modules::config::{
    atomic_write, clear_codebuddy_cn_app_cache, load_codebuddy_cn_app_cache, now_ms,
    save_codebuddy_cn_app_cache, store_dir,
};
#[cfg(target_os = "macos")]
use crate::modules::config::home_dir;
// 复用 process 模块带并发管道读取的正确实现；本地轮询版会在子进程输出
// 超过 64KB（如 `ps -axo pid=,args=`）时因管道写满而死锁到超时。
use crate::modules::process;
use crate::modules::process::run_cmd_timeout as run_cmd;
use crate::modules::vscode_cn_inject::{
    codebuddy_cn_data_dir, codebuddy_cn_state_db_path, inject_codebuddy_cn_secret,
    read_codebuddy_cn_secret,
};

const STATE_FILE: &str = "codebuddy_cn_ide.json";
#[cfg(target_os = "macos")]
const MACOS_BUNDLE_ID: &str = "com.tencent.codebuddycn";
#[cfg(target_os = "macos")]
const MACOS_APP_NAME: &str = "CodeBuddy CN.app";

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
    let domain = get_str(acc, "domain").unwrap_or_default();
    let refresh_token = get_str(acc, "refresh_token").unwrap_or_default();
    let access_token = get_str(acc, "access_token").unwrap_or_default();
    let token_type = get_str(acc, "token_type").unwrap_or_else(|| "Bearer".to_string());
    let expires_at = acc.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);

    json!({
        "id": "Tencent-Cloud.genie-ide-cn",
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
        if let Some(acc) = accounts.iter().find(|a| get_str(a, "uid").as_deref() == Some(uid)) {
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

/// 精确映像名：`CodeBuddy CN`（忽略 .exe / 路径 / 大小写）。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn is_codebuddy_cn_image_name(name: &str) -> bool {
    windows_image_stem(name).eq_ignore_ascii_case("CodeBuddy CN")
}

fn is_plain_codebuddy_image_name(name: &str) -> bool {
    windows_image_stem(name).eq_ignore_ascii_case("CodeBuddy")
}

/// 路径是否含独立目录分量 `CodeBuddy CN`（安装目录，不是国际版 `CodeBuddy`）。
fn path_contains_codebuddy_cn_dir(path: &str) -> bool {
    path.split(['\\', '/'])
        .any(|part| part.eq_ignore_ascii_case("CodeBuddy CN"))
}

/// Windows CN 可执行文件：`CodeBuddy CN.exe`，或位于 `CodeBuddy CN\` 目录下的 `CodeBuddy.exe`。
/// 国际版 `%LOCALAPPDATA%\Programs\CodeBuddy\CodeBuddy.exe` 不算。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn is_codebuddy_cn_windows_exe(path: &str) -> bool {
    if is_codebuddy_cn_image_name(path) {
        return true;
    }
    is_plain_codebuddy_image_name(path) && path_contains_codebuddy_cn_dir(path)
}

fn persist_cn_app_cache(path: &Path) {
    if load_codebuddy_cn_app_cache().as_deref() == Some(path) {
        return;
    }
    let _ = save_codebuddy_cn_app_cache(path);
}

#[cfg(target_os = "macos")]
fn macos_cn_app_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Applications").join(MACOS_APP_NAME),
        home.join("Applications").join(MACOS_APP_NAME),
    ]
}

#[cfg(target_os = "macos")]
fn macos_cn_main_patterns(resolved_app: Option<&Path>) -> Vec<String> {
    match resolved_app {
        Some(app) => vec![format!("{}/Contents/MacOS", app.display())],
        None => vec!["CodeBuddy CN.app/Contents/MacOS".to_string()],
    }
}

#[cfg(target_os = "macos")]
fn macos_cn_bundle_patterns(resolved_app: Option<&Path>) -> Vec<String> {
    match resolved_app {
        Some(app) => vec![app.display().to_string()],
        None => vec!["CodeBuddy CN.app".to_string()],
    }
}

#[cfg(target_os = "macos")]
fn macos_cn_running_app_path() -> Option<PathBuf> {
    let patterns = macos_cn_main_patterns(None);
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
fn macos_cn_mdfind_app_path() -> Option<PathBuf> {
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
fn macos_cn_app_path_resolved() -> Option<PathBuf> {
    if let Some(p) = macos_cn_running_app_path() {
        persist_cn_app_cache(&p);
        return Some(p);
    }
    if let Some(cached) = load_codebuddy_cn_app_cache() {
        if process::is_app_bundle(&cached) {
            return Some(cached);
        }
        clear_codebuddy_cn_app_cache();
    }
    for p in macos_cn_app_candidates(&home_dir()) {
        if process::is_app_bundle(&p) {
            persist_cn_app_cache(&p);
            return Some(p);
        }
    }
    if let Some(p) = macos_cn_mdfind_app_path() {
        if process::is_app_bundle(&p) {
            persist_cn_app_cache(&p);
            return Some(p);
        }
    }
    None
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn windows_cn_fallback_exe_candidates(
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
    let folders = ["CodeBuddy CN"];
    let exe_names = ["CodeBuddy CN.exe", "CodeBuddy.exe"];
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
fn keep_windows_cn_row(row: &process::WindowsProcessRow) -> bool {
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
    is_codebuddy_cn_windows_exe(&row.name)
        || is_codebuddy_cn_windows_exe(file_name)
        || (!path_s.is_empty() && is_codebuddy_cn_windows_exe(&path_s))
}

#[cfg(target_os = "windows")]
fn is_existing_cn_exe(path: &Path) -> bool {
    path.is_file() && is_codebuddy_cn_windows_exe(&path.to_string_lossy())
}

#[cfg(target_os = "windows")]
fn windows_cn_cim_process_script() -> &'static str {
    "Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
         Where-Object { $_.Name -eq 'CodeBuddy CN.exe' -or $_.Name -eq 'CodeBuddy.exe' } | \
         ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.ExecutablePath }"
}

#[cfg(target_os = "windows")]
fn windows_cn_process_rows() -> Vec<process::WindowsProcessRow> {
    let self_pid = std::process::id();
    if let Some(stdout) = process::ps_output(windows_cn_cim_process_script(), 5) {
        let rows: Vec<_> = process::parse_windows_process_rows(&stdout)
            .into_iter()
            .filter(|row| row.pid != self_pid && keep_windows_cn_row(row))
            .collect();
        if !rows.is_empty() {
            return rows;
        }
    }
    let mut rows = Vec::new();
    rows.extend(process::windows_tasklist_image_rows("CodeBuddy CN.exe"));
    rows.extend(process::windows_tasklist_image_rows("CodeBuddy.exe"));
    rows.into_iter()
        .filter(|row| row.pid != self_pid && keep_windows_cn_row(row))
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_cn_running_exe() -> Option<PathBuf> {
    let stdout = process::ps_output(windows_cn_cim_process_script(), 5)?;
    for row in process::parse_windows_process_rows(&stdout) {
        if !keep_windows_cn_row(&row) {
            continue;
        }
        if let Some(p) = row.exe_path {
            if is_existing_cn_exe(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn windows_cn_registry_exe_candidates() -> Vec<PathBuf> {
    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$out = @()
$names = @('CodeBuddy CN.exe', 'CodeBuddy.exe')
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
    if ($dnl -notmatch 'CodeBuddy CN') { return }
    $icon = $_.GetValue('DisplayIcon')
    if ($icon) { $out += [string]$icon }
    $loc = $_.GetValue('InstallLocation')
    if ($loc) {
      $out += [string](Join-Path $loc 'CodeBuddy CN.exe')
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
        if !is_codebuddy_cn_windows_exe(&parsed) {
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
fn windows_cn_exe_path_resolved() -> Option<PathBuf> {
    if let Some(p) = windows_cn_running_exe() {
        persist_cn_app_cache(&p);
        return Some(p);
    }
    if let Some(cached) = load_codebuddy_cn_app_cache() {
        if is_existing_cn_exe(&cached) {
            return Some(cached);
        }
        clear_codebuddy_cn_app_cache();
    }
    for p in windows_cn_registry_exe_candidates() {
        if is_existing_cn_exe(&p) {
            persist_cn_app_cache(&p);
            return Some(p);
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok();
    let pf = std::env::var("PROGRAMFILES").ok();
    let pf86 = std::env::var("PROGRAMFILES(X86)").ok();
    let user = std::env::var("USERNAME").ok();
    let drives = process::existing_windows_drives();
    for p in windows_cn_fallback_exe_candidates(
        local.as_deref(),
        pf.as_deref(),
        pf86.as_deref(),
        user.as_deref(),
        &drives,
    ) {
        if is_existing_cn_exe(&p) {
            persist_cn_app_cache(&p);
            return Some(p);
        }
    }
    None
}

#[cfg_attr(
    not(any(test, not(any(target_os = "macos", target_os = "windows")))),
    allow(dead_code)
)]
fn linux_cmdline_is_codebuddy_cn(cmdline: &str) -> bool {
    let lower = cmdline.to_ascii_lowercase();
    if lower.contains("wb-switch") || lower.contains("workbuddy-switch") {
        return false;
    }
    if lower.contains("crashpad") || lower.contains("--type=") {
        return false;
    }
    cmdline.contains("CodeBuddy CN")
        || lower.contains("codebuddy-cn")
        || lower.contains("codebuddycn")
}

#[cfg_attr(
    not(any(test, not(any(target_os = "macos", target_os = "windows")))),
    allow(dead_code)
)]
fn linux_exe_is_codebuddy_cn(exe: &Path) -> bool {
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim();
    name.eq_ignore_ascii_case("codebuddy-cn")
        || name.eq_ignore_ascii_case("codebuddycn")
        || is_codebuddy_cn_image_name(name)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_codebuddy_cn_pids() -> Vec<u32> {
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
            if linux_exe_is_codebuddy_cn(&exe) {
                pids.push(pid);
                continue;
            }
        }
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).replace('\0', " "),
            _ => continue,
        };
        if linux_cmdline_is_codebuddy_cn(&cmdline) {
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

/// 解析 CodeBuddy CN 应用路径（macOS: .app bundle；Windows: exe）。
pub fn codebuddy_cn_app_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos_cn_app_path_resolved()
    }
    #[cfg(target_os = "windows")]
    {
        windows_cn_exe_path_resolved()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(cached) = load_codebuddy_cn_app_cache() {
            if cached.is_file() && linux_exe_is_codebuddy_cn(&cached) {
                return Some(cached);
            }
            clear_codebuddy_cn_app_cache();
        }
        let candidates = [
            "/usr/bin/codebuddy-cn",
            "/usr/local/bin/codebuddy-cn",
            "/opt/codebuddy-cn/codebuddy-cn",
        ];
        for p in candidates {
            let path = PathBuf::from(p);
            if path.is_file() {
                persist_cn_app_cache(&path);
                return Some(path);
            }
        }
        None
    }
}

/// CodeBuddy CN 是否在运行（footer 语义 = GUI 主进程）。
pub fn is_codebuddy_cn_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        let resolved = macos_cn_app_path_resolved();
        let patterns = macos_cn_main_patterns(resolved.as_deref());
        !process::macos_pids_by_patterns(&patterns).is_empty()
    }
    #[cfg(target_os = "windows")]
    {
        !windows_cn_process_rows().is_empty()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        !linux_codebuddy_cn_pids().is_empty()
    }
}

#[cfg(target_os = "macos")]
fn close_codebuddy_cn_macos(timeout_secs: i64) -> Result<(), String> {
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
    let resolved = macos_cn_app_path_resolved();
    let main_patterns = macos_cn_main_patterns(resolved.as_deref());
    let bundle_patterns = macos_cn_bundle_patterns(resolved.as_deref());
    let remaining = || timeout.saturating_sub(started.elapsed()).max(Duration::from_millis(100));

    let quit_script = format!("quit app id \"{MACOS_BUNDLE_ID}\"");
    let quit = run_cmd("osascript", &["-e", quit_script.as_str()], 10);
    match quit {
        Some(out) if !out.status.success() => {
            eprintln!(
                "[codebuddy-cn-ide] osascript quit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        None => eprintln!("[codebuddy-cn-ide] osascript quit timed out"),
        _ => {}
    }

    let graceful = Duration::from_secs(8).min(remaining());
    let _ = process::wait_macos_main_gone(&main_patterns, graceful);

    let bundle_pids = process::macos_pids_by_patterns(&bundle_patterns);
    if !bundle_pids.is_empty() {
        eprintln!(
            "[codebuddy-cn-ide] killing {} bundle process(es)…",
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
        "CodeBuddy CN 进程无法完全关闭（残留进程: {}）。请手动执行: kill -9 {}",
        pids.join(", "),
        pids.join(" ")
    ))
}

#[cfg(target_os = "windows")]
fn close_codebuddy_cn_windows(timeout_secs: i64) -> Result<(), String> {
    let rows = windows_cn_process_rows();
    if rows.is_empty() {
        return Ok(());
    }
    let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);

    // 1) 优雅阶段：向拥有可见顶层窗口的进程（即 Electron GUI 主进程）发送
    //    WM_CLOSE，让 CodeBuddy CN 自行走窗口关闭/保存/退出流程。只影响主进程，
    //    渲染子进程会随主进程一并退出，无需逐个处理。
    let notified = process::windows_send_close_to_pids(&pids);
    if notified == 0 {
        // 未发现任何可见顶层窗口（如仅驻留托盘/后台）：退化为对所有进程执行
        // 不带 /F 的 taskkill——对 GUI 进程而言这等价于发送关闭请求。
        eprintln!("[codebuddy-cn-ide] no visible window found, fallback graceful taskkill…");
        for pid in &pids {
            let pid_s = pid.to_string();
            let _ = run_cmd("taskkill", &["/PID", &pid_s, "/T"], 10);
        }
    } else {
        eprintln!("[codebuddy-cn-ide] sent WM_CLOSE to {notified} visible window(s)");
    }

    // 2) 优雅窗口：等待进程全部消失（预算 8s 或剩余时间）。
    let graceful = Duration::from_secs(8).min(timeout);
    let remaining = process::wait_windows_pids_gone(&pids, graceful);
    if remaining.is_empty() {
        return Ok(());
    }

    // 3) 保护用户数据：若残留进程仍持有可见窗口（典型场景：IDE 弹了
    //    “未保存文件”确认框并阻塞退出），此时不自动强杀，避免丢失编辑内容，
    //    改为提示用户保存/关闭后重试。这是与原实现“8s 后无条件 /F”的关键差异。
    let with_window = process::windows_visible_window_pids(&remaining);
    if !with_window.is_empty() {
        return Err(format!(
            "CodeBuddy CN 仍有可见窗口未关闭（PID {}），可能包含未保存内容。请先在 CodeBuddy CN 中保存并关闭窗口后，再重试切换。",
            with_window
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // 4) 无窗口残留（后台进程）→ 可安全强制结束进程树。
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
    Err("CodeBuddy CN 进程无法关闭，请手动结束 CodeBuddy CN 进程".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn close_codebuddy_cn_linux(timeout_secs: i64) -> Result<(), String> {
    let pids = linux_codebuddy_cn_pids();
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
        "CodeBuddy CN 进程无法关闭（残留进程: {}）。请手动执行: kill -9 {}",
        leftover.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "),
        leftover.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ")
    ))
}

pub fn close_codebuddy_cn(timeout_secs: i64) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        close_codebuddy_cn_macos(timeout_secs)
    }
    #[cfg(target_os = "windows")]
    {
        close_codebuddy_cn_windows(timeout_secs)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        close_codebuddy_cn_linux(timeout_secs)
    }
}

#[cfg(target_os = "macos")]
fn validate_macos_cn_startup(app: &Path) -> Result<(), String> {
    let main_patterns = macos_cn_main_patterns(Some(app));
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
                "CodeBuddy CN 启动后立即退出（疑似残留单例锁）。请先手动打开一次 CodeBuddy CN（路径: {}）",
                app.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "启动 CodeBuddy CN 超时，未能确认运行（路径: {}）。请先手动打开一次 CodeBuddy CN。",
        app.display()
    ))
}

#[cfg(target_os = "macos")]
fn launch_codebuddy_cn_macos() -> Result<(), String> {
    let app = macos_cn_app_path_resolved().ok_or_else(|| {
        "未找到 CodeBuddy CN 应用（尝试路径: /Applications/CodeBuddy CN.app）。请先手动打开一次 CodeBuddy CN 后重试。".to_string()
    })?;
    if !process::is_app_bundle(&app) {
        return Err(format!(
            "未找到 CodeBuddy CN 应用（尝试路径: {}）。请先手动打开一次 CodeBuddy CN 后重试。",
            app.display()
        ));
    }

    let bundle_patterns = macos_cn_bundle_patterns(Some(&app));
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
                "启动 CodeBuddy CN 失败: {reason}（路径: {}）",
                app.display()
            ));
        }
        None => {
            return Err(format!(
                "启动 CodeBuddy CN 失败: open 超时（路径: {}）",
                app.display()
            ));
        }
    }

    validate_macos_cn_startup(&app)
}

/// Windows 启动存活校验：spawn 后轮询 CodeBuddy CN 进程出现并持续存活。
///
/// - 从未出现 → 超时 Err；
/// - 出现后持续存活 ≥ `sustain` → Ok；
/// - 出现过但随后消失（单例锁导致秒退的典型特征）→ 立即 Err。
/// 与 macOS 分支 `validate_macos_cn_startup` 语义对齐，避免“spawn 成功即假成功”。
#[cfg(target_os = "windows")]
fn validate_windows_cn_startup(exe: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let sustain = Duration::from_secs(10);
    let mut seen_at: Option<Instant> = None;
    while Instant::now() < deadline {
        let alive = !windows_cn_process_rows().is_empty();
        if alive {
            match seen_at {
                None => seen_at = Some(Instant::now()),
                Some(start) => {
                    if start.elapsed() >= sustain {
                        return Ok(());
                    }
                }
            }
        } else if seen_at.is_some() {
            return Err(format!(
                "CodeBuddy CN 启动后立即退出（疑似残留单例锁）。请先手动打开一次 CodeBuddy CN（路径: {}）。",
                exe.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "启动 CodeBuddy CN 超时，未能确认运行（路径: {}）。请先手动打开一次 CodeBuddy CN。",
        exe.display()
    ))
}

pub fn launch_codebuddy_cn() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        launch_codebuddy_cn_macos()
    }
    #[cfg(target_os = "windows")]
    {
        let exe = windows_cn_exe_path_resolved().ok_or_else(|| {
            "未找到 CodeBuddy CN 程序（尝试路径: %LOCALAPPDATA%\\Programs\\CodeBuddy CN\\CodeBuddy CN.exe）。请在 Windows 上打开 CodeBuddy CN 后重试。".to_string()
        })?;
        if !is_existing_cn_exe(&exe) {
            return Err(format!(
                "未找到 CodeBuddy CN 程序（尝试路径: {}）。请在 Windows 上打开 CodeBuddy CN 后重试。",
                exe.display()
            ));
        }
        persist_cn_app_cache(&exe);
        let spawned = process::cmd_builder(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 CodeBuddy CN 失败: {e}（路径: {}）", exe.display()))?;
        // 启动后存活校验：避免单例锁导致秒退却仍提示“切换成功”。
        validate_windows_cn_startup(&exe)?;
        drop(spawned);
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let exe = codebuddy_cn_app_path().ok_or_else(|| {
            "未找到 CodeBuddy CN 可执行文件（尝试路径: /usr/bin/codebuddy-cn）。请先手动打开一次。".to_string()
        })?;
        process::cmd_builder(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 CodeBuddy CN 失败: {e}（路径: {}）", exe.display()))?;
        Ok(())
    }
}

/// 状态：是否安装、是否运行、当前账号（仅来自本地状态文件 + 账号库，不读取钥匙串）。
pub fn status() -> Value {
    let data_dir = codebuddy_cn_data_dir();
    let db_path = codebuddy_cn_state_db_path();
    let installed = codebuddy_cn_app_path().is_some()
        || data_dir.as_ref().map(|p| p.exists()).unwrap_or(false);
    let db_exists = db_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    let running = is_codebuddy_cn_running();

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
        "appPath": codebuddy_cn_app_path().map(|p| p.to_string_lossy().to_string()),
        "activeAccountId": active_account_id,
        "activeAccountName": active_account_name,
        "detectedFrom": "state",
        "statePath": state_path().to_string_lossy(),
    })
}

/// 切换 CodeBuddy CN IDE 账号：关进程 → 注入 secret → 启动。
pub fn switch_account(account_id: &str, restart: bool) -> Result<Value, String> {
    let acc = account::find_account(account_id)
        .ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let token = get_str(&acc, "access_token")
        .ok_or_else(|| "账号缺少 access_token，无法注入 CodeBuddy CN".to_string())?;
    if token.is_empty() {
        return Err("账号 access_token 为空".to_string());
    }

    let data_dir = codebuddy_cn_data_dir()
        .ok_or_else(|| "无法定位 CodeBuddy CN 数据目录".to_string())?;
    if !data_dir.exists() {
        return Err(format!(
            "未找到 CodeBuddy CN 用户数据目录（{}）。请先手动打开 CodeBuddy CN 并登录一次。",
            data_dir.display()
        ));
    }

    if restart {
        eprintln!("[codebuddy-cn-ide] closing CodeBuddy CN…");
        close_codebuddy_cn(20)?;
    }

    let session = build_session_json(&acc);
    eprintln!("[codebuddy-cn-ide] injecting secret…");
    let db_path = inject_codebuddy_cn_secret(&session, Some(&data_dir)).map_err(|err| {
        if err.contains("Safe Storage") || err.contains("Keychain") {
            format!(
                "注入登录状态失败：{err}\n\n请先手动打开 CodeBuddy CN 并登录一次，确保 Keychain 中存在「CodeBuddy CN Safe Storage」条目后再试。"
            )
        } else {
            err
        }
    })?;

    set_active_account_id(account_id)?;

    if restart {
        eprintln!("[codebuddy-cn-ide] launching CodeBuddy CN…");
        launch_codebuddy_cn()?;
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
            format!("已写入 CodeBuddy IDE 凭证（{}）；请手动重启 CodeBuddy CN 生效", account::account_display_name(&acc))
        },
    }))
}

/// 从本机 CN IDE 读取当前 token；若能匹配账号库则返回匹配信息（不新建账号）。
pub fn detect_current_account() -> Result<Value, String> {
    let secret = read_codebuddy_cn_secret(None)?;
    let Some(secret) = secret else {
        return Ok(json!({
            "ok": true,
            "found": false,
            "message": "本机 CodeBuddy CN 未找到登录 secret",
        }));
    };
    let Some((uid, token)) = parse_token_from_secret(&secret) else {
        return Err("本地 CodeBuddy CN 登录信息解析失败".to_string());
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
        "message": "本机已登录 CodeBuddy CN，但账号库中无匹配账号；可先用「从本机导入」或扫码登录同步账号后再切换。",
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
        assert_eq!(v["id"], "Tencent-Cloud.genie-ide-cn");
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
        let key = crate::modules::vscode_cn_inject::secret_storage_item_key();
        assert!(key.contains("planning-genie.new.accessTokencn"));
        assert!(key.starts_with("secret://"));
    }

    #[test]
    fn cn_image_name_is_exact_not_international_codebuddy() {
        assert!(is_codebuddy_cn_image_name("CodeBuddy CN.exe"));
        assert!(is_codebuddy_cn_image_name("codebuddy cn"));
        assert!(is_codebuddy_cn_image_name(
            r"D:\Users\Zhou\AppData\Local\Programs\CodeBuddy CN\CodeBuddy CN.exe"
        ));
        assert!(!is_codebuddy_cn_image_name("CodeBuddy.exe"));
        assert!(!is_codebuddy_cn_image_name("CodeBuddy"));
        assert!(!is_codebuddy_cn_image_name("WorkBuddy.exe"));
        assert!(!is_codebuddy_cn_image_name("workbuddy-switch.exe"));
        assert!(!is_codebuddy_cn_image_name("wb-switch"));
        assert!(is_codebuddy_cn_windows_exe(
            r"C:\Users\Zhou\AppData\Local\Programs\CodeBuddy CN\CodeBuddy.exe"
        ));
        assert!(!is_codebuddy_cn_windows_exe(
            r"C:\Users\Zhou\AppData\Local\Programs\CodeBuddy\CodeBuddy.exe"
        ));
    }

    #[test]
    fn windows_cn_rows_drop_self_international_and_crashpad() {
        let stdout = "\
1001|workbuddy-switch|C:\\apps\\workbuddy-switch.exe
1002|CodeBuddy|D:\\Programs\\CodeBuddy\\CodeBuddy.exe
1003|CodeBuddy CN|D:\\Users\\Zhou\\AppData\\Local\\Programs\\CodeBuddy CN\\CodeBuddy CN.exe
1004|crashpad_handler|C:\\x\\crashpad_handler.exe
1005|wb-switch|
1006|CodeBuddy|C:\\Users\\Zhou\\AppData\\Local\\Programs\\CodeBuddy CN\\CodeBuddy.exe
";
        let kept: Vec<u32> = process::parse_windows_process_rows(stdout)
            .into_iter()
            .filter(keep_windows_cn_row)
            .map(|row| row.pid)
            .collect();
        assert_eq!(kept, vec![1003, 1006]);
    }

    #[test]
    fn windows_cn_fallback_candidates_include_local_and_program_files() {
        let cands = windows_cn_fallback_exe_candidates(
            Some(r"C:\Users\Zhou\AppData\Local"),
            Some(r"C:\Program Files"),
            Some(r"C:\Program Files (x86)"),
            Some("Zhou"),
            &['C'],
        );
        let s: Vec<String> = cands.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(s.iter().any(|p| p.contains("Programs") && p.contains("CodeBuddy CN.exe")));
        assert!(s.iter().any(|p| p.contains("CodeBuddy CN") && p.contains("CodeBuddy.exe")));
        assert!(s.iter().any(|p| p.contains("Program Files") && p.contains("CodeBuddy CN.exe")));
        assert!(!s.iter().any(|p| {
            p.contains("CodeBuddy.exe") && !path_contains_codebuddy_cn_dir(p)
        }));
    }

    #[test]
    fn linux_cmdline_matcher_accepts_cn_not_switcher() {
        assert!(linux_cmdline_is_codebuddy_cn("/opt/codebuddy-cn/codebuddy-cn --foo"));
        assert!(linux_cmdline_is_codebuddy_cn("/usr/bin/CodeBuddy CN"));
        assert!(linux_exe_is_codebuddy_cn(Path::new("/usr/bin/codebuddy-cn")));
        assert!(!linux_cmdline_is_codebuddy_cn("/usr/bin/workbuddy-switch"));
        assert!(!linux_cmdline_is_codebuddy_cn(
            "/opt/codebuddy-cn/codebuddy-cn --type=gpu-process"
        ));
        assert!(!linux_exe_is_codebuddy_cn(Path::new("/usr/bin/codebuddy")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cn_pattern_fallback_does_not_match_international_codebuddy() {
        assert_eq!(
            macos_cn_main_patterns(None),
            vec!["CodeBuddy CN.app/Contents/MacOS".to_string()]
        );
        assert_eq!(
            macos_cn_bundle_patterns(None),
            vec!["CodeBuddy CN.app".to_string()]
        );
        assert_eq!(
            macos_cn_main_patterns(Some(Path::new("/Applications/CodeBuddy CN.app"))),
            vec!["/Applications/CodeBuddy CN.app/Contents/MacOS".to_string()]
        );
        let cands = macos_cn_app_candidates(Path::new("/Users/tester"));
        let s: Vec<String> = cands.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(
            s,
            vec![
                "/Applications/CodeBuddy CN.app".to_string(),
                "/Users/tester/Applications/CodeBuddy CN.app".to_string(),
            ]
        );
        assert!(!s.iter().any(|p| p.ends_with("CodeBuddy.app")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cn_ps_filter_excludes_self_and_international() {
        let self_pid = std::process::id();
        let stdout = format!(
            "{self_pid} /Applications/workbuddy-switch.app/Contents/MacOS/wb-switch\n\
             6001 /Applications/CodeBuddy CN.app/Contents/MacOS/CodeBuddy CN --foo\n\
             6002 /Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy\n\
             6003 /bin/zsh -c 'echo CodeBuddy CN.app mention via wb-switch'\n\
             6004 /Applications/CodeBuddy CN.app/Contents/Resources/helper\n"
        );
        let main_kept = process::filter_ps_rows(&stdout, &macos_cn_main_patterns(None), self_pid);
        let main_pids: Vec<u32> = main_kept.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(main_pids, vec![6001]);

        let bundle_kept = process::filter_ps_rows(&stdout, &macos_cn_bundle_patterns(None), self_pid);
        let bundle_pids: Vec<u32> = bundle_kept.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(bundle_pids, vec![6001, 6004]);
    }
}
