//! CodeBuddy CN IDE（桌面客户端）账号切换。
//!
//! 复用 WorkBuddy 账号库中的 CN token（www.codebuddy.cn），写入
//! `~/Library/Application Support/CodeBuddy CN/.../state.vscdb` 的 Safe Storage
//! secret，并可选重启 CodeBuddy CN。与 CodeBuddy CLI（`~/.codebuddy`）完全独立。

use serde_json::{json, Value};
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::process::Command;
use std::time::Duration;
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::time::Instant;

use crate::modules::account::{self, get_str};
#[cfg(target_os = "macos")]
use crate::modules::config::home_dir;
use crate::modules::config::{atomic_write, now_ms, store_dir};
// 复用 process 模块带并发管道读取的正确实现；本地轮询版会在子进程输出
// 超过 64KB（如 `ps -axo pid=,args=`）时因管道写满而死锁到超时。
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

#[cfg(target_os = "macos")]
fn macos_app_candidates() -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        PathBuf::from("/Applications").join(MACOS_APP_NAME),
        home.join("Applications").join(MACOS_APP_NAME),
    ]
}

#[cfg(target_os = "macos")]
fn is_app_bundle(path: &Path) -> bool {
    path.is_dir() && path.join("Contents").join("Info.plist").is_file()
}

/// 解析 CodeBuddy CN.app 路径。
///
/// macOS 命中后缓存结果：状态查询会频繁调用本函数，而 mdfind（Spotlight）
/// 冷启动可能耗时数秒。未命中不缓存，以便运行期间新安装应用后能立即识别。
pub fn codebuddy_cn_app_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        static CACHED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        if let Some(path) = CACHED.get() {
            return Some(path.clone());
        }
        for p in macos_app_candidates() {
            if is_app_bundle(&p) {
                let _ = CACHED.set(p.clone());
                return Some(p);
            }
        }
        // mdfind fallback
        if let Ok(out) = Command::new("mdfind")
            .arg(format!("kMDItemCFBundleIdentifier == '{MACOS_BUNDLE_ID}'c"))
            .output()
        {
            if out.status.success() {
                if let Some(line) = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                {
                    let p = PathBuf::from(line);
                    if is_app_bundle(&p) {
                        let _ = CACHED.set(p.clone());
                        return Some(p);
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("CodeBuddy CN")
                    .join("CodeBuddy CN.exe"),
            );
        }
        if let Ok(pf) = std::env::var("PROGRAMFILES") {
            candidates.push(
                PathBuf::from(pf)
                    .join("CodeBuddy CN")
                    .join("CodeBuddy CN.exe"),
            );
        }
        candidates.into_iter().find(|p| p.is_file())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        [
            "/usr/bin/codebuddy-cn",
            "/usr/local/bin/codebuddy-cn",
            "/opt/codebuddy-cn/codebuddy-cn",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    }
}

/// CodeBuddy CN 是否在运行（macOS：包路径子串匹配）。
pub fn is_codebuddy_cn_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        let patterns: Vec<String> = match codebuddy_cn_app_path() {
            Some(app) => vec![format!("{}/Contents/MacOS", app.display())],
            None => vec![
                "CodeBuddy CN.app/Contents/MacOS".to_string(),
                "CodeBuddy CN.app".to_string(),
            ],
        };
        let out = run_cmd("ps", &["-axo", "pid=,args="], 5);
        let stdout = out
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let self_pid = std::process::id();
        for line in stdout.lines() {
            let line = line.trim_start();
            let Some(split) = line.find(|c: char| c.is_whitespace()) else {
                continue;
            };
            let (pid_s, rest) = line.split_at(split);
            let Ok(pid) = pid_s.trim().parse::<u32>() else {
                continue;
            };
            if pid == self_pid {
                continue;
            }
            let args = rest.trim();
            if args.contains("wb-switch") || args.contains("workbuddy-switch") {
                continue;
            }
            if patterns.iter().any(|p| args.contains(p.as_str())) {
                return true;
            }
        }
        false
    }
    #[cfg(target_os = "windows")]
    {
        let out = run_cmd(
            "tasklist",
            &["/FI", "IMAGENAME eq CodeBuddy CN.exe", "/FO", "CSV", "/NH"],
            5,
        );
        out.map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.to_ascii_lowercase().contains("codebuddy cn.exe")
        })
        .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        run_cmd("pgrep", &["-f", "codebuddy-cn"], 5)
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
    }
}

