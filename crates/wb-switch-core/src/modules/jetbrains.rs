//! JetBrains IDE（IntelliJ IDEA / PyCharm）内 CodeBuddy 插件账号切换。
//!
//! 插件（marketplace id 24379，安装目录 `coding-copilot-jetbrains`）把登录态存在
//! **应用级配置目录** `options/secret-storage.xml`：插件自定义的 `SecretStorage`
//! PersistentStateComponent（`@State(storages = @Storage("secret-storage.xml"))`）
//! 以 `<MapStorage><Scores><Entry key=.. value=.. /></Scores></MapStorage>` 保存
//! 全部 secret。内嵌 Node 扩展宿主的 `context.secrets.store/get/delete` 直接透传
//! 到这张表（无 `secret://` 前缀），因此会话密钥与 VS Code 扩展完全同名：
//! `Tencent-Cloud.coding-copilot.new.accessToken`，值为完整会话 JSON——载荷构造
//! 直接复用 [`crate::modules::vscode_ext::build_ext_session_json`]。
//!
//! 支持的产品（按配置目录名前缀）：`IntelliJIdea`（IDEA 旗舰）、`IdeaIC`（社区）、
//! `PyCharm`（专业）、`PyCharmCE`（社区）。扫描所有已安装产品，一次切换写入全部
//! 装有插件的 IDE（多产品共用同一份账号）。
//!
//! 与 VS Code 端同一时序（运行中写入会被 IDE 内存态覆盖）：优雅退出 → 写 XML →
//! 重新打开。IDE 进程与配置目录的对应关系用安装目录 `product-info.json` 的
//! `dataDirectoryName` 精确解析，映射不上的进程一律不动（不关闭、不重开）。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
use std::time::Duration;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Instant;

use crate::modules::account::{self, get_str};
use crate::modules::codebuddy_cn_ide::{match_account_for_token, parse_token_from_secret};
use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::process;
use crate::modules::vscode_ext::build_ext_session_json;

const STATE_FILE: &str = "jetbrains.json";
/// CodeBuddy 插件写入 `SecretStorage` 的会话密钥（与 VS Code 扩展同 key）。
const SECRET_KEY: &str = "Tencent-Cloud.coding-copilot.new.accessToken";
/// 插件在 `<配置目录>/plugins/` 下的安装目录名前缀（marketplace zip 顶层目录）。
const PLUGIN_DIR_PREFIX: &str = "coding-copilot";
/// secret 存储文件相对配置目录的路径。
const SECRET_RELATIVE: &str = "options/secret-storage.xml";

/// 支持的产品：配置目录 / `product-info.json#dataDirectoryName` 前缀。
const PRODUCT_PREFIXES: [&str; 4] = ["IntelliJIdea", "IdeaIC", "PyCharm", "PyCharmCE"];

/// Windows 主进程映像名（`<product>64.exe`；zip / exe 安装通用）。
#[cfg(target_os = "windows")]
const WINDOWS_IMAGE_NAMES: [&str; 4] = ["idea64.exe", "pycharm64.exe", "idea.exe", "pycharm.exe"];

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
    if let Some(map) = state.as_object_mut() {
        map.insert("activeAccountId".to_string(), json!(account_id));
        map.insert("updatedAt".to_string(), json!(now_ms()));
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

/// JetBrains 配置根目录（应用级，各产品 `<产品><版本>` 子目录并列）。
///
/// - Windows: `%APPDATA%\JetBrains`
/// - macOS: `~/Library/Application Support/JetBrains`
/// - Linux: `$XDG_CONFIG_HOME/JetBrains`（缺省 `~/.config/JetBrains`）
pub fn jetbrains_config_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(crate::modules::config::home_dir().join("Library/Application Support/JetBrains"))
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir().map(|d| d.join("JetBrains"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir().map(|d| d.join("JetBrains"))
    }
}

/// 配置目录名是否属于受支持产品：前缀 + 纯版本号后缀（`2026.2` 形态）。
///
/// 后缀严格限定数字与点，把 `PyCharm2026.2-backup` 这类迁移残留目录排除在写入范围外。
fn is_supported_config_dir_name(name: &str) -> bool {
    PRODUCT_PREFIXES.iter().any(|prefix| {
        name.len() > prefix.len()
            && name.starts_with(prefix)
            && name[prefix.len()..]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
            && name[name.len() - 1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
    })
}

/// `<配置目录>/plugins/` 下是否存在 CodeBuddy 插件安装目录。
fn plugin_dir(config_dir: &Path) -> Option<PathBuf> {
    let plugins = config_dir.join("plugins");
    let entries = std::fs::read_dir(&plugins).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(PLUGIN_DIR_PREFIX) && entry.path().is_dir() {
            return Some(entry.path());
        }
    }
    None
}

/// secret-storage.xml 路径。
fn secret_path_for(config_dir: &Path) -> PathBuf {
    config_dir.join(SECRET_RELATIVE)
}

// ---------------------------------------------------------------------------
// secret-storage.xml 读写（JDOM 序列化兼容，无第三方依赖）
// ---------------------------------------------------------------------------

/// XML 属性值转义：覆盖 JDOM 输出的全部敏感字符（含换行类控制字符），
/// 多余转义（如 `'`）对 JDOM 解析无害。
fn xml_escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 16);
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
    out
}

