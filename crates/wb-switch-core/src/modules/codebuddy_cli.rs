//! CodeBuddy CLI 账号轮换桥接。
//!
//! Windows 直接维护 `settings.json.env.CODEBUDDY_AUTH_TOKEN`，绕过 CLI
//! 执行 `apiKeyHelper` 时的路径兼容问题；macOS/Linux 继续使用 helper。
//! 两种模式都复用 wb-switch 的 WorkBuddy 账号库，但保持独立的当前账号。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::modules::account;
use crate::modules::config::{atomic_write, home_dir, now_ms};

const ROTATE_DIR: &str = ".codebuddy-rotate";
const STATE_FILE: &str = "state.json";
/// macOS/Linux 直接配置带 Node shebang 的 helper.cjs。旧 Windows helper
/// 常量只用于识别已发布版本，便于状态迁移和非 Windows 兼容测试。
const HELPER_FILE: &str = "helper.cjs";
const LOGIC_FILE: &str = "helper.cjs";
const LEGACY_HELPER_FILE: &str = "helper.sh";
const LEGACY_WINDOWS_HELPER_FILE: &str = "helper.cmd";
const SETTINGS_DIR: &str = ".codebuddy";
const SETTINGS_FILE: &str = "settings.json";
const CODEBUDDY_AUTH_TOKEN: &str = "CODEBUDDY_AUTH_TOKEN";
const STANDARD_HELPER: &str = include_str!("../../../../scripts/codebuddy-cli-helper.cjs");

fn rotate_dir() -> PathBuf {
    home_dir().join(ROTATE_DIR)
}

fn state_path() -> PathBuf {
    rotate_dir().join(STATE_FILE)
}

fn settings_path() -> PathBuf {
    home_dir().join(SETTINGS_DIR).join(SETTINGS_FILE)
}

fn clean_bearer_token(token: &str) -> &str {
    let token = token.trim();
    if token == "Bearer" {
        return "";
    }
    token.strip_prefix("Bearer ").unwrap_or(token).trim()
}

fn settings_env_token(value: &Value) -> Option<&str> {
    value
        .get("env")
        .and_then(Value::as_object)
        .and_then(|env| env.get(CODEBUDDY_AUTH_TOKEN))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn process_env_token_present() -> bool {
    std::env::var_os(CODEBUDDY_AUTH_TOKEN)
        .is_some_and(|token| !token.to_string_lossy().trim().is_empty())
}

fn ensure_no_process_env_override() -> Result<(), String> {
    if process_env_token_present() {
        return Err(auth_config_error(
            "环境阶段",
            "检测到进程环境变量 CODEBUDDY_AUTH_TOKEN；它会覆盖 settings.json，请先删除该用户或系统环境变量并重启应用与 CodeBuddy CLI",
        ));
    }
    Ok(())
}

fn write_settings_env_token(value: &mut Value, token: &str) -> Result<(), String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "CodeBuddy settings.json 顶层不是对象".to_string())?;
    let env = object.entry("env").or_insert_with(|| json!({}));
    let env = env
        .as_object_mut()
        .ok_or_else(|| "CodeBuddy settings.json 的 env 字段不是对象".to_string())?;
    env.insert(CODEBUDDY_AUTH_TOKEN.to_string(), json!(token));
    Ok(())
}

fn persist_settings_at(settings: &Path, value: &Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(value).map_err(|_| {
        auth_config_error("配置阶段", "无法生成 CodeBuddy settings.json")
    })?;
    if let Some(parent) = settings.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            auth_config_error(
                "配置阶段",
                "无法创建 CodeBuddy 配置目录，请检查用户目录权限",
            )
        })?;
    }
    atomic_write(&settings, &content).map_err(|_| {
        auth_config_error(
            "配置阶段",
            "无法写入 CodeBuddy settings.json，请检查文件权限",
        )
    })
}

fn validate_persisted_env_token_at(settings: &PathBuf, expected_token: &str) -> Result<(), String> {
    let persisted = read_json_file(settings).ok_or_else(|| {
        auth_config_error("配置阶段", "写入后无法重新读取 CodeBuddy settings.json")
    })?;
    if settings_env_token(&persisted).map(clean_bearer_token)
        != Some(clean_bearer_token(expected_token))
    {
        return Err(auth_config_error(
            "配置阶段",
            "写入后的认证信息与所选账号不一致",
        ));
    }
    Ok(())
}

fn prepare_settings_env_update(
    settings: &PathBuf,
    token: &str,
) -> Result<(Option<String>, Value), String> {
    let previous = std::fs::read_to_string(settings).ok();
    let mut value = previous
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "CodeBuddy settings.json 不是有效 JSON")?
        .unwrap_or_else(|| json!({}));
    write_settings_env_token(&mut value, clean_bearer_token(token))?;
    Ok((previous, value))
}

