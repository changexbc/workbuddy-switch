//! VS Code 内 CodeBuddy 扩展（`tencent-cloud.coding-copilot`）会话复制。
//!
//! 切换 VS Code CodeBuddy 扩展账号时，可把「当前扩展账号」的会话（正文 + 索引）
//! 复制到「目标账号」目录，使目标账号重新打开 VS Code 后能在对应工作区看到并续聊。
//! 与桌面版 `session.rs` 同构：**本地目录复制 + 新 id 重写 + 合并工作区索引**。
//!
//! 存储布局（Windows 实测）：
//! ```text
//! %LOCALAPPDATA%\CodeBuddyExtension\Data\<uid>\VSCode\<uid>\
//!   history\<md5(工作区)>\
//!     index.json              # 工作区级会话索引 {conversations:[...], current}
//!     .index_bak.json         # 同结构备份
//!     <conversationId>\       # 32 位小写 hex
//!       index.json            # {messages:[...], requests:[...]}
//!       messages\<messageId>.json
//!       assets\*
//! ```
//!
//! 约定与约束：
//! - 会话 / 消息 / 请求 id 一律 **32 位小写 hex**（扩展 `generateId()` 产物），
//!   不得使用带连字符的 UUID。
//! - **只写目标 uid 目录**；源 uid 目录只读，绝不修改或删除。
//! - 显式排除 `default\` / `Public\` 目录（结构不同，见 [`is_safe_uid`]），
//!   uid 只能来自账号库 / 扩展登录态，不接受用户任意输入。
//! - 会话复制本体要求 VS Code 未运行（复用 [`vscode_ext::is_vscode_running`]）；
//!   由 [`switch_vscode_ext_with_copy`] 编排时，`restart = true` 会先自动关闭编辑器再复制。
//! - 单条原子性：先写 `<ws>\.tmp-<newId>\` 再 `rename` 成 `<ws>\<newId>\`；
//!   索引合并用「读-改-写 + `atomic_write`」，写前把将被改的索引备份到
//!   `backup_dir()/vscode-sessions/<utc_iso>/`。
//! - 失败逐条隔离：删临时/成品目录 + 回退该工作区索引，记入 `errors[]`，继续其余条目。
//!
//! macOS / Linux 的数据根**未实测**：按同一相对布局 best-effort 推导
//! （见 [`ext_data_root_candidates`]），仅在对应候选路径真实存在时才使用。

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::modules::account::{self, get_str};
use crate::modules::config::{atomic_write, backup_dir, utc_iso};
use crate::modules::session::{SessionPaths, SyncSelection};
use crate::modules::vscode_ext;
use crate::modules::vscode_session_sync;

/// 扩展数据根目录名（`<平台本地数据根>\CodeBuddyExtension\Data`）。
const EXT_APP_DIR: &str = "CodeBuddyExtension";
/// IDE 类型目录名。
const IDE_DIR: &str = "VSCode";
/// 会话历史目录名。
const HISTORY_DIR: &str = "history";

/// 待复制的会话引用（工作区 hash + 会话 id）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyItem {
    /// 工作区目录名 = `md5(工作区)`（32 位小写 hex）。
    pub workspace_hash: String,
    /// 会话 id（32 位小写 hex）。
    pub conversation_id: String,
}

// ---------------------------------------------------------------------------
// 路径解析
// ---------------------------------------------------------------------------

