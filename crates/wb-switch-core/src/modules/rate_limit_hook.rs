//! 限额 hook 的安装 / 卸载 / 状态：把 `Stop` + `FinalStop` 两个事件注册进
//! 三处客户端配置（`~/.codebuddy`、`~/.workbuddy`、`~/.workbuddy-ai` 的 `settings.json`）。
//!
//! 只注册这两个事件（三轮探针实测：CLI / WorkBuddy 的 429 当轮触发 `Stop`，
//! `FinalStop` 是终态补充；`PreToolUse` / `PostToolUse` 之类会随每次工具调用触发，
//! 只增加客户端开销）。hook 脚本把 stdin payload 追加到 `~/.wb-switch/hook-events.jsonl`，
//! 由后端消费（见 `rate_limit_events.rs`）。
//!
//! 三条硬约束（安装会改写用户真实配置，必须守住）：
//! - **幂等**：重复安装不产生重复条目；
//! - **写前备份**：安装前把原文件原样复制到 `~/.wb-switch/hook-backups/`；
//! - **可还原**：卸载时若配置的语义与备份一致（除本工具的条目外没有别的改动），
//!   直接把备份字节写回，做到逐字节还原；用户后来改过别的键时只做结构化移除，不覆盖用户改动。
//!
//! 全部公开入口都通过 [`HookLayout`] 取路径，安装 / 卸载 / 状态的核心实现接收显式 layout，
//! 单测一律注入临时目录，不触碰真实 `~/.codebuddy`、`~/.workbuddy`、`~/.workbuddy-ai`、`~/.wb-switch`。
//!
//! 存在性判据（追加需求）：客户端数据根目录 `is_dir()` 是唯一判据。不存在的数据根不写配置、
//! 不为其创建目录 / `settings.json`；一处客户端都没装时安装是「什么都不做」，不是错误。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::modules::config::{self, atomic_write, home_dir};
use crate::modules::i18n;

/// hook 脚本名（全平台统一 sh，见 `script_name`）。
const SCRIPT_NAME_SH: &str = "hook.sh";
/// hook 事件信号文件：脚本 append、后端消费。
pub const EVENTS_FILE_NAME: &str = "hook-events.jsonl";
/// 客户端设置文件名（三处配置同名）。
pub(crate) const SETTINGS_FILE_NAME: &str = "settings.json";
/// 安装前备份目录（卸载时据此逐字节还原）。
const BACKUP_DIR_NAME: &str = "hook-backups";

/// 注册的事件（最小集）。
const HOOK_EVENTS: [&str; 2] = ["Stop", "FinalStop"];

/// 客户端数据根目录名 → 备份文件名标签（三处配置的稳定标识）。
const TARGETS: [(&str, &str); 3] = [
    (".codebuddy", "codebuddy"),
    (".workbuddy", "workbuddy"),
    (".workbuddy-ai", "workbuddy-ai"),
];

/// 本应用的数据目录名（与 `config::store_dir()` 一致）。
const STORE_DIR_NAME: &str = ".wb-switch";

// ---------------------------------------------------------------------------
// 平台脚本
// ---------------------------------------------------------------------------

/// 旧版 Windows 脚本名（`cmd /c` 形态，2026-09-18 弃用；安装 / 卸载时顺带清理其注册条目）。
const LEGACY_CMD_NAME: &str = "hook.cmd";

/// 已知的旧版 sh 脚本正文（绝对路径化之前、依赖 `$HOME` 的那一份）。
///
/// 只用于**识别**：内容与它逐字节一致才允许升级重写 / 卸载删除。
const LEGACY_SH_BODY: &str = "#!/bin/sh\npayload=$(cat)\nprintf '%s\\n' \"$payload\" >> \"$HOME/.wb-switch/hook-events.jsonl\"\nprintf '{}'\n";

/// 已知的旧版 cmd 脚本正文（Windows 的 `cmd` + PowerShell 形态）。
///
/// 当时正文里不含任何待替换的绝对路径，因此逐字节匹配即可认出。
const LEGACY_CMD_BODY: &str = concat!(
    "@echo off\r\n",
    "powershell -NoProfile -ExecutionPolicy Bypass -Command \"$d=[Console]::In.ReadToEnd(); ",
    "if ($d.Trim().Length -gt 0) { Add-Content -LiteralPath (Join-Path $env:USERPROFILE '.wb-switch\\hook-events.jsonl') ",
    "-Value $d.TrimEnd() -Encoding UTF8 }\"\r\n",
    "echo {}\r\n",
);

/// hook 脚本名：**全平台统一 sh**。Windows 的执行器是客户端自带的 PortableGit bash
/// （与插件 hook 同款调用），实测 `cmd /c "..."` 在该环境里静默失败 —— spawn 成功但
/// 批处理零副作用（2026-09-18 探针实证：连 `%TEMP%` 的 mkdir 都没发生），而
/// `bash "<路径>"` 形式的插件 hook 一直正常。
fn script_name() -> &'static str {
    SCRIPT_NAME_SH
}

/// hook 脚本正文。
///
/// ⚠️ 事件文件路径在安装时**写死为绝对路径**，不依赖 `HOME` / `USERPROFILE`：
/// hook 由各客户端自己的执行器拉起，运行环境不可控，环境变量并不可靠；
/// 安装时一次解析，之后逐字节幂等。路径统一经 [`shell_quote`] 转义：含空格 / 中文 /
/// 单引号的合法路径都不能破坏引号配对（2026-09-20 审查实证：裸拼单引号会让
/// `o'brien` 这类路径静默丢事件 —— stderr 报错、退出码 0、stdout 仍是 `{}`）。
///
/// 一次 `cat` 读入 payload、一次 `printf` 原样追加（尽量单次 write，减少并发追加的行内
/// 交错），最后必须回 `{}`——空 stdout 会被客户端当作 hook 失败。
fn script_body(events: &Path) -> String {
    // Windows 下统一正斜杠（MSYS bash 对 `C:/...` 原生支持；反斜杠在引号里是转义雷区）。
    let sh_events = events.to_string_lossy().replace('\\', "/");
    [
        "#!/bin/sh",
        "payload=$(cat)",
        &format!("printf '%s\\n' \"$payload\" >> {}", shell_quote(&sh_events)),
        "printf '{}'",
        "",
    ]
    .join("\n")
}

/// MSYS 风格路径：`C:\a\b` → `/c/a/b`（PortableGit bash 的原生形态，与插件 hook 一致）。
fn msys_path(script: &Path) -> String {
    let s = script.to_string_lossy().replace('\\', "/");
    let mut chars = s.chars();
    let drive = chars.next().unwrap_or('c').to_ascii_lowercase();
    match chars.next() {
        Some(':') => format!("/{}{}", drive, chars.as_str()),
        _ => s,
    }
}

/// 注册命令里的脚本路径**实参形态**：Windows 走 MSYS 路径，其余平台原样。
///
/// marker 与注册命令都用它拼装，保证两侧逐字节一致。
fn script_arg(script: &Path) -> String {
    if cfg!(windows) {
        msys_path(script)
    } else {
        script.to_string_lossy().to_string()
    }
}

