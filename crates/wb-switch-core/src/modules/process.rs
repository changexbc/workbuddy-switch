//! 进程控制：检测、关闭、启动 WorkBuddy。
//!
//! 对照 server.py `is_workbuddy_running` / `_wait_process_gone` /
//! `close_workbuddy` / `launch_workbuddy`。

// Windows 映像名/路径辅助在非 Windows 上只供单测使用。
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

// macOS 的 app 路径解析收敛在本模块内（`macos_workbuddy_app_path_*`），不再回引 auth_file。
#[cfg(not(target_os = "macos"))]
use crate::modules::auth_file;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::modules::config;

/// 创建子进程命令。Windows 上加 CREATE_NO_WINDOW，避免每次执行 tasklist/powershell
/// 等控制台命令时闪出 cmd 黑窗口（GUI 应用卡顿/跳动的主因）。
fn cmd_builder(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// 运行命令并等待退出，超时则 kill 并返回 None（对应 Python `subprocess.run(timeout=...)`）。
///
/// 注意：stdout/stderr 必须与等待并发读取——先等退出再读会在输出超过
/// 64KB 管道缓冲时死锁（`ps -axo` 全量输出在进程多的机器上很容易超过），
/// 子进程写满阻塞永不退出，最终被超时 kill 并返回 None。
fn run_cmd_timeout(program: &str, args: &[&str], timeout_secs: u64) -> Option<Output> {
    let mut child = cmd_builder(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;

    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut status = None;
    while status.is_none() {
        match child.try_wait() {
            Ok(Some(s)) => status = Some(s),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // 管道随进程退出关闭，读线程随即结束。
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    Some(Output {
        status: status.unwrap(),
        stdout: out,
        stderr: err,
    })
}

fn image_stem(name: &str) -> &str {
    let name = name.trim();
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        name[..name.len() - 4].trim()
    } else {
        name
    }
}

/// Windows 路径可能含 `\`；在非 Windows 上 `Path::file_name` 不会按 `\` 切分。
fn image_name_from_path_str(s: &str) -> &str {
    s.rsplit(['\\', '/']).next().unwrap_or(s).trim()
}

/// 本工具自身的映像名（忽略 .exe、大小写）。
fn is_self_image_name(name: &str) -> bool {
    let stem = image_stem(image_name_from_path_str(name));
    stem.eq_ignore_ascii_case("workbuddy-switch") || stem.eq_ignore_ascii_case("wb-switch")
}

/// 精确匹配 WorkBuddy / CodeBuddy 映像，禁止子串命中 workbuddy-switch。
fn is_workbuddy_image_name(name: &str) -> bool {
    let stem = image_stem(image_name_from_path_str(name));
    stem.eq_ignore_ascii_case("WorkBuddy") || stem.eq_ignore_ascii_case("CodeBuddy")
}

fn is_crashpad_helper_name(name: &str) -> bool {
    image_name_from_path_str(name)
        .to_ascii_lowercase()
        .contains("crashpad_handler")
}

/// 解析卸载项 DisplayIcon：去掉引号和可选的 `,0` 图标索引。
fn parse_windows_display_icon(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let path = if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            rest[..end].trim()
        } else {
            rest.trim().trim_matches('"').trim()
        }
    } else {
        s.trim_matches('"').trim()
    };
    let path = if let Some((p, idx)) = path.rsplit_once(',') {
        if idx.trim().parse::<i32>().is_ok() {
            p.trim().trim_matches('"').trim()
        } else {
            path
        }
    } else {
        path
    };
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn windows_path(parts: &[&str]) -> PathBuf {
    PathBuf::from(parts.join("\\"))
}

/// 环境变量默认目录 + 盘符扫描候选（不访问文件系统，便于非 Windows 单测）。
fn windows_fallback_exe_candidates(
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
    let mut push_win_dir = |parts: &[&str]| {
        let mut wb = parts.to_vec();
        wb.push("WorkBuddy.exe");
        push(windows_path(&wb));
        let mut cb = parts.to_vec();
        cb.push("CodeBuddy.exe");
        push(windows_path(&cb));
    };

    if let Some(local) = local_appdata.map(str::trim).filter(|s| !s.is_empty()) {
        push_win_dir(&[local, "Programs", "WorkBuddy"]);
        push_win_dir(&[local, "Programs", "CodeBuddy"]);
    }
    if let Some(pf) = program_files.map(str::trim).filter(|s| !s.is_empty()) {
        push_win_dir(&[pf, "WorkBuddy"]);
        push_win_dir(&[pf, "CodeBuddy"]);
    }
    if let Some(pf86) = program_files_x86.map(str::trim).filter(|s| !s.is_empty()) {
        push_win_dir(&[pf86, "WorkBuddy"]);
        push_win_dir(&[pf86, "CodeBuddy"]);
    }

    let user = username.map(str::trim).filter(|s| !s.is_empty());
    for drive in drives {
        let letter = drive.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() {
            continue;
        }
        let root = format!("{letter}:");
        if let Some(user) = user {
            push_win_dir(&[
                &root,
                "Users",
                user,
                "AppData",
                "Local",
                "Programs",
                "WorkBuddy",
            ]);
            push_win_dir(&[
                &root,
                "Users",
                user,
                "AppData",
                "Local",
                "Programs",
                "CodeBuddy",
            ]);
        }
        push_win_dir(&[&root, "Program Files", "WorkBuddy"]);
        push_win_dir(&[&root, "Program Files", "CodeBuddy"]);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsProcessRow {
    pid: u32,
    name: String,
    exe_path: Option<PathBuf>,
}

fn parse_simple_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in line.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn parse_tasklist_csv(stdout: &str) -> Vec<WindowsProcessRow> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with("INFO:") {
                return None;
            }
            let cols = parse_simple_csv_line(line);
            let name = cols.first()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let pid = cols.get(1)?.trim().parse::<u32>().ok()?;
            Some(WindowsProcessRow {
                pid,
                name,
                exe_path: None,
            })
        })
        .collect()
}

fn parse_windows_process_rows(stdout: &str) -> Vec<WindowsProcessRow> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(3, '|');
            let pid = parts.next()?.trim().parse::<u32>().ok()?;
            let name = parts.next().unwrap_or("").trim().to_string();
            let path_s = parts.next().unwrap_or("").trim();
            let exe_path = if path_s.is_empty() {
                None
            } else {
                Some(PathBuf::from(path_s))
            };
            Some(WindowsProcessRow {
                pid,
                name,
                exe_path,
            })
        })
        .collect()
}