fn commit_settings_env_update(
    settings: &PathBuf,
    previous: Option<&str>,
    value: &Value,
    expected_token: &str,
) -> Result<(), String> {
    if let Err(error) = persist_settings_at(settings, value)
        .and_then(|_| validate_persisted_env_token_at(settings, expected_token))
    {
        restore_file(settings, previous);
        return Err(error);
    }
    Ok(())
}

fn read_json_file(path: &PathBuf) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn helper_command() -> Option<String> {
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

/// `apiKeyHelper` 在 CodeBuddy 2.138.0 中先作为文件路径解析；
/// 这里只识别单一路径，并保留旧 `.cmd` / `.sh` helper 的升级通道。
fn command_path(command: &str) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 只把单一路径视为项目 helper；不解析或接管用户的其他 shell 命令。
    // Windows 绝对路径可能含空格；CodeBuddy 2.138.0 会先将路径解析为
    // workdir 绝对路径，再交给 shell，所以不能在设置里额外包引号。
    let windows_absolute = trimmed.len() >= 3
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
        && trimmed.as_bytes()[1] == b':'
        && matches!(trimmed.as_bytes()[2], b'\\' | b'/');
    if trimmed.starts_with('\'')
        || trimmed.starts_with('"')
        || (!windows_absolute && trimmed.chars().any(char::is_whitespace))
    {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn helper_path() -> Option<PathBuf> {
    helper_command().and_then(|command| command_path(&command))
}

fn helper_is_configured() -> bool {
    helper_path().map(|path| path.is_file()).unwrap_or(false)
}

fn helper_migration_required() -> bool {
    let Some(path) = helper_path() else {
        return false;
    };
    is_legacy_helper_path(&path, &rotate_dir(), cfg!(windows))
}

fn is_legacy_helper_path(path: &Path, directory: &Path, windows: bool) -> bool {
    same_path(
        path,
        &directory.join(LEGACY_WINDOWS_HELPER_FILE),
        windows,
    ) || same_path(
        path,
        &directory.join(LEGACY_HELPER_FILE),
        windows,
    )
}

fn helper_is_current() -> bool {
    let Some(path) = helper_path() else {
        return false;
    };
    path.is_file()
        && same_path(&path, &rotate_dir().join(HELPER_FILE), cfg!(windows))
        && helper_supports_account_ids()
}

fn helper_supports_account_ids() -> bool {
    // 升级前 Windows 可能仍配置 `.cmd` 跳板，因此同时检查实际
    // helper.cjs；新安装会直接指向 helper.cjs。
    let configured = helper_path().and_then(|path| std::fs::read_to_string(path).ok());
    let logic = std::fs::read_to_string(rotate_dir().join(LOGIC_FILE)).ok();
    configured
        .into_iter()
        .chain(logic)
        .any(|source| source.contains("activeAccountId"))
}

fn comparable_path(path: &Path, windows: bool) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if windows {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn same_path(left: &Path, right: &Path, windows: bool) -> bool {
    if let (Ok(left), Ok(right)) = (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        return comparable_path(&left, windows) == comparable_path(&right, windows);
    }
    comparable_path(left, windows) == comparable_path(right, windows)
}

#[cfg(any(windows, test))]
fn select_windows_configured_path(
    path: &Path,
    short_path: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if path_is_posix_eval_safe(path) {
        return Ok(path.to_path_buf());
    }
    short_path
        .filter(|candidate| path_is_posix_eval_safe(candidate))
        .ok_or_else(|| {
            helper_validation_error(
                "配置阶段",
                "helper 路径含有 shell 不安全字符，且 Windows 无法生成可供 CodeBuddy 2.138.0 执行的短路径",
            )
        })
}

#[cfg(any(windows, test))]
fn path_is_posix_eval_safe(path: &Path) -> bool {
    !path.to_string_lossy().chars().any(|character| {
        character.is_whitespace()
            || matches!(
                character,
                '&' | '|'
                    | ';'
                    | '\''
                    | '"'
                    | '`'
                    | '$'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '!'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '{'
                    | '}'
            )
    })
}

#[cfg(windows)]
fn windows_short_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, buffer_len: u32) -> u32;
    }

    let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let required = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return None;
    }
    let mut output = vec![0_u16; required as usize + 1];
    let written =
        unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), output.len() as u32) };
    if written == 0 || written as usize >= output.len() {
        return None;
    }
    output.truncate(written as usize);
    Some(PathBuf::from(std::ffi::OsString::from_wide(&output)))
}

#[cfg(windows)]
fn configured_helper_path(path: &Path) -> Result<PathBuf, String> {
    select_windows_configured_path(path, windows_short_path(path))
}

