//! CodeBuddy CLI 账号轮换桥接。
//!
//! Windows 直接维护 `settings.json.env.CODEBUDDY_AUTH_TOKEN`，绕过 CLI
//! 执行 `apiKeyHelper` 时的路径兼容问题；macOS/Linux 继续使用 helper。
//! 两种模式都复用 wb-switch 的 WorkBuddy 账号库，但保持独立的当前账号。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;
// Instant 只在 macOS/Linux 的进程轮询路径里使用；Windows 上无条件导入会触发 unused_imports，
// 而 CI 的 setup-rust-toolchain 默认带 -D warnings，会直接编译失败。
#[cfg(not(target_os = "windows"))]
use std::time::Instant;

use crate::modules::account;
use crate::modules::config::{atomic_write, home_dir, now_ms};
use crate::modules::process;
use crate::modules::variant::WbVariant;

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
/// CLI 运行中会话注册表目录名（`~/.codebuddy/sessions/<pid>.json`）。
const SESSIONS_DIR_NAME: &str = "sessions";
/// 会话存活判据：`now - lastHeartbeat <= 该值` 即视为活着。
///
/// 对齐客户端 `WORKER_HEARTBEAT_TIMEOUT_MS`（120_000）。客户端心跳是 30s 一次的
/// 纯定时器（与是否正在对话无关），所以这个判据只回答「有没有活着的 CLI 进程」，
/// 不回答「用户是否正在用」——不要拿它当"活跃保护"用。
const LIVE_SESSION_STALE_MS: i64 = 120_000;
const CODEBUDDY_AUTH_TOKEN: &str = "CODEBUDDY_AUTH_TOKEN";
const CODEBUDDY_INTERNET_ENVIRONMENT: &str = "CODEBUDDY_INTERNET_ENVIRONMENT";
const CODEBUDDY_BASE_URL: &str = "CODEBUDDY_BASE_URL";
const CN_INTERNET_ENVIRONMENT: &str = "internal";
/// 官网 IAM：国际版「不设，或 public」。必须写成明确值，不能只删 key：
/// CLI 启动会把 local_storage 里的 Environment-Cache 写进 process.env，
/// settings.json 缺省时就会继续走国内站。
const AI_INTERNET_ENVIRONMENT: &str = "public";
const CN_CLI_ENDPOINT: &str = "https://copilot.tencent.com";
const AI_CLI_ENDPOINT: &str = "https://www.codebuddy.ai";
/// CLI `resolveModelBaseURL`：若设置了 `CODEBUDDY_BASE_URL`，会原样当作 OpenAI
/// client `baseURL`，**不会**再拼 `/v2`。写成门户根地址
/// `https://www.codebuddy.ai` 会 POST `/chat/completions` 到官网 nginx，返回
/// `405 Not Allowed`（nginx/1.27.3）。国际版 OpenAI 兼容接口是 `${endpoint}/v2`。
const AI_CLI_OPENAI_BASE_URL: &str = "https://www.codebuddy.ai/v2";
/// CodeBuddy CLI ProductManager 写入 `~/.codebuddy/local_storage/entry_<md5(key)>.info`
/// 的固定 key。切换档位时必须改这两份缓存，否则 settings.json 改了 CLI 仍打国内站。
const CLI_ENV_CACHE_KEY: &str = "CodeBuddy-Environment-Cache";
const CLI_ENDPOINT_CACHE_KEY: &str = "CodeBuddy-Endpoint-Cache";
const CLI_PRODUCT_CACHE_KEY: &str = "CodeBuddy-Product-Cache";
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

/// CLI 会话注册表目录（`~/.codebuddy/sessions`）。
///
/// **唯一**拼接点：轮换的存活门控与限额归因都从这里取路径，不要再各写一份
/// `home_dir().join(".codebuddy").join("sessions")`。
pub(crate) fn sessions_dir() -> PathBuf {
    home_dir().join(SETTINGS_DIR).join(SESSIONS_DIR_NAME)
}

/// 注册表文件名形态（对齐客户端 `PID_FILE_PATTERN`）：`<pid>.json` 或 `manual-*.json`。
///
/// 客户端还会在该目录放别的东西，只有这两种形态才是会话记录。
fn is_session_registry_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    if stem.chars().all(|character| character.is_ascii_digit()) && !stem.is_empty() {
        return true;
    }
    stem.strip_prefix("manual-")
        .is_some_and(|rest| !rest.is_empty())
}

/// 单份注册表是否心跳新鲜；缺 `lastHeartbeat` / 类型不符 → false（跳过）。
///
/// `lastHeartbeat` 在未来（时钟回拨、客户端时钟偏差）时 `now - hb` 为负，仍然算活着 ——
/// 保守侧：宁可不切，也不要关掉一个可能活着的进程。
fn heartbeat_is_live(value: &Value, now_ms: i64) -> bool {
    value
        .get("lastHeartbeat")
        .and_then(Value::as_i64)
        .is_some_and(|heartbeat| now_ms - heartbeat <= LIVE_SESSION_STALE_MS)
}

/// `dir` 下是否存在心跳新鲜的 CLI 会话；`now_ms` 由调用方给定（便于单测与复用同一时刻）。
///
/// - 只认 `^(\d+|manual-.+)\.json$`；坏 JSON / 缺 `lastHeartbeat` 的文件跳过；
/// - 目录不存在 → `false`（不是错误，未接入 CLI 的机器就是这种形态）；
/// - 目录存在但读不了 → `eprintln!` 告警后 `false`：**不能**因为读不到就永久卡住轮换，
///   真出现"旧进程还活着"时由归因侧的丢弃兜底。
///
/// 刻意**不**做 `kill(pid, 0)` / `ps` 探测：客户端自己会清理死进程的注册表文件，
/// 心跳过期已经足够，另行探测只会在跨平台用户权限差异上引入新的失败面。
fn has_live_session_in(dir: &Path, now_ms: i64) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "[codebuddy-cli] 无法读取会话注册表目录 {}：{error}；本次按无会话处理",
                dir.display()
            );
            return false;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_session_registry_file)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if heartbeat_is_live(&value, now_ms) {
            return true;
        }
    }
    false
}