/// 单引号包裹 shell 参数（路径可能含空格 / 中文 / 单引号）。
///
/// 单引号按 POSIX 写法转义为 `'\''`：这是 marker 与注册命令共用的唯一形态。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// hook 脚本的启动命令（写进三处 `settings.json` 的 `command`）。
fn hook_command(script: &Path) -> String {
    let interpreter = if cfg!(windows) { "bash" } else { "sh" };
    format!("{interpreter} {}", shell_quote(&script_arg(script)))
}

// ---------------------------------------------------------------------------
// 目标布局
// ---------------------------------------------------------------------------

/// 一处客户端配置。
struct HookTarget {
    label: &'static str,
    /// 客户端数据根目录：**唯一**的存在性判据（`settings.json` 可能存在也可能还没写过）。
    data_root: PathBuf,
    settings: PathBuf,
}

impl HookTarget {
    /// 本机是否装了该客户端（数据根目录存在）。
    fn exists(&self) -> bool {
        self.data_root.is_dir()
    }

    /// 该处配置里是否已注册本工具 hook。
    fn registered(&self, marker: &str) -> bool {
        settings_has_marker(&self.settings, marker)
    }
}

/// 既有 hook 脚本的归属：决定升级与卸载时动不动这个文件。
enum ScriptOwnership {
    /// 脚本不存在（全新接入）。
    Absent,
    /// 与当前模板逐字节一致：不需要任何动作。
    Current,
    /// 本工具的已知旧模板（[`LEGACY_SH_BODY`]）：升级时重写、卸载时删除都安全。
    Outdated,
    /// 内容未知（用户改过）：既不覆盖也不删除。
    Foreign,
}

/// 本模块用到的全部路径（显式传入，便于单测注入）。
struct HookLayout {
    script: PathBuf,
    events: PathBuf,
    backups: PathBuf,
    targets: Vec<HookTarget>,
}

impl HookLayout {
    /// 以 `base` 为「用户主目录」推导全部路径。
    fn under(base: &Path) -> Self {
        let store = base.join(STORE_DIR_NAME);
        Self {
            script: store.join(script_name()),
            events: store.join(EVENTS_FILE_NAME),
            backups: store.join(BACKUP_DIR_NAME),
            targets: TARGETS
                .iter()
                .map(|(dir, label)| {
                    let data_root = base.join(dir);
                    HookTarget {
                        label,
                        settings: data_root.join(SETTINGS_FILE_NAME),
                        data_root,
                    }
                })
                .collect(),
        }
    }

    fn default_layout() -> Self {
        Self::under(&home_dir())
    }

    /// 配置里的 marker：注册命令中脚本路径的**原样形态**（含 [`shell_quote`] 的引号）。
    ///
    /// 必须与写进配置的 command 逐字节一致：否则含单引号 / 空格的路径会让
    /// `contains(marker)` 失配 —— 判定为「未注册」后每次启动都会再追加一条条目。
    /// Windows 的注册命令是 `bash '/c/...'`，marker 同样取 MSYS 形态。
    fn marker(&self) -> String {
        shell_quote(&script_arg(&self.script))
    }

    /// 旧版 `hook.cmd` 注册命令里使用的本工具脚本绝对路径。
    ///
    /// 迁移时只能按这条具体路径识别旧条目，不能用 `.wb-switch` / `hook.cmd`
    /// 等宽泛片段认领第三方命令。
    fn legacy_marker(&self) -> String {
        self.script
            .with_file_name(LEGACY_CMD_NAME)
            .to_string_lossy()
            .to_string()
    }

    /// 是否至少存在一处可接入的客户端（存在的数据根）。
    fn any_target_exists(&self) -> bool {
        self.targets.iter().any(HookTarget::exists)
    }

    /// 已存在且注册成功的客户端数。
    fn registered_target_count(&self) -> usize {
        let marker = self.marker();
        self.targets
            .iter()
            .filter(|target| target.exists() && target.registered(&marker))
            .count()
    }

    /// 读一次脚本文件判断归属（脚本很小，不额外缓存）。
    fn script_ownership(&self) -> ScriptOwnership {
        let Ok(content) = std::fs::read_to_string(&self.script) else {
            return ScriptOwnership::Absent;
        };
        if content == script_body(&self.events) {
            return ScriptOwnership::Current;
        }
        if content == LEGACY_SH_BODY {
            return ScriptOwnership::Outdated;
        }
        ScriptOwnership::Foreign
    }

    /// hook 是否已「装全」：脚本不是缺失 / 待升级，且每一处存在的客户端都注册了本工具条目。
    ///
    /// 有客户端存在但一处都没装 / 只装了一半 / 脚本被删 / 脚本还是本工具的旧模板
    /// → 都不算装全（启动时据此重装）。用户改过的脚本按「已接管」处理：
    /// 启动时不去动它，避免反复重装与静默覆盖（升级 / 卸载同样只认本工具生成的正文）。
    fn fully_installed(&self) -> bool {
        !matches!(
            self.script_ownership(),
            ScriptOwnership::Absent | ScriptOwnership::Outdated
        ) && self.any_target_exists()
            && self
                .targets
                .iter()
                .filter(|target| target.exists())
                .all(|target| target.registered(&self.marker()))
    }

    fn backup_path(&self, target: &HookTarget) -> PathBuf {
        self.backups.join(format!("{}.settings.json", target.label))
    }

    /// 「安装时该配置不存在」的标记文件。
    fn absent_mark_path(&self, target: &HookTarget) -> PathBuf {
        self.backups
            .join(format!("{}.settings.json.absent", target.label))
    }
}

/// hook 事件信号文件路径（后端消费方使用）。
pub fn events_path() -> PathBuf {
    HookLayout::default_layout().events
}

/// 本工具 hook 脚本的绝对路径（配置里的 marker；扫描范围判定用）。
pub(crate) fn hook_marker() -> String {
    HookLayout::default_layout().marker()
}

/// 该处客户端配置是否需要日志扫描：**数据根存在** 且 该配置未注册本工具 hook。
///
/// 逐来源判定（而不是「整体装了 hook 就不扫日志」）：某处配置注册失败 / 用户手删条目时，
/// 只有那一处回退日志扫描，其余来源继续走 hook。路径与 marker 全部显式传入（单测注入 tempdir）。
pub(crate) fn needs_log_scan(data_root: &Path, settings: &Path, marker: &str) -> bool {
    data_root.is_dir() && !settings_has_marker(settings, marker)
}

// ---------------------------------------------------------------------------
// 配置读写
// ---------------------------------------------------------------------------

fn read_settings(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 条目是否属于本工具：嵌套格式里任一 command hook 的命令包含当前脚本路径。
///
/// `legacy_marker` 只在清理时传入，且必须是本工具旧 `hook.cmd` 的绝对路径；
/// 存在性判定不认旧形态，否则旧条目会挡住新脚本的自动迁移 —— 2026-09-18 实证。
fn entry_is_ours(entry: &Value, marker: &str, legacy_marker: Option<&str>) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("type").and_then(Value::as_str) == Some("command")
                    && hook
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| {
                            command.contains(marker)
                                || legacy_marker.is_some_and(|legacy| command.contains(legacy))
                        })
            })
        })
}