fn keep_windows_workbuddy_row(row: &WindowsProcessRow) -> bool {
    let path_s = row
        .exe_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file_name = image_name_from_path_str(&path_s);
    if is_self_image_name(&row.name) || is_self_image_name(file_name) {
        return false;
    }
    if is_crashpad_helper_name(&row.name) || is_crashpad_helper_name(file_name) {
        return false;
    }
    is_workbuddy_image_name(&row.name) || is_workbuddy_image_name(file_name)
}

fn filter_windows_workbuddy_rows(rows: &[WindowsProcessRow]) -> Vec<WindowsProcessRow> {
    let mut out = Vec::new();
    for row in rows {
        if !keep_windows_workbuddy_row(row) {
            continue;
        }
        if out.iter().any(|r: &WindowsProcessRow| r.pid == row.pid) {
            continue;
        }
        out.push(row.clone());
    }
    out
}

fn parse_windows_registry_path_lines(stdout: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(parsed) = parse_windows_display_icon(line) else {
            continue;
        };
        let name = image_name_from_path_str(&parsed);
        if is_self_image_name(name) {
            continue;
        }
        if !name.is_empty() && !is_workbuddy_image_name(name) {
            continue;
        }
        let pb = PathBuf::from(parsed);
        if !out.iter().any(|e| e == &pb) {
            out.push(pb);
        }
    }
    out
}

/// 路径最后一段必须是 WorkBuddy/CodeBuddy 映像名（忽略 .exe）。
fn is_workbuddy_exe_file_name(path: &Path) -> bool {
    let owned = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| image_name_from_path_str(&path.to_string_lossy()).to_string());
    is_workbuddy_image_name(&owned)
}

#[cfg(target_os = "windows")]
fn is_existing_workbuddy_exe(path: &Path) -> bool {
    path.is_file() && is_workbuddy_exe_file_name(path)
}

/// 记住已存在的 exe；缓存已是同一路径则不重复写。
#[cfg(target_os = "windows")]
fn persist_workbuddy_exe(path: &Path) {
    if !is_existing_workbuddy_exe(path) {
        return;
    }
    if config::load_workbuddy_exe_cache().as_deref() == Some(path) {
        return;
    }
    let _ = config::save_workbuddy_exe_cache(path);
}

/// Windows：执行 PowerShell 并取 stdout。
#[cfg(target_os = "windows")]
fn ps_output(script: &str, timeout_secs: u64) -> Option<String> {
    let out = run_cmd_timeout(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
        timeout_secs,
    )?;
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(target_os = "windows")]
fn windows_tasklist_image_rows(image: &str) -> Vec<WindowsProcessRow> {
    let filter = format!("IMAGENAME eq {image}");
    let Some(out) = run_cmd_timeout("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"], 5) else {
        return Vec::new();
    };
    parse_tasklist_csv(&String::from_utf8_lossy(&out.stdout))
}

/// 精确映像名收集 WorkBuddy/CodeBuddy PID，排除本工具、自身 PID 与 crashpad。
#[cfg(target_os = "windows")]
fn windows_workbuddy_process_rows() -> Vec<WindowsProcessRow> {
    let self_pid = std::process::id();
    let mut rows = Vec::new();
    rows.extend(windows_tasklist_image_rows("WorkBuddy.exe"));
    rows.extend(windows_tasklist_image_rows("CodeBuddy.exe"));
    filter_windows_workbuddy_rows(&rows)
        .into_iter()
        .filter(|r| r.pid != self_pid)
        .collect()
}

#[cfg(target_os = "windows")]
fn is_windows_pid_running(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    match run_cmd_timeout("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"], 5) {
        Some(out) => parse_tasklist_csv(&String::from_utf8_lossy(&out.stdout))
            .iter()
            .any(|r| r.pid == pid),
        None => true,
    }
}