/// 是否存在活着的 CodeBuddy CLI 会话（`~/.codebuddy/sessions/*.json`，心跳新鲜）。
///
/// 自动轮换的存活门控用这个判据：只要返回 true 就不切账号（活进程持的是旧 key，
/// 切了也不生效，还会破坏「活进程 key == 当前账号」的不变式）。
pub fn has_live_session(now_ms: i64) -> bool {
    has_live_session_in(&sessions_dir(), now_ms)
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

fn process_internet_environment() -> Option<String> {
    std::env::var(CODEBUDDY_INTERNET_ENVIRONMENT)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

fn ensure_region_env_compatible(variant: WbVariant) -> Result<(), String> {
    let Some(value) = process_internet_environment() else {
        return Ok(());
    };
    let is_cn_env =
        value.eq_ignore_ascii_case(CN_INTERNET_ENVIRONMENT) || value.eq_ignore_ascii_case("ioa");
    let is_ai_env = value.eq_ignore_ascii_case(AI_INTERNET_ENVIRONMENT)
        || value.eq_ignore_ascii_case("external");
    let conflict = match variant {
        WbVariant::Cn => !is_cn_env,
        WbVariant::Ai => is_cn_env || !(is_ai_env || value.is_empty()),
    };
    if conflict {
        return Err(auth_config_error(
            "环境阶段",
            "检测到进程环境变量 CODEBUDDY_INTERNET_ENVIRONMENT 与所选账号档位冲突；它会覆盖 settings.json，请先删除该用户或系统环境变量并重启应用与 CodeBuddy CLI",
        ));
    }
    Ok(())
}

fn env_object_mut(value: &mut Value) -> Result<&mut serde_json::Map<String, Value>, String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "CodeBuddy settings.json 顶层不是对象".to_string())?;
    let env = object.entry("env").or_insert_with(|| json!({}));
    env.as_object_mut()
        .ok_or_else(|| "CodeBuddy settings.json 的 env 字段不是对象".to_string())
}

fn apply_cli_region_env(value: &mut Value, variant: WbVariant) -> Result<(), String> {
    let env = env_object_mut(value)?;
    match variant {
        WbVariant::Cn => {
            env.insert(
                CODEBUDDY_INTERNET_ENVIRONMENT.to_string(),
                json!(CN_INTERNET_ENVIRONMENT),
            );
            env.remove(CODEBUDDY_BASE_URL);
        }
        WbVariant::Ai => {
            env.insert(
                CODEBUDDY_INTERNET_ENVIRONMENT.to_string(),
                json!(AI_INTERNET_ENVIRONMENT),
            );
            env.insert(
                CODEBUDDY_BASE_URL.to_string(),
                json!(AI_CLI_OPENAI_BASE_URL),
            );
        }
    }
    Ok(())
}

fn cli_local_storage_dir_for_settings(settings: &Path) -> PathBuf {
    settings.parent().unwrap_or(settings).join("local_storage")
}

fn cli_cache_filename(key: &str) -> &'static str {
    match key {
        CLI_ENV_CACHE_KEY => "entry_3bab4ce61838088127d444e4cc042d6d.info",
        CLI_ENDPOINT_CACHE_KEY => "entry_933d5543e80177622c17a73869c0fad7.info",
        CLI_PRODUCT_CACHE_KEY => "entry_604f48c944053e01d9546675443286c1.info",
        _ => "entry_unknown.info",
    }
}

fn write_cli_json_string_cache(path: &Path, value: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            auth_config_error(
                "配置阶段",
                "无法创建 CodeBuddy CLI 缓存目录，请检查用户目录权限",
            )
        })?;
    }
    let content = serde_json::to_string(&json!(value))
        .map_err(|_| auth_config_error("配置阶段", "无法生成 CodeBuddy CLI 缓存"))?;
    atomic_write(path, &content)
        .map_err(|_| auth_config_error("配置阶段", "无法写入 CodeBuddy CLI 缓存，请检查文件权限"))
}

fn sync_cli_runtime_cache_at(storage_dir: &Path, variant: WbVariant) -> Result<(), String> {
    let env_path = storage_dir.join(cli_cache_filename(CLI_ENV_CACHE_KEY));
    let endpoint_path = storage_dir.join(cli_cache_filename(CLI_ENDPOINT_CACHE_KEY));
    let product_path = storage_dir.join(cli_cache_filename(CLI_PRODUCT_CACHE_KEY));
    // 产品包缓存按环境选 product.internal.json / product.json；切档位必须丢掉。
    let _ = std::fs::remove_file(&product_path);
    match variant {
        WbVariant::Cn => {
            write_cli_json_string_cache(&env_path, CN_INTERNET_ENVIRONMENT)?;
            write_cli_json_string_cache(&endpoint_path, CN_CLI_ENDPOINT)?;
        }
        WbVariant::Ai => {
            write_cli_json_string_cache(&env_path, AI_INTERNET_ENVIRONMENT)?;
            write_cli_json_string_cache(&endpoint_path, AI_CLI_ENDPOINT)?;
        }
    }
    Ok(())
}

fn sync_cli_runtime_cache(settings: &Path, variant: WbVariant) -> Result<(), String> {
    sync_cli_runtime_cache_at(&cli_local_storage_dir_for_settings(settings), variant)
}