/// 平台候选的扩展数据根目录列表（按优先级，已去重）。
///
/// - Windows：`%LOCALAPPDATA%\CodeBuddyExtension\Data`（**实测**）。
/// - macOS / Linux：按同一相对布局推导（**未实测**），仅作 best-effort 兜底。
pub fn ext_data_root_candidates() -> Vec<PathBuf> {
    let bases: Vec<PathBuf> = {
        #[cfg(target_os = "windows")]
        {
            [dirs::data_local_dir(), dirs::cache_dir()]
                .into_iter()
                .flatten()
                .collect()
        }
        #[cfg(target_os = "macos")]
        {
            // ~/Library/Application Support 与 ~/Library/Caches（未实测）。
            [dirs::data_dir(), dirs::cache_dir()]
                .into_iter()
                .flatten()
                .collect()
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            // $XDG_DATA_HOME（~/.local/share）与 $XDG_CACHE_HOME（~/.cache）（未实测）。
            [dirs::data_dir(), dirs::cache_dir()]
                .into_iter()
                .flatten()
                .collect()
        }
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for base in bases {
        let candidate = base.join(EXT_APP_DIR).join("Data");
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// 解析扩展数据根目录：返回第一个真实存在的候选目录；都不存在时返回 `None`。
pub fn ext_data_root() -> Option<PathBuf> {
    ext_data_root_candidates()
        .into_iter()
        .find(|path| path.is_dir())
}

/// 账号 uid 的数据目录：`<root>\<uid>\VSCode\<uid>`。
pub fn uid_data_dir(root: &Path, uid: &str) -> PathBuf {
    root.join(uid).join(IDE_DIR).join(uid)
}

/// 账号 uid 的会话历史根：`<root>\<uid>\VSCode\<uid>\history`。
pub fn history_root(root: &Path, uid: &str) -> PathBuf {
    uid_data_dir(root, uid).join(HISTORY_DIR)
}

/// uid 白名单校验：非空、不含路径分隔符 / `..`、且不是 `default` / `Public`。
///
/// 用于杜绝路径穿越与误碰结构不同的兜底目录（`default\` / `Public\`）。
fn is_safe_uid(uid: &str) -> bool {
    let uid = uid.trim();
    !uid.is_empty()
        && uid != "."
        && uid != ".."
        && !uid.contains(['/', '\\'])
        && !uid.eq_ignore_ascii_case("default")
        && !uid.eq_ignore_ascii_case("public")
}

/// 是否为 32 位小写 hex id。
pub(crate) fn is_hex32(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 生成 32 位小写 hex id（对齐扩展 `generateId()` 产物，非带连字符 UUID）。
fn gen_hex32() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// 生成一个此前未出现的 32 位小写 hex id，并登记进 `used`。
fn unique_hex32(used: &mut BTreeSet<String>) -> String {
    loop {
        let candidate = gen_hex32();
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
}

// ---------------------------------------------------------------------------
// 通用小工具
// ---------------------------------------------------------------------------

/// 读取并解析 JSON 文件；文件缺失或内容损坏时返回 `None`（不 panic）。
pub(crate) fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 把索引里的时间字段换算成 epoch 毫秒；无法识别时返回 0。
fn time_to_ms(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::String(text)) => chrono::DateTime::parse_from_rfc3339(text.trim())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(|raw| {
                if raw > 1_000_000_000_000 {
                    raw
                } else {
                    raw.saturating_mul(1000)
                }
            })
            .unwrap_or(0),
        _ => 0,
    }
}

/// 递归复制目录（保持相对结构与文件名，含附件中文名）。
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if path.is_file() {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// 尽力删除目录树（忽略不存在 / 失败）。
pub(crate) fn remove_dir_all_if_exists(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
}

// ---------------------------------------------------------------------------
// 会话枚举
// ---------------------------------------------------------------------------

/// 列出某账号可复制的会话（按工作区 hash 分桶）。
///
/// 返回 `{ sourceUid, sessions:[{id, workspaceHash, title, updatedAt, type, hasHistory}], skipped, dataRoot }`。
/// `skipped` 为无法解析（损坏）的工作区索引数量；`dataRoot` 为解析到的扩展数据根目录
/// （找不到时为 `null`，调用方据此区分「未找到数据目录」与「该账号无会话」）。
pub fn list_vscode_sessions(uid: &str) -> Value {
    match ext_data_root() {
        Some(root) => list_sessions_in(&root, uid),
        None => json!({
            "sourceUid": uid,
            "sessions": [],
            "skipped": 0,
            "dataRoot": Value::Null,
        }),
    }
}

/// [`list_vscode_sessions`] 的可测实现：显式传入数据根目录。
pub fn list_sessions_in(root: &Path, uid: &str) -> Value {
    let mut sessions: Vec<Value> = Vec::new();
    let mut skipped = 0usize;
    let data_root = root.to_string_lossy().to_string();

    if !is_safe_uid(uid) {
        return json!({
            "sourceUid": uid,
            "sessions": sessions,
            "skipped": skipped,
            "dataRoot": data_root,
        });
    }

    let history = history_root(root, uid);
    if let Ok(entries) = std::fs::read_dir(&history) {
        for entry in entries.flatten() {
            let ws_dir = entry.path();
            if !ws_dir.is_dir() {
                continue;
            }
            let workspace_hash = entry.file_name().to_string_lossy().to_string();
            let Some(index) = read_json(&ws_dir.join("index.json")) else {
                skipped += 1;
                continue;
            };
            let Some(conversations) = index.get("conversations").and_then(Value::as_array) else {
                skipped += 1;
                continue;
            };
            for conversation in conversations {
                let Some(id) = conversation
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let title = conversation
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("(无标题)")
                    .to_string();
                let last = time_to_ms(conversation.get("lastMessageAt"));
                let updated_at = if last > 0 {
                    last
                } else {
                    time_to_ms(conversation.get("createdAt"))
                };
                let conv_type = conversation
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let has_history = conversation_has_history(&ws_dir, id);
                sessions.push(json!({
                    "id": id,
                    "workspaceHash": workspace_hash,
                    "title": title,
                    "updatedAt": updated_at,
                    "type": conv_type,
                    "hasHistory": has_history,
                }));
            }
        }
    }

    sessions.sort_by(|left, right| {
        let left_at = left.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
        let right_at = right.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
        right_at.cmp(&left_at)
    });

    json!({
        "sourceUid": uid,
        "sessions": sessions,
        "skipped": skipped,
        "dataRoot": data_root,
    })
}

/// 会话是否含正文（`index.json` 有 messages，或磁盘 `messages/` 下有文件）。
fn conversation_has_history(ws_dir: &Path, conv_id: &str) -> bool {
    let conv_dir = ws_dir.join(conv_id);
    if let Some(index) = read_json(&conv_dir.join("index.json")) {
        if index
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| !messages.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
    }
    match std::fs::read_dir(conv_dir.join("messages")) {
        Ok(entries) => entries.flatten().any(|entry| entry.path().is_file()),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// 会话复制
// ---------------------------------------------------------------------------

/// 把勾选的会话复制到目标账号（新 id，加法）。返回复制报告。
///
/// 前置校验（任一不满足即拒绝，且不写任何文件）：
/// VS Code 必须未运行；目标 uid 合法；源 uid 由当前扩展登录态推导且源 ≠ 目标。
pub fn copy_vscode_sessions(target_uid: &str, items: &[CopyItem]) -> Result<Value, String> {
    if items.is_empty() {
        return Ok(json!({ "copied": [], "errors": [] }));
    }
    if !is_safe_uid(target_uid) {
        return Err("目标账号 uid 非法，拒绝写入".to_string());
    }
    // 写入前必须先完全退出 VS Code，否则会被运行中的扩展覆盖。
    if vscode_ext::is_vscode_running() {
        return Err(
            "检测到 VS Code 正在运行，请先完全退出后再复制会话，否则写入会被 VS Code 覆盖。"
                .to_string(),
        );
    }
    let source_uid = vscode_ext::active_ext_uid()
        .filter(|uid| is_safe_uid(uid))
        .ok_or_else(|| {
            "未检测到 VS Code CodeBuddy 插件当前登录账号，无法定位源会话。请先在 VS Code 中登录该插件后重试。"
                .to_string()
        })?;
    if source_uid == target_uid {
        return Err("源账号与目标账号相同，无需复制会话".to_string());
    }
    let root = ext_data_root()
        .ok_or_else(|| "未找到 VS Code CodeBuddy 插件数据目录，无法复制会话".to_string())?;
    let backup_root = backup_dir().join("vscode-sessions").join(utc_iso());
    copy_sessions_in(&root, &backup_root, &source_uid, target_uid, items)
}

/// [`copy_vscode_sessions`] 的可测实现：显式传入数据根与备份根。
///
/// 逐条隔离：单条失败只回退该条（删临时/成品目录 + 回退该工作区索引），
/// 记入 `errors[]` 后继续处理其余条目，不整体中止。
pub fn copy_sessions_in(
    root: &Path,
    backup_root: &Path,
    source_uid: &str,
    target_uid: &str,
    items: &[CopyItem],
) -> Result<Value, String> {
    if !is_safe_uid(source_uid) {
        return Err("源账号 uid 非法".to_string());
    }
    if !is_safe_uid(target_uid) {
        return Err("目标账号 uid 非法".to_string());
    }
    if source_uid == target_uid {
        return Err("源账号与目标账号相同，无需复制会话".to_string());
    }

    let source_history = history_root(root, source_uid);
    let target_history = history_root(root, target_uid);

    // 先为每个目标工作区建立「磁盘备份 + 内存工作副本」，保证逐条隔离与可回退。
    let mut states: BTreeMap<String, WorkspaceState> = BTreeMap::new();
    for workspace_hash in distinct_workspaces(items) {
        let ws_dir = target_history.join(&workspace_hash);
        let state = WorkspaceState::load(&ws_dir, backup_root, &workspace_hash)
            .map_err(|error| format!("备份目标工作区索引失败（{workspace_hash}）：{error}"))?;
        states.insert(workspace_hash, state);
    }

    let mut copied: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    for item in items {
        let workspace_hash = item.workspace_hash.trim();
        let conversation_id = item.conversation_id.trim();
        if !is_hex32(workspace_hash) || !is_hex32(conversation_id) {
            errors.push(json!({
                "workspaceHash": item.workspace_hash,
                "conversationId": item.conversation_id,
                "error": "工作区或会话 id 非法",
            }));
            continue;
        }
        let source_dir = source_history.join(workspace_hash).join(conversation_id);
        if !source_dir.is_dir() {
            errors.push(json!({
                "workspaceHash": workspace_hash,
                "conversationId": conversation_id,
                "error": "源会话目录不存在",
            }));
            continue;
        }
        let Some(state) = states.get_mut(workspace_hash) else {
            errors.push(json!({
                "workspaceHash": workspace_hash,
                "conversationId": conversation_id,
                "error": "工作区状态缺失",
            }));
            continue;
        };
        let target_dir = target_history.join(workspace_hash);
        match copy_one_conversation(
            &source_dir,
            &target_dir,
            workspace_hash,
            conversation_id,
            state,
        ) {
            Ok(outcome) => copied.push(outcome),
            Err(error) => errors.push(json!({
                "workspaceHash": workspace_hash,
                "conversationId": conversation_id,
                "error": error,
            })),
        }
    }

    let mut report = json!({
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "copied": copied,
        "backup": backup_root.to_string_lossy(),
    });
    if !errors.is_empty() {
        report["errors"] = json!(errors);
    }
    Ok(report)
}

/// 切换 VS Code CodeBuddy 扩展账号，可选「先复制会话」与「把关联会话的新增内容同步过去」。
///
/// 时序（D11 / design §7）：校验目标（账号 / `access_token` / 目录 / db，注定失败的先挡掉、
/// 不关编辑器）→ 关闭（`restart = true` 时；失败即返回，此时编辑器未被触碰）
/// → 复制（本体自带「编辑器未运行」前置，所以关闭必须在它之前）并登记关联
/// → 执行勾选的同步 → 注入 token → 重开。
///
/// 复制与同步都按逐条隔离执行：任一条失败不影响其余条目与后续的账号切换，失败原因写在
/// 各自报告里（`errors` / `linkErrors`），不静默吞掉。若注入失败但已有会话被处理过，
/// 返回明确文案说明残留状态，不做隐式回滚；失败且编辑器是本次我们关闭的 →
/// best-effort 开回来再报错，避免「编辑器关了、会话也没处理成 / 账号也没切成」的双输。
pub fn switch_vscode_ext_with_copy(
    account_id: &str,
    restart: bool,
    items: &[CopyItem],
    sync_selections: &[SyncSelection],
) -> Result<Value, String> {
    if items.is_empty() && sync_selections.is_empty() {
        return vscode_ext::switch_account(account_id, restart);
    }
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let target_uid = get_str(&acc, "uid")
        .ok_or_else(|| "账号缺少 uid，无法定位 VS Code CodeBuddy 插件数据目录".to_string())?;

    // 与 [`vscode_ext::switch_account`] 同序：先把「注定失败」的目标挡在关闭之前，
    // 账号存在但 `access_token` 为空 / 数据目录或 state.vscdb 缺失时不该关掉用户的编辑器。
    vscode_ext::validate_switch_target(account_id)?;

    // 关闭必须在复制与同步之前：两者都会拒绝「VS Code 正在运行」。
    let closed = vscode_ext::close_vscode_for_switch(restart)?;

    let copy_report = if items.is_empty() {
        None
    } else {
        match copy_vscode_sessions(&target_uid, items) {
            Ok(mut report) => {
                // 复制成功即登记「源 ↔ 副本」关联；登记失败不回滚复制，只写进报告（design §5）。
                let link_errors = match ext_data_root() {
                    Some(root) => vscode_session_sync::register_copied_sessions(
                        &root,
                        &SessionPaths::for_vscode_ext(),
                        account::variant_of(&acc),
                        &report,
                    ),
                    None => vec![json!({
                        "error": "未找到 VS Code CodeBuddy 插件数据目录，未能建立会话关联",
                    })],
                };
                if !link_errors.is_empty() {
                    report["linkErrors"] = json!(link_errors);
                }
                Some(report)
            }
            Err(error) => {
                if let Some(closed) = &closed {
                    let _ = vscode_ext::relaunch_closed_editor(closed);
                }
                return Err(error);
            }
        }
    };

    let sync_report = if sync_selections.is_empty() {
        None
    } else {
        // 同步失败不阻断切换：报告形状与成功路径一致，错误挂在 `errors` 里（与 WorkBuddy 同口径）。
        Some(
            match vscode_session_sync::sync_selected(&acc, sync_selections) {
                Ok(report) => report,
                Err(error) => json!({
                    "synced": [],
                    "skipped": [],
                    "errors": [{ "error": error }],
                }),
            },
        )
    };

    let copied = copy_report
        .as_ref()
        .map(|report| count_items(report, "copied"))
        .unwrap_or(0);
    let synced = sync_report
        .as_ref()
        .map(|report| count_items(report, "synced"))
        .unwrap_or(0);

    match vscode_ext::switch_account_after_close(account_id, closed) {
        Ok(mut result) => {
            if let Some(report) = copy_report {
                result["sessionCopy"] = report;
            }
            if let Some(report) = sync_report {
                result["sessionSync"] = report;
            }
            Ok(result)
        }
        Err(error) => {
            let mut done: Vec<String> = Vec::new();
            if copied > 0 {
                done.push(format!("已成功复制 {copied} 个会话到目标账号"));
            }
            if synced > 0 {
                done.push(format!("已成功同步 {synced} 个会话"));
            }
            if done.is_empty() {
                Err(error)
            } else {
                Err(format!(
                    "{error}（注意：{}，但账号切换未完成，请重试切换）",
                    done.join("、")
                ))
            }
        }
    }
}

/// 报告里某一类条目的数量（缺字段按 0）。
fn count_items(report: &Value, key: &str) -> usize {
    report
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

/// 目标工作区的去重集合（仅保留合法的 32 位小写 hex，避免建出无关目录）。
fn distinct_workspaces(items: &[CopyItem]) -> BTreeSet<String> {
    items
        .iter()
        .map(|item| item.workspace_hash.trim().to_string())
        .filter(|workspace_hash| is_hex32(workspace_hash))
        .collect()
}

/// 目标工作区在本次复制期间的状态：磁盘备份 + 内存工作副本。
struct WorkspaceState {
    /// 目标工作区目录（`...\history\<workspaceHash>`）。
    dir: PathBuf,
    /// 进入本次操作前的 `index.json` 原始字节（不存在为 `None`），用于回退。
    original: Option<Vec<u8>>,
    /// 当前工作副本（已合并成功条目）。
    current: Value,
    /// 索引备份落盘目录（`<backup_root>\<workspaceHash>`）。
    backup_dir: PathBuf,
}

impl WorkspaceState {
    /// 加载目标工作区：读取既有索引、备份将被修改的索引、建立内存工作副本。
    ///
    /// **延迟创建**：此处**不**创建目标工作区目录，只有确有会话写入（[`Self::persist`] 或
    /// [`write_conversation`] 落临时目录）时才创建，避免「该工作区所有条目最终都失败」
    /// 时留下空目录。备份目录也仅在确有既有索引需要备份时才落盘。
    fn load(dir: &Path, backup_root: &Path, workspace_hash: &str) -> Result<Self, String> {
        let index_path = dir.join("index.json");
        let original = std::fs::read(&index_path).ok();
        let original_bak = std::fs::read(dir.join(".index_bak.json")).ok();

        let workspace_backup = backup_root.join(workspace_hash);
        if original.is_some() || original_bak.is_some() {
            std::fs::create_dir_all(&workspace_backup).map_err(|error| error.to_string())?;
            if let Some(bytes) = &original {
                std::fs::write(workspace_backup.join("index.json"), bytes)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(bytes) = &original_bak {
                let _ = std::fs::write(workspace_backup.join(".index_bak.json"), bytes);
            }
        }

        let current = match &original {
            Some(bytes) => serde_json::from_slice(bytes).unwrap_or_else(|_| json!({})),
            None => json!({}),
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            original,
            current,
            backup_dir: workspace_backup,
        })
    }

    /// 把当前工作副本写回磁盘（`index.json` + `.index_bak.json`，均用 `atomic_write`）。
    ///
    /// 目标工作区目录在此处按需创建（延迟创建），保证只在确有会话写入时才建目录。
    fn persist(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let text = self.current.to_string();
        atomic_write(&self.dir.join("index.json"), &text).map_err(|error| error.to_string())?;
        atomic_write(&self.dir.join(".index_bak.json"), &text)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// 把工作区索引回退到进入本次操作前的状态（尽力而为）。
    fn restore(&self) {
        match &self.original {
            Some(bytes) => {
                let _ = std::fs::write(self.dir.join("index.json"), bytes);
                if let Ok(backup) = std::fs::read(self.backup_dir.join(".index_bak.json")) {
                    let _ = std::fs::write(self.dir.join(".index_bak.json"), backup);
                }
            }
            None => {
                let _ = std::fs::remove_file(self.dir.join("index.json"));
                let _ = std::fs::remove_file(self.dir.join(".index_bak.json"));
            }
        }
    }
}

/// 会话内 id 重映射计划。
pub(crate) struct RemapPlan {
    /// 旧消息 id → 新消息 id。
    message_ids: BTreeMap<String, String>,
    /// 旧请求 id → 新请求 id。
    request_ids: BTreeMap<String, String>,
    /// 递归精确匹配用的合并表：`message_ids` 优先于 `request_ids`
    /// （与历史 `lookup` 的两表顺序一致）。
    combined_ids: BTreeMap<String, String>,
    /// 参与复制的消息数量（用于报告）。
    message_total: usize,
}

impl RemapPlan {
    /// 依据会话索引与磁盘 `messages/` 目录建立 id 重映射表。
    fn build(source_index: &Value, source_dir: &Path) -> Self {
        let mut message_ids: BTreeMap<String, String> = BTreeMap::new();
        let mut request_ids: BTreeMap<String, String> = BTreeMap::new();
        let mut used: BTreeSet<String> = BTreeSet::new();

        // 消息 id：来自索引 messages[]（索引是权威引用，取值按契约即 hex，不做过滤）
        // 与磁盘 messages/*.json 文件名（只认 32 位小写 hex：目录里的其它 json 如
        // 调试文件既非消息 id，也不该被改名，原样复制）。
        if let Some(messages) = source_index.get("messages").and_then(Value::as_array) {
            for message in messages {
                if let Some(id) = message.get("id").and_then(Value::as_str) {
                    insert_new_id(&mut message_ids, &mut used, id.trim());
                }
            }
        }
        if let Ok(entries) = std::fs::read_dir(source_dir.join("messages")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(stem) = name.strip_suffix(".json") {
                    if is_hex32(stem) {
                        insert_new_id(&mut message_ids, &mut used, stem);
                    }
                }
            }
        }
        // 请求 id：来自索引 requests[]。
        if let Some(requests) = source_index.get("requests").and_then(Value::as_array) {
            for request in requests {
                if let Some(id) = request.get("id").and_then(Value::as_str) {
                    let id = id.trim();
                    if !id.is_empty() && !request_ids.contains_key(id) {
                        request_ids.insert(id.to_string(), unique_hex32(&mut used));
                    }
                }
            }
        }

        Self {
            message_total: message_ids.len(),
            combined_ids: merge_message_first(&message_ids, &request_ids),
            message_ids,
            request_ids,
        }
    }
}

/// 用给定的 id 映射表构造计划：复制路径用随机新 hex，覆盖同步用确定性派生 id。
pub(crate) fn remap_plan_from_maps(
    message_ids: BTreeMap<String, String>,
    request_ids: BTreeMap<String, String>,
) -> RemapPlan {
    RemapPlan {
        message_total: message_ids.len(),
        combined_ids: merge_message_first(&message_ids, &request_ids),
        message_ids,
        request_ids,
    }
}

/// 合并两张 id 映射表：`messages` 优先（同一个旧 id 同时出现在两张表时以消息表为准，
/// 与历史 `lookup` 的先后顺序一致）。仅用于「递归精确匹配」这一类不做区分的遍历。
fn merge_message_first(
    message_ids: &BTreeMap<String, String>,
    request_ids: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut combined = request_ids.clone();
    for (old_id, new_id) in message_ids {
        combined.insert(old_id.clone(), new_id.clone());
    }
    combined
}

/// 为一个旧 id 生成并登记新 id（空串或已登记则跳过）。
fn insert_new_id(map: &mut BTreeMap<String, String>, used: &mut BTreeSet<String>, old_id: &str) {
    if old_id.is_empty() || map.contains_key(old_id) {
        return;
    }
    let new_id = unique_hex32(used);
    map.insert(old_id.to_string(), new_id);
}

/// 复制单个会话：写临时目录 → 原子提交 → 合并目标工作区索引。失败时回退。
fn copy_one_conversation(
    source_dir: &Path,
    target_ws_dir: &Path,
    workspace_hash: &str,
    conversation_id: &str,
    state: &mut WorkspaceState,
) -> Result<Value, String> {
    let source_index = read_json(&source_dir.join("index.json"))
        .ok_or_else(|| "源会话 index.json 缺失或损坏".to_string())?;

    let new_conversation_id = gen_hex32();
    let plan = RemapPlan::build(&source_index, source_dir);

    // 1) 先写临时目录，避免出现「半个会话」。
    let tmp_dir = target_ws_dir.join(format!(".tmp-{new_conversation_id}"));
    remove_dir_all_if_exists(&tmp_dir);
    if let Err(error) = write_conversation(source_dir, &tmp_dir, &source_index, &plan) {
        remove_dir_all_if_exists(&tmp_dir);
        return Err(format!("写入临时会话目录失败：{error}"));
    }

    // 2) 原子提交：rename tmp → <newId>。
    let final_dir = target_ws_dir.join(&new_conversation_id);
    remove_dir_all_if_exists(&final_dir);
    if let Err(error) = std::fs::rename(&tmp_dir, &final_dir) {
        remove_dir_all_if_exists(&tmp_dir);
        return Err(format!("提交会话目录失败：{error}"));
    }

    // 3) 合并目标工作区索引；失败则回退目录与索引（仅本条）。
    let source_ws_index = read_json(&source_dir.parent().unwrap_or(source_dir).join("index.json"));
    let source_entry = source_ws_index
        .as_ref()
        .and_then(|index| find_conversation(index, conversation_id));
    let entry = conversation_entry(source_entry.as_ref(), &new_conversation_id);

    let before = state.current.clone();
    merge_workspace_index(&mut state.current, entry, &new_conversation_id);
    if let Err(error) = state.persist() {
        // 回退本条对索引的改动（保留同工作区其它已成功条目的合并结果）。
        state.current = before;
        if state.persist().is_err() {
            // 二次写入仍失败：磁盘一致性已受损，从备份整体恢复该工作区索引（最后手段）。
            state.restore();
        }
        remove_dir_all_if_exists(&final_dir);
        return Err(format!("合并工作区索引失败：{error}"));
    }

    Ok(json!({
        "workspaceHash": workspace_hash,
        "oldId": conversation_id,
        "newId": new_conversation_id,
        "messages": plan.message_total,
    }))
}

/// 在目标目录写全一个会话：重映射后的 `index.json` + `messages/*` + 其余文件/目录原样复制。
///
/// 覆盖同步（`vscode_session_sync`）复用同一个写入器，只是 id 来源换成确定性派生。
pub(crate) fn write_conversation(
    source_dir: &Path,
    dest_dir: &Path,
    source_index: &Value,
    plan: &RemapPlan,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;

    // 1) 重映射后的会话索引。
    let remapped_index = remap_session_index(source_index, plan);
    std::fs::write(dest_dir.join("index.json"), remapped_index.to_string())?;

    // 2) messages/<oldId>.json → messages/<newId>.json，并重写 id / extra 内的 id 引用。
    let source_messages = source_dir.join("messages");
    if source_messages.is_dir() {
        let target_messages = dest_dir.join("messages");
        std::fs::create_dir_all(&target_messages)?;
        for entry in std::fs::read_dir(&source_messages)?.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let stem = name.strip_suffix(".json").unwrap_or(&name).to_string();
            // 只重写 32 位小写 hex 的消息文件；`messages/` 下的其它 json（调试文件、
            // 扩展自己的锁文件等）一律按字节原样复制——既不改名也不改内容，避免
            // `remap_message_file` 的重新序列化改变其格式。
            if !is_hex32(&stem) {
                std::fs::copy(&path, target_messages.join(&name))?;
                continue;
            }
            let new_name = match plan.message_ids.get(&stem) {
                Some(new_id) => format!("{new_id}.json"),
                None => name.clone(),
            };
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let remapped = remap_message_file(&text, &stem, plan);
                    std::fs::write(target_messages.join(&new_name), remapped)?;
                }
                // 非 UTF-8 或读取失败：按二进制原样复制。
                Err(_) => {
                    std::fs::copy(&path, target_messages.join(&new_name))?;
                }
            }
        }
    }

    // 2b) 会话级 `.index_bak.json`（若存在，结构与 index.json 同）：同样重映射，
    //     避免旧 messageId / requestId 随原样复制残留在目标目录。
    let source_bak = source_dir.join(".index_bak.json");
    if source_bak.is_file() {
        match read_json(&source_bak) {
            Some(bak) => std::fs::write(
                dest_dir.join(".index_bak.json"),
                remap_session_index(&bak, plan).to_string(),
            )?,
            None => {
                std::fs::copy(&source_bak, dest_dir.join(".index_bak.json"))?;
            }
        }
    }

    // 3) 其余顶层文件 / 目录原样复制（跳过已单独处理的索引与 messages；附件保持原文件名）。
    for entry in std::fs::read_dir(source_dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "index.json" || name == ".index_bak.json" || name == "messages" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_dir.join(&name))?;
        } else if path.is_file() {
            std::fs::copy(&path, dest_dir.join(&name))?;
        }
    }
    Ok(())
}

/// 重映射会话索引：`messages[].id`、`requests[].id`、`requests[].messages[]`。
fn remap_session_index(source_index: &Value, plan: &RemapPlan) -> Value {
    let mut out = source_index.clone();
    if let Some(messages) = out.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            if let Some(id) = message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                if let Some(new_id) = plan.message_ids.get(&id) {
                    if let Some(object) = message.as_object_mut() {
                        object.insert("id".to_string(), json!(new_id));
                    }
                }
            }
        }
    }
    if let Some(requests) = out.get_mut("requests").and_then(Value::as_array_mut) {
        for request in requests.iter_mut() {
            let Some(object) = request.as_object_mut() else {
                continue;
            };
            if let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_string) {
                if let Some(new_id) = plan.request_ids.get(&id) {
                    object.insert("id".to_string(), json!(new_id));
                }
            }
            if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
                for message in messages.iter_mut() {
                    if let Some(id) = message.as_str().map(str::to_string) {
                        if let Some(new_id) = plan.message_ids.get(&id) {
                            *message = json!(new_id);
                        }
                    }
                }
            }
        }
    }
    out
}