#[cfg(not(windows))]
fn configured_helper_path(path: &Path) -> Result<PathBuf, String> {
    Ok(path.to_path_buf())
}

fn restore_file(path: &Path, previous: Option<&str>) {
    if let Some(previous) = previous {
        let _ = atomic_write(path, previous);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

fn helper_validation_error(stage: &str, cause: &str) -> String {
    format!(
        "CodeBuddy CLI helper 验证失败（{stage}）：{cause}。请确认 Git Bash 和 Node.js 可用，然后重试；如仍失败，请查看 CodeBuddy CLI 日志"
    )
}

fn auth_config_error(stage: &str, cause: &str) -> String {
    format!("CodeBuddy CLI 认证配置失败（{stage}）：{cause}")
}

fn validate_helper_output(output: &Output, expected_token: &str) -> Result<(), String> {
    validate_helper_result(
        output.status.success(),
        output.status.code(),
        &output.stdout,
        expected_token,
    )
}

fn validate_helper_result(
    success: bool,
    exit_code: Option<i32>,
    stdout: &[u8],
    expected_token: &str,
) -> Result<(), String> {
    if !success {
        let code = exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "未知".to_string());
        return Err(helper_validation_error(
            "执行阶段",
            &format!("helper 退出码为 {code}"),
        ));
    }
    let stdout = String::from_utf8_lossy(stdout);
    if stdout.trim().is_empty() {
        return Err(helper_validation_error(
            "输出阶段",
            "helper 未返回认证结果",
        ));
    }
    if stdout.trim() != format!("Bearer {expected_token}") {
        return Err(helper_validation_error(
            "输出阶段",
            "helper 返回的认证结果与所选账号不一致",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_shell_candidates() -> Vec<PathBuf> {
    use std::os::windows::process::CommandExt;

    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("CODEBUDDY_CODE_GIT_BASH_PATH") {
        candidates.push(PathBuf::from(path));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(&root).join("Git/bin/bash.exe"));
            candidates.push(PathBuf::from(root).join("Git/usr/bin/bash.exe"));
        }
    }
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(root).join("Programs/Git/bin/bash.exe"));
    }
    let mut where_git = Command::new("where.exe");
    where_git.creation_flags(0x0800_0000);
    if let Ok(output) = where_git.arg("git.exe").output() {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let git = PathBuf::from(line.trim());
            if let Some(cmd_dir) = git.parent() {
                if let Some(git_root) = cmd_dir.parent() {
                    candidates.push(git_root.join("bin/bash.exe"));
                    candidates.push(git_root.join("usr/bin/bash.exe"));
                }
            }
        }
    }
    // 最后才使用 PATH 里的裸 bash.exe，避免优先命中 System32/WSL
    // launcher；前面的候选顺序与 CodeBuddy 2.138.0 的 Git Bash 发现逻辑一致。
    candidates.push(PathBuf::from("bash.exe"));
    candidates
}

fn run_helper_command(command: &str) -> Result<Output, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // 与 CodeBuddy 2.138.0 `normalizeWindowsCommandForPosixEval` 保持一致：
        // 在 Git Bash eval 前把 Windows 路径分隔符转为 `/`。
        let command = command.replace('\\', "/");
        for shell in windows_shell_candidates() {
            let mut process = Command::new(shell);
            process.creation_flags(0x0800_0000);
            match process.arg("-c").arg(&command).output() {
                Ok(output) => return Ok(output),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(helper_validation_error(
                        "启动阶段",
                        "无法启动 Git Bash shell",
                    ))
                }
            }
        }
        Err(helper_validation_error(
            "启动阶段",
        "未找到 Git Bash bash.exe",
        ))
    }
    #[cfg(not(windows))]
    {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|_| helper_validation_error("启动阶段", "无法启动 /bin/sh"))
    }
}

fn account_token(account: &Value) -> Result<&str, String> {
    account
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| auth_config_error(
            "账号阶段",
            "所选账号没有可用的认证信息，请先重新登录或刷新 Token",
        ))
}

fn settings_account_token(account: &Value) -> Result<&str, String> {
    let token = clean_bearer_token(account_token(account)?);
    if token.is_empty() {
        return Err(auth_config_error(
            "账号阶段",
            "所选账号没有可用的认证信息，请先重新登录或刷新 Token",
        ));
    }
    Ok(token)
}