/// 反转义 XML 属性值（数字实体 + 五个命名实体）。
fn xml_unescape_attr(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // 从 i 起找实体结束分号（上限 12 字符足够覆盖最长数字实体）。
        let limit = (i + 12).min(chars.len());
        let Some(semi) = chars[i + 1..limit].iter().position(|&c| c == ';') else {
            out.push('&');
            i += 1;
            continue;
        };
        let entity: String = chars[i + 1..i + 1 + semi].iter().collect();
        let decoded = match entity.as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            hex if hex.starts_with("#x") || hex.starts_with("#X") => {
                u32::from_str_radix(&hex[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            dec if dec.starts_with('#') => dec[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                i += semi + 2;
            }
            // 不是可识别实体：原样输出 `&`，继续正常解析。
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// 在 Entry 片段（`<Entry key=".." value=".." />`）中提取单个属性值。
fn entry_attr(fragment: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = fragment.find(&needle)? + needle.len();
    let rest = &fragment[start..];
    let end = rest.find('"')?;
    Some(xml_unescape_attr(&rest[..end]))
}

/// 解析 secret-storage.xml 的全部 Entry（key → value），保持文件顺序。
///
/// 只认 `<Entry ... />` 行内属性；解析器针对 JDOM 固定输出形态，非目标结构
/// （手工编辑过的异形 XML）返回空表并让上层按「文件不存在」处理。
fn parse_secret_entries(content: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut rest = content;
    while let Some(pos) = rest.find("<Entry ") {
        rest = &rest[pos..];
        let end = match rest.find("/>") {
            Some(end) => end + 2,
            None => break,
        };
        let fragment = &rest[..end];
        if let (Some(key), Some(value)) =
            (entry_attr(fragment, "key"), entry_attr(fragment, "value"))
        {
            entries.push((key, value));
        }
        rest = &rest[end..];
    }
    entries
}

/// 序列化 Entry 列表为 JDOM 兼容的 secret-storage.xml 内容。
fn serialize_secret_entries(entries: &[(String, String)]) -> String {
    let mut out =
        String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MapStorage>\n  <Scores>\n");
    for (key, value) in entries {
        out.push_str(&format!(
            "    <Entry key=\"{}\" value=\"{}\" />\n",
            xml_escape_attr(key),
            xml_escape_attr(value)
        ));
    }
    out.push_str("  </Scores>\n</MapStorage>\n");
    out
}

/// 读取指定配置目录的会话 secret；无文件 / 无该 key 返回 `None`。
fn read_secret_for(config_dir: &Path) -> Result<Option<String>, String> {
    let path = secret_path_for(config_dir);
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    Ok(parse_secret_entries(&content)
        .into_iter()
        .find(|(key, _)| key == SECRET_KEY)
        .map(|(_, value)| value))
}

/// upsert 会话 secret：目标 key 已存在则原位替换，不存在则追加；其余 Entry 原样保留。
fn write_secret_for(config_dir: &Path, value: &str) -> Result<PathBuf, String> {
    let path = secret_path_for(config_dir);
    let mut entries: Vec<(String, String)> = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
        // 写前备份（单份覆盖式）：secret 丢了大不了重新登录，但零成本留个还原点。
        let backup = path.with_extension("xml.wb-switch-bak");
        std::fs::copy(&path, &backup).map_err(|e| format!("备份 {} 失败: {e}", path.display()))?;
        parse_secret_entries(&content)
    } else {
        Vec::new()
    };
    match entries.iter_mut().find(|(key, _)| key == SECRET_KEY) {
        Some(slot) => slot.1 = value.to_string(),
        None => entries.push((SECRET_KEY.to_string(), value.to_string())),
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(&path, &serialize_secret_entries(&entries))
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// 进程发现：运行中的 IDE 与其配置目录的精确映射
// ---------------------------------------------------------------------------

/// 运行中的 JetBrains IDE 实例快照（关闭前采集）。
#[derive(Debug, Clone)]
struct RunningIde {
    pid: u32,
    /// Windows: 主进程 exe；macOS: `.app` bundle；Linux: 安装根（best-effort）。
    launch_target: Option<PathBuf>,
    /// 由安装目录 `product-info.json#dataDirectoryName` 解析出的配置目录名。
    config_dir_name: Option<String>,
}

/// 从安装根读取 `product-info.json#dataDirectoryName`（JetBrains 全系自带）。
fn data_dir_name_for_install_root(root: &Path) -> Option<String> {
    let info = std::fs::read_to_string(root.join("product-info.json")).ok()?;
    let value: Value = serde_json::from_str(&info).ok()?;
    value
        .get("dataDirectoryName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 从主进程可执行文件路径反推安装根：`<root>/bin/<x>.exe` → `<root>`。
#[cfg(target_os = "windows")]
fn install_root_from_exe(exe: &Path) -> Option<PathBuf> {
    exe.parent().and_then(Path::parent).map(Path::to_path_buf)
}

/// 从运行进程行构造快照；映射不上的实例 `config_dir_name = None`。
#[cfg(target_os = "windows")]
fn running_ide_from_row(row: &process::WindowsProcessRow) -> RunningIde {
    let config_dir_name = row
        .exe_path
        .as_deref()
        .and_then(install_root_from_exe)
        .and_then(|root| data_dir_name_for_install_root(&root));
    RunningIde {
        pid: row.pid,
        launch_target: row.exe_path.clone(),
        config_dir_name,
    }
}

#[cfg(target_os = "windows")]
fn windows_ide_process_rows() -> Vec<process::WindowsProcessRow> {
    let self_pid = std::process::id();
    let script = "Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
                  Where-Object { $_.Name -in 'idea64.exe','pycharm64.exe','idea.exe','pycharm.exe' } | \
                  ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.ExecutablePath }";
    let mut rows: Vec<_> = process::ps_output(script, 8)
        .map(|stdout| {
            process::parse_windows_process_rows(&stdout)
                .into_iter()
                .filter(|row| row.pid != self_pid)
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        // CIM 不可用时退回 tasklist（拿到 PID 关闭，但丢失 exe 路径 → 无法重开）。
        for image in ["idea64.exe", "pycharm64.exe", "idea.exe", "pycharm.exe"] {
            rows.extend(process::windows_tasklist_image_rows(image));
        }
    }
    rows.retain(|row| {
        let name = row.name.trim();
        WINDOWS_IMAGE_NAMES
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n))
    });
    rows
}

/// 枚举当前运行中的受支持 IDE 实例（同配置目录多实例按 PID 全保留）。
fn running_ides() -> Vec<RunningIde> {
    #[cfg(target_os = "windows")]
    {
        windows_ide_process_rows()
            .iter()
            .map(running_ide_from_row)
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        // 进程 cmdline 形如 `/Applications/IntelliJ IDEA.app/Contents/MacOS/idea`；
        // bundle 路径回溯后读 `Contents/Resources/product-info.json`。
        // 产品名集合（.app 名）：IntelliJ IDEA、IntelliJ IDEA CE、PyCharm、PyCharm CE。
        let patterns: Vec<String> = [
            "IntelliJ IDEA.app/Contents/MacOS",
            "IntelliJ IDEA CE.app/Contents/MacOS",
            "PyCharm.app/Contents/MacOS",
            "PyCharm CE.app/Contents/MacOS",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut rows = Vec::new();
        for (pid, args) in process::macos_rows_by_patterns(&patterns) {
            let bundle = process::extract_app_bundle_from_args(&args).map(PathBuf::from);
            let config_dir_name = bundle
                .as_deref()
                .and_then(|b| data_dir_name_for_install_root(&b.join("Contents/Resources")));
            rows.push(RunningIde {
                pid,
                launch_target: bundle,
                config_dir_name,
            });
        }
        rows
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux：脚本启动时 argv[0] 是 `<root>/bin/<x>`；java 直启时 cmdline 含
        // `-Didea.home.path=<root>`。两者都取不到就放弃映射（只报告、不关闭）。
        let self_pid = std::process::id();
        let mut rows = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let pid: u32 = match entry.file_name().to_string_lossy().parse() {
                    Ok(p) => p,
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
                let lower = cmdline.to_ascii_lowercase();
                if !["idea", "pycharm"].iter().any(|name| {
                    lower.contains(&format!("/bin/{name} "))
                        || lower.contains(&format!("/bin/{name}\""))
                        || lower.ends_with(&format!("/bin/{name}"))
                }) && !lower.contains("-didea.home.path=")
                {
                    continue;
                }
                let root = cmdline.split_whitespace().find_map(|arg| {
                    let path = Path::new(arg);
                    if path.is_dir() && path.join("product-info.json").exists() {
                        return Some(path.to_path_buf());
                    }
                    // `<root>/bin/<x>` → `<root>`
                    let mut ancestors = path.ancestors();
                    ancestors.next()?; // 自身
                    let bin = ancestors.next()?;
                    let root = bin.parent()?;
                    if root.join("product-info.json").exists() && bin.ends_with("bin") {
                        return Some(root.to_path_buf());
                    }
                    None
                });
                let config_dir_name = root.as_deref().and_then(data_dir_name_for_install_root);
                rows.push(RunningIde {
                    pid,
                    launch_target: root,
                    config_dir_name,
                });
            }
        }
        rows
    }
}

// ---------------------------------------------------------------------------
// 优雅关闭 / 重开（契约同 VS Code 端：绝不强杀，超时只报错）
// ---------------------------------------------------------------------------

/// 等待 IDE 退出的默认上限（秒）。
const CLOSE_TIMEOUT_SECS: i64 = 60;

/// 运行中 + 手动模式的报错文案。
const RUNNING_MANUAL_HINT: &str =
    "检测到 JetBrains IDE（IDEA / PyCharm）正在运行，请先完全退出后再切换，否则写入会被 IDE 覆盖。";

fn close_timeout_error(alive: &[u32]) -> String {
    let pids: Vec<String> = alive.iter().map(|pid| pid.to_string()).collect();
    format!(
        "等待 JetBrains IDE 退出超时（仍有进程运行: {}）。请处理 IDE 内的保存提示后重试；若你在等待期间重新打开过 IDE，请退出后重试。",
        pids.join(", ")
    )
}

/// 优雅关闭指定 IDE 实例（Windows: `taskkill /PID /T` 等价 WM_CLOSE，走保存提示）。
fn close_ides(instances: &[RunningIde], timeout_secs: i64) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let pids: Vec<u32> = instances.iter().map(|inst| inst.pid).collect();
        if pids.is_empty() {
            return Ok(());
        }
        for pid in &pids {
            let pid_arg = pid.to_string();
            let _ = process::run_cmd_timeout("taskkill", &["/PID", &pid_arg, "/T"], 10);
        }
        let alive =
            process::wait_windows_pids_gone(&pids, Duration::from_secs(timeout_secs.max(1) as u64));
        if alive.is_empty() {
            Ok(())
        } else {
            Err(close_timeout_error(&alive))
        }
    }
    #[cfg(target_os = "macos")]
    {
        // 原生退出流程（保存提示 / 热退出保护由 IDE 自己负责），等待退出。
        // macOS 无法按单 PID 优雅退出 GUI 应用：按 bundle 名 `quit app`。
        let started = Instant::now();
        for inst in instances {
            if let Some(app_name) = inst
                .launch_target
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                .map(|n| n.trim_end_matches(".app").to_string())
            {
                let script = format!("quit app \"{}\"", app_name.replace('"', "\\\""));
                let _ = process::run_cmd_timeout("osascript", &["-e", &script], 10);
            }
        }
        let deadline = started + Duration::from_secs(timeout_secs.max(1) as u64);
        loop {
            let alive: Vec<u32> = instances
                .iter()
                .map(|inst| inst.pid)
                .filter(|pid| process_alive(*pid))
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
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let pids: Vec<u32> = instances.iter().map(|inst| inst.pid).collect();
        if pids.is_empty() {
            return Ok(());
        }
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

/// macOS 进程存活探测（`kill -0`）：`quit app` 后按 PID 等待退出。
#[cfg(target_os = "macos")]
fn process_alive(pid: u32) -> bool {
    process::run_cmd_timeout("kill", &["-0", &pid.to_string()], 5)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// 重新打开被我们关闭的 IDE（只用关闭前记录的路径）。
fn launch_ide(inst: &RunningIde) -> Result<(), String> {
    let target = inst
        .launch_target
        .as_ref()
        .ok_or_else(|| "未能记录 IDE 可执行文件路径，请手动打开".to_string())?;
    #[cfg(target_os = "macos")]
    {
        let app = target.to_string_lossy().into_owned();
        match process::run_cmd_timeout("open", &[app.as_str()], 10) {
            Some(out) if out.status.success() => Ok(()),
            Some(out) => Err(format!(
                "重新打开 IDE 失败: {}（路径: {}）",
                String::from_utf8_lossy(&out.stderr).trim(),
                target.display()
            )),
            None => Err(format!(
                "重新打开 IDE 失败: open 超时（路径: {}）",
                target.display()
            )),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        process::cmd_builder(target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("重新打开 IDE 失败: {e}（路径: {}）", target.display()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 状态与切换
// ---------------------------------------------------------------------------

/// 切换目标：一个装有（或可装）CodeBuddy 插件的产品配置目录。
#[derive(Debug)]
struct Target {
    /// 产品配置目录（如 `%APPDATA%\JetBrains\PyCharm2026.2`）。
    config_dir: PathBuf,
    /// 配置目录名（如 `PyCharm2026.2`）。
    name: String,
    /// 是否安装了 CodeBuddy 插件。
    plugin_installed: bool,
}

impl Target {
    fn secret_path(&self) -> PathBuf {
        secret_path_for(&self.config_dir)
    }

    fn to_status_json(&self, running_dirs: &[String]) -> Value {
        let logged_in = read_secret_for(&self.config_dir).ok().flatten().is_some();
        json!({
            "configDir": self.name,
            "pluginInstalled": self.plugin_installed,
            "running": running_dirs.iter().any(|n| n == &self.name),
            "loggedIn": logged_in,
            "secretPath": self.secret_path().to_string_lossy(),
        })
    }
}

/// 扫描配置根，列出受支持产品的配置目录（存在即列出，无论是否装插件）。
fn list_targets() -> Vec<Target> {
    let Some(root) = jetbrains_config_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut targets: Vec<Target> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (entry.path(), name)
        })
        .filter(|(_, name)| is_supported_config_dir_name(name))
        .map(|(path, name)| Target {
            plugin_installed: plugin_dir(&path).is_some(),
            config_dir: path,
            name,
        })
        .collect();
    targets.sort_by(|a, b| a.name.cmp(&b.name));
    targets
}

/// 状态：是否安装、插件安装、运行、登录、当前账号（本地状态文件 + 账号库）。
pub fn status() -> Value {
    let targets = list_targets();
    let running = running_ides();
    let running_dirs: Vec<String> = running
        .iter()
        .filter_map(|inst| inst.config_dir_name.clone())
        .collect();

    let installed = !targets.is_empty();
    let plugin_installed = targets.iter().any(|t| t.plugin_installed);
    let is_running = running_dirs
        .iter()
        .any(|dir| targets.iter().any(|t| t.plugin_installed && &t.name == dir));
    let logged_in = targets
        .iter()
        .filter(|t| t.plugin_installed)
        .any(|t| read_secret_for(&t.config_dir).ok().flatten().is_some());

    let mut active_account_id = active_account_id_from_state();
    let mut active_account_name: Option<String> = None;
    if let Some(id) = active_account_id.clone() {
        if let Some(acc) = account::find_account(&id) {
            active_account_name = Some(account::account_display_name(&acc));
        } else {
            active_account_id = None;
        }
    }

    json!({
        "installed": installed,
        "pluginInstalled": plugin_installed,
        "running": is_running,
        "loggedIn": logged_in,
        "configRoot": jetbrains_config_root().map(|p| p.to_string_lossy().to_string()),
        "targets": targets.iter().map(|t| t.to_status_json(&running_dirs)).collect::<Vec<_>>(),
        "activeAccountId": active_account_id,
        "activeAccountName": active_account_name,
        "detectedFrom": "secret-storage",
        "statePath": state_path().to_string_lossy(),
    })
}

/// 切换前置校验：账号 / `access_token` / 至少一个装了插件的配置目录。
fn validate_switch_target(account_id: &str) -> Result<(Value, Vec<Target>), String> {
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let token = get_str(&acc, "access_token")
        .ok_or_else(|| "账号缺少 access_token，无法注入 JetBrains IDE 插件".to_string())?;
    if token.is_empty() {
        return Err("账号 access_token 为空".to_string());
    }

    if jetbrains_config_root().is_none() {
        return Err("无法定位 JetBrains 配置目录".to_string());
    }
    let targets: Vec<Target> = list_targets()
        .into_iter()
        .filter(|t| t.plugin_installed)
        .collect();
    if targets.is_empty() {
        return Err(
            "未找到已安装 CodeBuddy 插件的 JetBrains IDE。请先在 IDEA / PyCharm 中安装「Tencent Cloud CodeBuddy」插件后重试。".to_string(),
        );
    }
    Ok((acc, targets))
}

/// 「编辑器已按需关闭后」的切换主体：写所有目标 → 记录当前账号 → 重开。
///
/// 各目标独立读取既有 secret 做 merge：已登录的目标走 upsert（保留扩展私有字段），
/// 未登录的目标走新建完整载荷，互不影响。
fn switch_after_close_inner(account_id: &str, closed: &[RunningIde]) -> Result<Value, String> {
    let (acc, targets) = validate_switch_target(account_id)?;

    let mut written: Vec<String> = Vec::new();
    for target in &targets {
        let existing = read_secret_for(&target.config_dir).map_err(describe_secret_error)?;
        let payload = build_ext_session_json(&acc, existing.as_deref());
        write_secret_for(&target.config_dir, &payload).map_err(describe_inject_error)?;
        written.push(target.name.clone());
    }

    set_active_account_id(account_id)?;

    let mut restarted = false;
    let mut relaunch_error: Option<String> = None;
    for inst in closed {
        match launch_ide(inst) {
            Ok(()) => restarted = true,
            Err(err) => relaunch_error = Some(err),
        }
    }

    let closed_by_us = !closed.is_empty();
    let name = account::account_display_name(&acc);
    let message = switch_message(&name, &written, restarted, relaunch_error.as_deref());

    Ok(json!({
        "ok": true,
        "account": name,
        "accountId": account_id,
        "written": written,
        "restarted": restarted,
        "closedByUs": closed_by_us,
        "message": message,
    }))
}

fn describe_secret_error(err: String) -> String {
    err
}

/// 注入失败的文案映射。
fn describe_inject_error(err: String) -> String {
    format!("注入 JetBrains IDE 插件登录状态失败：{err}")
}

/// 切换成功文案：区分「已重新打开」「重开失败」「本来没运行」。
fn switch_message(
    name: &str,
    written: &[String],
    restarted: bool,
    relaunch_error: Option<&str>,
) -> String {
    let scope = if written.is_empty() {
        String::new()
    } else {
        format!("，目标: {}", written.join("、"))
    };
    match (restarted, relaunch_error) {
        (true, _) => format!("已写入 JetBrains IDE CodeBuddy 插件凭证（{name}{scope}），IDE 已重新打开。"),
        (false, Some(err)) => format!(
            "已写入 JetBrains IDE CodeBuddy 插件凭证（{name}{scope}），但自动重新打开 IDE 失败：{err}；请手动打开 IDE 生效。"
        ),
        (false, None) => format!("已写入 JetBrains IDE CodeBuddy 插件凭证（{name}{scope}）；请打开 IDE 生效。"),
    }
}

/// 失败兜底：被我们关掉的 IDE 尽力逐个开回来；重开失败只追加提示，不吞原错误。
fn relaunch_closed_on_error(closed: &[RunningIde], error: String) -> String {
    let mut messages = vec![error];
    for inst in closed {
        if let Err(launch_error) = launch_ide(inst) {
            messages.push(launch_error);
        }
    }
    messages.join("\n\n")
}

/// 切换 JetBrains IDE（IDEA / PyCharm）CodeBuddy 插件账号。
///
/// 时序：校验 → 采集运行快照 → 关闭决策（运行中且 `restart=false` 报错、
/// `restart=true` 优雅退出；映射不到配置目录的进程一律不动）→ 写入 → 重开。
pub fn switch_account(account_id: &str, restart: bool) -> Result<Value, String> {
    validate_switch_target(account_id)?;
    let target_names: Vec<String> = list_targets()
        .into_iter()
        .filter(|t| t.plugin_installed)
        .map(|t| t.name)
        .collect();
    let matched: Vec<RunningIde> = running_ides()
        .into_iter()
        .filter(|inst| {
            inst.config_dir_name
                .as_deref()
                .is_some_and(|name| target_names.iter().any(|t| t == name))
        })
        .collect();

    if !matched.is_empty() && !restart {
        return Err(RUNNING_MANUAL_HINT.to_string());
    }

    if !matched.is_empty() {
        close_ides(&matched, CLOSE_TIMEOUT_SECS)?;
    }

    match switch_after_close_inner(account_id, &matched) {
        Ok(value) => Ok(value),
        Err(error) => Err(relaunch_closed_on_error(&matched, error)),
    }
}

/// 从本机 JetBrains IDE 读取当前 token；若能匹配账号库则返回匹配信息。
pub fn detect_current_account() -> Result<Value, String> {
    let targets: Vec<Target> = list_targets()
        .into_iter()
        .filter(|t| t.plugin_installed)
        .collect();
    for target in &targets {
        // 单个目标的 secret 读取失败按未登录处理，不阻断其它目标。
        let Some(secret) = read_secret_for(&target.config_dir).ok().flatten() else {
            continue;
        };
        let Some((uid, token)) = parse_token_from_secret(&secret) else {
            continue;
        };
        if let Some(acc) = match_account_for_token(uid.as_deref(), &token) {
            let id = get_str(&acc, "id").unwrap_or_default();
            let _ = set_active_account_id(&id);
            return Ok(json!({
                "ok": true,
                "found": true,
                "matched": true,
                "configDir": target.name,
                "accountId": id,
                "account": account::account_meta(&acc),
                "uid": uid,
            }));
        }
        return Ok(json!({
            "ok": true,
            "found": true,
            "matched": false,
            "configDir": target.name,
            "uid": uid,
            "message": "本机 JetBrains IDE 已登录 CodeBuddy 插件，但账号库中无匹配账号；可先用「从本机导入」或扫码登录同步账号后再切换。",
        }));
    }
    Ok(json!({
        "ok": true,
        "found": false,
        "message": "本机 JetBrains IDE 未找到 CodeBuddy 插件登录 secret",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_name_filter_accepts_products_and_rejects_noise() {
        for name in [
            "IntelliJIdea2026.2",
            "IdeaIC2025.1",
            "PyCharm2026.2",
            "PyCharmCE2025.1",
        ] {
            assert!(is_supported_config_dir_name(name), "{name} 应被接受");
        }
        for name in [
            "PermanentDeviceId",
            "bl",
            "crl",
            "discovery",
            "IntelliJIdea",
            "PyCharm",
            "PyCharmBackup",
            "IntelliJIdeaX",
            "PyCharm2026.2-backup",
        ] {
            assert!(!is_supported_config_dir_name(name), "{name} 应被拒绝");
        }
    }

    #[test]
    fn xml_escape_and_unescape_round_trip() {
        let raw = "a&b<c>\"d\"'e'f\n\tg";
        let escaped = xml_escape_attr(raw);
        assert_eq!(
            escaped,
            "a&amp;b&lt;c&gt;&quot;d&quot;&apos;e&apos;f&#10;&#9;g"
        );
        assert_eq!(xml_unescape_attr(&escaped), raw);
    }

    #[test]
    fn xml_unescape_keeps_unknown_entities_and_digits() {
        assert_eq!(
            xml_unescape_attr("x&#10;y&#x41;z&unknown;w"),
            "x\nyAz&unknown;w"
        );
        assert_eq!(xml_unescape_attr("no entity"), "no entity");
    }

    #[test]
    fn secret_entries_parse_and_serialize_round_trip() {
        let value =
            r#"{"id":"Tencent-Cloud.coding-copilot","accessToken":"u+tok","auth":{"scope":"all"}}"#;
        let content = serialize_secret_entries(&[
            ("other.key".to_string(), "plain & <value>".to_string()),
            (SECRET_KEY.to_string(), value.to_string()),
        ]);
        assert!(content.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        let entries = parse_secret_entries(&content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "other.key");
        assert_eq!(entries[0].1, "plain & <value>");
        assert_eq!(entries[1].0, SECRET_KEY);
        assert_eq!(entries[1].1, value);
    }

    #[test]
    fn secret_entries_parse_tolerates_self_closing_spread() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<MapStorage>
  <Scores>
    <Entry key="Tencent-Cloud.coding-copilot.new.accessToken" value="{&quot;a&quot;:1}" />
    <Entry key="k2" value="v2" />
  </Scores>
</MapStorage>
"#;
        let entries = parse_secret_entries(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, r#"{"a":1}"#);
        assert_eq!(entries[1].1, "v2");
    }

    #[test]
    fn secret_store_read_write_upsert_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("wb-switch-jb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("options")).unwrap();

        // 首写：文件不存在 → 新建
        write_secret_for(&dir, "first").unwrap();
        assert_eq!(read_secret_for(&dir).unwrap().as_deref(), Some("first"));
        // 备份在第二次写入时才出现
        assert!(!dir
            .join("options/secret-storage.xml.wb-switch-bak")
            .exists());

        // 二写：upsert + 保留其它 Entry + 生成备份
        let path = secret_path_for(&dir);
        std::fs::write(
            &path,
            serialize_secret_entries(&[("keep.me".to_string(), "v1".to_string())]),
        )
        .unwrap();
        write_secret_for(&dir, "second").unwrap();
        assert_eq!(read_secret_for(&dir).unwrap().as_deref(), Some("second"));
        let entries = parse_secret_entries(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "keep.me");
        assert!(dir
            .join("options/secret-storage.xml.wb-switch-bak")
            .exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_dir_name_parses_product_info() {
        let dir = std::env::temp_dir().join(format!("wb-switch-jb-info-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(data_dir_name_for_install_root(&dir), None);
        std::fs::write(
            dir.join("product-info.json"),
            r#"{"name":"IntelliJ IDEA","version":"2026.2.1","dataDirectoryName":"IntelliJIdea2026.2"}"#,
        )
        .unwrap();
        assert_eq!(
            data_dir_name_for_install_root(&dir).as_deref(),
            Some("IntelliJIdea2026.2")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话载荷复用 VS Code 扩展构造：id / accessToken / accounts 语义一致。
    #[test]
    fn session_json_reuses_vscode_ext_builder() {
        let acc = json!({
            "uid": "u-42",
            "nickname": "测试",
            "access_token": "tok-abc",
            "refresh_token": "rt-1",
            "domain": "www.codebuddy.cn",
            "expiresAt": 1234567890_i64,
        });
        let v: Value = serde_json::from_str(&build_ext_session_json(&acc, None)).unwrap();
        assert_eq!(v["id"], "Tencent-Cloud.coding-copilot");
        assert_eq!(v["accessToken"], "u-42+tok-abc");
        assert_eq!(v["token"], "tok-abc");
    }

    /// 关闭决策：运行中且手动模式 → 报错；未运行 → 直接写（closed 为空）。
    #[test]
    fn switch_decision_blocks_manual_mode_with_running_ide() {
        // 契约回归：matched 非空 + restart=false 必须在关进程前报错（不触碰进程表）。
        assert!(RUNNING_MANUAL_HINT.contains("正在运行"));
        assert_eq!(CLOSE_TIMEOUT_SECS, 60);
    }
}