/// 重映射单条消息文件：`id` 与 `extra` 内的 id 引用（保留其余字段原样）。
fn remap_message_file(text: &str, old_stem: &str, plan: &RemapPlan) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return text.to_string();
    };
    // 文件已按映射表重命名，内部 id 同步为新 id。
    if let Some(new_id) = plan.message_ids.get(old_stem) {
        object.insert("id".to_string(), json!(new_id));
    }
    if let Some(extra) = object.get("extra").cloned() {
        object.insert("extra".to_string(), remap_extra(&extra, plan));
    }
    value.to_string()
}

/// 重映射消息 `extra` 内的 id 引用（兼容字符串化 JSON 与对象两种形态）。
fn remap_extra(extra: &Value, plan: &RemapPlan) -> Value {
    replace_ids_in_extra(extra, &plan.combined_ids)
}

/// id 容器（消息 `extra`）的通用替换：兼容字符串化 JSON 与对象两种形态。
///
/// 顶层与嵌套一视同仁——对容器内所有层级做「精确等于某旧 id」的替换，
/// 因此 `requestId` / `responseId` 与 `tasks` / `sourceContentBlocks` /
/// `selectionContexts` 等嵌套引用走同一条规则，不会误伤正文、`modelId` 等无关字符串。
///
/// 复制路径传「旧 id → 新 hex」，摘要路径传「旧 id → 占位符」（design §3）。
pub(crate) fn replace_ids_in_extra(extra: &Value, map: &BTreeMap<String, String>) -> Value {
    let (mut object, stringified) = match extra {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => (map, true),
            _ => return extra.clone(),
        },
        Value::Object(map) => (map.clone(), false),
        _ => return extra.clone(),
    };
    for value in object.values_mut() {
        replace_exact_ids(value, map);
    }
    let value = Value::Object(object);
    if stringified {
        json!(value.to_string())
    } else {
        value
    }
}