fn validate_helper_for_account(command: &str, account: &Value) -> Result<(), String> {
    let token = account_token(account)?;
    let output = run_helper_command(command)?;
    validate_helper_output(&output, token)
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

fn account_index_by_token(accounts: &[Value], token: &str) -> Option<(usize, String)> {
    let expected = clean_bearer_token(token);
    if expected.is_empty() {
        return None;
    }
    accounts.iter().enumerate().find_map(|(index, account)| {
        let actual = account
            .get("access_token")
            .and_then(Value::as_str)
            .map(clean_bearer_token)?;
        if actual != expected {
            return None;
        }
        let id = account.get("id").and_then(Value::as_str)?.to_string();
        Some((index, id))
    })
}

/// Windows 静态认证下，把当前 CLI 账号刷新后的 token 同步到 settings。
/// 仅当 settings 当前 token 能匹配该账号（或状态明确指向该账号）时写入，
/// 避免后台刷新覆盖用户刚刚手动选择的其他账号；失败只返回脱敏错误。
pub fn sync_windows_env_for_account(
    account_value: &Value,
    previous_access_token: Option<&str>,
) -> Result<bool, String> {
    if !cfg!(windows) {
        return Ok(false);
    }
    ensure_no_process_env_override()?;
    let settings = settings_path();
    let Some(current_settings) = read_json_file(&settings) else {
        return Ok(false);
    };
    let Some(current_token) = settings_env_token(&current_settings) else {
        return Ok(false);
    };
    let accounts = account::load_accounts();
    let state = load_state();
    let Some((_, active_id)) = state_account_index(&state, &accounts) else {
        return Ok(false);
    };
    let account_id = account_value.get("id").and_then(Value::as_str);
    if account_id != Some(active_id.as_str()) {
        return Ok(false);
    }
    let Some(updated_token) = account_value.get("access_token").and_then(Value::as_str) else {
        return Ok(false);
    };
    // settings 必须仍是刷新前 token（或已同步的新 token）；否则视为用户已切换/手工修改。
    let current = clean_bearer_token(current_token);
    let previous_matches = previous_access_token
        .map(clean_bearer_token)
        .is_some_and(|token| token == current);
    let already_synced = clean_bearer_token(updated_token) == current;
    if !previous_matches && !already_synced {
        return Ok(false);
    }
    if already_synced {
        return Ok(true);
    }
    let (previous, value) = prepare_settings_env_update(&settings, updated_token)?;
    commit_settings_env_update(&settings, previous.as_deref(), &value, updated_token)?;
    Ok(true)
}

/// 返回脱敏的 CLI 轮换状态，不返回 token 或 helper 内容。
pub fn status() -> Value {
    let accounts = account::load_accounts();
    let state = load_state();
    let settings = read_json_file(&settings_path()).unwrap_or_else(|| json!({}));
    let env_token = settings_env_token(&settings);
    let env_configured = env_token.is_some();
    let environment_override = cfg!(windows) && process_env_token_present();
    let active = if cfg!(windows) {
        env_token.and_then(|token| account_index_by_token(&accounts, token))
    } else {
        state_account_index(&state, &accounts)
    };
    let expected_active = state_account_index(&state, &accounts);
    let configured = if cfg!(windows) {
        env_configured && !environment_override
    } else {
        helper_is_configured()
    };
    let migration_required = if cfg!(windows) {
        environment_override || (!env_configured && helper_is_configured())
    } else {
        helper_migration_required()
    };
    json!({
        "configured": configured,
        "authMode": if cfg!(windows) { "settings-env" } else { "api-key-helper" },
        "environmentOverride": environment_override,
        "helperCurrent": if cfg!(windows) { env_configured } else { helper_is_current() },
        "migrationRequired": migration_required,
        "syncPending": cfg!(windows) && env_configured && active.is_none() && expected_active.is_some(),
        "settingsPresent": settings_path().is_file(),
        "helperPresent": helper_path().map(|path| path.is_file()).unwrap_or(false),
        "helperSupportsAccountIds": helper_supports_account_ids(),
        "activeIndex": active.as_ref().map(|(index, _)| *index),
        "activeAccountId": active.as_ref().map(|(_, id)| id),
        "activeAccountName": active.and_then(|(_, id)| account::find_account(&id).map(|account| account::account_display_name(&account))),
        "accountCount": accounts.len(),
        "statePath": state_path().to_string_lossy(),
    })
}

fn install_env_auth() -> Result<Value, String> {
    ensure_no_process_env_override()?;
    let settings = settings_path();
    let accounts = account::load_accounts();
    let state = load_state();
    let active = state_account_index(&state, &accounts)
        .and_then(|(index, _)| accounts.get(index))
        .or_else(|| accounts.first())
        .ok_or_else(|| {
            auth_config_error("账号阶段", "当前没有可供 CodeBuddy CLI 使用的账号")
        })?;
    let token = settings_account_token(active)?;
    let (previous_settings, settings_value) = prepare_settings_env_update(&settings, token)?;
    commit_settings_env_update(
        &settings,
        previous_settings.as_deref(),
        &settings_value,
        token,
    )?;

    Ok(json!({
        "ok": true,
        "configured": true,
        "authMode": "settings-env",
        "helperPresent": helper_path().map(|path| path.is_file()).unwrap_or(false),
        "helperSupportsAccountIds": helper_supports_account_ids(),
        "verified": true,
        "message": "CodeBuddy CLI 认证配置已更新；当前运行会话不会切换，请由 ACP 重新加载会话或重启 CLI 后生效",
    }))
}

/// Windows 写入 settings env 认证；其他平台安装/升级项目提供的 helper。
/// 只有用户显式调用这个命令时才会修改用户级配置。
///
/// 兼容旧版：早期 helper 是 `helper.sh`（bash + python3），若当前配置的是
/// wb-switch 的旧 helper，允许直接原地升级，并清理旧文件。
pub fn install_helper() -> Result<Value, String> {
    if cfg!(windows) {
        return install_env_auth();
    }
    let target = rotate_dir().join(HELPER_FILE);
    let logic_target = rotate_dir().join(LOGIC_FILE);
    let legacy_target = rotate_dir().join(LEGACY_HELPER_FILE);
    let legacy_windows_target = rotate_dir().join(LEGACY_WINDOWS_HELPER_FILE);
    if let Some(current_command) = helper_command() {
        let Some(current) = command_path(&current_command) else {
            return Err("已有其他 CodeBuddy CLI apiKeyHelper 命令；请先确认后再替换".to_string());
        };
        if !same_path(&current, &target, cfg!(windows))
            && !same_path(&current, &legacy_target, cfg!(windows))
            && !same_path(&current, &legacy_windows_target, cfg!(windows))
        {
            return Err("已有其他 CodeBuddy CLI helper；请先确认后再替换".to_string());
        }
    }

    let settings = settings_path();
    let previous_settings = std::fs::read_to_string(&settings).ok();
    let previous_logic = std::fs::read_to_string(&logic_target).ok();
    let mut settings_value = if let Some(content) = previous_settings.as_ref() {
        serde_json::from_str(content).map_err(|_| "CodeBuddy settings.json 不是有效 JSON")?
    } else {
        json!({})
    };
    if !settings_value.is_object() {
        return Err("CodeBuddy settings.json 顶层不是对象".to_string());
    }

    std::fs::create_dir_all(rotate_dir()).map_err(|_| {
        helper_validation_error("安装阶段", "无法创建 helper 目录，请检查用户目录权限")
    })?;
    // 各平台都直接配置这份 helper.cjs。
    atomic_write(&logic_target, STANDARD_HELPER).map_err(|_| {
        helper_validation_error("安装阶段", "无法写入 helper.cjs，请检查文件权限")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(&logic_target, std::fs::Permissions::from_mode(0o700)).is_err()
        {
            restore_file(&logic_target, previous_logic.as_deref());
            return Err(helper_validation_error(
                "安装阶段",
                "无法设置 helper 执行权限",
            ));
        }
    }
    // CodeBuddy 2.138.0 会先将 apiKeyHelper 当作文件路径解析，然后在
    // Windows Git Bash eval 前将 `C:\\...` 归一化为 `C:/...`。这里必须
    // 保持为未引号的绝对路径，否则 CLI 会错将它解析到当前工作目录。
    let configured_target = match configured_helper_path(&target) {
        Ok(path) => path,
        Err(error) => {
            restore_file(&logic_target, previous_logic.as_deref());
            return Err(error);
        }
    };
    let configured_command = configured_target.to_string_lossy().to_string();
    settings_value["apiKeyHelper"] = json!(configured_command);
    let content = match serde_json::to_string_pretty(&settings_value) {
        Ok(content) => content,
        Err(_) => {
            restore_file(&logic_target, previous_logic.as_deref());
            return Err(helper_validation_error(
                "配置阶段",
                "无法生成 CodeBuddy settings.json",
            ));
        }
    };
    if let Some(parent) = settings.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            restore_file(&logic_target, previous_logic.as_deref());
            return Err(helper_validation_error(
                "配置阶段",
                "无法创建 CodeBuddy 配置目录，请检查用户目录权限",
            ));
        }
    }
    if atomic_write(&settings, &content).is_err() {
        restore_file(&logic_target, previous_logic.as_deref());
        return Err(helper_validation_error(
            "配置阶段",
            "无法写入 CodeBuddy settings.json，请检查文件权限",
        ));
    }

    let accounts = account::load_accounts();
    let state = load_state();
    let active = state_account_index(&state, &accounts)
        .and_then(|(index, _)| accounts.get(index))
        .ok_or_else(|| {
            helper_validation_error(
                "账号阶段",
                "当前没有可供 helper 验证的账号，请先添加账号",
            )
        });
    let validation =
        active.and_then(|account| validate_helper_for_account(&configured_command, account));
    if let Err(error) = validation {
        restore_file(&settings, previous_settings.as_deref());
        restore_file(&logic_target, previous_logic.as_deref());
        return Err(error);
    }

    // 验证通过后再清理旧版 helper.sh，避免失败时破坏旧配置。
    if legacy_target.exists() && legacy_target != target {
        let _ = std::fs::remove_file(&legacy_target);
    }
    if legacy_windows_target.exists() && legacy_windows_target != target {
        let _ = std::fs::remove_file(&legacy_windows_target);
    }

    Ok(json!({
        "ok": true,
        "configured": true,
        "authMode": "api-key-helper",
        "helperPresent": true,
        "helperSupportsAccountIds": true,
        "verified": true,
        "message": "CodeBuddy CLI 认证配置已更新；当前运行会话不会切换，请由 ACP 重新加载会话或重启 CLI 后生效",
    }))
}

