//! 进程控制：检测、关闭、启动 WorkBuddy。
//!
//! 对照 server.py `is_workbuddy_running` / `_wait_process_gone` /
//! `close_workbuddy` / `launch_workbuddy`。

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::modules::auth_file;

/// 运行命令并等待退出，超时则 kill 并返回 None（对应 Python `subprocess.run(timeout=...)`）。
fn run_cmd_timeout(program: &str, args: &[&str], timeout_secs: u64) -> Option<Output> {
    let mut child = Command::new(program)
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

/// WorkBuddy 是否在运行。
pub fn is_workbuddy_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        match Command::new("pgrep").args(["-f", "WorkBuddy.app/Contents/MacOS"]).output() {
            Ok(out) => out.status.success() && !out.stdout.is_empty(),
            Err(_) => false,
        }
    }
    #[cfg(target_os = "windows")]
    {
        match Command::new("tasklist").output() {
            Ok(out) => String::from_utf8_lossy(&out.stdout).contains("WorkBuddy.exe"),
            Err(_) => false,
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        match Command::new("pgrep").args(["-f", "workbuddy"]).output() {
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
        let _ = run_cmd_timeout("taskkill", &["/IM", "WorkBuddy.exe", "/T", "/F"], 10);
        if wait_process_gone(timeout_secs as f64) {
            return Ok(());
        }
        return Err("WorkBuddy 进程无法关闭，请手动结束 WorkBuddy.exe".to_string());
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

/// 启动 WorkBuddy（macOS open -n -a 强制新实例）。对照 `launch_workbuddy`。
pub fn launch_workbuddy() {
    let app = auth_file::workbuddy_app_path();
    #[cfg(target_os = "macos")]
    {
        // 旧进程若还在（极端情况），先强杀，避免 open 新实例被单实例锁挡掉
        if is_workbuddy_running() {
            let _ = run_cmd_timeout("pkill", &["-9", "-f", "WorkBuddy.app/Contents/MacOS"], 10);
            wait_process_gone(5.0);
        }
        let _ = Command::new("open")
            .args(["-n", "-a"])
            .arg(&app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let exe = if app.extension().is_some_and(|e| e == "exe") {
            app
        } else {
            app.join("WorkBuddy.exe")
        };
        let _ = Command::new(&exe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = Command::new(&app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}