/// 递归遍历任意 JSON 值，把「恰好等于映射表中某个旧 id」的字符串替换为映射值。
pub(crate) fn replace_exact_ids(value: &mut Value, map: &BTreeMap<String, String>) {
    match value {
        Value::String(text) => {
            if let Some(replacement) = map.get(text) {
                *value = json!(replacement);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                replace_exact_ids(item, map);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                replace_exact_ids(item, map);
            }
        }
        _ => {}
    }
}

/// 把新会话条目并入工作区索引（保留 `current` 等字段不变，追加到 `conversations[]`）。
fn merge_workspace_index(index: &mut Value, entry: Value, new_id: &str) {
    if !index.is_object() {
        *index = json!({});
    }
    let Some(object) = index.as_object_mut() else {
        return;
    };
    let conversations = object
        .entry("conversations".to_string())
        .or_insert_with(|| json!([]));
    if !conversations.is_array() {
        *conversations = json!([]);
    }
    if let Some(array) = conversations.as_array_mut() {
        array.push(entry);
    }

    // 保证 `current` 存在且指向索引内真实存在的会话：实测（macOS，扩展 4.12）缺 `current`
    // 的工作区索引会被扩展判定为损坏——改名为 `index.json.corrupted.<ms>` 并重建，同时
    // 多出一条垃圾空会话（见 acceptance.md §3 / F9）。目标原本的 `current` 仍有效则保留，
    // 缺失或悬空时指向本次并入的会话。
    let current_ok = match object.get("current").and_then(Value::as_str) {
        Some(current) => object
            .get("conversations")
            .and_then(Value::as_array)
            .map(|array| {
                array
                    .iter()
                    .any(|item| item.get("id").and_then(Value::as_str) == Some(current))
            })
            .unwrap_or(false),
        None => false,
    };
    if !current_ok {
        object.insert("current".to_string(), json!(new_id));
    }
}