/// 将 CodeBuddy CLI 的当前账号设置为 WorkBuddy 账号库中的目标账号。
pub fn set_active_account(account_id: &str) -> Result<Value, String> {
    if cfg!(windows) {
        ensure_no_process_env_override()?;
    }
    if !cfg!(windows) && helper_migration_required() {
        return Err(
            "检测到旧版 CodeBuddy CLI helper；请先在账号页升级 CLI helper"
                .to_string(),
        );
    }
    if !cfg!(windows) && !helper_is_configured() {
        return Err(
            "未检测到 CodeBuddy CLI apiKeyHelper；请先在 ~/.codebuddy/settings.json 配置轮换 helper"
                .to_string(),
        );
    }

    let accounts = account::load_accounts();
    let Some((index, canonical_id)) = account_index(&accounts, account_id) else {
        return Err("账号不存在".to_string());
    };

    // Windows 先验证并生成完整 settings，再写独立账号状态，避免无效 JSON、
    // 缺失 token 等前置错误造成只有 state.json 被修改的半成功。
    let windows_settings = if cfg!(windows) {
        let settings = settings_path();
        let token = settings_account_token(&accounts[index])?;
        let (previous, value) = prepare_settings_env_update(&settings, token)?;
        Some((settings, previous, value, token.to_string()))
    } else {
        None
    };

    let previous_state = std::fs::read_to_string(state_path()).ok();
    let mut state = load_state();
    state["active"] = json!(index);
    state["activeAccountId"] = json!(canonical_id);
    state["updatedAt"] = json!(now_ms());
    std::fs::create_dir_all(rotate_dir()).map_err(|_| {
        if cfg!(windows) {
            auth_config_error("状态阶段", "无法创建 CLI 账号状态目录，请检查用户目录权限")
        } else {
            helper_validation_error("状态阶段", "无法创建 helper 状态目录，请检查用户目录权限")
        }
    })?;
    let content = serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?;
    atomic_write(&state_path(), &content).map_err(|_| {
        if cfg!(windows) {
            auth_config_error("状态阶段", "无法写入所选 CLI 账号状态，请检查文件权限")
        } else {
            helper_validation_error("状态阶段", "无法写入所选账号状态，请检查文件权限")
        }
    })?;

    if let Some((settings, previous_settings, settings_value, token)) = windows_settings {
        if let Err(error) = commit_settings_env_update(
            &settings,
            previous_settings.as_deref(),
            &settings_value,
            &token,
        ) {
            restore_file(&state_path(), previous_state.as_deref());
            return Err(error);
        }
    } else {
        let command = helper_command().ok_or_else(|| {
            helper_validation_error("配置阶段", "apiKeyHelper 配置为空")
        })?;
        if let Err(error) = validate_helper_for_account(&command, &accounts[index]) {
            restore_file(&state_path(), previous_state.as_deref());
            return Err(error);
        }
    }

    Ok(json!({
        "ok": true,
        "configured": true,
        "synced": true,
        "verified": true,
        "authMode": if cfg!(windows) { "settings-env" } else { "api-key-helper" },
        "activeIndex": index,
        "activeAccountId": canonical_id,
        "message": if cfg!(windows) {
            "CodeBuddy CLI 默认账号已更新；当前运行会话不会切换，请由 ACP 重新加载会话或重启 CLI 后生效"
        } else {
            "CodeBuddy CLI 默认账号已更新；当前运行会话不会切换，请由 ACP 重新加载会话或重启 CLI 后生效"
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn helper_test_dir() -> PathBuf {
        std::env::temp_dir().join(format!("wb-switch-codebuddy-helper-{}", uuid::Uuid::new_v4()))
    }

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

    #[test]
    fn settings_env_token_preserves_helper_and_other_env_values() {
        let mut settings = json!({
            "apiKeyHelper": "C:/Users/tester/bin/wb-helper.bat",
            "trustedDirectories": ["C:/Users/tester"],
            "env": { "HTTPS_PROXY": "http://127.0.0.1:7890" }
        });
        write_settings_env_token(&mut settings, "RAW_SECRET").unwrap();
        assert_eq!(settings_env_token(&settings), Some("RAW_SECRET"));
        assert_eq!(
            settings["apiKeyHelper"],
            "C:/Users/tester/bin/wb-helper.bat"
        );
        assert_eq!(settings["env"]["HTTPS_PROXY"], "http://127.0.0.1:7890");
        assert_eq!(settings["env"][CODEBUDDY_AUTH_TOKEN], "RAW_SECRET");
    }

    #[test]
    fn settings_env_token_rejects_non_object_env() {
        let mut settings = json!({"env": "invalid"});
        let error = write_settings_env_token(&mut settings, "SECRET").unwrap_err();
        assert!(error.contains("env 字段不是对象"));
        assert!(!error.contains("SECRET"));
    }

    #[test]
    fn settings_env_file_update_roundtrips_and_preserves_existing_fields() {
        let test_dir = helper_test_dir();
        let settings = test_dir.join(".codebuddy").join("settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(
            &settings,
            r#"{
  "apiKeyHelper": "C:/Users/tester/bin/wb-helper.bat",
  "trustedDirectories": ["C:/Users/tester"],
  "env": { "HTTPS_PROXY": "http://127.0.0.1:7890" }
}"#,
        )
        .unwrap();

        let (previous, value) =
            prepare_settings_env_update(&settings, "Bearer RAW_SECRET").unwrap();
        commit_settings_env_update(&settings, previous.as_deref(), &value, "RAW_SECRET").unwrap();

        let persisted = read_json_file(&settings).unwrap();
        assert_eq!(settings_env_token(&persisted), Some("RAW_SECRET"));
        assert_eq!(persisted["apiKeyHelper"], "C:/Users/tester/bin/wb-helper.bat");
        assert_eq!(persisted["trustedDirectories"][0], "C:/Users/tester");
        assert_eq!(persisted["env"]["HTTPS_PROXY"], "http://127.0.0.1:7890");
        fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn windows_current_account_is_derived_from_persisted_token() {
        let accounts = vec![
            json!({"id": "a1", "access_token": "TOKEN_ONE"}),
            json!({"id": "a2", "access_token": "TOKEN_TWO"}),
        ];
        assert_eq!(
            account_index_by_token(&accounts, "Bearer TOKEN_TWO"),
            Some((1, "a2".to_string()))
        );
        assert_eq!(account_index_by_token(&accounts, "UNKNOWN"), None);
    }

    #[test]
    fn settings_account_token_rejects_empty_bearer_value() {
        let error = settings_account_token(&json!({"access_token": "Bearer "})).unwrap_err();
        assert!(error.contains("没有可用的认证信息"));
        assert!(!error.contains("Bearer"));
    }

    #[test]
    fn invalid_settings_file_is_not_overwritten_or_leaked() {
        let test_dir = helper_test_dir();
        let settings = test_dir.join("settings.json");
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(&settings, "not-json SECRET_ON_DISK").unwrap();

        let error = prepare_settings_env_update(&settings, "NEW_SECRET").unwrap_err();
        assert!(error.contains("不是有效 JSON"));
        assert!(!error.contains("NEW_SECRET"));
        assert!(!error.contains("SECRET_ON_DISK"));
        assert_eq!(fs::read_to_string(&settings).unwrap(), "not-json SECRET_ON_DISK");
        fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn recognizes_legacy_windows_path_and_direct_cjs_path() {
        assert_eq!(
            command_path(r"C:\Users\tester\.codebuddy-rotate\helper.cmd"),
            Some(PathBuf::from(
                r"C:\Users\tester\.codebuddy-rotate\helper.cmd"
            ))
        );
        assert_eq!(
            command_path(r"C:\Users\test user\.codebuddy-rotate\helper.cjs"),
            Some(PathBuf::from(
                r"C:\Users\test user\.codebuddy-rotate\helper.cjs"
            ))
        );
        assert!(command_path("node helper.cjs").is_none());
    }

    #[test]
    fn windows_paths_compare_case_and_separator_insensitively() {
        assert!(same_path(
            Path::new(r"C:\Users\Tester\.codebuddy-rotate\helper.cmd"),
            Path::new("c:/users/tester/.codebuddy-rotate/helper.cmd"),
            true,
        ));
    }

    #[test]
    fn windows_space_path_requires_a_space_free_short_path() {
        let original = Path::new(r"C:\Users\Test User\.codebuddy-rotate\helper.cjs");
        assert_eq!(
            select_windows_configured_path(
                original,
                Some(PathBuf::from(r"C:\Users\TESTUS~1\.codebuddy-rotate\helper.cjs")),
            )
            .unwrap(),
            PathBuf::from(r"C:\Users\TESTUS~1\.codebuddy-rotate\helper.cjs")
        );
        let error = select_windows_configured_path(original, None).unwrap_err();
        assert!(error.contains("配置阶段"));
        assert!(error.contains("shell 不安全字符"));
    }

    #[test]
    fn windows_shell_metacharacters_also_require_a_safe_short_path() {
        for path in [
            r"C:\Users\Test&User\.codebuddy-rotate\helper.cjs",
            r"C:\Users\Test(User)\.codebuddy-rotate\helper.cjs",
            r#"C:\Users\Test'User\.codebuddy-rotate\helper.cjs"#,
        ] {
            assert!(!path_is_posix_eval_safe(Path::new(path)));
            assert!(select_windows_configured_path(Path::new(path), None).is_err());
        }
        assert!(path_is_posix_eval_safe(Path::new(
            r"C:\Users\TESTUS~1\.codebuddy-rotate\helper.cjs"
        )));
    }

    #[test]
    fn legacy_windows_helper_requires_migration_even_when_logic_supports_ids() {
        let directory = Path::new(r"C:\Users\tester\.codebuddy-rotate");
        assert!(is_legacy_helper_path(
            &directory.join(LEGACY_WINDOWS_HELPER_FILE),
            directory,
            true,
        ));
        assert!(is_legacy_helper_path(
            &directory.join(LEGACY_HELPER_FILE),
            directory,
            true,
        ));
        assert!(!is_legacy_helper_path(
            &directory.join(HELPER_FILE),
            directory,
            true,
        ));
    }

    #[test]
    fn helper_validation_errors_never_include_stdout_or_token() {
        let secret = "SECRET_ACCESS_TOKEN";
        let error = validate_helper_result(true, Some(0), b"Bearer OTHER_SECRET\n", secret)
            .unwrap_err();
        assert!(error.contains("输出阶段"));
        assert!(!error.contains(secret));
        assert!(!error.contains("OTHER_SECRET"));

        let error = validate_helper_result(false, Some(127), b"Bearer LEAKED\n", secret)
            .unwrap_err();
        assert!(error.contains("退出码为 127"));
        assert!(!error.contains(secret));
        assert!(!error.contains("LEAKED"));
    }

    #[test]
    fn helper_selects_active_account_and_fails_without_selected_token() {
        let test_dir = helper_test_dir();
        let rotate_dir = test_dir.join("rotate");
        let accounts_file = test_dir.join("accounts.json");
        fs::create_dir_all(&rotate_dir).unwrap();
        fs::write(
            &accounts_file,
            serde_json::to_vec(&json!([
                {"id": "a1", "access_token": "SECRET_ONE"},
                {"id": "a2", "access_token": "SECRET_TWO"}
            ]))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            rotate_dir.join(STATE_FILE),
            serde_json::to_vec(&json!({"activeAccountId": "a2", "active": 0})).unwrap(),
        )
        .unwrap();

        let output = Command::new("node")
            .arg("-e")
            .arg(STANDARD_HELPER)
            .env("CODEBUDDY_ROTATE_DIR", &rotate_dir)
            .env("WB_SWITCH_ACCOUNTS_FILE", &accounts_file)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "Bearer SECRET_TWO\n");

        fs::write(
            &accounts_file,
            serde_json::to_vec(&json!([
                {"id": "a1", "access_token": "SECRET_ONE"},
                {"id": "a2"}
            ]))
            .unwrap(),
        )
        .unwrap();
        let output = Command::new("node")
            .arg("-e")
            .arg(STANDARD_HELPER)
            .env("CODEBUDDY_ROTATE_DIR", &rotate_dir)
            .env("WB_SWITCH_ACCOUNTS_FILE", &accounts_file)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("SECRET_ONE"));

        fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn restores_previous_file_after_failed_validation() {
        let test_dir = helper_test_dir();
        fs::create_dir_all(&test_dir).unwrap();
        let existing = test_dir.join("existing.json");
        fs::write(&existing, "old").unwrap();
        restore_file(&existing, Some("old"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "old");

        let newly_created = test_dir.join("new.json");
        fs::write(&newly_created, "temporary").unwrap();
        restore_file(&newly_created, None);
        assert!(!newly_created.exists());
        fs::remove_dir_all(test_dir).unwrap();
    }
}
