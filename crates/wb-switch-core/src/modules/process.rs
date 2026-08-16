//! 进程控制：检测、关闭、启动 WorkBuddy。
//!
//! 对照 server.py `is_workbuddy_running` / `_wait_process_gone` /
//! `close_workbuddy` / `launch_workbuddy`。

use std::io::Read;
#[cfg(target_os = "windows")]
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// 创建子进程命令。Windows 上加 CREATE_NO_WINDOW，避免每次执行 tasklist/powershell
/// 等控制台命令时闪出 cmd 黑窗口（GUI 应用卡顿/跳动的主因）。
fn cmd_builder(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

use crate::modules::auth_file;

/// 运行命令并等待退出，超时则 kill 并返回 None（对应 Python `subprocess.run(timeout=...)`）。
fn run_cmd_timeout(program: &str, args: &[&str], timeout_secs: u64) -> Option<Output> {
    let mut child = cmd_builder(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut status = None;
    while status.is_none() {
        match child.try_wait() {
            Ok(Some(s)) => status = Some(s),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    let _ = stdout.read_to_end(&mut out);
    let _ = stderr.read_to_end(&mut err);
    Some(Output {
        status: status.unwrap(),
        stdout: out,
        stderr: err,
    })
}

/// WorkBuddy 相关进程名匹配模式（进程名不固定，动态识别覆盖常见命名）。
#[cfg(target_os = "windows")]
const PROCESS_NAME_RE: &str = "WorkBuddy|CodeBuddy";

/// Windows：执行 PowerShell 并取首行输出（去空白）。
#[cfg(target_os = "windows")]
fn ps_first_line(script: &str) -> Option<String> {
    let out = cmd_builder("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Windows：动态查找 WorkBuddy 可执行文件路径。
///
/// 顺序：运行中的进程（最可靠，能拿到真实安装位置）→ 注册表
/// Uninstall 的 InstallLocation → None（由调用方兜底默认路径）。
/// 用户可能装在任意盘符/目录，因此不能写死路径。
#[cfg(target_os = "windows")]
pub fn windows_workbuddy_exe_path() -> Option<PathBuf> {
    // 1) 运行进程的 Path
    if let Some(p) = ps_first_line(&format!(
        "$p = Get-Process | Where-Object {{ $_.Name -match '{}' }} | Select-Object -First 1; \
         if ($p -and $p.Path) {{ $p.Path }}",
        PROCESS_NAME_RE
    )) {
        let pb = PathBuf::from(&p);
        if pb.exists() && pb.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            return Some(pb);
        }
    }
    // 2) 注册表 InstallLocation
    if let Some(dir) = ps_first_line(
        "$paths = Get-ChildItem 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKCU:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' -ErrorAction SilentlyContinue | \
         Where-Object { $_.GetValue('DisplayName') -match 'WorkBuddy|CodeBuddy' } | \
         ForEach-Object { $_.GetValue('InstallLocation') } | Where-Object { $_ }; \
         if ($paths) { $paths | Select-Object -First 1 }",
    ) {
        for exe in [PathBuf::from(&dir).join("WorkBuddy.exe"), PathBuf::from(&dir).join("CodeBuddy.exe")] {
            if exe.exists() {
                return Some(exe);
            }
        }
    }
    None
}

/// Windows：当前运行中的 WorkBuddy 进程名（供 taskkill）。
#[cfg(target_os = "windows")]
fn windows_workbuddy_process_name() -> Option<String> {
    ps_first_line(&format!(
        "$p = Get-Process | Where-Object {{ $_.Name -match '{}' }} | Select-Object -First 1; \
         if ($p) {{ $p.ProcessName }}",
        PROCESS_NAME_RE
    ))
}

/// WorkBuddy 是否在运行。
pub fn is_workbuddy_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        match cmd_builder("pgrep").args(["-f", "WorkBuddy.app/Contents/MacOS"]).output() {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }
    #[cfg(target_os = "windows")]
    {
        // 进程名不固定，动态匹配（tasklist 输出按名称片段判断）
        match cmd_builder("tasklist").output() {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
                s.contains("workbuddy") || s.contains("codebuddy")
            }
            Err(_) => false,
        }
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
    if !is_workbuddy_running() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        // 1) 优雅退出：用 bundle id，失败不阻塞
        let quit = run_cmd_timeout("osascript", &["-e", "quit app id \"com.workbuddy.workbuddy\""], 10);
        if let Some(o) = &quit {
            if !o.status.success() {
                eprintln!("[close] osascript quit failed: {}", String::from_utf8_lossy(&o.stderr));
            }
        } else {
            eprintln!("[close] osascript quit timed out");
        }
        // 2) 等待优雅退出生效
        if wait_process_gone(timeout_secs as f64) {
            return Ok(());
        }
        eprintln!("[close] graceful quit not effective, forcing kill…");
        // 3) 兜底强杀
        let kill = run_cmd_timeout("pkill", &["-9", "-f", "WorkBuddy.app/Contents/MacOS"], 10);
        if let Some(o) = &kill {
            if !o.status.success() {
                eprintln!("[close] pkill failed: {}", String::from_utf8_lossy(&o.stderr));
            }
        } else {
            eprintln!("[close] pkill timed out");
        }
        if wait_process_gone(timeout_secs as f64) {
            return Ok(());
        }
        return Err(
            "WorkBuddy 进程无法关闭，请手动在终端执行: pkill -9 -f \"WorkBuddy.app/Contents/MacOS\""
                .to_string(),
        );
    }

    #[cfg(target_os = "windows")]
    {
        // 动态拿进程名再 kill（进程名不固定，不能写死 WorkBuddy.exe）
        if let Some(name) = windows_workbuddy_process_name() {
            let _ = run_cmd_timeout("taskkill", &["/IM", &name, "/T", "/F"], 10);
        }
        if wait_process_gone(timeout_secs as f64) {
            return Ok(());
        }
        return Err("WorkBuddy 进程无法关闭，请手动结束 WorkBuddy/CodeBuddy 进程".to_string());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
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

/// 启动 WorkBuddy（macOS open -n -a 强制新实例）。失败返回错误信息。对照 `launch_workbuddy`。
pub fn launch_workbuddy() -> Result<(), String> {
    let app = auth_file::workbuddy_app_path();
    #[cfg(target_os = "macos")]
    {
        // 旧进程若还在（极端情况），先强杀，避免 open 新实例被单实例锁挡掉
        if is_workbuddy_running() {
            let _ = run_cmd_timeout("pkill", &["-9", "-f", "WorkBuddy.app/Contents/MacOS"], 10);
            wait_process_gone(5.0);
        }
        let _ = cmd_builder("open")
            .args(["-n", "-a"])
            .arg(&app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let exe = if app.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
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