/// 只认 CodeBuddy CLI / prewarm 的包路径，排除 IDE（`.app` / `Programs\CodeBuddy`）
/// 和本工具。禁止用 `codebuddy` 单字去匹配。
///
/// 只在 macOS 的 `list_codebuddy_cli_pids` 与 Linux 的 `/proc` 扫描路径里使用 ——
/// Windows 走 PowerShell 查询，不需要它。Windows 的非测试构建会因此报 dead_code，
/// 而 CI 的 setup-rust-toolchain 默认带 `-D warnings`，故显式放行（保留测试可用）。
#[cfg_attr(all(target_os = "windows", not(test)), allow(dead_code))]
fn is_codebuddy_cli_process_args(args: &str) -> bool {
    let lower = args.to_ascii_lowercase();
    if lower.contains("wb-switch") || lower.contains("workbuddy-switch") {
        return false;
    }
    if lower.contains(".app/contents/") {
        return false;
    }
    if lower.contains("codebuddy cn") || lower.contains("codebuddycn") {
        return false;
    }
    if lower.contains("\\programs\\codebuddy\\") || lower.contains("/programs/codebuddy/") {
        return false;
    }
    lower.contains("@tencent-ai/codebuddy-code")
        || lower.contains("codebuddy-code/dist-server")
        || lower.contains("codebuddy-code\\dist-server")
        || lower.contains("codebuddy-code/bin/")
        || lower.contains("codebuddy-code\\bin\\")
        || lower.contains("/bin/codebuddy")
        || lower.contains("\\bin\\codebuddy")
        || lower.contains("codebuddy.cmd")
        || lower.contains("cbc-prewarm")
}

fn current_cli_variant(accounts: &[Value], state: &Value) -> Option<WbVariant> {
    let active = if cfg!(windows) {
        read_json_file(&settings_path())
            .as_ref()
            .and_then(settings_env_token)
            .and_then(|token| account_index_by_token(accounts, token))
    } else {
        state_account_index(state, accounts)
    }?;
    accounts.get(active.0).map(WbVariant::from_account)
}

fn list_codebuddy_cli_pids() -> Vec<u32> {
    let self_pid = std::process::id();
    #[cfg(target_os = "macos")]
    {
        let patterns = [
            "@tencent-ai/codebuddy-code".to_string(),
            "cbc-prewarm".to_string(),
            "/bin/codebuddy".to_string(),
        ];
        process::macos_rows_by_patterns(&patterns)
            .into_iter()
            .filter(|(pid, args)| *pid != self_pid && is_codebuddy_cli_process_args(args))
            .map(|(pid, _)| pid)
            .collect()
    }
    #[cfg(target_os = "windows")]
    {
        let script = "Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
             Where-Object { \
               $_.CommandLine -and ( \
                 $_.CommandLine -like '*@tencent-ai/codebuddy-code*' -or \
                 $_.CommandLine -like '*codebuddy-code*dist-server*' -or \
                 $_.CommandLine -like '*\\bin\\codebuddy*' -or \
                 $_.CommandLine -like '*cbc-prewarm*' -or \
                 $_.CommandLine -like '*codebuddy.cmd*' \
               ) \
             } | ForEach-Object { $_.ProcessId }";
        let Some(stdout) = process::ps_output(script, 5) else {
            return Vec::new();
        };
        return stdout
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .filter(|pid| *pid != self_pid)
            .collect();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut pids = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return pids;
        };
        for entry in entries.flatten() {
            let pid: u32 = match entry.file_name().to_string_lossy().parse() {
                Ok(pid) => pid,
                Err(_) => continue,
            };
            if pid == self_pid {
                continue;
            }
            let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(bytes) if !bytes.is_empty() => {
                    String::from_utf8_lossy(&bytes).replace('\0', " ")
                }
                _ => continue,
            };
            if is_codebuddy_cli_process_args(&cmdline) {
                pids.push(pid);
            }
        }
        pids
    }
}