/// 配置里是否已注册本工具的 hook（脚本存在性由调用方另外判定）。
fn config_has_marker(root: &Value, marker: &str) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            HOOK_EVENTS.iter().any(|event| {
                hooks
                    .get(*event)
                    .and_then(Value::as_array)
                    .is_some_and(|entries| entries.iter().any(|e| entry_is_ours(e, marker, None)))
            })
        })
}

/// 指定配置文件的 marker 命中判定。
///
/// 文件缺失 / 非法 JSON / 结构不符一律视为「未注册」：这些情况下事件不会到达，
/// 该来源必须回退日志扫描。
pub(crate) fn settings_has_marker(path: &Path, marker: &str) -> bool {
    read_settings(path).is_some_and(|root| config_has_marker(&root, marker))
}

/// 取出可变的事件数组；结构不符（`hooks` 不是对象 / 事件不是数组）时报错而不是覆盖。
fn event_entries<'a>(root: &'a mut Value, event: &str) -> Result<&'a mut Vec<Value>, String> {
    let Some(object) = root.as_object_mut() else {
        return Err(i18n::tr(
            "The config root is not a JSON object",
            "설정 루트가 JSON 객체가 아닙니다",
        )
        .to_string());
    };
    let hooks = object
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(i18n::tr(
            "`hooks` is not a JSON object",
            "`hooks`가 JSON 객체가 아닙니다",
        )
        .to_string());
    };
    let list = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
    list.as_array_mut().ok_or_else(|| {
        i18n::pick(
            format!("`hooks.{event}` is not an array"),
            format!("`hooks.{event}`가 배열이 아닙니다"),
        )
    })
}

/// 插入（已存在则更新）本工具在某个事件下的条目：先摘掉旧的同源条目，再追加一条。
fn upsert_event(
    root: &mut Value,
    event: &str,
    command: &str,
    marker: &str,
    legacy_marker: Option<&str>,
) -> Result<(), String> {
    let entries = event_entries(root, event)?;
    entries.retain(|entry| !entry_is_ours(entry, marker, legacy_marker));
    entries.push(json!({
        "matcher": "",
        "hooks": [{ "type": "command", "command": command }],
    }));
    Ok(())
}

/// 移除本工具在全部已注册事件下的条目；空数组 / 空 `hooks` 对象一并摘掉。
fn remove_event_entries(root: &mut Value, marker: &str, legacy_marker: Option<&str>) {
    let Some(object) = root.as_object_mut() else {
        return;
    };
    let Some(hooks) = object.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for event in HOOK_EVENTS {
        let Some(list) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        list.retain(|entry| !entry_is_ours(entry, marker, legacy_marker));
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        object.remove("hooks");
    }
}

fn pretty(root: &Value) -> String {
    serde_json::to_string_pretty(root).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 安装 / 卸载 / 状态
// ---------------------------------------------------------------------------

/// hook 安装状态（含三处配置逐项结果）。
pub fn hook_status() -> Value {
    status_at(&HookLayout::default_layout())
}

/// 安装（幂等）：生成脚本 + 在**存在客户端**的配置里注册 `Stop` / `FinalStop`，写前备份。
///
/// 「接入 hook」同时清除用户的「卸载过」标记（恢复默认接入语义）。
pub fn install_hook() -> Result<Value, String> {
    install_and_clear_opt_out(
        &HookLayout::default_layout(),
        &config::rate_limit_config_file(),
    )
}

/// 卸载：移除三处配置里属于本工具的条目（可逐字节还原），并清理本工具生成的脚本。
///
/// 「卸载 hook」同时记下用户的拒绝（`hookOptOut = true`），之后不再自动接入。
pub fn uninstall_hook() -> Result<Value, String> {
    uninstall_and_opt_out(
        &HookLayout::default_layout(),
        &config::rate_limit_config_file(),
    )
}

/// 启动时的默认接入：`enabled && !hookOptOut && 存在任一客户端 && hook 未装全` 才安装。
///
/// 幂等、非阻塞、失败静默（下次启动重试）；返回是否改动了注册状态（调用方据此作废扫描缓存）。
pub fn auto_install_on_startup() -> bool {
    let cfg = config::load_rate_limit_config();
    let enabled = cfg.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let opt_out = cfg
        .get("hookOptOut")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    auto_install_at(&HookLayout::default_layout(), enabled, opt_out)
}

fn auto_install_at(layout: &HookLayout, enabled: bool, opt_out: bool) -> bool {
    if !enabled || opt_out || !layout.any_target_exists() || layout.fully_installed() {
        return false;
    }
    match install_at(layout) {
        Ok(_) => true,
        // 部分目标失败时，只要有一处已经注册，扫描范围就已经变了。
        Err(_) => layout.registered_target_count() > 0,
    }
}

fn status_at(layout: &HookLayout) -> Value {
    let marker = layout.marker();
    let script_exists = layout.script.is_file();
    let mut configured = 0;
    let targets: Vec<Value> = layout
        .targets
        .iter()
        .map(|target| {
            // 不存在的数据根：不参与安装，也不看配置（它根本不该有配置）。
            let exists = target.exists();
            let installed = exists && target.registered(&marker);
            if installed {
                configured += 1;
            }
            json!({
                "label": target.label,
                "path": target.settings.to_string_lossy(),
                "exists": exists,
                "installed": installed,
            })
        })
        .collect();
    json!({
        "scriptPath": layout.script.to_string_lossy(),
        "scriptExists": script_exists,
        "eventsPath": layout.events.to_string_lossy(),
        "installed": script_exists && configured > 0,
        "targets": targets,
    })
}

fn install_at(layout: &HookLayout) -> Result<Value, String> {
    // 一处客户端都没装：什么都不做（不建目录、不写配置、不生成脚本），不是错误。
    if !layout.any_target_exists() {
        return Ok(status_at(layout));
    }
    write_script(layout)?;
    let command = hook_command(&layout.script);
    let marker = layout.marker();
    let legacy_marker = layout.legacy_marker();
    let mut errors = Vec::new();
    for target in &layout.targets {
        // 只对**存在**的客户端写配置：不存在的数据根不创建目录 / 空 settings.json。
        if !target.exists() {
            continue;
        }
        if let Err(error) = install_target(layout, target, &command, &marker, &legacy_marker) {
            errors.push(format!("{}：{error}", target.settings.display()));
        }
    }
    if errors.is_empty() {
        Ok(status_at(layout))
    } else {
        Err(errors.join("；"))
    }
}

/// 写入 / 升级 hook 脚本。
///
/// - 已是当前模板 → 不写（避免重复安装改 mtime）；
/// - 是本工具的已知旧模板 → 升级重写；
/// - 内容未知（用户改过）→ **不覆盖**，返回错误让调用方明确报出。
fn write_script(layout: &HookLayout) -> Result<(), String> {
    match layout.script_ownership() {
        ScriptOwnership::Current => return Ok(()),
        ScriptOwnership::Foreign => {
            return Err(i18n::pick(
                format!(
                    "{} was not generated by this tool (it may have been edited manually); the original file was kept and not overwritten",
                    layout.script.display()
                ),
                format!(
                    "{} 파일은 이 도구가 생성한 스크립트가 아닙니다(직접 수정했을 수 있음). 원본 파일을 그대로 두었으며 덮어쓰지 않았습니다",
                    layout.script.display()
                ),
            ));
        }
        ScriptOwnership::Absent | ScriptOwnership::Outdated => {}
    }
    if let Some(parent) = layout.script.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            i18n::pick(
                format!("Failed to create {}: {error}", parent.display()),
                format!("{} 생성 실패: {error}", parent.display()),
            )
        })?;
    }
    std::fs::write(&layout.script, script_body(&layout.events)).map_err(|error| {
        i18n::pick(
            format!("Failed to write {}: {error}", layout.script.display()),
            format!("{} 쓰기 실패: {error}", layout.script.display()),
        )
    })
}