pub fn close_codebuddy_cn(timeout_secs: i64) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let timeout = Duration::from_secs(timeout_secs.max(1) as u64);
        let started = Instant::now();
        let quit = format!("quit app id \"{MACOS_BUNDLE_ID}\"");
        let _ = run_cmd("osascript", &["-e", quit.as_str()], 10);

        // 等主进程优雅退出
        while started.elapsed() < Duration::from_secs(8).min(timeout) {
            if !is_codebuddy_cn_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(400));
        }

        // 强杀包内进程
        let app = codebuddy_cn_app_path();
        let pattern = app
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "CodeBuddy CN.app".to_string());
        let out = run_cmd("ps", &["-axo", "pid=,args="], 5);
        let stdout = out
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let self_pid = std::process::id();
        let mut pids = Vec::new();
        for line in stdout.lines() {
            let line = line.trim_start();
            let Some(split) = line.find(|c: char| c.is_whitespace()) else {
                continue;
            };
            let (pid_s, rest) = line.split_at(split);
            let Ok(pid) = pid_s.trim().parse::<u32>() else {
                continue;
            };
            if pid == self_pid {
                continue;
            }
            let args = rest.trim();
            if args.contains("wb-switch") || args.contains("workbuddy-switch") {
                continue;
            }
            if args.contains(&pattern) {
                pids.push(pid);
            }
        }
        if !pids.is_empty() {
            let mut args = vec!["-9".to_string()];
            args.extend(pids.iter().map(|p| p.to_string()));
            let owned: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let _ = run_cmd("kill", &owned, 10);
        }
        let deadline = Instant::now() + timeout.saturating_sub(started.elapsed());
        while Instant::now() < deadline {
            if !is_codebuddy_cn_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        if is_codebuddy_cn_running() {
            return Err("CodeBuddy CN 进程无法完全关闭，请手动退出后再试".to_string());
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let _ = timeout_secs;
        let _ = run_cmd("taskkill", &["/IM", "CodeBuddy CN.exe", "/T"], 10);
        std::thread::sleep(Duration::from_secs(2));
        if is_codebuddy_cn_running() {
            let _ = run_cmd("taskkill", &["/IM", "CodeBuddy CN.exe", "/T", "/F"], 10);
        }
        if is_codebuddy_cn_running() {
            return Err("CodeBuddy CN 进程无法关闭".to_string());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = timeout_secs;
        let _ = run_cmd("pkill", &["-15", "-f", "codebuddy-cn"], 10);
        std::thread::sleep(Duration::from_secs(2));
        if is_codebuddy_cn_running() {
            let _ = run_cmd("pkill", &["-9", "-f", "codebuddy-cn"], 10);
        }
        Ok(())
    }
}

pub fn launch_codebuddy_cn() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let app = codebuddy_cn_app_path().ok_or_else(|| {
            "未找到 CodeBuddy CN 应用。请确认已安装到 /Applications/CodeBuddy CN.app".to_string()
        })?;
        let app_s = app.to_string_lossy();
        let out = run_cmd("open", &["-n", "-a", app_s.as_ref()], 10);
        match out {
            Some(o) if o.status.success() => Ok(()),
            Some(o) => {
                let reason = String::from_utf8_lossy(&o.stderr).trim().to_string();
                Err(format!(
                    "启动 CodeBuddy CN 失败: {}（路径: {}）",
                    if reason.is_empty() {
                        format!("open 退出码 {}", o.status.code().unwrap_or(-1))
                    } else {
                        reason
                    },
                    app.display()
                ))
            }
            None => Err(format!("启动 CodeBuddy CN 失败: open 超时（路径: {}）", app.display())),
        }
    }
    #[cfg(target_os = "windows")]
    {
        let exe = codebuddy_cn_app_path().ok_or_else(|| {
            "未找到 CodeBuddy CN 程序，请先安装 CodeBuddy CN".to_string()
        })?;
        Command::new(&exe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 CodeBuddy CN 失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let exe = codebuddy_cn_app_path().ok_or_else(|| {
            "未找到 CodeBuddy CN 可执行文件".to_string()
        })?;
        Command::new(&exe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 CodeBuddy CN 失败: {e}"))?;
        Ok(())
    }
}

/// 状态：是否安装、是否运行、当前账号（优先本地状态文件，其次尝试读 secret）。
pub fn status() -> Value {
    let data_dir = codebuddy_cn_data_dir();
    let db_path = codebuddy_cn_state_db_path();
    let installed = codebuddy_cn_app_path().is_some()
        || data_dir.as_ref().map(|p| p.exists()).unwrap_or(false);
    let db_exists = db_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    let running = is_codebuddy_cn_running();

    let mut active_account_id = active_account_id_from_state();
    let mut active_account_name: Option<String> = None;
    let mut detected_from = "state".to_string();

    if let Some(id) = active_account_id.clone() {
        if let Some(acc) = account::find_account(&id) {
            active_account_name = Some(account::account_display_name(&acc));
        } else {
            active_account_id = None;
        }
    }

    // 尝试从本机 secret 匹配（Keychain 可用时）
    if active_account_id.is_none() {
        if let Ok(Some(secret)) = read_codebuddy_cn_secret(None) {
            if let Some((uid, token)) = parse_token_from_secret(&secret) {
                if let Some(acc) = match_account_for_token(uid.as_deref(), &token) {
                    active_account_id = get_str(&acc, "id");
                    active_account_name = Some(account::account_display_name(&acc));
                    detected_from = "local-secret".to_string();
                }
            }
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
        "detectedFrom": detected_from,
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
}