#[cfg(target_os = "windows")]
fn wait_windows_pids_gone(pids: &[u32], timeout: Duration) -> Vec<u32> {
    if pids.is_empty() {
        return Vec::new();
    }
    let deadline = Instant::now() + timeout;
    loop {
        let alive: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| is_windows_pid_running(*pid))
            .collect();
        if alive.is_empty() || Instant::now() >= deadline {
            return alive;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(target_os = "windows")]
fn existing_windows_drives() -> Vec<char> {
    ('A'..='Z')
        .filter(|c| Path::new(&format!(r"{c}:\")).exists())
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_running_workbuddy_exe() -> Option<PathBuf> {
    let script = "Get-Process -Name WorkBuddy,CodeBuddy -ErrorAction SilentlyContinue | \
         ForEach-Object { $p = ''; try { $p = $_.Path } catch {}; '{0}|{1}|{2}' -f $_.Id, $_.ProcessName, $p }";
    let stdout = ps_output(script, 5)?;
    for row in filter_windows_workbuddy_rows(&parse_windows_process_rows(&stdout)) {
        if let Some(p) = row.exe_path {
            if is_existing_workbuddy_exe(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn windows_registry_exe_candidates() -> Vec<PathBuf> {
    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$out = @()
$appNames = @('WorkBuddy.exe','CodeBuddy.exe')
$appHives = @(
  'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths',
  'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths',
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths'
)
foreach ($hive in $appHives) {
  foreach ($n in $appNames) {
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
    if ($dnl -notmatch 'WorkBuddy|CodeBuddy') { return }
    $icon = $_.GetValue('DisplayIcon')
    if ($icon) { $out += [string]$icon }
    $loc = $_.GetValue('InstallLocation')
    if ($loc) {
      $out += [string](Join-Path $loc 'WorkBuddy.exe')
      $out += [string](Join-Path $loc 'CodeBuddy.exe')
    }
  }
}
$out | ForEach-Object { $_ }
"#;
    let Some(stdout) = ps_output(script, 8) else {
        return Vec::new();
    };
    parse_windows_registry_path_lines(&stdout)
}

/// Windows：动态查找 WorkBuddy 可执行文件路径。
///
/// 顺序：运行中进程 Path → 上次成功路径缓存 → 注册表 App Paths / Uninstall
/// （含 WOW6432Node、DisplayIcon）→ LOCALAPPDATA/Program Files 与各盘符常见目录。
/// 命中且文件存在则写入缓存；缓存指向丢失文件则丢弃。
#[cfg(target_os = "windows")]
pub fn windows_workbuddy_exe_path() -> Option<PathBuf> {
    if let Some(p) = windows_running_workbuddy_exe() {
        persist_workbuddy_exe(&p);
        return Some(p);
    }

    if let Some(cached) = config::load_workbuddy_exe_cache() {
        if is_existing_workbuddy_exe(&cached) {
            return Some(cached);
        }
        config::clear_workbuddy_exe_cache();
    }

    for p in windows_registry_exe_candidates() {
        if is_existing_workbuddy_exe(&p) {
            persist_workbuddy_exe(&p);
            return Some(p);
        }
    }

    let local = std::env::var("LOCALAPPDATA").ok();
    let pf = std::env::var("PROGRAMFILES").ok();
    let pf86 = std::env::var("PROGRAMFILES(X86)").ok();
    let user = std::env::var("USERNAME").ok();
    let drives = existing_windows_drives();
    let candidates = windows_fallback_exe_candidates(
        local.as_deref(),
        pf.as_deref(),
        pf86.as_deref(),
        user.as_deref(),
        &drives,
    );
    for p in candidates {
        if is_existing_workbuddy_exe(&p) {
            persist_workbuddy_exe(&p);
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// macOS：进程枚举 / 关闭 / 启动 / app 路径动态探测
//
// 本地不变量（对齐 Windows 进程契约的自我约束，注释只描述本地规则）：
// - 一律用 `ps -axo pid=,args=` 全量输出，在 Rust 内做**大小写敏感**子串匹配；
//   不直接裸用 pgrep/pkill 字符串（pgrep -f 是大小写不敏感子串匹配，会把命令行
//   里恰好引用路径的无关进程一并命中，实测不可靠）。
// - 排除自身 pid 与 args 含 wb-switch / workbuddy-switch 的 PID（自排除）。
// - 「主进程（GUI）」= argv 含 `<app>/Contents/MacOS/`，仅用于 footer「运行中」
//   语义与启动成功校验；「包内任意进程」= argv 含 `<app>`（含 Contents/MacOS 与
//   Contents/Resources 下的守护子进程），用于 close 的最终清杀与 launch 前的保险
//   清杀（释放目标应用持有的 single-instance launcher 位）。
// - 包内清杀只按 PID 枚举后 `kill -9 <pid>...` 批量，不按名称子串匹配。
// ---------------------------------------------------------------------------

/// 优雅退出用的目标 bundle id（实测正确 id；曾用/错误 id 不是它）。
#[cfg(target_os = "macos")]
const MACOS_QUIT_BUNDLE_ID: &str = "com.tencent.workbuddy.mac";

/// 主进程层字面量回退（路径探测失败时使用）。
#[cfg(target_os = "macos")]
const MACOS_MAIN_LITERAL_SUFFIXES: [&str; 2] = [
    "WorkBuddy.app/Contents/MacOS",
    "CodeBuddy.app/Contents/MacOS",
];

/// 包内层字面量回退（含全小写变体，防用户把 .app 目录改名为小写）。
#[cfg(target_os = "macos")]
const MACOS_BUNDLE_LITERAL_NAMES: [&str; 4] = [
    "WorkBuddy.app",
    "CodeBuddy.app",
    "workbuddy.app",
    "codebuddy.app",
];

/// 解析单行 `ps -axo pid=,args=` 输出为 (pid, args)。
/// ps 输出 pid 列无表头、可能带前导空格，args 保留原始大小写。
#[cfg(target_os = "macos")]
fn parse_ps_row(line: &str) -> Option<(u32, String)> {
    let line = line.trim_start();
    if line.is_empty() {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace())?;
    let (pid_s, rest) = line.split_at(split);
    let pid = pid_s.trim().parse::<u32>().ok()?;
    let args = rest.trim();
    if args.is_empty() {
        return None;
    }
    Some((pid, args.to_string()))
}

/// args 是否命中任一大小写敏感子串模式。
#[cfg(target_os = "macos")]
fn ps_row_matches_any(args: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| args.contains(p.as_str()))
}

/// 从 `ps -axo pid=,args=` 全量输出中收集命中模式的 (pid, args)。
/// 过滤：排除自身 pid；排除 args 含 wb-switch / workbuddy-switch 的 PID。
/// 结果按 pid 去重。残留误杀面仅剩「用户进程的 args 主动引用目标 .app 路径」
/// 这一刻意场景（对齐 Windows 契约记录的残余风险）。
#[cfg(target_os = "macos")]
fn filter_ps_rows(stdout: &str, patterns: &[String], self_pid: u32) -> Vec<(u32, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some((pid, args)) = parse_ps_row(line) else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if args.contains("wb-switch") || args.contains("workbuddy-switch") {
            continue;
        }
        if !ps_row_matches_any(&args, patterns) {
            continue;
        }
        if seen.insert(pid) {
            out.push((pid, args));
        }
    }
    out
}

/// 主进程层匹配模式：解析到 app 路径时用 `<app>/Contents/MacOS`；
/// 探测失败回退字面量（含 CodeBuddy 变体）。纯函数。
#[cfg(target_os = "macos")]
fn macos_main_patterns(resolved_app: Option<&Path>) -> Vec<String> {
    match resolved_app {
        Some(app) => vec![format!("{}/Contents/MacOS", app.display())],
        None => MACOS_MAIN_LITERAL_SUFFIXES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// 包内层匹配模式：解析到 app 路径时用其完整路径；探测失败回退字面量
/// （WorkBuddy/CodeBuddy 及全小写变体）。纯函数。
#[cfg(target_os = "macos")]
fn macos_bundle_patterns(resolved_app: Option<&Path>) -> Vec<String> {
    match resolved_app {
        Some(app) => vec![app.display().to_string()],
        None => MACOS_BUNDLE_LITERAL_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

#[cfg(target_os = "macos")]
fn ps_all_rows() -> String {
    match run_cmd_timeout("ps", &["-axo", "pid=,args="], 5) {
        Some(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        None => String::new(),
    }
}

/// 按模式枚举命中进程（含自排除），去重后返回 (pid, args)。
#[cfg(target_os = "macos")]
fn macos_rows_by_patterns(patterns: &[String]) -> Vec<(u32, String)> {
    filter_ps_rows(&ps_all_rows(), patterns, std::process::id())
}

/// 按模式枚举命中 PID（含自排除）。
#[cfg(target_os = "macos")]
fn macos_pids_by_patterns(patterns: &[String]) -> Vec<u32> {
    macos_rows_by_patterns(patterns)
        .into_iter()
        .map(|(pid, _)| pid)
        .collect()
}

/// 从主进程 argv 提取 `.app` bundle 路径。
/// argv 形如 `<dir>/<Name>.app/Contents/MacOS/<binary>`，取首个 `.app/Contents/MacOS`
/// 出现位置、截到 `.app` 结尾。shell 调用层保持薄，不做复杂解析。
#[cfg(target_os = "macos")]
fn extract_app_bundle_from_args(args: &str) -> Option<PathBuf> {
    let idx = args.find(".app/Contents/MacOS")?;
    let path = &args[..idx + 4]; // 含 `.app`
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// macOS app bundle 谓词：目录存在且含 Contents/Info.plist。
#[cfg(target_os = "macos")]
fn is_app_bundle(path: &Path) -> bool {
    path.is_dir() && path.join("Contents").join("Info.plist").is_file()
}

/// 常见安装位置候选：/Applications、~/Applications × WorkBuddy/CodeBuddy。
/// 纯函数（不访问文件系统），供探测与单测复用。
#[cfg(target_os = "macos")]
fn app_bundle_candidates(home: &Path) -> Vec<PathBuf> {
    let home_apps = home.join("Applications");
    let bases = [Path::new("/Applications"), home_apps.as_path()];
    let mut out = Vec::new();
    for base in bases {
        for name in ["WorkBuddy.app", "CodeBuddy.app"] {
            let p = base.join(name);
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// 命中即写缓存；已是同一路径则不重复写（mac 判 app bundle 目录）。
#[cfg(target_os = "macos")]
fn persist_macos_app_cache(path: &Path) {
    if !is_app_bundle(path) {
        return;
    }
    if config::load_workbuddy_exe_cache().as_deref() == Some(path) {
        return;
    }
    let _ = config::save_workbuddy_exe_cache(path);
}

/// 运行中主进程路径探测：扫主进程层字面量命中行，提取 `.app` 并校验为 bundle。
#[cfg(target_os = "macos")]
fn macos_running_app_path() -> Option<PathBuf> {
    let patterns = macos_main_patterns(None);
    for (_pid, args) in macos_rows_by_patterns(&patterns) {
        if let Some(p) = extract_app_bundle_from_args(&args) {
            if is_app_bundle(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// mdfind 按 bundle id 探测应用路径（Spotlight 不可用/未命中则静默跳过）。
#[cfg(target_os = "macos")]
fn macos_mdfind_app_path() -> Option<PathBuf> {
    let query = format!("kMDItemCFBundleIdentifier == '{}'c", MACOS_QUIT_BUNDLE_ID);
    let out = run_cmd_timeout("mdfind", &[query.as_str()], 5)?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

/// macOS app 路径动态探测（对齐 Windows 契约风格）。
///
/// 顺序：运行中主进程 → 缓存（mac 判 app bundle 目录）→ 常见位置
/// （/Applications、~/Applications × WorkBuddy/CodeBuddy）→ mdfind bundle id。
/// 命中即写缓存；缓存指向已不存在的 bundle 则丢弃并继续。
/// 全部失败返回 None，由调用方决定默认路径 / 字面量回退。
#[cfg(target_os = "macos")]
fn macos_workbuddy_app_path_resolved() -> Option<PathBuf> {
    if let Some(p) = macos_running_app_path() {
        persist_macos_app_cache(&p);
        return Some(p);
    }
    if let Some(cached) = config::load_workbuddy_exe_cache() {
        if is_app_bundle(&cached) {
            return Some(cached);
        }
        config::clear_workbuddy_exe_cache();
    }
    let home = config::home_dir();
    for p in app_bundle_candidates(&home) {
        if is_app_bundle(&p) {
            persist_macos_app_cache(&p);
            return Some(p);
        }
    }
    if let Some(p) = macos_mdfind_app_path() {
        if is_app_bundle(&p) {
            persist_macos_app_cache(&p);
            return Some(p);
        }
    }
    None
}

/// macOS：解析 WorkBuddy app 路径；全部失败回退默认 `/Applications/WorkBuddy.app`
/// （供启动失败文案与包模式回退使用，与改造前的默认值等价）。
#[cfg(target_os = "macos")]
pub fn macos_workbuddy_app_path() -> PathBuf {
    macos_workbuddy_app_path_resolved()
        .unwrap_or_else(|| PathBuf::from("/Applications/WorkBuddy.app"))
}

/// `kill -9` 按 PID 批量强杀；不按字符串匹配。失败仅打日志。
#[cfg(target_os = "macos")]
fn kill_macos_pids(pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let owned: Vec<String> = std::iter::once("-9".to_string())
        .chain(pids.iter().map(|pid| pid.to_string()))
        .collect();
    let args: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    match run_cmd_timeout("kill", &args, 10) {
        Some(out) if !out.status.success() => {
            eprintln!(
                "[process] kill -9 failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        None => eprintln!("[process] kill -9 timed out"),
        _ => {}
    }
}

/// 轮询等待「按模式命中」的进程集合为空；超时返回仍存活的 pid。
#[cfg(target_os = "macos")]
fn wait_macos_patterns_empty(patterns: &[String], timeout: Duration) -> Vec<u32> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let deadline = Instant::now() + timeout;
    loop {
        let alive = macos_pids_by_patterns(patterns);
        if alive.is_empty() || Instant::now() >= deadline {
            return alive;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// 轮询等待「主进程层」消失；超时返回是否已消失。语义同 `wait_process_gone`，
/// 但使用显式主进程模式，避免每轮重新解析 app 路径。
#[cfg(target_os = "macos")]
fn wait_macos_main_gone(main_patterns: &[String], timeout: Duration) -> bool {
    wait_macos_patterns_empty(main_patterns, timeout).is_empty()
}

/// 关闭 WorkBuddy（macOS）：
/// 1) 用正确 bundle id 发 osascript 优雅退出（失败不阻塞）；
/// 2) 给主进程一段优雅窗口（≤8s）消失；
/// 3) 清杀「包内任意进程」残留（含 Contents/Resources 下守护子进程），释放
///    single-instance launcher 位；
/// 4) 轮询包内集合为空；仍残留则报错（含手动 kill 提示）。
///
/// 注意：**不**以「主进程不在」做早退 —— 仅守护子进程存活的场景必须执行清杀。
#[cfg(target_os = "macos")]
fn close_workbuddy_macos(timeout_secs: i64) -> Result<(), String> {
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
    let resolved = macos_workbuddy_app_path_resolved();
    let main_patterns = macos_main_patterns(resolved.as_deref());
    let bundle_patterns = macos_bundle_patterns(resolved.as_deref());
    let remaining = || timeout.saturating_sub(started.elapsed()).max(Duration::from_millis(100));

    // 1) 优雅退出
    let quit_script = format!("quit app id \"{MACOS_QUIT_BUNDLE_ID}\"");
    let quit = run_cmd_timeout("osascript", &["-e", quit_script.as_str()], 10);
    match quit {
        Some(out) if !out.status.success() => {
            eprintln!(
                "[close] osascript quit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        None => eprintln!("[close] osascript quit timed out"),
        _ => {}
    }

    // 2) 优雅窗口等待主进程消失
    let graceful = Duration::from_secs(8).min(remaining());
    let main_graceful = wait_macos_main_gone(&main_patterns, graceful);
    if !main_graceful {
        eprintln!("[close] graceful quit not effective, forcing bundle kill…");
    }

    // 3) 清杀包内残留（主进程 + 守护子进程）
    let bundle_pids = macos_pids_by_patterns(&bundle_patterns);
    if !bundle_pids.is_empty() {
        eprintln!("[close] killing {} bundle process(es)…", bundle_pids.len());
        kill_macos_pids(&bundle_pids);
    }

    // 4) 等包内集合为空（剩余预算）
    let leftover = wait_macos_patterns_empty(&bundle_patterns, remaining());
    if leftover.is_empty() {
        return Ok(());
    }
    let pids: Vec<String> = leftover.iter().map(|pid| pid.to_string()).collect();
    Err(format!(
        "WorkBuddy 进程无法完全关闭（残留进程: {}）。请手动执行: kill -9 {}",
        pids.join(", "),
        pids.join(" ")
    ))
}

/// 启动存活校验：500ms 轮询，总上限 ~30s。
/// - 主进程从未出现 → 超时 Err；
/// - 出现后持续存活 ≥10s → Ok；
/// - 出现过但随后消失（目标应用单例锁秒退特征 ~2s）→ 立即 Err。
///
/// progress 可选：出现/确认阶段推送心跳，避免"正在启动…"长时间静默被误判卡死。
#[cfg(target_os = "macos")]
fn validate_macos_startup(app: &Path, progress: Option<&dyn Fn(&str)>) -> Result<(), String> {
    let say = |msg: &str| {
        if let Some(p) = progress {
            p(msg);
        }
    };
    let main_patterns = macos_main_patterns(Some(app));
    let deadline = Instant::now() + Duration::from_secs(30);
    let sustain = Duration::from_secs(10);
    let beat_every = Duration::from_secs(2);
    let mut seen_at: Option<Instant> = None;
    let mut last_beat: Option<Instant> = None;
    while Instant::now() < deadline {
        let now = Instant::now();
        let alive = !macos_pids_by_patterns(&main_patterns).is_empty();
        if alive {
            match seen_at {
                None => {
                    seen_at = Some(now);
                    say("WorkBuddy 窗口已出现，正在确认稳定运行…");
                }
                Some(start) => {
                    if now.duration_since(start) >= sustain {
                        say("WorkBuddy 已确认运行。");
                        return Ok(());
                    }
                    // 心跳：出现后每 ~2s 推一次（首拍从 ~2s 起，不推 0s），
                    // 进度平滑又不至于每 500ms 刷屏
                    let since_seen = now.duration_since(start);
                    let due = since_seen >= beat_every
                        && last_beat
                            .map(|b| now.duration_since(b) >= beat_every)
                            .unwrap_or(true);
                    if due {
                        last_beat = Some(now);
                        say(&format!(
                            "已稳定运行 {}s / 10s…",
                            since_seen.as_secs()
                        ));
                    }
                }
            }
        } else if seen_at.is_some() {
            return Err(
                "WorkBuddy 启动后立即退出，疑似残留单例锁。请手动打开一次 WorkBuddy 后再试。"
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "WorkBuddy 启动超时，未能确认运行（路径: {}）。请手动打开排查。",
        app.display()
    ))
}

/// 启动 WorkBuddy（macOS）：路径存在性检查 → 保险清杀包内残留 → open -n -a
/// → 轮询确认主进程出现且持续存活（覆盖单例锁导致秒退的场景）。
///
/// progress 可选：清杀/启动/存活确认阶段推送心跳（见 `validate_macos_startup`）。
#[cfg(target_os = "macos")]
fn launch_workbuddy_macos(progress: Option<&dyn Fn(&str)>) -> Result<(), String> {
    let say = |msg: &str| {
        if let Some(p) = progress {
            p(msg);
        }
    };
    let app = macos_workbuddy_app_path();
    if !is_app_bundle(&app) {
        return Err(format!(
            "未找到 WorkBuddy 应用（尝试路径: {}）。请先手动打开一次 WorkBuddy 后重试。",
            app.display()
        ));
    }

    // 保险清杀：包内残留（如持锁守护）非空则 kill -9 全部并等到空（5s）。
    let bundle_patterns = macos_bundle_patterns(Some(&app));
    let bundle_pids = macos_pids_by_patterns(&bundle_patterns);
    if !bundle_pids.is_empty() {
        say("正在清理残留进程…");
        kill_macos_pids(&bundle_pids);
        let _ = wait_macos_patterns_empty(&bundle_patterns, Duration::from_secs(5));
    }

    // open -n -a <app>：同步执行并检查退出码
    say("正在启动 WorkBuddy…");
    let app_lossy = app.to_string_lossy();
    let open = run_cmd_timeout("open", &["-n", "-a", app_lossy.as_ref()], 10);
    match open {
        Some(out) if out.status.success() => {}
        Some(out) => {
            let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let reason = if reason.is_empty() {
                format!("open 退出码 {}", out.status.code().unwrap_or(-1))
            } else {
                reason
            };
            return Err(format!("启动 WorkBuddy 失败: {reason}（路径: {}）", app.display()));
        }
        None => {
            return Err(format!(
                "启动 WorkBuddy 失败: open 超时（路径: {}）",
                app.display()
            ));
        }
    }

    validate_macos_startup(&app, progress)
}

/// WorkBuddy 是否在运行。
pub fn is_workbuddy_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        // footer 语义 = GUI 主进程在运行：仅包内守护子进程存活不算运行中。
        // 使用解析出的主进程模式，解析失败回退字面量。
        let resolved = macos_workbuddy_app_path_resolved();
        let patterns = macos_main_patterns(resolved.as_deref());
        !macos_pids_by_patterns(&patterns).is_empty()
    }
    #[cfg(target_os = "windows")]
    {
        !windows_workbuddy_process_rows().is_empty()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        match cmd_builder("pgrep").args(["-f", "workbuddy"]).output() {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }
}

/// 轮询等待 WorkBuddy 进程全部退出，返回是否已退出。对照 `_wait_process_gone`。
pub fn wait_process_gone(timeout_secs: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_secs);
    while Instant::now() < deadline {
        if !is_workbuddy_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    !is_workbuddy_running()
}

/// 关闭 WorkBuddy：优雅退出 → 超时后强杀 → 确认进程消失。对照 `close_workbuddy`。
pub fn close_workbuddy(timeout_secs: i64) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        close_workbuddy_windows(timeout_secs)
    }
    #[cfg(target_os = "macos")]
    {
        close_workbuddy_macos(timeout_secs)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if !is_workbuddy_running() {
            return Ok(());
        }
        let _ = run_cmd_timeout("pkill", &["-15", "-f", "workbuddy"], 10);
        if wait_process_gone((timeout_secs.min(5)) as f64) {
            return Ok(());
        }
        let _ = run_cmd_timeout("pkill", &["-9", "-f", "workbuddy"], 10);
        if wait_process_gone(timeout_secs as f64) {
            return Ok(());
        }
        return Err("WorkBuddy 进程无法关闭，请手动结束 workbuddy 进程".to_string());
    }
}

/// 先对目标 PID `taskkill /PID /T`（无 /F），超时再 `/F`；按 PID 等待，不按名称子串。
#[cfg(target_os = "windows")]
fn close_workbuddy_windows(timeout_secs: i64) -> Result<(), String> {
    let rows = windows_workbuddy_process_rows();
    if rows.is_empty() {
        return Ok(());
    }
    let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
    for pid in &pids {
        let pid_s = pid.to_string();
        let _ = run_cmd_timeout("taskkill", &["/PID", &pid_s, "/T"], 10);
    }

    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
    let graceful_budget = Duration::from_secs(8).min(timeout);
    let remaining = wait_windows_pids_gone(&pids, graceful_budget);
    if remaining.is_empty() {
        return Ok(());
    }

    for pid in &remaining {
        let pid_s = pid.to_string();
        let _ = run_cmd_timeout("taskkill", &["/PID", &pid_s, "/T", "/F"], 10);
    }
    let rest = timeout
        .saturating_sub(started.elapsed())
        .max(Duration::from_secs(1));
    let leftover = wait_windows_pids_gone(&remaining, rest);
    if leftover.is_empty() {
        return Ok(());
    }
    Err("WorkBuddy 进程无法关闭，请手动结束 WorkBuddy/CodeBuddy 进程".to_string())
}

/// 启动 WorkBuddy。失败返回可读错误。对照 `launch_workbuddy`。
///
/// progress 可选：macOS 启动与存活确认阶段推送心跳（避免界面长时间静默）；
/// Windows/Linux 分支忽略该参数。
pub fn launch_workbuddy(progress: Option<&dyn Fn(&str)>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        launch_workbuddy_macos(progress)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = progress;
        let app = auth_file::workbuddy_app_path();
        #[cfg(target_os = "windows")]
        {
            let exe = if app
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
            {
                app.clone()
            } else {
                app.join("WorkBuddy.exe")
            };
            if !exe.exists() {
                return Err(format!(
                    "未找到 WorkBuddy 程序（尝试路径: {}）。请在 Windows 上打开 WorkBuddy 后重试。",
                    exe.display()
                ));
            }
            persist_workbuddy_exe(&exe);
            cmd_builder(&exe)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("启动 WorkBuddy 失败: {e}（路径: {}）", exe.display()))?;
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = cmd_builder(&app)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_image_names_are_detected() {
        assert!(is_self_image_name("workbuddy-switch"));
        assert!(is_self_image_name("workbuddy-switch.exe"));
        assert!(is_self_image_name("WB-SWITCH.EXE"));
        assert!(is_self_image_name("wb-switch"));
        assert!(is_self_image_name(r"C:\apps\workbuddy-switch.exe"));
        assert!(!is_self_image_name("WorkBuddy.exe"));
        assert!(!is_self_image_name("WorkBuddy"));
        assert!(!is_self_image_name("CodeBuddy.exe"));
    }

    #[test]
    fn workbuddy_image_name_is_exact_not_substring() {
        assert!(is_workbuddy_image_name("WorkBuddy.exe"));
        assert!(is_workbuddy_image_name("workbuddy"));
        assert!(is_workbuddy_image_name("CodeBuddy.exe"));
        assert!(is_workbuddy_image_name("CODEBUDDY"));
        assert!(!is_workbuddy_image_name("workbuddy-switch.exe"));
        assert!(!is_workbuddy_image_name("wb-switch"));
        assert!(!is_workbuddy_image_name("WorkBuddy Helper.exe"));
        assert!(!is_workbuddy_image_name("MyWorkBuddy.exe"));
        assert!(is_workbuddy_exe_file_name(Path::new("WorkBuddy.exe")));
        assert!(is_workbuddy_exe_file_name(Path::new(
            r"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe"
        )));
        assert!(!is_workbuddy_exe_file_name(Path::new(
            "workbuddy-switch.exe"
        )));
        assert!(!is_workbuddy_exe_file_name(Path::new("Uninstall.exe")));
    }

    #[test]
    fn parse_display_icon_strips_quotes_and_index() {
        assert_eq!(
            parse_windows_display_icon(
                r#""D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe,0""#
            ),
            Some(r"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe".to_string())
        );
        assert_eq!(
            parse_windows_display_icon(
                r#""D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe",0"#
            ),
            Some(r"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe".to_string())
        );
        assert_eq!(
            parse_windows_display_icon(r"C:\Program Files\WorkBuddy\WorkBuddy.exe"),
            Some(r"C:\Program Files\WorkBuddy\WorkBuddy.exe".to_string())
        );
        assert_eq!(parse_windows_display_icon("  "), None);
    }

    #[test]
    fn fallback_candidates_include_d_drive_for_zhou() {
        let cands = windows_fallback_exe_candidates(
            Some(r"C:\Users\Zhou\AppData\Local"),
            Some(r"C:\Program Files"),
            Some(r"C:\Program Files (x86)"),
            Some("Zhou"),
            &['C', 'D'],
        );
        let want = r"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe";
        assert!(
            cands.iter().any(|p| p.to_string_lossy() == want),
            "missing {want} in {cands:?}"
        );
        let d_pf = r"D:\Program Files\WorkBuddy\WorkBuddy.exe";
        assert!(
            cands.iter().any(|p| p.to_string_lossy() == d_pf),
            "missing {d_pf} in {cands:?}"
        );
        let local_default = r"C:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe";
        assert!(
            cands.iter().any(|p| p.to_string_lossy() == local_default),
            "missing {local_default} in {cands:?}"
        );
    }

    #[test]
    fn process_rows_drop_self_and_crashpad() {
        let stdout = "\
1001|workbuddy-switch|C:\\apps\\workbuddy-switch.exe
1002|wb-switch|
1003|WorkBuddy|D:\\Users\\Zhou\\AppData\\Local\\Programs\\WorkBuddy\\WorkBuddy.exe
1004|crashpad_handler|C:\\x\\crashpad_handler.exe
1005|CodeBuddy|
1006|WorkBuddy Helper|
";
        let kept = filter_windows_workbuddy_rows(&parse_windows_process_rows(stdout));
        let pids: Vec<u32> = kept.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![1003, 1005]);
    }

    #[test]
    fn only_switcher_process_is_not_workbuddy_running() {
        let stdout = "4400|workbuddy-switch.exe|C:\\Users\\Zhou\\AppData\\Local\\Programs\\wb-switch\\workbuddy-switch.exe\n";
        let kept = filter_windows_workbuddy_rows(&parse_windows_process_rows(stdout));
        assert!(kept.is_empty());

        let csv = "\
\"workbuddy-switch.exe\",\"4400\",\"Console\",\"1\",\"10,000 K\"
\"wb-switch.exe\",\"4401\",\"Console\",\"1\",\"8,000 K\"
";
        let kept = filter_windows_workbuddy_rows(&parse_tasklist_csv(csv));
        assert!(kept.is_empty());
    }

    #[test]
    fn tasklist_csv_keeps_exact_workbuddy_image() {
        let csv = "\
\"WorkBuddy.exe\",\"1234\",\"Console\",\"1\",\"50,123 K\"
\"workbuddy-switch.exe\",\"4400\",\"Console\",\"1\",\"10,000 K\"
INFO: No tasks are running which match the specified criteria.
";
        let kept = filter_windows_workbuddy_rows(&parse_tasklist_csv(csv));
        assert_eq!(kept.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![1234]);
    }

    #[test]
    fn registry_lines_parse_display_icon_and_skip_self() {
        let stdout = r#"
"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe,0"
C:\Users\Zhou\AppData\Local\Programs\workbuddy-switch\workbuddy-switch.exe
D:\Program Files\WorkBuddy\Uninstall.exe
D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe
"#;
        let paths = parse_windows_registry_path_lines(stdout);
        let s: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            s,
            vec![r"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe".to_string(),]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_parse_ps_row_extracts_pid_and_args() {
        assert_eq!(
            parse_ps_row("  1234 /Applications/WorkBuddy.app/Contents/MacOS/WorkBuddy --foo"),
            Some((
                1234,
                "/Applications/WorkBuddy.app/Contents/MacOS/WorkBuddy --foo".to_string()
            ))
        );
        assert_eq!(
            parse_ps_row("4321  /usr/bin/swiftc"),
            Some((4321, "/usr/bin/swiftc".to_string()))
        );
        assert_eq!(parse_ps_row(""), None);
        assert_eq!(parse_ps_row("   "), None);
        assert_eq!(parse_ps_row("1235"), None); // 无 args
        assert_eq!(parse_ps_row("not-a-pid /x"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_pattern_fallback_selection() {
        // 主进程层：解析到路径 → 只用该路径的 Contents/MacOS；失败 → 字面量变体
        assert_eq!(
            macos_main_patterns(None),
            vec![
                "WorkBuddy.app/Contents/MacOS".to_string(),
                "CodeBuddy.app/Contents/MacOS".to_string()
            ]
        );
        assert_eq!(
            macos_main_patterns(Some(Path::new("/Applications/WorkBuddy.app"))),
            vec!["/Applications/WorkBuddy.app/Contents/MacOS".to_string()]
        );

        // 包内层：解析到路径 → 只用完整路径；失败 → 字面量 + 全小写变体
        assert_eq!(
            macos_bundle_patterns(None),
            vec![
                "WorkBuddy.app".to_string(),
                "CodeBuddy.app".to_string(),
                "workbuddy.app".to_string(),
                "codebuddy.app".to_string()
            ]
        );
        assert_eq!(
            macos_bundle_patterns(Some(Path::new("/Applications/CodeBuddy.app"))),
            vec!["/Applications/CodeBuddy.app".to_string()]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_ps_filter_matches_case_sensitively_and_excludes_self() {
        let self_pid = std::process::id();
        // 5002 用小写目录：pgrep -f 大小写不敏感会命中，这里必须不命中。
        // 5003 全大写引用：旧的 pgrep -f 不敏感匹配的典型反例，这里必须不命中。
        // 5005 args 含 wb-switch：非自身 pid 也按自排除规则剔除。
        let stdout = format!(
            "{self_pid} /Applications/workbuddy-switch.app/Contents/MacOS/wb-switch\n\
             5001 /Applications/WorkBuddy.app/Contents/MacOS/WorkBuddy --foo\n\
             5002 /Applications/workbuddy.app/Contents/MacOS/WorkBuddy\n\
             5003 /bin/zsh -c 'echo WORKBUDDY.APP/CONTENTS/MACOS mention'\n\
             5004 /Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy\n\
             5005 /opt/tool/wb-switch/helper run\n"
        );
        let patterns = macos_main_patterns(None);
        let kept = filter_ps_rows(&stdout, &patterns, self_pid);
        let pids: Vec<u32> = kept.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(pids, vec![5001, 5004]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_bundle_filter_keeps_bundle_daemons_and_drops_self() {
        let self_pid = std::process::id();
        // 包内任意进程：主进程 + Contents/Resources 下守护 + 引用 bundle 的自家进程。
        // 5012 用小写目录名（对应字面量全小写变体）。5014 是无关 .app，不误杀。
        let stdout = format!(
            "{self_pid} /Applications/workbuddy-switch.app/Contents/MacOS/workbuddy-switch\n\
             5011 /Applications/WorkBuddy.app/Contents/Resources/app.asar.unpacked/cli/vendor/sandbox/5.5.5/sandbox-center --config x\n\
             5012 /Applications/workbuddy.app/Contents/Resources/.../sandbox-center\n\
             5013 node ~/.workbuddy/binaries/cliguard-daemon --app_bundle /Applications/WorkBuddy.app\n\
             5014 /Applications/SomeOther.app/Contents/MacOS/SomeOther\n"
        );
        let patterns = macos_bundle_patterns(None);
        let kept = filter_ps_rows(&stdout, &patterns, self_pid);
        let pids: Vec<u32> = kept.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(pids, vec![5011, 5012, 5013]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_extract_app_bundle_from_args() {
        assert_eq!(
            extract_app_bundle_from_args("/Applications/WorkBuddy.app/Contents/MacOS/WorkBuddy --foo"),
            Some(PathBuf::from("/Applications/WorkBuddy.app"))
        );
        assert_eq!(
            extract_app_bundle_from_args("/Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy"),
            Some(PathBuf::from("/Applications/CodeBuddy.app"))
        );
        assert_eq!(extract_app_bundle_from_args("/usr/bin/ssh host"), None);
        assert_eq!(extract_app_bundle_from_args(""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_bundle_candidates_order() {
        let cands = app_bundle_candidates(Path::new("/Users/tester"));
        let s: Vec<String> = cands.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(
            s,
            vec![
                "/Applications/WorkBuddy.app".to_string(),
                "/Applications/CodeBuddy.app".to_string(),
                "/Users/tester/Applications/WorkBuddy.app".to_string(),
                "/Users/tester/Applications/CodeBuddy.app".to_string(),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_is_app_bundle_requires_dir_with_info_plist() {
        let dir = std::env::temp_dir().join(format!(
            "wb-switch-app-bundle-test-{}",
            uuid::Uuid::new_v4()
        ));
        let app = dir.join("WorkBuddy.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents").join("Info.plist"), "<plist/>").unwrap();
        assert!(is_app_bundle(&app));

        let contents = app.join("Contents");
        assert!(!is_app_bundle(&contents)); // Contents 不是 bundle 根

        let plain = dir.join("plain-dir");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!is_app_bundle(&plain)); // 无 Info.plist

        assert!(!is_app_bundle(&dir.join("missing.app"))); // 不存在
        std::fs::remove_dir_all(dir).unwrap();
    }
}