fn install_target(
    layout: &HookLayout,
    target: &HookTarget,
    command: &str,
    marker: &str,
    legacy_marker: &str,
) -> Result<(), String> {
    let original = std::fs::read_to_string(&target.settings).ok();
    let current = match &original {
        Some(text) => Some(
            serde_json::from_str::<Value>(text).map_err(|_| {
                i18n::tr(
                    "Not valid JSON; skipped without making any changes",
                    "올바른 JSON이 아니어서 건너뛰었습니다(변경 사항 없음)",
                )
                .to_string()
            })?,
        ),
        None => None,
    };
    // 备份基线：本次安装「除了本工具条目之外」的内容。
    let baseline = match &current {
        Some(root) => {
            let mut clean = root.clone();
            remove_event_entries(&mut clean, marker, Some(legacy_marker));
            clean
        }
        None => json!({}),
    };
    refresh_backup(
        layout,
        target,
        original.as_deref(),
        &baseline,
        marker,
        legacy_marker,
    )?;

    let mut root = current.unwrap_or_else(|| json!({}));
    for event in HOOK_EVENTS {
        upsert_event(&mut root, event, command, marker, Some(legacy_marker))?;
    }
    let content = pretty(&root);
    if original.as_deref() == Some(content.as_str()) {
        return Ok(());
    }
    // 数据根已存在（`install_at` 的调用前提），这里不再为「不存在的客户端」补建目录。
    atomic_write(&target.settings, &content).map_err(|error| {
        i18n::pick(
            format!("Failed to write: {error}"),
            format!("쓰기 실패: {error}"),
        )
    })?;
    Ok(())
}

/// 维护备份基线：已有且语义一致就保留；否则记录「当前减去本工具条目」的形态。
///
/// 首次安装（原文件没有我们的 marker）时基线取**原文件原始字节**，卸载可直接逐字节还原。
fn refresh_backup(
    layout: &HookLayout,
    target: &HookTarget,
    original: Option<&str>,
    baseline: &Value,
    marker: &str,
    legacy_marker: &str,
) -> Result<(), String> {
    let backup = layout.backup_path(target);
    if let Some(parent) = backup.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            i18n::pick(
                format!("Failed to create {}: {error}", parent.display()),
                format!("{} 생성 실패: {error}", parent.display()),
            )
        })?;
    }
    let absent = layout.absent_mark_path(target);
    match original {
        None => {
            if !backup.exists() && !absent.exists() {
                std::fs::write(&absent, "").map_err(|error| {
                    i18n::pick(
                        format!("Failed to write the backup marker: {error}"),
                        format!("백업 표시 파일 쓰기 실패: {error}"),
                    )
                })?;
            }
        }
        Some(text) => {
            let raw_is_clean = !text.contains(marker) && !text.contains(legacy_marker);
            if let Ok(existing) = std::fs::read_to_string(&backup) {
                let same = serde_json::from_str::<Value>(&existing).ok().as_ref() == Some(baseline);
                if same {
                    return Ok(());
                }
            }
            let content = if raw_is_clean {
                text.to_string()
            } else {
                pretty(baseline)
            };
            std::fs::write(&backup, content).map_err(|error| {
                i18n::pick(
                    format!("Failed to write the backup: {error}"),
                    format!("백업 쓰기 실패: {error}"),
                )
            })?;
        }
    }
    Ok(())
}

/// 「接入 hook」：安装 + 清除 `hookOptOut`。
///
/// 用户意图先于结果记录：即便本次安装部分失败，也认为用户已重新接受默认接入。
fn install_and_clear_opt_out(layout: &HookLayout, config_path: &Path) -> Result<Value, String> {
    let result = install_at(layout);
    let _ = config::set_rate_limit_hook_opt_out_at(config_path, false);
    result
}

/// 「卸载 hook」：移除注册条目 + 记录 `hookOptOut`（用户拒绝自动接入）。
///
/// 两件事必须一起做，避免「配置已还原但下次启动又被自动装回」的状态不一致。
fn uninstall_and_opt_out(layout: &HookLayout, config_path: &Path) -> Result<Value, String> {
    let result = uninstall_at(layout);
    let _ = config::set_rate_limit_hook_opt_out_at(config_path, true);
    result
}

fn uninstall_at(layout: &HookLayout) -> Result<Value, String> {
    let marker = layout.marker();
    let legacy_marker = layout.legacy_marker();
    let mut errors = Vec::new();
    for target in &layout.targets {
        if let Err(error) = uninstall_target(layout, target, &marker, &legacy_marker) {
            errors.push(format!("{}：{error}", target.settings.display()));
        }
    }
    // 脚本只在本工具生成的那一份（当前模板或已知旧模板）时删除；用户改过就保留，不做猜测。
    if matches!(
        layout.script_ownership(),
        ScriptOwnership::Current | ScriptOwnership::Outdated
    ) {
        let _ = std::fs::remove_file(&layout.script);
    }
    // 旧版 cmd 脚本（2026-09-18 弃用）一并清理，但**必须内容匹配已知生成版本**：
    // 只看 `is_file()` 会把用户改写过的 hook.cmd 当作本工具产物删掉（2026-09-20 审查反例）。
    let legacy_script = layout.script.with_file_name(LEGACY_CMD_NAME);
    if std::fs::read_to_string(&legacy_script).is_ok_and(|content| content == LEGACY_CMD_BODY) {
        let _ = std::fs::remove_file(&legacy_script);
    }
    if errors.is_empty() {
        Ok(status_at(layout))
    } else {
        Err(errors.join("；"))
    }
}