fn terminate_codebuddy_cli_pids(pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let owned: Vec<String> = std::iter::once("-15".to_string())
            .chain(pids.iter().map(u32::to_string))
            .collect();
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let _ = process::run_cmd_timeout("kill", &args, 10);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        process::kill_macos_pids(pids);
    }
    #[cfg(target_os = "windows")]
    {
        for pid in pids {
            let pid_s = pid.to_string();
            let _ = process::run_cmd_timeout("taskkill", &["/PID", &pid_s, "/T"], 10);
        }
        let remaining = process::wait_windows_pids_gone(pids, Duration::from_secs(3));
        for pid in remaining {
            let pid_s = pid.to_string();
            let _ = process::run_cmd_timeout("taskkill", &["/PID", &pid_s, "/T", "/F"], 10);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let term: Vec<String> = std::iter::once("-15".to_string())
            .chain(pids.iter().map(u32::to_string))
            .collect();
        let args: Vec<&str> = term.iter().map(String::as_str).collect();
        let _ = process::run_cmd_timeout("kill", &args, 10);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let alive: Vec<u32> = pids
                .iter()
                .copied()
                .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
                .collect();
            if alive.is_empty() || Instant::now() >= deadline {
                if !alive.is_empty() {
                    let kill: Vec<String> = std::iter::once("-9".to_string())
                        .chain(alive.iter().map(u32::to_string))
                        .collect();
                    let args: Vec<&str> = kill.iter().map(String::as_str).collect();
                    let _ = process::run_cmd_timeout("kill", &args, 10);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn close_running_codebuddy_cli() -> (bool, usize) {
    let pids = list_codebuddy_cli_pids();
    if pids.is_empty() {
        return (false, 0);
    }
    terminate_codebuddy_cli_pids(&pids);
    (true, pids.len())
}

/// 切换结果文案（唯一构造点）：**始终**说明是否关闭了正在运行的 CLI。
///
/// 有进程 → 报出关闭数量（用户需要知道当前会话被打断）；
/// 无进程 → 明说"未发现"，避免用户以为提示语是模板敷衍。
fn region_switch_message(variant: WbVariant, region_changed: bool, closed_count: usize) -> String {
    let region = if variant == WbVariant::Ai {
        "国际版"
    } else {
        "国内版"
    };
    match (region_changed, closed_count) {
        (true, 0) => format!("已切换到{region}。未发现正在运行的 CodeBuddy CLI，新开会话即可"),
        (true, closed_count) => format!(
            "已切换到{region}并关闭正在运行的 CodeBuddy CLI（{closed_count} 个进程）。请重新打开 CLI 后再发会话"
        ),
        (false, 0) => {
            "CodeBuddy CLI 默认账号已更新。未发现正在运行的 CodeBuddy CLI，新开会话即可".to_string()
        }
        (false, closed_count) => format!(
            "CodeBuddy CLI 默认账号已更新，并关闭正在运行的 CodeBuddy CLI（{closed_count} 个进程）。请重新打开 CLI 后再发会话"
        ),
    }
}

/// 关闭进程之后发生的失败：CLI 已经退出，但账号没切成功。
///
/// 关闭不可回滚，所以错误必须显式说明"我已经把你的 CLI 关了"——否则用户只会看到
/// "写文件失败"，然后奇怪为什么终端里的会话掉了。无进程可关时不追加这句。
fn after_close_error(error: String, closed_count: usize) -> String {
    if closed_count == 0 {
        return error;
    }
    format!("{error}；已关闭 {closed_count} 个正在运行的 CodeBuddy CLI，但账号未切换成功，请重试")
}

/// 切换的写入阶段：**先关闭正在运行的 CLI，再写 `state.json`**。
///
/// 顺序不可互换——先写 state 会留下「新 state + 旧进程仍活」的窗口：旧进程仍持旧 key，
/// 还可能把旧站点缓存写回去。关闭成功与否都不阻断切换（关不掉就只告警计数）。
///
/// 抽成函数只为可测：单测注入 `close` 回调与临时路径，断言关闭发生在写入之前。
fn close_then_write_state<F>(
    state_file: &Path,
    content: &str,
    close: F,
) -> Result<(bool, usize), String>
where
    F: FnOnce() -> (bool, usize),
{
    let (closed, closed_count) = close();
    atomic_write(state_file, content)
        .map(|_| (closed, closed_count))
        .map_err(|_| {
            after_close_error(
                if cfg!(windows) {
                    auth_config_error("状态阶段", "无法写入所选 CLI 账号状态，请检查文件权限")
                } else {
                    helper_validation_error("状态阶段", "无法写入所选账号状态，请检查文件权限")
                },
                closed_count,
            )
        })
}

fn write_settings_env_token(value: &mut Value, token: &str) -> Result<(), String> {
    let env = env_object_mut(value)?;
    env.insert(CODEBUDDY_AUTH_TOKEN.to_string(), json!(token));
    Ok(())
}

fn persist_settings_at(settings: &Path, value: &Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(value)
        .map_err(|_| auth_config_error("配置阶段", "无法生成 CodeBuddy settings.json"))?;
    if let Some(parent) = settings.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            auth_config_error(
                "配置阶段",
                "无法创建 CodeBuddy 配置目录，请检查用户目录权限",
            )
        })?;
    }
    atomic_write(settings, &content).map_err(|_| {
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
    variant: WbVariant,
) -> Result<(Option<String>, Value), String> {
    let previous = std::fs::read_to_string(settings).ok();
    let mut value = previous
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "CodeBuddy settings.json 不是有效 JSON")?
        .unwrap_or_else(|| json!({}));
    write_settings_env_token(&mut value, clean_bearer_token(token))?;
    apply_cli_region_env(&mut value, variant)?;
    Ok((previous, value))
}

fn persist_cli_region_env(variant: WbVariant) -> Result<(), String> {
    let settings = settings_path();
    let previous = std::fs::read_to_string(&settings).ok();
    let mut value = previous
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "CodeBuddy settings.json 不是有效 JSON")?
        .unwrap_or_else(|| json!({}));
    apply_cli_region_env(&mut value, variant)?;
    persist_settings_at(&settings, &value).inspect_err(|_| {
        restore_file(&settings, previous.as_deref());
    })?;
    if let Err(error) = sync_cli_runtime_cache(&settings, variant) {
        restore_file(&settings, previous.as_deref());
        return Err(error);
    }
    Ok(())
}

fn commit_settings_env_update(
    settings: &PathBuf,
    previous: Option<&str>,
    value: &Value,
    expected_token: &str,
    variant: WbVariant,
) -> Result<(), String> {
    if let Err(error) = persist_settings_at(settings, value)
        .and_then(|_| validate_persisted_env_token_at(settings, expected_token))
        .and_then(|_| sync_cli_runtime_cache(settings, variant))
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
    same_path(path, &directory.join(LEGACY_WINDOWS_HELPER_FILE), windows)
        || same_path(path, &directory.join(LEGACY_HELPER_FILE), windows)
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
    #[cfg(windows)]
    let hint = "请确认 Git Bash 和 Node.js 可用，然后重试；如仍失败，请查看 CodeBuddy CLI 日志";
    #[cfg(not(windows))]
    let hint = "请确认 Node.js 可用，然后重试；如仍失败，请查看 CodeBuddy CLI 日志";
    format!("CodeBuddy CLI helper 验证失败（{stage}）：{cause}。{hint}")
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
        return Err(helper_validation_error("输出阶段", "helper 未返回认证结果"));
    }
    if stdout.trim() != format!("Bearer {expected_token}") {
        return Err(helper_validation_error(
            "输出阶段",
            "helper 返回的认证结果与所选账号不一致",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn node_path_from_shell_output(stdout: &[u8]) -> Option<PathBuf> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find_map(|line| {
            let path = PathBuf::from(line.trim());
            (path.is_absolute() && path.file_name().is_some_and(|name| name == "node"))
                .then_some(path)
        })
}

#[cfg(target_os = "macos")]
fn macos_node_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("node"),
        PathBuf::from("/opt/homebrew/bin/node"),
        PathBuf::from("/usr/local/bin/node"),
    ];
    // Finder/LaunchServices 不会继承终端（尤其是 nvm）注入的 PATH。
    // 这里只让用户的登录 shell 定位 node，helper 本身仍由 Rust 直接执行，
    // 避免 .zshrc 的欢迎语等输出污染 helper 的 stdout。
    if let Ok(output) = Command::new("/bin/zsh")
        .args(["-lic", "command -v node"])
        .output()
    {
        if let Some(path) = node_path_from_shell_output(&output.stdout) {
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }
    candidates
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
                    ));
                }
            }
        }
        Err(helper_validation_error(
            "启动阶段",
            "未找到 Git Bash bash.exe",
        ))
    }
    #[cfg(target_os = "macos")]
    {
        for node in macos_node_candidates() {
            match Command::new(node).arg(command).output() {
                Ok(output) => return Ok(output),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(helper_validation_error(
                        "启动阶段",
                        "无法使用 Node.js 执行 helper",
                    ));
                }
            }
        }
        Err(helper_validation_error(
            "启动阶段",
            "未找到可用的 Node.js；如通过 nvm 安装，请确认登录 shell 能执行 node",
        ))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
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
        .ok_or_else(|| {
            auth_config_error(
                "账号阶段",
                "所选账号没有可用的认证信息，请先重新登录或刷新 Token",
            )
        })
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
    let (previous, value) = prepare_settings_env_update(
        &settings,
        updated_token,
        WbVariant::from_account(account_value),
    )?;
    commit_settings_env_update(
        &settings,
        previous.as_deref(),
        &value,
        updated_token,
        WbVariant::from_account(account_value),
    )?;
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
        "activeAccountName": active.as_ref().and_then(|(_, id)| account::find_account(id).map(|account| account::account_display_name(&account))),
        "activeAccountVariant": active.as_ref().and_then(|(index, _)| accounts.get(*index)).map(WbVariant::from_account).map(WbVariant::as_str),
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
        .ok_or_else(|| auth_config_error("账号阶段", "当前没有可供 CodeBuddy CLI 使用的账号"))?;
    ensure_region_env_compatible(WbVariant::from_account(active))?;
    let token = settings_account_token(active)?;
    let (previous_settings, settings_value) =
        prepare_settings_env_update(&settings, token, WbVariant::from_account(active))?;
    commit_settings_env_update(
        &settings,
        previous_settings.as_deref(),
        &settings_value,
        token,
        WbVariant::from_account(active),
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
    atomic_write(&logic_target, STANDARD_HELPER)
        .map_err(|_| helper_validation_error("安装阶段", "无法写入 helper.cjs，请检查文件权限"))?;
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
    let active_account =
        state_account_index(&state, &accounts).and_then(|(index, _)| accounts.get(index));
    let variant = active_account.map(WbVariant::from_account);
    let validation = active_account
        .ok_or_else(|| {
            helper_validation_error("账号阶段", "当前没有可供 helper 验证的账号，请先添加账号")
        })
        .and_then(|account| validate_helper_for_account(&configured_command, account));
    if let Err(error) = validation {
        restore_file(&settings, previous_settings.as_deref());
        restore_file(&logic_target, previous_logic.as_deref());
        return Err(error);
    }
    if let Some(variant) = variant {
        if let Err(error) = persist_cli_region_env(variant) {
            restore_file(&settings, previous_settings.as_deref());
            restore_file(&logic_target, previous_logic.as_deref());
            return Err(error);
        }
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

/// 把 CodeBuddy CLI 的当前账号设为账号库中的目标账号（手动切换与自动轮换的唯一入口）。
///
/// 顺序固定：**前置校验 → 关闭正在运行的 CLI → 写 `state.json` → 校验 helper/区域环境**。
/// 先关后写是为了维持不变式「活着的 CLI 进程持有的 key == `state.json` 的 activeAccountId」：
/// key 是进程级快照，先写 state 就会留下「新 state + 旧进程仍活」的窗口。
///
/// 两个细节：
/// - 关闭失败 / 没有进程都不阻断切换（只影响返回文案里的数量）；
/// - 关闭**不可回滚**：后续任一阶段失败时，错误里都会说明 CLI 已被关闭。
pub fn switch_active_account(account_id: &str) -> Result<Value, String> {
    if cfg!(windows) {
        ensure_no_process_env_override()?;
    }
    if !cfg!(windows) && helper_migration_required() {
        return Err("检测到旧版 CodeBuddy CLI helper；请先在账号页升级 CLI helper".to_string());
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
    let variant = WbVariant::from_account(&accounts[index]);
    ensure_region_env_compatible(variant)?;

    // Windows 先验证并生成完整 settings 值（纯内存，不落盘），避免无效 JSON、
    // 缺失 token 等前置错误造成"CLI 已关、账号没切"。
    let windows_settings = if cfg!(windows) {
        let settings = settings_path();
        let token = settings_account_token(&accounts[index])?;
        let (previous, value) = prepare_settings_env_update(&settings, token, variant)?;
        Some((settings, previous, value, token.to_string()))
    } else {
        None
    };

    // 目录先建好：写 state 是"关进程之后"的事，那一段里不再留失败点。
    std::fs::create_dir_all(rotate_dir()).map_err(|_| {
        if cfg!(windows) {
            auth_config_error("状态阶段", "无法创建 CLI 账号状态目录，请检查用户目录权限")
        } else {
            helper_validation_error("状态阶段", "无法创建 helper 状态目录，请检查用户目录权限")
        }
    })?;

    let previous_state = std::fs::read_to_string(state_path()).ok();
    let switched_at = now_ms();
    let mut state = load_state();
    let previous_variant = current_cli_variant(&accounts, &state);
    state["active"] = json!(index);
    state["activeAccountId"] = json!(canonical_id);
    state["updatedAt"] = json!(switched_at);
    let content = serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?;
    // 顺序关键点：关闭在前、写 state 在后（用例固定这个顺序）。
    let (cli_closed, closed_count) =
        close_then_write_state(&state_path(), &content, close_running_codebuddy_cli)?;

    if let Some((settings, previous_settings, settings_value, token)) = windows_settings {
        if let Err(error) = commit_settings_env_update(
            &settings,
            previous_settings.as_deref(),
            &settings_value,
            &token,
            variant,
        ) {
            restore_file(&state_path(), previous_state.as_deref());
            return Err(after_close_error(error, closed_count));
        }
    } else {
        // helper 校验必须在 state 写完之后：helper 是照 `state.json` 选 token 的，
        // 先校验等于拿旧账号去比对新账号。
        let validation = helper_command()
            .ok_or_else(|| helper_validation_error("配置阶段", "apiKeyHelper 配置为空"))
            .and_then(|command| validate_helper_for_account(&command, &accounts[index]))
            .and_then(|_| persist_cli_region_env(variant));
        if let Err(error) = validation {
            restore_file(&state_path(), previous_state.as_deref());
            return Err(after_close_error(error, closed_count));
        }
    }

    let region_changed = previous_variant != Some(variant);

    Ok(json!({
        "ok": true,
        "configured": true,
        "synced": true,
        "verified": true,
        "authMode": if cfg!(windows) { "settings-env" } else { "api-key-helper" },
        "activeIndex": index,
        "activeAccountId": canonical_id,
        "regionChanged": region_changed,
        "cliClosed": cli_closed,
        "closedProcessCount": closed_count,
        "message": region_switch_message(variant, region_changed, closed_count),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn helper_test_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb-switch-codebuddy-helper-{}",
            uuid::Uuid::new_v4()
        ))
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

    // -----------------------------------------------------------------------
    // 会话存活判据（`has_live_session`）
    // -----------------------------------------------------------------------

    /// 一个临时 sessions 目录；用例结束后删除 `parent()` 之外的兄弟文件。
    fn sessions_fixture() -> (PathBuf, PathBuf) {
        let root = helper_test_dir();
        let dir = root.join("sessions");
        fs::create_dir_all(&dir).unwrap();
        (root, dir)
    }

    /// 写一份注册表文件（`<pid>.json` / `manual-*.json` / 其它名字都由 `name` 决定）。
    fn write_session_file(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    /// 心跳为 `heartbeat` 的注册表内容（字段与客户端形态一致）。
    fn session_json(heartbeat: i64) -> String {
        json!({
            "pid": 9312,
            "sessionId": "01a0aeb3-1f70-7d85-b356-0faa85650c1f",
            "startedAt": heartbeat,
            "lastHeartbeat": heartbeat,
        })
        .to_string()
    }

    #[test]
    fn sessions_dir_is_the_single_join_point_under_codebuddy_home() {
        let dir = sessions_dir();
        assert_eq!(
            dir.file_name().and_then(|name| name.to_str()),
            Some(SESSIONS_DIR_NAME)
        );
        assert_eq!(
            dir.parent().and_then(|parent| parent.file_name()),
            Some(std::ffi::OsStr::new(SETTINGS_DIR))
        );
        assert_eq!(
            dir.parent().and_then(|parent| parent.parent()),
            Some(crate::modules::config::home_dir().as_path())
        );
    }

    #[test]
    fn fresh_heartbeat_marks_the_session_alive_and_stale_one_does_not() {
        let (root, dir) = sessions_fixture();
        let now = 1_800_000_000_000;

        // 恰好等于阈值：仍算活着（`<=` 与客户端超时判定一致）。
        write_session_file(
            &dir,
            "9312.json",
            &session_json(now - LIVE_SESSION_STALE_MS),
        );
        assert!(has_live_session_in(&dir, now));

        // 超过阈值 1ms：过期。
        write_session_file(
            &dir,
            "9312.json",
            &session_json(now - LIVE_SESSION_STALE_MS - 1),
        );
        assert!(!has_live_session_in(&dir, now));

        // 未来心跳（时钟回拨 / 客户端时钟偏差）：保守视为活着。
        write_session_file(&dir, "9312.json", &session_json(now + 5 * 60_000));
        assert!(has_live_session_in(&dir, now));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn manual_session_files_count_and_other_names_are_ignored() {
        let (root, dir) = sessions_fixture();
        let now = 1_800_000_000_000;
        let fresh = now - 1_000;

        // `manual-<name>.json` 是客户端另一种合法形态。
        write_session_file(&dir, "manual-review.json", &session_json(fresh));
        assert!(has_live_session_in(&dir, now));
        fs::remove_file(dir.join("manual-review.json")).unwrap();

        // 其它形态都不认（含空 stem、缺 `-` 后缀、非 .json 扩展名）。
        for name in [
            "session.json",
            "manual-.json",
            ".json",
            "9312.json.bak",
            "9312",
            "notes.txt",
        ] {
            write_session_file(&dir, name, &session_json(fresh));
        }
        assert!(!has_live_session_in(&dir, now));
        // 认形态但内容坏 / 缺字段 → 跳过，不阻塞其它文件。
        write_session_file(&dir, "9313.json", "not-json");
        write_session_file(&dir, "9314.json", &json!({"pid": 9314}).to_string());
        assert!(!has_live_session_in(&dir, now));
        // 同目录里加一份新鲜的心跳文件 → 活着。
        write_session_file(&dir, "9315.json", &session_json(fresh));
        assert!(has_live_session_in(&dir, now));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_sessions_dir_is_not_an_error() {
        let (root, dir) = sessions_fixture();
        let missing = root.join("missing-sessions");
        assert!(!has_live_session_in(&missing, 1_800_000_000_000));
        assert!(!missing.exists(), "不得因为探测而创建目录");
        // 路径存在但不是目录（例如同名文件）同样按无会话处理。
        write_session_file(&root, "not-a-dir", "x");
        assert!(!has_live_session_in(&root.join("not-a-dir"), 1));
        assert!(!has_live_session_in(&dir, 1_800_000_000_000));
        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_sessions_dir_is_reported_as_no_session() {
        use std::os::unix::fs::PermissionsExt;

        let (root, dir) = sessions_fixture();
        write_session_file(&dir, "9312.json", &session_json(1_800_000_000_000));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        // root 不受权限位约束（CI 容器常见）：此时构造不出该场景，跳过而不是假失败。
        if std::fs::read_dir(&dir).is_ok() {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            fs::remove_dir_all(root).ok();
            return;
        }
        assert!(!has_live_session_in(&dir, 1_800_000_000_000));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(root).ok();
    }

    // -----------------------------------------------------------------------
    // 切换顺序：先关进程，再写 state.json
    // -----------------------------------------------------------------------

    #[test]
    fn state_is_written_only_after_the_cli_is_closed() {
        let root = helper_test_dir();
        fs::create_dir_all(&root).unwrap();
        let state_file = root.join("state.json");
        let order = std::cell::RefCell::new(Vec::new());

        let (closed, count) = close_then_write_state(&state_file, "{\"active\":1}", || {
            order.borrow_mut().push("close");
            // 关闭的瞬间不能已经存在新 state：否则旧进程会读到新账号。
            assert!(!state_file.exists(), "写 state 必须发生在关闭进程之后");
            (true, 2)
        })
        .expect("切换写入");
        order.borrow_mut().push("write");

        assert_eq!(*order.borrow(), vec!["close", "write"]);
        assert!(closed);
        assert_eq!(count, 2);
        assert_eq!(fs::read_to_string(&state_file).unwrap(), "{\"active\":1}");

        // 生产形态：`state.json` 在上一次切换时就已经存在。此时"没写出新内容"才是顺序证据 ——
        // 若先写后关，关闭的回调里读到的会是新内容（或至少不再是旧内容）。
        fs::write(&state_file, "{\"active\":0}").unwrap();
        let (_, count) = close_then_write_state(&state_file, "{\"active\":2}", || {
            assert_eq!(
                fs::read_to_string(&state_file).unwrap(),
                "{\"active\":0}",
                "关闭进程时 state.json 必须还是旧内容"
            );
            (true, 1)
        })
        .expect("切换写入");
        assert_eq!(count, 1);
        assert_eq!(fs::read_to_string(&state_file).unwrap(), "{\"active\":2}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_write_failure_reports_the_already_closed_cli() {
        let root = helper_test_dir();
        fs::create_dir_all(&root).unwrap();
        // 父路径是普通文件 → 写入必然失败，且失败发生在关闭之后。
        fs::write(root.join("blocker"), "x").unwrap();
        let state_file = root.join("blocker").join("state.json");

        let error = close_then_write_state(&state_file, "{}", || (true, 3)).unwrap_err();
        assert!(
            error.contains("已关闭 3 个"),
            "必须说明 CLI 已被关闭: {error}"
        );

        // 没有关到进程时不追加那句话（错误保持原样）。
        let error = close_then_write_state(&state_file, "{}", || (false, 0)).unwrap_err();
        assert!(!error.contains("已关闭"), "无进程可关时不追加: {error}");
        fs::remove_dir_all(root).unwrap();
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
            prepare_settings_env_update(&settings, "Bearer RAW_SECRET", WbVariant::Cn).unwrap();
        commit_settings_env_update(
            &settings,
            previous.as_deref(),
            &value,
            "RAW_SECRET",
            WbVariant::Cn,
        )
        .unwrap();

        let persisted = read_json_file(&settings).unwrap();
        assert_eq!(settings_env_token(&persisted), Some("RAW_SECRET"));
        assert_eq!(
            persisted["apiKeyHelper"],
            "C:/Users/tester/bin/wb-helper.bat"
        );
        assert_eq!(persisted["trustedDirectories"][0], "C:/Users/tester");
        assert_eq!(persisted["env"]["HTTPS_PROXY"], "http://127.0.0.1:7890");
        assert_eq!(
            persisted["env"][CODEBUDDY_INTERNET_ENVIRONMENT],
            CN_INTERNET_ENVIRONMENT
        );
        fs::remove_dir_all(test_dir).unwrap();
    }

    #[test]
    fn codebuddy_cli_process_args_match_package_and_skip_ide() {
        assert!(is_codebuddy_cli_process_args(
            "node /Users/me/.nvm/versions/node/v22.20.0/lib/node_modules/@tencent-ai/codebuddy-code/dist-server/codebuddy.js"
        ));
        assert!(is_codebuddy_cli_process_args(
            "node C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\@tencent-ai\\codebuddy-code\\bin\\codebuddy"
        ));
        assert!(is_codebuddy_cli_process_args(
            "/Users/me/.nvm/versions/node/v22.20.0/lib/node_modules/@tencent-ai/codebuddy-code/bin/cbc-prewarm"
        ));
        assert!(is_codebuddy_cli_process_args(
            "node /Users/me/.nvm/versions/node/v22.20.0/bin/codebuddy --acp"
        ));
        assert!(!is_codebuddy_cli_process_args(
            "/Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy"
        ));
        assert!(!is_codebuddy_cli_process_args(
            "/Applications/CodeBuddy CN.app/Contents/MacOS/CodeBuddy CN"
        ));
        assert!(!is_codebuddy_cli_process_args(
            r"C:\Users\me\AppData\Local\Programs\CodeBuddy\CodeBuddy.exe"
        ));
        assert!(!is_codebuddy_cli_process_args(
            "node /Users/me/Documents/github-project/wb-switch/src-tauri"
        ));
    }

    #[test]
    fn apply_cli_region_env_writes_internal_for_cn_and_public_for_ai() {
        let mut settings = json!({
            "env": {
                "HTTPS_PROXY": "http://127.0.0.1:7890",
                CODEBUDDY_INTERNET_ENVIRONMENT: "internal",
                CODEBUDDY_BASE_URL: AI_CLI_ENDPOINT
            }
        });
        apply_cli_region_env(&mut settings, WbVariant::Ai).unwrap();
        assert_eq!(
            settings["env"][CODEBUDDY_INTERNET_ENVIRONMENT],
            AI_INTERNET_ENVIRONMENT
        );
        assert_eq!(settings["env"][CODEBUDDY_BASE_URL], AI_CLI_OPENAI_BASE_URL);
        assert_eq!(settings["env"]["HTTPS_PROXY"], "http://127.0.0.1:7890");
        apply_cli_region_env(&mut settings, WbVariant::Cn).unwrap();
        assert_eq!(
            settings["env"][CODEBUDDY_INTERNET_ENVIRONMENT],
            CN_INTERNET_ENVIRONMENT
        );
        assert!(settings["env"].get(CODEBUDDY_BASE_URL).is_none());
    }

    #[test]
    fn apply_cli_region_env_replaces_portal_root_base_url_with_openai_v2() {
        let mut settings = json!({
            "env": { CODEBUDDY_BASE_URL: "https://www.codebuddy.ai" }
        });
        apply_cli_region_env(&mut settings, WbVariant::Ai).unwrap();
        assert_eq!(
            settings["env"][CODEBUDDY_BASE_URL],
            "https://www.codebuddy.ai/v2"
        );
    }

    #[test]
    fn sync_cli_runtime_cache_rewrites_endpoint_and_drops_internal_env_for_ai() {
        let dir = helper_test_dir().join("local_storage");
        fs::create_dir_all(&dir).unwrap();
        let env_path = dir.join(cli_cache_filename(CLI_ENV_CACHE_KEY));
        let endpoint_path = dir.join(cli_cache_filename(CLI_ENDPOINT_CACHE_KEY));
        let product_path = dir.join(cli_cache_filename(CLI_PRODUCT_CACHE_KEY));
        fs::write(&env_path, "\"internal\"").unwrap();
        fs::write(&endpoint_path, "\"https://copilot.tencent.com\"").unwrap();
        fs::write(&product_path, "gzip-placeholder").unwrap();

        sync_cli_runtime_cache_at(&dir, WbVariant::Ai).unwrap();
        assert!(!product_path.exists());
        assert_eq!(
            fs::read_to_string(&env_path).unwrap(),
            serde_json::to_string(&json!(AI_INTERNET_ENVIRONMENT)).unwrap()
        );
        assert_eq!(
            fs::read_to_string(&endpoint_path).unwrap(),
            serde_json::to_string(&json!(AI_CLI_ENDPOINT)).unwrap()
        );

        sync_cli_runtime_cache_at(&dir, WbVariant::Cn).unwrap();
        assert_eq!(
            fs::read_to_string(&env_path).unwrap(),
            serde_json::to_string(&json!(CN_INTERNET_ENVIRONMENT)).unwrap()
        );
        assert_eq!(
            fs::read_to_string(&endpoint_path).unwrap(),
            serde_json::to_string(&json!(CN_CLI_ENDPOINT)).unwrap()
        );
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
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

        let error =
            prepare_settings_env_update(&settings, "NEW_SECRET", WbVariant::Cn).unwrap_err();
        assert!(error.contains("不是有效 JSON"));
        assert!(!error.contains("NEW_SECRET"));
        assert!(!error.contains("SECRET_ON_DISK"));
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            "not-json SECRET_ON_DISK"
        );
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
                Some(PathBuf::from(
                    r"C:\Users\TESTUS~1\.codebuddy-rotate\helper.cjs"
                )),
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
        let error =
            validate_helper_result(true, Some(0), b"Bearer OTHER_SECRET\n", secret).unwrap_err();
        assert!(error.contains("输出阶段"));
        assert!(!error.contains(secret));
        assert!(!error.contains("OTHER_SECRET"));

        let error =
            validate_helper_result(false, Some(127), b"Bearer LEAKED\n", secret).unwrap_err();
        assert!(error.contains("退出码为 127"));
        assert!(!error.contains(secret));
        assert!(!error.contains("LEAKED"));
    }

    #[test]
    fn extracts_node_path_without_accepting_shell_noise() {
        // 判据是「绝对路径 + 文件名为 node」，而绝对路径的形态按平台不同
        // （Windows 需盘符或 UNC 前缀），fixture 必须按平台给。
        let node = if cfg!(windows) {
            r"C:\Users\test\node-v22\node"
        } else {
            "/Users/test/.nvm/versions/node/v22/bin/node"
        };
        let output = format!("welcome to the shell\n{node}\n");
        assert_eq!(
            node_path_from_shell_output(output.as_bytes()),
            Some(PathBuf::from(node))
        );
        assert_eq!(node_path_from_shell_output(b"node\nwelcome\n"), None);
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
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Bearer SECRET_TWO\n"
        );

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