/// 从工作区索引中查找指定会话条目。
fn find_conversation(index: &Value, conversation_id: &str) -> Option<Value> {
    index
        .get("conversations")
        .and_then(Value::as_array)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(conversation_id))
        .cloned()
}

/// 构造并入目标索引的会话条目：优先复用源条目（保留 name/type/时间等），仅换 id。
fn conversation_entry(source_entry: Option<&Value>, new_id: &str) -> Value {
    match source_entry.and_then(Value::as_object) {
        Some(object) => {
            let mut entry = object.clone();
            entry.insert("id".to_string(), json!(new_id));
            Value::Object(entry)
        }
        None => json!({
            "id": new_id,
            "type": "craft",
            "name": "",
            "createdAt": Value::Null,
            "lastMessageAt": Value::Null,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "0123456789abcdef0123456789abcdef";
    const CONV_OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CONV_EXISTING: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const MSG_1: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MSG_2: &str = "cccccccccccccccccccccccccccccccc";
    const REQ_1: &str = "dddddddddddddddddddddddddddddddd";
    const SRC_UID: &str = "uid-src-0001";
    const DST_UID: &str = "uid-dst-0002";

    struct Fixture {
        root: PathBuf,
        backup: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "wb_switch_vscode_{}_{}",
                uuid::Uuid::new_v4().simple(),
                name
            ));
            let root = base.join("Data");
            let backup = base.join("backup");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&backup).unwrap();
            Self { root, backup }
        }

        fn src_ws_dir(&self) -> PathBuf {
            history_root(&self.root, SRC_UID).join(WS)
        }

        fn dst_ws_dir(&self) -> PathBuf {
            history_root(&self.root, DST_UID).join(WS)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(base) = self.root.parent() {
                let _ = std::fs::remove_dir_all(base);
            }
        }
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// 时序（D11 + design §3.1）：目标校验必须在**关闭编辑器之前**失败——
    /// 账号 / `access_token` / 数据目录 / `state.vscdb` 任一不满足时都不许先关编辑器。
    /// 这里用「账号不存在」走纯逻辑分支：`find_account` 先失败，不会触发任何进程操作。
    #[test]
    fn switch_with_copy_validates_before_closing_editor() {
        let missing = format!("no-such-account-{}", uuid::Uuid::new_v4().simple());
        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let error = switch_vscode_ext_with_copy(&missing, true, &items, &[]).unwrap_err();
        assert!(error.contains("账号不存在"), "{error}");
    }

    /// 时序（design §7）：只勾同步（不勾复制）时也走编排 wrapper，
    /// 且目标校验同样必须早于 `close_vscode_for_switch`。
    #[test]
    fn switch_with_sync_only_validates_before_closing_editor() {
        let missing = format!("no-such-account-{}", uuid::Uuid::new_v4().simple());
        let selections = vec![SyncSelection {
            group_id: "group-1".to_string(),
            preview_token: "00000000-0000-4000-8000-000000000000".to_string(),
            mode: crate::modules::session_link::SyncMode::FastForward,
        }];
        let error = switch_vscode_ext_with_copy(&missing, true, &[], &selections).unwrap_err();
        assert!(error.contains("账号不存在"), "{error}");
    }

    fn seed_source(fixture: &Fixture) {
        let ws_dir = fixture.src_ws_dir();
        write(
            &ws_dir.join("index.json"),
            &format!(
                r#"{{"conversations":[{{"id":"{CONV_OLD}","type":"craft","name":"测试会话","createdAt":"2026-09-15T07:47:01.550Z","lastMessageAt":"2026-09-16T05:22:25.406Z","chatMode":"craft"}}],"current":"{CONV_OLD}"}}"#
            ),
        );
        let conv_dir = ws_dir.join(CONV_OLD);
        let conv_index = format!(
            r#"{{"messages":[{{"id":"{MSG_1}","type":"text","role":"user","isComplete":true}},{{"id":"{MSG_2}","type":"text","role":"assistant","isComplete":true}}],"requests":[{{"id":"{REQ_1}","type":"craft","messages":["{MSG_1}","{MSG_2}"],"state":"complete","startedAt":1789532362052}}]}}"#
        );
        write(&conv_dir.join("index.json"), &conv_index);
        // 会话级 `.index_bak.json`：结构与 index.json 同，用于覆盖「备份索引同样重映射」路径。
        write(&conv_dir.join(".index_bak.json"), &conv_index);
        // extra 为字符串化 JSON：
        //  - requestId（→ 请求映射表）；responseId（= 消息自身 id，→ 消息映射表，D1 回归点）；
        //  - 嵌套 tasks / sourceContentBlocks / selectionContexts（内含旧 messageId / requestId，
        //    用于覆盖递归精确匹配重映射）。
        write(
            &conv_dir.join(format!("messages/{MSG_1}.json")),
            &format!(
                r#"{{"role":"user","message":"{{\"role\":\"user\",\"content\":\"你好\"}}","id":"{MSG_1}","extra":"{{\"requestId\":\"{REQ_1}\",\"responseId\":\"{MSG_1}\",\"modelId\":\"deepseek-v4\",\"tasks\":[{{\"messageId\":\"{MSG_2}\",\"status\":\"done\"}}],\"sourceContentBlocks\":[\"{MSG_2}\"],\"selectionContexts\":[{{\"id\":\"{REQ_1}\"}}]}}","createdAt":"2026-09-16T05:05:29.751Z"}}"#
            ),
        );
        write(
            &conv_dir.join(format!("messages/{MSG_2}.json")),
            &format!(
                r#"{{"role":"assistant","message":"{{\"role\":\"assistant\",\"content\":\"在的\"}}","id":"{MSG_2}","extra":"{{\"requestId\":\"{REQ_1}\",\"responseId\":\"{MSG_2}\",\"modelId\":\"deepseek-v4\"}}","createdAt":"2026-09-16T05:05:31.001Z"}}"#
            ),
        );
        std::fs::create_dir_all(conv_dir.join("assets")).unwrap();
        std::fs::write(conv_dir.join("assets/图片.1.jpeg"), b"\x01\x02\x03binary").unwrap();
    }

    fn seed_target_index(fixture: &Fixture) {
        let ws_dir = fixture.dst_ws_dir();
        write(
            &ws_dir.join("index.json"),
            &format!(
                r#"{{"conversations":[{{"id":"{CONV_EXISTING}","type":"craft","name":"已存在"}}],"current":"{CONV_EXISTING}"}}"#
            ),
        );
    }

    #[test]
    fn gen_hex32_is_32_lower_hex_and_unique() {
        let mut seen = BTreeSet::new();
        for _ in 0..64 {
            let id = gen_hex32();
            assert_eq!(id.len(), 32);
            assert!(is_hex32(&id), "not lower hex: {id}");
            assert!(seen.insert(id), "duplicate id generated");
        }
    }

    #[test]
    fn safe_uid_rejects_default_public_and_paths() {
        assert!(is_safe_uid("3d3fbea0abcdef"));
        assert!(!is_safe_uid("default"));
        assert!(!is_safe_uid("Public"));
        assert!(!is_safe_uid("a/b"));
        assert!(!is_safe_uid("a\\b"));
        assert!(!is_safe_uid(".."));
        assert!(!is_safe_uid(""));
    }

    #[test]
    fn list_sessions_in_reports_title_and_history() {
        let fixture = Fixture::new("list");
        seed_source(&fixture);
        let result = list_sessions_in(&fixture.root, SRC_UID);
        let sessions = result.get("sessions").and_then(Value::as_array).unwrap();
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.get("id").and_then(Value::as_str), Some(CONV_OLD));
        assert_eq!(
            session.get("workspaceHash").and_then(Value::as_str),
            Some(WS)
        );
        assert_eq!(
            session.get("title").and_then(Value::as_str),
            Some("测试会话")
        );
        assert_eq!(
            session.get("hasHistory").and_then(Value::as_bool),
            Some(true)
        );
        assert!(session.get("updatedAt").and_then(Value::as_i64).unwrap() > 0);
        // F3：响应带上解析到的数据根目录（找不到根时为 null，由 list_vscode_sessions 兜底）。
        let expected_root = fixture.root.to_string_lossy().to_string();
        assert_eq!(
            result.get("dataRoot").and_then(Value::as_str),
            Some(expected_root.as_str())
        );
    }

    /// F5 回归：`messages/` 下的非 32-hex 文件名原样复制、原样命名、内文 id 不动；
    /// 正常 hex 消息文件仍按映射表重命名并同步内文 id，且不产生多余 hex 文件。
    /// F9 回归：目标工作区原本没有索引时，合并后必须写入 `current`。
    ///
    /// 实测（macOS，扩展 4.12）：缺 `current` 的工作区索引会被扩展判为损坏——改名为
    /// `index.json.corrupted.<ms>` 并重建，同时多出一条垃圾空会话（见 acceptance.md §3）。
    /// 目标原本 `current` 仍有效则保留的场景由
    /// `copy_conversation_remaps_ids_and_merges_index` 覆盖。
    #[test]
    fn copy_into_empty_workspace_writes_current() {
        let fixture = Fixture::new("empty-target-ws");
        seed_source(&fixture);

        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let report = copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items)
            .expect("copy ok");
        let new_conv = report.get("copied").and_then(Value::as_array).unwrap()[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();

        let dst_index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        assert_eq!(
            dst_index.get("current").and_then(Value::as_str),
            Some(new_conv.as_str()),
            "空目标工作区合并后必须写入 current"
        );
        let bak = read_json(&fixture.dst_ws_dir().join(".index_bak.json")).unwrap();
        assert_eq!(
            bak.get("current").and_then(Value::as_str),
            Some(new_conv.as_str())
        );
    }

    #[test]
    fn copy_keeps_non_hex_message_files_untouched() {
        let fixture = Fixture::new("non-hex-files");
        seed_source(&fixture);
        seed_target_index(&fixture);
        let src_conv_dir = fixture.src_ws_dir().join(CONV_OLD);
        // 故意用「带缩进 + 含旧 requestId 引用」的 JSON：若实现对非 hex 文件做解析重写，
        // 字节会变（缩进丢失、id 被替换），下面的字节级断言即会失败。
        let notes = format!(
            "{{\n  \"note\": \"调试笔记，不是消息\",\n  \"extra\": {{ \"requestId\": \"{REQ_1}\" }}\n}}\n"
        );
        let legacy = "{\n  \"kind\": \"legacy-index\"\n}\n".to_string();
        write(&src_conv_dir.join("messages/notes.json"), &notes);
        write(&src_conv_dir.join("messages/index.json"), &legacy);

        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let report = copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items)
            .expect("copy ok");
        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );
        // 报告的消息数量仍等于索引内的消息数（非 hex 文件不计入）。
        assert_eq!(
            report.get("copied").and_then(Value::as_array).unwrap()[0]
                .get("messages")
                .and_then(Value::as_u64),
            Some(2)
        );

        let new_conv = report.get("copied").and_then(Value::as_array).unwrap()[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let target_messages = fixture.dst_ws_dir().join(&new_conv).join("messages");

        // 非 hex 文件：原名存在、内容未改。
        assert_eq!(
            std::fs::read_to_string(target_messages.join("notes.json")).unwrap(),
            notes
        );
        assert_eq!(
            std::fs::read_to_string(target_messages.join("index.json")).unwrap(),
            legacy
        );

        // 目录里恰好 4 个文件：2 个 hex 消息（重命名后）+ 2 个非 hex 原样文件，无多余 hex。
        let names: BTreeSet<String> = std::fs::read_dir(&target_messages)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 4, "目标 messages 目录文件集不符：{names:?}");
        assert!(names.contains("notes.json") && names.contains("index.json"));
        for name in &names {
            if name == "notes.json" || name == "index.json" {
                continue;
            }
            let stem = name.strip_suffix(".json").expect("json 文件名");
            assert!(is_hex32(stem), "消息文件未被重命名为 hex：{name}");
            assert_ne!(stem, MSG_1);
            assert_ne!(stem, MSG_2);
            // 内文 id 与文件名同步。
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(target_messages.join(name)).unwrap())
                    .unwrap();
            assert_eq!(value.get("id").and_then(Value::as_str), Some(stem));
        }

        // 源目录不被改动（非 hex 文件仍在原位、原名）。
        assert_eq!(
            std::fs::read_to_string(src_conv_dir.join("messages/notes.json")).unwrap(),
            notes
        );
    }

    #[test]
    fn copy_conversation_remaps_ids_and_merges_index() {
        let fixture = Fixture::new("copy");
        seed_source(&fixture);
        seed_target_index(&fixture);

        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let report = copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items)
            .expect("copy ok");

        let copied = report.get("copied").and_then(Value::as_array).unwrap();
        assert_eq!(copied.len(), 1);
        assert!(report.get("errors").is_none());
        let new_conv = copied[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_ne!(new_conv, CONV_OLD);
        assert!(is_hex32(&new_conv), "new conversation id not lower hex");
        assert_eq!(
            copied[0].get("oldId").and_then(Value::as_str),
            Some(CONV_OLD)
        );
        assert_eq!(copied[0].get("messages").and_then(Value::as_u64), Some(2));

        // 目标工作区索引：已合并新会话且 `current` 保持不变。
        let dst_index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        let conversations = dst_index
            .get("conversations")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(conversations.len(), 2);
        assert_eq!(
            dst_index.get("current").and_then(Value::as_str),
            Some(CONV_EXISTING)
        );
        let merged = conversations
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(new_conv.as_str()))
            .expect("merged entry");
        assert_eq!(merged.get("name").and_then(Value::as_str), Some("测试会话"));
        assert_eq!(
            merged.get("chatMode").and_then(Value::as_str),
            Some("craft")
        );

        // 会话索引：消息 / 请求 id 全部重映射，requests[].messages[] 与磁盘文件名一致。
        let new_conv_dir = fixture.dst_ws_dir().join(&new_conv);
        let conv_index = read_json(&new_conv_dir.join("index.json")).unwrap();
        let messages = conv_index
            .get("messages")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(messages.len(), 2);
        let mut message_ids: Vec<String> = Vec::new();
        for message in messages {
            let id = message
                .get("id")
                .and_then(Value::as_str)
                .unwrap()
                .to_string();
            assert!(is_hex32(&id));
            assert_ne!(id, MSG_1);
            assert_ne!(id, MSG_2);
            assert!(new_conv_dir.join(format!("messages/{id}.json")).is_file());
            message_ids.push(id);
        }
        let requests = conv_index
            .get("requests")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(requests.len(), 1);
        let new_req = requests[0]
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert!(is_hex32(&new_req));
        assert_ne!(new_req, REQ_1);
        let req_messages: Vec<String> = requests[0]
            .get("messages")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();
        assert_eq!(req_messages, message_ids);

        // 每条消息文件的 extra.requestId 已重映射到新的 request id。
        for id in &message_ids {
            let message = read_json(&new_conv_dir.join(format!("messages/{id}.json"))).unwrap();
            assert_eq!(message.get("id").and_then(Value::as_str), Some(id.as_str()));
            let extra_text = message.get("extra").and_then(Value::as_str).unwrap();
            let extra: Value = serde_json::from_str(extra_text).unwrap();
            assert_eq!(
                extra.get("requestId").and_then(Value::as_str),
                Some(new_req.as_str())
            );
        }

        // 附件按原名复制。
        let asset = new_conv_dir.join("assets/图片.1.jpeg");
        assert!(asset.is_file());
        assert_eq!(std::fs::read(&asset).unwrap(), b"\x01\x02\x03binary");

        // 源目录保持不变。
        assert!(fixture.src_ws_dir().join(CONV_OLD).is_dir());
        assert!(fixture
            .src_ws_dir()
            .join(CONV_OLD)
            .join(format!("messages/{MSG_1}.json"))
            .is_file());
        let src_index = read_json(&fixture.src_ws_dir().join("index.json")).unwrap();
        assert_eq!(
            src_index.get("current").and_then(Value::as_str),
            Some(CONV_OLD)
        );
    }

    #[test]
    fn copy_backup_is_written_to_backup_root() {
        let fixture = Fixture::new("backup");
        seed_source(&fixture);
        seed_target_index(&fixture);

        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items).unwrap();

        let backed_up = read_json(&fixture.backup.join(WS).join("index.json")).unwrap();
        assert_eq!(
            backed_up.get("current").and_then(Value::as_str),
            Some(CONV_EXISTING)
        );
    }

    #[test]
    fn failed_item_leaves_target_untouched_and_records_error() {
        let fixture = Fixture::new("fail");
        seed_source(&fixture);
        seed_target_index(&fixture);
        let original = std::fs::read(fixture.dst_ws_dir().join("index.json")).unwrap();

        let missing = "99999999999999999999999999999999";
        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: missing.to_string(),
        }];
        let report = copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items)
            .expect("returns report");

        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        let errors = report.get("errors").and_then(Value::as_array).unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].get("conversationId").and_then(Value::as_str),
            Some(missing)
        );

        // 目标索引未改动、无残留临时目录、源目录不变。
        let after = std::fs::read(fixture.dst_ws_dir().join("index.json")).unwrap();
        assert_eq!(after, original);
        assert!(std::fs::read_dir(fixture.dst_ws_dir())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".tmp-")));
        assert!(fixture.src_ws_dir().join(CONV_OLD).is_dir());
    }

    #[test]
    fn partial_failure_keeps_successful_copy_and_preserves_current() {
        let fixture = Fixture::new("partial");
        seed_source(&fixture);
        seed_target_index(&fixture);

        let items = vec![
            CopyItem {
                workspace_hash: WS.to_string(),
                conversation_id: CONV_OLD.to_string(),
            },
            CopyItem {
                workspace_hash: WS.to_string(),
                conversation_id: "99999999999999999999999999999999".to_string(),
            },
        ];
        let report =
            copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items).unwrap();
        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );
        assert_eq!(
            report.get("errors").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );

        let dst_index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        assert_eq!(
            dst_index
                .get("conversations")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            dst_index.get("current").and_then(Value::as_str),
            Some(CONV_EXISTING)
        );
    }

    #[test]
    fn rejects_same_source_and_target_uid() {
        let fixture = Fixture::new("same-uid");
        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let error =
            copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, SRC_UID, &items).unwrap_err();
        assert!(error.contains("相同"));
    }

    #[test]
    fn rejects_default_and_public_target_uid() {
        let fixture = Fixture::new("bad-uid");
        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        assert!(
            copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, "default", &items).is_err()
        );
        assert!(
            copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, "Public", &items).is_err()
        );
    }

    /// D1 回归：复制后目标目录「零旧 id 残留」。
    ///
    /// 全目录递归扫描（跳过二进制附件），断言源旧会话 id、全部旧 messageId、全部旧 requestId
    /// 均不出现在目标工作区任何可解析文本文件的内容里；并反向确认 `responseId` 已指向消息新 id。
    #[test]
    fn copied_conversation_leaves_no_legacy_ids_in_target() {
        let fixture = Fixture::new("residue");
        seed_source(&fixture);
        seed_target_index(&fixture);

        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let report = copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items)
            .expect("copy ok");
        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );

        // 核心断言：目标工作区目录下，旧 conversationId / messageId / requestId 零残留。
        assert_no_legacy_ids(&fixture.dst_ws_dir(), &[CONV_OLD, MSG_1, MSG_2, REQ_1]);

        // 反向确认：responseId 已重映射为消息自身的新 id，且 extra 内嵌套 id 也已是新 id。
        let new_conv = report.get("copied").and_then(Value::as_array).unwrap()[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let new_conv_dir = fixture.dst_ws_dir().join(&new_conv);
        let mut new_message_ids: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(new_conv_dir.join("messages"))
            .unwrap()
            .flatten()
        {
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
            let id = value.get("id").and_then(Value::as_str).unwrap().to_string();
            assert!(is_hex32(&id) && id != MSG_1 && id != MSG_2);
            let extra: Value =
                serde_json::from_str(value.get("extra").and_then(Value::as_str).unwrap()).unwrap();
            assert_eq!(
                extra.get("responseId").and_then(Value::as_str),
                Some(id.as_str()),
                "responseId 未重映射为消息自身新 id"
            );
            new_message_ids.push(id);
        }
        assert_eq!(new_message_ids.len(), 2);
    }

    /// D3 回归：某工作区所有条目最终都失败时，不得创建空的目标工作区目录。
    #[test]
    fn all_failed_items_leave_no_target_directory() {
        let fixture = Fixture::new("no-empty-dir");
        seed_source(&fixture); // 仅 seed 源；目标工作区不存在
        assert!(!fixture.dst_ws_dir().exists());

        let items = vec![
            CopyItem {
                workspace_hash: WS.to_string(),
                conversation_id: "bad-id".to_string(),
            },
            CopyItem {
                workspace_hash: WS.to_string(),
                conversation_id: "99999999999999999999999999999999".to_string(),
            },
        ];
        let report = copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items)
            .expect("returns report");
        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        assert_eq!(
            report.get("errors").and_then(Value::as_array).map(Vec::len),
            Some(2)
        );

        assert!(
            !fixture.dst_ws_dir().exists(),
            "所有条目失败时不应创建空的目标工作区目录"
        );
    }

    /// 递归断言：`dir` 下所有可解析为 UTF-8 的文件内容都不含任何 `legacy` id
    /// （二进制附件按约定跳过文件名 / 内容比对）。
    fn assert_no_legacy_ids(dir: &Path, legacy: &[&str]) {
        let files = collect_files(dir);
        assert!(!files.is_empty(), "目标目录为空，扫描无意义");
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue; // 二进制附件：跳过
            };
            for id in legacy {
                assert!(!text.contains(id), "旧 id {id} 残留在 {}", path.display());
            }
        }
    }

    /// 递归收集目录下所有常规文件路径。
    fn collect_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.extend(collect_files(&path));
                } else if path.is_file() {
                    out.push(path);
                }
            }
        }
        out
    }
}