fn uninstall_target(
    layout: &HookLayout,
    target: &HookTarget,
    marker: &str,
    legacy_marker: &str,
) -> Result<(), String> {
    let Some(original) = std::fs::read_to_string(&target.settings).ok() else {
        return Ok(());
    };
    let Ok(mut root) = serde_json::from_str::<Value>(&original) else {
        // 损坏的配置不动：宁可留着 marker，也不覆盖用户（或客户端）写坏的内容。
        return Ok(());
    };
    remove_event_entries(&mut root, marker, Some(legacy_marker));
    let cleaned = pretty(&root);

    if let Ok(bytes) = std::fs::read_to_string(layout.backup_path(target)) {
        if serde_json::from_str::<Value>(&bytes).ok().as_ref() == Some(&root) {
            // 语义与备份一致 = 安装之后没有别的改动 → 直接写回原始字节。
            if bytes != original {
                atomic_write(&target.settings, &bytes).map_err(|error| {
                    i18n::pick(
                        format!("Restore failed: {error}"),
                        format!("복원 실패: {error}"),
                    )
                })?;
            }
            return Ok(());
        }
    } else if layout.absent_mark_path(target).exists() && root == json!({}) {
        // 安装前该配置不存在，卸载后内容为空 → 恢复「不存在」。
        std::fs::remove_file(&target.settings).map_err(|error| {
            i18n::pick(
                format!("Failed to delete the empty config: {error}"),
                format!("빈 설정 삭제 실패: {error}"),
            )
        })?;
        return Ok(());
    }

    if cleaned != original {
        atomic_write(&target.settings, &cleaned).map_err(|error| {
            i18n::pick(
                format!("Failed to write: {error}"),
                format!("쓰기 실패: {error}"),
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> HookLayout {
        let base =
            std::env::temp_dir().join(format!("wb-switch-hook-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&base).expect("临时主目录");
        HookLayout::under(&base)
    }

    /// 路径里带空格 / 中文 / 单引号的临时主目录：这些都是合法且真实存在的用户目录形态。
    fn awkward_layout() -> HookLayout {
        let base = std::env::temp_dir().join(format!(
            "wb switch 中文 o'brien {}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&base).expect("临时主目录");
        HookLayout::under(&base)
    }

    /// 删除临时主目录（`HookLayout` 不持有 Drop，测试结束手动清理）。
    fn cleanup(layout: &HookLayout) {
        if let Some(base) = layout.script.parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(base);
        }
    }

    /// 真正执行生成的脚本：`sh <script>`，把 payload 从 stdin 喂进去，返回（stdout, stderr, 退出码）。
    #[cfg(unix)]
    fn run_script(script: &Path, payload: &str) -> (String, String, i32) {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("sh")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sh 必须可用");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("写入 payload");
        let output = child.wait_with_output().expect("等待脚本结束");
        (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
            output.status.code().unwrap_or(-1),
        )
    }

    fn target<'a>(layout: &'a HookLayout, label: &str) -> &'a HookTarget {
        layout
            .targets
            .iter()
            .find(|target| target.label == label)
            .expect("目标配置必须存在")
    }

    fn write_settings(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().expect("父目录")).expect("配置目录");
        std::fs::write(path, content).expect("写入配置");
    }

    /// 「本机装了该客户端」：创建数据根目录（存在性判据）。
    fn install_client(layout: &HookLayout, label: &str) {
        std::fs::create_dir_all(&target(layout, label).data_root).expect("客户端数据根");
    }

    /// 临时目录下的限额监听配置路径（绝不触碰真实 `~/.wb-switch`）。
    fn config_path(layout: &HookLayout) -> PathBuf {
        layout.events.with_file_name("rate_limit_config.json")
    }

    #[test]
    fn script_body_appends_payload_and_always_returns_empty_object() {
        // 事件路径必须写死为绝对路径：执行器环境不可控，env 不可靠（2026-09-18 实证）。
        let events = Path::new("/tmp/base")
            .join(".wb-switch")
            .join("hook-events.jsonl");
        let body = script_body(&events);
        assert!(body.contains("hook-events.jsonl"), "{body}");
        assert!(!body.contains("USERPROFILE"), "不得依赖环境变量：{body}");
        assert!(!body.contains("$HOME"), "不得依赖环境变量：{body}");
        // 路径必须经 shell_quote 包裹（含空格 / 中文 / 单引号都不能破坏重定向）。
        assert!(
            body.contains(&format!(
                ">> {}",
                shell_quote(&events.to_string_lossy().replace('\\', "/"))
            )),
            "{body}"
        );
        assert!(body.trim_end().ends_with("printf '{}'"), "{body}");
        // 不再注入 `_hookTs`：事件行就是原样 payload（模型归因只认当次 payload）。
        assert!(!body.contains("_hookTs"), "{body}");
        assert!(!body.contains("date"), "{body}");
    }

    #[test]
    fn hook_command_quotes_the_script_path() {
        let interpreter = if cfg!(windows) { "bash" } else { "sh" };
        let path = Path::new("/Users/a b/.wb-switch/hook.sh");
        assert_eq!(
            hook_command(path),
            format!("{interpreter} '/Users/a b/.wb-switch/hook.sh'")
        );
        // 单引号路径按 POSIX 写法转义 —— 裸拼会破坏引号配对，事件被静默丢弃
        // （2026-09-20 审查反例）。
        let quoted = Path::new("/Users/o'brien/.wb-switch/hook.sh");
        assert_eq!(
            hook_command(quoted),
            format!("{interpreter} '/Users/o'\\''brien/.wb-switch/hook.sh'")
        );
    }

    /// marker 必须与注册命令里的路径形态逐字节一致：否则含引号的路径会判定为「未注册」，
    /// 每次启动重复追加条目（2026-09-20 审查反例）。
    #[test]
    fn marker_matches_the_registered_command() {
        let layout = awkward_layout();
        let command = hook_command(&layout.script);
        assert!(
            command.contains(&layout.marker()),
            "marker {} 必须命中命令 {command}",
            layout.marker()
        );
        cleanup(&layout);
    }

    /// Windows 的注册命令用 bash + MSYS 路径（与插件 hook 同款）；
    /// `cmd /c` 在 HookExecutor 的 bash 环境里静默失败（2026-09-18 探针实证）。
    #[test]
    #[cfg(windows)]
    fn hook_command_uses_bash_on_windows() {
        let path = Path::new(r"C:\Users\a b\.wb-switch\hook.sh");
        assert_eq!(hook_command(path), "bash '/c/Users/a b/.wb-switch/hook.sh'");
        let quoted = Path::new(r"C:\Users\o'brien\.wb-switch\hook.sh");
        assert_eq!(
            hook_command(quoted),
            "bash '/c/Users/o'\\''brien/.wb-switch/hook.sh'"
        );
    }

    /// 生成的脚本必须**真正跑得起来**：原样 append payload、stdout 恰好是 `{}`
    /// （客户端据此判定 hook 成功）。只断言脚本文本包含路径是不够的。
    #[cfg(unix)]
    #[test]
    fn generated_script_appends_the_payload_and_returns_empty_object() {
        let layout = awkward_layout();
        // 数据根存在 → `install_at` 会生成脚本并写入三处配置。
        install_client(&layout, "codebuddy");
        install_at(&layout).expect("安装");

        let payload = json!({
            "session_id": "s-1",
            "transcript_path": "/tmp/projects/s-1.jsonl",
            "hook_event_name": "Stop",
            "model": "hy3",
            "last_assistant_message": "429 您的使用量已超出频率限制，将在 2026-09-17 17:59:27 UTC+8 重置",
        })
        .to_string();
        let (stdout, stderr, code) = run_script(&layout.script, &payload);
        assert_eq!(stdout, "{}", "hook 必须回 `{{}}`（空 stdout 会被判失败）");
        assert_eq!(code, 0, "stderr：{stderr}");
        assert!(stderr.is_empty(), "路径转义正确时不该有 stderr：{stderr}");

        let written = std::fs::read_to_string(&layout.events).expect("事件文件必须被写入");
        assert_eq!(written, format!("{payload}\n"), "事件行必须是原样 payload");
        // JSON 保真（引号 / 中文 / 空格都不被 shell 改写）。
        let parsed: Value = serde_json::from_str(written.trim_end()).expect("事件行仍是合法 JSON");
        assert_eq!(parsed["model"], "hy3");

        // 大 payload（429 的完整助手消息可达数百 KB）同样一次写入、单行保真。
        let big = json!({
            "session_id": "s-2",
            "model": "hy3",
            "last_assistant_message": "429 ".repeat(150_000),
        })
        .to_string();
        let (stdout, stderr, code) = run_script(&layout.script, &big);
        assert_eq!(stdout, "{}");
        assert_eq!(code, 0, "stderr：{stderr}");
        let written = std::fs::read_to_string(&layout.events).expect("事件文件");
        assert_eq!(written, format!("{payload}\n{big}\n"));

        cleanup(&layout);
    }

    /// 单引号 / 空格 / 中文路径下安装必须幂等：marker 与注册命令形态一致，不会反复追加条目，
    /// 并且脚本真的能写入事件文件（2026-09-20 审查：单引号路径曾静默丢事件）。
    #[cfg(unix)]
    #[test]
    fn install_is_idempotent_for_awkward_paths_and_the_script_writes_events() {
        let layout = awkward_layout();
        install_client(&layout, "codebuddy");
        write_settings(&target(&layout, "codebuddy").settings, "{}");

        for _ in 0..2 {
            install_at(&layout).expect("安装必须成功");
        }
        let root = read_settings(&target(&layout, "codebuddy").settings).expect("配置");
        assert_eq!(
            root["hooks"]["Stop"].as_array().expect("Stop 数组").len(),
            1,
            "含引号 / 空格的路径不得重复追加条目：{root}"
        );
        assert!(layout.fully_installed());

        let payload = r#"{"session_id":"s-3","model":"hy3"}"#;
        let (stdout, _, _) = run_script(&layout.script, payload);
        assert_eq!(stdout, "{}");
        assert_eq!(
            std::fs::read_to_string(&layout.events).expect("事件文件"),
            format!("{payload}\n")
        );

        cleanup(&layout);
    }

    /// 旧模板升级 / 用户改过的脚本保护：
    /// - 已知旧模板（依赖 `$HOME` 的 sh 脚本）→ 安装时升级为当前模板；
    /// - 内容未知（用户改过）→ 安装不覆盖、卸载不删除，并明确报错。
    #[test]
    fn install_upgrades_known_templates_and_never_overwrites_user_edits() {
        let layout = layout();
        install_client(&layout, "codebuddy");
        std::fs::create_dir_all(layout.script.parent().expect("store")).expect("store 目录");

        // ① 已知旧模板 → 升级。
        std::fs::write(&layout.script, LEGACY_SH_BODY).expect("写旧脚本");
        install_at(&layout).expect("旧模板升级必须成功");
        assert_eq!(
            std::fs::read_to_string(&layout.script).expect("脚本"),
            script_body(&layout.events),
            "已知旧模板必须升级为当前模板"
        );

        // ② 用户改过的脚本 → 不覆盖、不删除，并报出。
        let mine = "#!/bin/sh\necho mine\n";
        std::fs::write(&layout.script, mine).expect("写用户脚本");
        let error = install_at(&layout).expect_err("未知内容不得静默覆盖");
        assert!(error.contains("not overwritten"), "{error}");
        assert_eq!(std::fs::read_to_string(&layout.script).expect("脚本"), mine);
        // 「已接管」的脚本不再触发自动重装。
        assert!(
            !auto_install_at(&layout, true, false),
            "用户改过的脚本按已接管处理，不反复重装"
        );
        uninstall_at(&layout).expect("卸载");
        assert_eq!(
            std::fs::read_to_string(&layout.script).expect("脚本仍应保留"),
            mine
        );
    }

    /// 旧版 `hook.cmd` 只在**内容匹配已知生成版本**时清理；用户改写过的必须保留
    /// （2026-09-20 审查反例：仅凭 `is_file()` 无条件删除）。
    #[test]
    fn legacy_cmd_script_is_removed_only_when_it_matches_the_known_template() {
        let layout = layout();
        let legacy = layout.script.with_file_name(LEGACY_CMD_NAME);
        std::fs::create_dir_all(layout.script.parent().expect("store")).expect("store 目录");

        std::fs::write(&legacy, LEGACY_CMD_BODY).expect("写旧 cmd 脚本");
        uninstall_at(&layout).expect("卸载");
        assert!(!legacy.exists(), "本工具生成的旧 cmd 脚本随卸载清理");

        let mine = "@echo off\r\necho mine\r\n";
        std::fs::write(&legacy, mine).expect("写用户 cmd 脚本");
        uninstall_at(&layout).expect("卸载");
        assert_eq!(
            std::fs::read_to_string(&legacy).expect("用户脚本必须保留"),
            mine
        );

        cleanup(&layout);
    }

    #[test]
    fn legacy_migration_only_removes_the_current_tools_hook_command() {
        let layout = layout();
        let codebuddy = target(&layout, "codebuddy");
        let legacy = format!("cmd /c \"{}\"", layout.legacy_marker());
        let current = hook_command(&layout.script);
        let third_party = format!(
            "cmd /c \"{}-third-party/hook.cmd\"",
            layout.script.parent().expect("store").display()
        );
        write_settings(
            &codebuddy.settings,
            &json!({
                "hooks": {
                    "Stop": [
                        { "matcher": "", "hooks": [{ "type": "command", "command": legacy }] },
                        { "matcher": "", "hooks": [{ "type": "command", "command": third_party }] },
                    ]
                }
            })
            .to_string(),
        );

        install_at(&layout).expect("安装必须成功");

        let root = read_settings(&codebuddy.settings).expect("配置");
        let stop = root["hooks"]["Stop"].as_array().expect("Stop 数组");
        assert_eq!(stop.len(), 2, "旧条目应替换为当前条目，第三方条目保留");
        assert!(
            stop.iter().any(|entry| {
                entry["hooks"][0]["command"].as_str() == Some(third_party.as_str())
            }),
            "第三方 hook 不得被迁移逻辑认领"
        );
        assert!(
            stop.iter()
                .any(|entry| entry["hooks"][0]["command"].as_str() == Some(current.as_str())),
            "本工具旧 hook 应替换为当前命令"
        );

        cleanup(&layout);
    }

    #[test]
    fn install_is_idempotent_and_preserves_third_party_hooks() {
        let layout = layout();
        let codebuddy = target(&layout, "codebuddy");
        write_settings(
            &codebuddy.settings,
            &serde_json::to_string_pretty(&json!({
                "model": "deepseek-v4.1-flash",
                "hooks": {
                    "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": "echo user" }] }],
                    "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo tool" }] }],
                },
            }))
            .expect("序列化"),
        );

        for _ in 0..2 {
            install_at(&layout).expect("安装必须成功");
        }

        let root = read_settings(&codebuddy.settings).expect("配置可解析");
        let stop = root["hooks"]["Stop"].as_array().expect("Stop 数组");
        assert_eq!(stop.len(), 2, "只应追加一条我们的条目：{stop:?}");
        assert_eq!(stop[0]["hooks"][0]["command"], "echo user", "用户条目保留");
        // 全平台统一 sh 脚本名（Windows 执行器是 bash，不再用 cmd）。
        assert!(stop[1]["hooks"][0]["command"]
            .as_str()
            .expect("命令")
            .contains(SCRIPT_NAME_SH));
        assert_eq!(
            root["hooks"]["FinalStop"]
                .as_array()
                .expect("FinalStop 数组")
                .len(),
            1
        );
        assert_eq!(
            root["hooks"]["PreToolUse"]
                .as_array()
                .expect("PreToolUse 数组")
                .len(),
            1,
            "其它事件不得被动"
        );
        assert_eq!(root["model"], "deepseek-v4.1-flash");
        // 未注册的事件一个都不能多。
        assert_eq!(
            root["hooks"].as_object().expect("hooks 对象").len(),
            3,
            "只有 Stop / FinalStop 是本工具新增的"
        );
    }

    #[test]
    fn uninstall_restores_the_settings_file_byte_for_byte() {
        let layout = layout();
        for label in ["codebuddy", "workbuddy", "workbuddy-ai"] {
            let target = target(&layout, label);
            // 故意用非标准格式（紧凑 + 无缩进）验证「逐字节还原」，而不是「重排后相等」。
            write_settings(
                &target.settings,
                r#"{"language":"简体中文","hooks":{"Stop":[{"matcher":"x","hooks":[{"type":"command","command":"echo keep"}]}]},"model":"hy3"}"#,
            );
        }

        install_at(&layout).expect("安装");
        let installed = read_settings(&target(&layout, "codebuddy").settings).expect("已安装");
        assert!(config_has_marker(&installed, &layout.marker()));

        uninstall_at(&layout).expect("卸载");
        for label in ["codebuddy", "workbuddy", "workbuddy-ai"] {
            let target = target(&layout, label);
            assert_eq!(
                std::fs::read_to_string(&target.settings).expect("配置存在"),
                r#"{"language":"简体中文","hooks":{"Stop":[{"matcher":"x","hooks":[{"type":"command","command":"echo keep"}]}]},"model":"hy3"}"#,
                "{label} 必须逐字节还原"
            );
        }
        assert!(!layout.script.is_file(), "本工具生成的脚本在卸载后应清理");
    }

    #[test]
    fn uninstall_keeps_changes_the_user_made_after_install() {
        let layout = layout();
        let codebuddy = target(&layout, "codebuddy");
        write_settings(&codebuddy.settings, "{}");
        install_at(&layout).expect("安装");

        // 安装之后用户（或客户端）改了别的键。
        let mut root = read_settings(&codebuddy.settings).expect("配置");
        root["statusLine"] = json!({ "type": "command" });
        write_settings(&codebuddy.settings, &pretty(&root));

        uninstall_at(&layout).expect("卸载");
        let root = read_settings(&codebuddy.settings).expect("配置");
        assert_eq!(root["statusLine"]["type"], "command", "用户的改动必须保留");
        assert!(
            !config_has_marker(&root, &layout.marker()),
            "本工具条目必须移除"
        );
        assert!(root.get("hooks").is_none(), "空 hooks 键一并摘掉");
    }

    #[test]
    fn install_creates_missing_settings_and_uninstall_removes_them_again() {
        let layout = layout();
        let workbuddy = target(&layout, "workbuddy");
        install_client(&layout, "workbuddy");
        assert!(!workbuddy.settings.exists());

        install_at(&layout).expect("安装");
        let root = read_settings(&workbuddy.settings).expect("新配置可解析");
        assert!(config_has_marker(&root, &layout.marker()));

        uninstall_at(&layout).expect("卸载");
        assert!(
            !workbuddy.settings.exists(),
            "安装前不存在 → 卸载后也不存在"
        );
    }

    /// 不存在的数据根不参与安装：不创建目录、不写配置，状态里 `exists = false`。
    #[test]
    fn install_skips_clients_whose_data_root_is_missing() {
        let layout = layout();
        install_client(&layout, "codebuddy");
        let codebuddy = target(&layout, "codebuddy");
        write_settings(&codebuddy.settings, "{}");

        let status = install_at(&layout).expect("只对已安装的客户端安装，不是错误");
        assert_eq!(status["installed"], json!(true));
        for label in ["workbuddy", "workbuddy-ai"] {
            let target = target(&layout, label);
            assert!(!target.data_root.exists(), "{label} 的数据根不得被创建");
            assert!(!target.settings.exists(), "{label} 不得被写入配置");
        }
        for entry in status["targets"].as_array().expect("targets") {
            let expected = entry["label"] == "codebuddy";
            assert_eq!(entry["exists"], json!(expected), "{entry}");
            assert_eq!(entry["installed"], json!(expected), "{entry}");
        }
    }

    /// 一处客户端都没装：安装是「什么都不做」，不生成脚本、不建目录，也不是错误。
    #[test]
    fn install_without_any_client_does_nothing() {
        let layout = layout();
        let status = install_at(&layout).expect("没有客户端不是错误");
        assert_eq!(status["installed"], json!(false));
        assert_eq!(status["scriptExists"], json!(false), "不生成脚本");
        for target in &layout.targets {
            assert!(!target.exists(), "{} 的数据根不得被创建", target.label);
            assert!(!target.settings.exists(), "{} 不得被写入配置", target.label);
        }
    }

    /// `exists` 只反映数据根存在性（客户端可能还没写过 `settings.json`）。
    #[test]
    fn status_exists_reflects_the_data_root_not_the_settings_file() {
        let layout = layout();
        install_client(&layout, "workbuddy");
        let status = status_at(&layout);
        let workbuddy = status["targets"]
            .as_array()
            .expect("targets")
            .iter()
            .find(|entry| entry["label"] == "workbuddy")
            .expect("workbuddy 目标")
            .clone();
        assert_eq!(workbuddy["exists"], json!(true), "数据根在即视为已安装");
        assert_eq!(workbuddy["installed"], json!(false));
        assert!(!target(&layout, "workbuddy").settings.exists());

        let codebuddy = status["targets"]
            .as_array()
            .expect("targets")
            .iter()
            .find(|entry| entry["label"] == "codebuddy")
            .expect("codebuddy 目标")
            .clone();
        assert_eq!(codebuddy["exists"], json!(false));
        assert_eq!(status["installed"], json!(false), "一处都没注册");
    }

    /// 逐来源扫描判定：数据根存在 且 该处配置未注册 hook 才扫日志。
    ///
    /// 关键场景：某处配置注册失败（非法 JSON / 用户手删条目）时**只有那一处**回退扫描。
    #[test]
    fn log_scan_is_decided_per_source() {
        let layout = layout();
        let marker = layout.marker();
        for label in ["codebuddy", "workbuddy", "workbuddy-ai"] {
            install_client(&layout, label);
        }
        install_at(&layout).expect("安装");

        // 全部注册成功 → 三处都不需要日志扫描。
        for label in ["codebuddy", "workbuddy", "workbuddy-ai"] {
            let target = target(&layout, label);
            assert!(
                !needs_log_scan(&target.data_root, &target.settings, &marker),
                "{label} 已注册 hook 时不该扫日志"
            );
        }

        // 用户手删了 workbuddy-ai 的条目（其余两处仍在）→ 只有这一处回退扫描。
        let ai = target(&layout, "workbuddy-ai");
        write_settings(&ai.settings, "{}");
        assert!(
            needs_log_scan(&ai.data_root, &ai.settings, &marker),
            "条目被删的来源必须回退日志扫描"
        );
        for label in ["codebuddy", "workbuddy"] {
            let target = target(&layout, label);
            assert!(
                !needs_log_scan(&target.data_root, &target.settings, &marker),
                "{label} 不受其它来源影响"
            );
        }

        // 配置写坏（非法 JSON）同样按「未注册」处理 → 该来源回退扫描。
        write_settings(&ai.settings, "{ not json");
        assert!(needs_log_scan(&ai.data_root, &ai.settings, &marker));

        // 数据根不存在 → 不扫描（无论配置长什么样）。
        let missing = layout.script.parent().expect("store").join("missing");
        assert!(!needs_log_scan(
            &missing,
            &missing.join(SETTINGS_FILE_NAME),
            &marker
        ));
    }

    /// 「卸载 = opt-out，接入 = 清除」：配置文件只保留已知字段，且不动 `enabled`。
    #[test]
    fn uninstall_records_opt_out_and_install_clears_it() {
        let layout = layout();
        let config = config_path(&layout);
        install_client(&layout, "codebuddy");
        write_settings(&target(&layout, "codebuddy").settings, "{}");

        // 用户关掉了限额监听（enabled=false），卸载时必须原样保留。
        config::save_rate_limit_config_at(&config, &json!({ "enabled": false })).expect("写入配置");
        uninstall_and_opt_out(&layout, &config).expect("卸载");
        let saved = config::load_rate_limit_config_at(&config);
        assert_eq!(saved["hookOptOut"], json!(true), "卸载即记录拒绝");
        assert_eq!(saved["enabled"], json!(false), "不得改动限额监听开关");
        // 卸载后重启：opt-out 让自动接入直接放弃（不会又被装回）。
        assert!(
            !auto_install_at(&layout, true, true),
            "卸载过就不再自动装回"
        );

        install_and_clear_opt_out(&layout, &config).expect("接入");
        let saved = config::load_rate_limit_config_at(&config);
        assert_eq!(saved["hookOptOut"], json!(false), "接入即恢复默认接入语义");
        assert_eq!(saved["enabled"], json!(false));
        // 重新接入后恢复默认接入：hook 失效（脚本被删）时启动会重新装。
        std::fs::remove_file(&layout.script).expect("删除脚本");
        assert!(
            auto_install_at(&layout, true, false),
            "清除 opt-out 后恢复默认接入"
        );

        // 安装失败也要记下用户意图（否则下次启动又会自动装回）。
        let broken = target(&layout, "workbuddy");
        install_client(&layout, "workbuddy");
        write_settings(&broken.settings, "{ not json");
        install_and_clear_opt_out(&layout, &config).expect_err("非法 JSON 必须报错");
        assert_eq!(
            config::load_rate_limit_config_at(&config)["hookOptOut"],
            json!(false)
        );
        uninstall_and_opt_out(&layout, &config).expect("卸载");
        assert_eq!(
            config::load_rate_limit_config_at(&config)["hookOptOut"],
            json!(true)
        );
    }

    /// 默认接入的前置条件：开关开启 && 未 opt-out && 存在客户端 && hook 未装全。
    #[test]
    fn auto_install_respects_its_preconditions() {
        let layout = layout();

        // ① 一处客户端都没有 → 什么都不做。
        assert!(!auto_install_at(&layout, true, false));
        assert!(!layout.script.exists());
        assert!(!layout.any_target_exists());

        // ② 开关关闭 / 用户已卸载过 → 不安装。
        install_client(&layout, "codebuddy");
        assert!(!auto_install_at(&layout, false, false));
        assert!(!auto_install_at(&layout, true, true));
        assert!(!layout.script.exists(), "前置条件不满足时不得生成脚本");

        // ③ 条件满足 → 自动安装（幂等）。
        assert!(auto_install_at(&layout, true, false));
        assert!(layout.fully_installed());

        // ④ 已装全 → 不再重复动文件。
        assert!(!auto_install_at(&layout, true, false));

        // ⑤ 只装了一半（新客户端在安装之后出现）→ 补齐。
        install_client(&layout, "workbuddy");
        assert!(!layout.fully_installed());
        assert!(auto_install_at(&layout, true, false));
        assert!(layout.fully_installed());

        // ⑥ 脚本被删（hook 失效）→ 重新生成。
        std::fs::remove_file(&layout.script).expect("删除脚本");
        assert!(auto_install_at(&layout, true, false));
        assert!(layout.fully_installed());
    }

    #[test]
    fn malformed_settings_are_left_untouched() {
        let layout = layout();
        let workbuddy = target(&layout, "workbuddy");
        write_settings(&workbuddy.settings, "{ not json");
        let codebuddy = target(&layout, "codebuddy");
        write_settings(&codebuddy.settings, "{}");

        let error = install_at(&layout).expect_err("非法 JSON 必须报错");
        assert!(error.contains("workbuddy"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&workbuddy.settings).expect("原文件仍在"),
            "{ not json",
            "损坏的配置不得被覆盖"
        );
        // 其它目标照常安装（单个失败不阻断）。
        assert!(config_has_marker(
            &read_settings(&codebuddy.settings).expect("配置"),
            &layout.marker()
        ));
    }

    #[test]
    fn malformed_hooks_shape_is_reported_without_overwriting() {
        let layout = layout();
        let codebuddy = target(&layout, "codebuddy");
        write_settings(
            &codebuddy.settings,
            &json!({ "hooks": ["not-an-object"] }).to_string(),
        );
        let error = install_at(&layout).expect_err("`hooks` 不是对象必须报错");
        assert!(error.contains("hooks"), "{error}");
        assert_eq!(
            read_settings(&codebuddy.settings).expect("配置")["hooks"][0],
            "not-an-object"
        );
    }

    #[test]
    fn status_tracks_the_marker_and_the_script() {
        let layout = layout();
        let status = status_at(&layout);
        assert_eq!(status["installed"], json!(false));
        assert_eq!(status["scriptExists"], json!(false));
        assert_eq!(status["targets"].as_array().expect("targets").len(), 3);
        for target in status["targets"].as_array().expect("targets") {
            assert_eq!(target["exists"], json!(false), "{target}");
        }

        for label in ["codebuddy", "workbuddy", "workbuddy-ai"] {
            install_client(&layout, label);
        }
        install_at(&layout).expect("安装");
        let status = status_at(&layout);
        assert_eq!(status["installed"], json!(true));
        assert_eq!(status["scriptExists"], json!(true));
        for target in status["targets"].as_array().expect("targets") {
            assert_eq!(target["installed"], json!(true), "{target}");
        }

        // 配置里手删 marker（模拟客户端升级冲掉了注册）→ 不再视为已安装。
        let codebuddy = target(&layout, "codebuddy");
        std::fs::remove_file(&codebuddy.settings).expect("删除配置");
        assert_eq!(
            status_at(&layout)["installed"],
            json!(true),
            "还有两处配置在"
        );
        for label in ["workbuddy", "workbuddy-ai"] {
            std::fs::remove_file(&target(&layout, label).settings).expect("删除配置");
        }
        assert_eq!(status_at(&layout)["installed"], json!(false));
    }

    #[test]
    fn events_path_lives_in_the_store_directory() {
        assert!(events_path().ends_with(EVENTS_FILE_NAME));
        assert!(events_path().to_string_lossy().contains(STORE_DIR_NAME));
    }
}
