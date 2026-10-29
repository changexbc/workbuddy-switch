//! 会话列表与按需复制（路径 B：生成新 id，云端可正常同步）。
//!
//! 对照 server.py `current_user_uid` / `list_sessions_for_user` /
//! `_find_project_jsonl` / `copy_session_to_user` / `_register_edge_sync_mapping` /
//! `copy_sessions_for_switch` / `backup_workbuddy_db` / `workbuddy_db_path`。
//!
//! WorkBuddy 5.x 数据三件套（缺一不可）：
//!   1) 正文：`~/.workbuddy/projects/{workspace}/{cid}.jsonl`（JSONL 含 sessionId 字段）
//!   2) 元数据：`~/.workbuddy/workbuddy.db` sessions 表（id = conversation id = UUID）
//!   3) 云端映射：`~/.workbuddy/edge-sync-mapping-v{N}.db` edge_sync_mapping
//!      （文件名版本号由客户端演进，按最大版本号动态发现）
//!      （session_id=conversation_id，msg_channel=convmsg:{uid} 决定云端归属）
//!      **不由本工具写入**：复制只写 1)、2)，映射行由 edge-sync 扩展在客户端下次
//!      启动迁移副本时自行写入（预写会让客户端判定"已迁移"跳过上传，云端缺会话）。
//!
//! 复制收口（design §4）：所有复制入口统一走 [`copy_sessions_for_switch`]，在同一把
//! 档位操作锁内先恢复未完成操作、再查询关联组；同一逻辑会话只保留一个有效副本，
//! 目标 UUID 在任何副本写入前持久化，任一阶段失败都不报告完整成功，恢复复用同一
//! UUID 且不产生第二个副本。
//!
//! 同步契约（design §5 / §6）：[`session_links_preview`] 只读预览双方共同参与的关联组并
//! 下发预览凭据；[`sync_sessions_for_switch`] 在执行前重新加载账号身份、成员、基线与
//! 正文并逐项核对凭据，然后按「备份 → 正文 → 数据库 → 组表」逐个阶段写入。
//!
//! 写入前置条件（design §5.4）：本次同步的备份必须成功且可核验（唯一目录、数据库
//! 一致性快照、目标正文逐文件备份 + 摘要、恢复清单）。备份不可信时**零写入**：不碰
//! 目标正文、不改数据库、不提交基线。任一阶段中断都保留未完成操作，下次切换按同一份
//! 清单补完（复用同一目标 UUID 与新基线引用），不把未完成的写入报告成 `synced`。

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::modules::account;
use crate::modules::auth_file;
use crate::modules::config::{now_ms, store_dir};
use crate::modules::process;
use crate::modules::session_backup::{
    self, BackupLifecycle, CleanupOutcome, CLEANUP_STATE_SAFE_TERMINATED,
    OPERATION_LIFECYCLE_VERSION,
};
use crate::modules::session_link::{
    self, full_digest_of, BaselineState, ContentSnapshot, ContentState, LinkGroup, LinkMember,
    LinkStore, MemberState, NormalizedContent, OpPhase, Operation, OperationMember, PreviewBinding,
    PreviewMemberBinding, RecoveryIssue, RecoveryReport, StoreState, SyncDecision, SyncMode,
    SyncVerdict, NORMALIZATION_VERSION, OPERATION_VERSION,
};
use crate::modules::variant::WbVariant;

/// 关联存储的命名空间：决定关联表 / 基线 / 预览凭据 / 存储锁的名字。
///
/// 三个宿主（WorkBuddy 桌面版、VS Code CodeBuddy 插件、CodeBuddy IDE）共用同一份内核
/// （[`crate::modules::session_link`]），但各自的关联关系互不可见：
/// 同一工具存储根下按命名空间取不同的文件名与目录名，避免互相污染。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkNamespace {
    /// WorkBuddy 桌面版（默认，路径与改造前逐字相同）。
    #[default]
    WorkBuddy,
    /// VS Code CodeBuddy 插件的会话（独立文件名与目录）。
    VscodeExt,
    /// CodeBuddy IDE（国内版桌面客户端）的会话（独立文件名与目录）。
    CodeBuddyIde,
}

/// 会话操作涉及的路径集合：工具存储根（`~/.wb-switch`）与档位数据根。
///
/// 生产入口用 [`SessionPaths::for_variant`]（WorkBuddy）与 [`SessionPaths::for_vscode_ext`]
/// （VS Code 插件）；单测注入临时目录，绝不触碰真实 `~/.wb-switch` 或客户端数据目录。
#[derive(Clone, Debug)]
pub struct SessionPaths {
    /// 工具存储根：关联表、基线、操作日志、锁与备份都在这里。
    pub store_root: PathBuf,
    /// 档位数据根：`projects/`、`workbuddy.db`、`edge-sync-mapping-*.db`。
    /// VS Code 命名空间下不使用该字段（恒为空路径）。
    pub data_root: PathBuf,
    /// 该档位的官方登录态文件（来源账号 uid 的判据）。
    /// VS Code 命名空间下不使用该字段（恒为空路径）。
    pub auth_file: PathBuf,
    /// 关联存储命名空间；决定下面几个 `*_links*` 路径的名字。
    pub link_namespace: LinkNamespace,
}

impl SessionPaths {
    pub fn for_variant(variant: WbVariant) -> Self {
        Self {
            store_root: store_dir(),
            data_root: variant.data_root(),
            auth_file: variant.auth_file_path(),
            link_namespace: LinkNamespace::WorkBuddy,
        }
    }

    /// VS Code CodeBuddy 插件的关联存储路径。
    ///
    /// 只用到 `store_root`：关联表 / 基线 / 预览凭据 / 存储锁都落在 `~/.wb-switch` 下
    /// VS Code 专属的名字里（design §2）；扩展的会话文件由调用方按数据根另行解析，
    /// 不走 `data_root` / `auth_file`（因此两者留空，避免误用）。
    pub fn for_vscode_ext() -> Self {
        Self::for_vscode_ext_at(store_dir())
    }

    /// [`Self::for_vscode_ext`] 的可测实现：显式传入工具存储根。
    pub fn for_vscode_ext_at(store_root: PathBuf) -> Self {
        Self {
            store_root,
            data_root: PathBuf::new(),
            auth_file: PathBuf::new(),
            link_namespace: LinkNamespace::VscodeExt,
        }
    }

    /// CodeBuddy IDE（国内版桌面客户端）的关联存储路径。
    ///
    /// 与 [`Self::for_vscode_ext`] 同构：只用到 `store_root`，会话文件由调用方按数据根另行解析。
    pub fn for_codebuddy_ide() -> Self {
        Self::for_codebuddy_ide_at(store_dir())
    }

    /// [`Self::for_codebuddy_ide`] 的可测实现：显式传入工具存储根。
    pub fn for_codebuddy_ide_at(store_root: PathBuf) -> Self {
        Self {
            store_root,
            data_root: PathBuf::new(),
            auth_file: PathBuf::new(),
            link_namespace: LinkNamespace::CodeBuddyIde,
        }
    }

    pub fn workbuddy_db(&self) -> PathBuf {
        self.data_root.join("workbuddy.db")
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.data_root.join("projects")
    }

    pub fn edge_sync_db(&self, variant: WbVariant) -> PathBuf {
        edge_sync_db_path(&self.data_root, variant)
    }

    pub fn backup_root(&self) -> PathBuf {
        self.store_root.join("backups")
    }

    /// 关联组主表：三个目标各一份，互不可见（design §2）。
    pub fn session_links_file(&self) -> PathBuf {
        match self.link_namespace {
            LinkNamespace::WorkBuddy => self.store_root.join("session_links.json"),
            LinkNamespace::VscodeExt => self.store_root.join("vscode_session_links.json"),
            LinkNamespace::CodeBuddyIde => self.store_root.join("codebuddy_ide_session_links.json"),
        }
    }

    /// 关联存储目录（基线 / 凭据 / 操作日志的父目录）。
    pub fn session_links_dir(&self) -> PathBuf {
        match self.link_namespace {
            LinkNamespace::WorkBuddy => self.store_root.join("session-links"),
            LinkNamespace::VscodeExt => self.store_root.join("vscode-session-links"),
            LinkNamespace::CodeBuddyIde => self.store_root.join("codebuddy-ide-session-links"),
        }
    }

    pub fn baselines_dir(&self) -> PathBuf {
        self.session_links_dir().join("baselines")
    }

    pub fn operations_dir(&self) -> PathBuf {
        self.session_links_dir().join("operations")
    }

    /// 预览凭据目录（design §6：绑定保存在服务端，前端只拿 id）。
    /// 不属于未完成痕迹——凭据是一次性的，主文件缺失时不得据此拒绝初始化。
    pub fn preview_tokens_dir(&self) -> PathBuf {
        self.session_links_dir().join("previews")
    }

    pub fn locks_dir(&self) -> PathBuf {
        self.store_root.join("locks")
    }

    pub fn variant_ops_lock_file(&self, variant: WbVariant) -> PathBuf {
        self.locks_dir()
            .join(format!("session-ops-{}.lock", variant.as_str()))
    }

    /// Long-running client-scoped session operation lock. IDE and VS Code stores are shared
    /// across WorkBuddy variants, so their mutations must serialize across both variants.
    pub fn client_ops_lock_file(&self) -> PathBuf {
        let name = match self.link_namespace {
            LinkNamespace::WorkBuddy => "workbuddy",
            LinkNamespace::VscodeExt => "vscode-ext",
            LinkNamespace::CodeBuddyIde => "codebuddy-ide",
        };
        self.locks_dir().join(format!("session-client-{name}.lock"))
    }

    /// 关联存储的短时全局锁文件：与主表同生命周期，按命名空间分开。
    pub fn link_store_lock_file(&self) -> PathBuf {
        match self.link_namespace {
            LinkNamespace::WorkBuddy => self.locks_dir().join("session-links.lock"),
            LinkNamespace::VscodeExt => self.locks_dir().join("vscode-session-links.lock"),
            LinkNamespace::CodeBuddyIde => {
                self.locks_dir().join("codebuddy-ide-session-links.lock")
            }
        }
    }
}

/// 打开数据库并设置 busy_timeout（对照 Python `sqlite3.connect(timeout=5)`）。
pub(crate) fn open_db(path: &Path, read_only: bool) -> Option<Connection> {
    let conn = if read_only {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?
    } else {
        Connection::open(path).ok()?
    };
    let _ = conn.busy_timeout(Duration::from_secs(5));
    Some(conn)
}

/// 客户端数据根下的会话数据库（按档位取根）。
pub fn workbuddy_db_path(variant: WbVariant) -> PathBuf {
    variant.data_root().join("workbuddy.db")
}

/// 映射库文件名解析：`edge-sync-mapping.db` 记 0，`edge-sync-mapping-vN.db` 记 N
/// （N 为整数）。其它名字（含 `-shm` / `-wal` 伴生文件，它们不以 `.db` 结尾）返回 None。
fn edge_sync_db_version(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let middle = name
        .strip_prefix("edge-sync-mapping")?
        .strip_suffix(".db")?;
    if middle.is_empty() {
        return Some(0);
    }
    let digits = middle.strip_prefix("-v")?;
    // `u64::parse` also accepts a leading `+`; WorkBuddy's filename contract is
    // digits only, so reject non-canonical names instead of treating them as candidates.
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// 没有任何候选时返回的默认文件名（保留各档位既有的「云端映射库 xxx 不存在」文案）。
fn edge_sync_db_default_name(variant: WbVariant) -> &'static str {
    match variant {
        WbVariant::Cn => "edge-sync-mapping-v2.db",
        WbVariant::Ai => "edge-sync-mapping-v4.db",
    }
}

/// 云端映射库解析：扫描数据根下所有 `edge-sync-mapping*.db`，返回版本号最大的一个。
///
/// 写死任何版本都会再次失效：WorkBuddy 客户端自行演进文件名，本机实测 v2（迁移残留）、
/// v3、v4 并存，而 v3 从未出现在本工具任何代码历史里（2026-09 实测）。
/// 判据不用 mtime——本工具自身写入会刷新 mtime，按它选会自我强化错误结果；
/// 也不用行数——需逐个打开数据库，还要处理损坏与锁。两个档位走同一套发现逻辑。
/// 目录不存在、读取失败或没有任何候选时回落到默认文件名，不得 panic。
fn edge_sync_db_path(root: &Path, variant: WbVariant) -> PathBuf {
    let best = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let version = edge_sync_db_version(&path)?;
            path.is_file().then_some((version, path))
        })
        // 同版本号时按路径定序，保证结果与目录遍历顺序无关。
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_path().cmp(b.1.as_path())));
    match best {
        Some((_, path)) => path,
        None => root.join(edge_sync_db_default_name(variant)),
    }
}

/// 会话复制能力探测：数据根同时具备 `projects/` 目录与 `workbuddy.db` 的
/// `sessions` 表才算可用（design D6）。
///
/// 为什么必须探测而不是按档位写死：国际版数据根与国内版**不同构**——本机实测
/// 国际版数据根下没有 `projects/`、edge-sync 为 v4。若直接套用国内版假设，
/// 会写出「有 db 记录但没有正文」的半成品会话。
///
/// 纯函数，接受根路径参数以便用临时目录做单元测试。
pub fn session_copy_supported_at(root: &Path) -> bool {
    if !root.join("projects").is_dir() {
        return false;
    }
    let db = root.join("workbuddy.db");
    if !db.is_file() {
        return false;
    }
    let Some(conn) = open_db(&db, true) else {
        return false;
    };
    table_exists(&conn, "sessions")
}

/// 档位不支持会话复制时的统一错误文案。
pub const SESSION_COPY_UNSUPPORTED: &str = "该档位暂不支持会话复制";

/// 目标账号是当前登录账号、且 WorkBuddy 正在运行时的统一错误文案。
///
/// 写"非当前登录账号"的副本实测免关安全（`research/verification-app-running-write.md`
/// 探针 5）：客户端不使用该账号的数据、切号后自然可见；写"当前登录账号"仍必须等
/// App 停止写入——运行实例看不到写入，继续对话会让会话状态分叉。
pub const SESSION_COPY_APP_RUNNING: &str =
    "目标账号正在 WorkBuddy 中使用，已阻止修改会话数据；请先退出 WorkBuddy 后重试";

/// 目标档客户端运行时的写入门禁：仅当目标账号是该档位**当前登录账号**时拦截。
///
/// 无法读取登录态时保守拦截（宁可要求退出，不做无法判定的写入）。
fn app_running_blocks_target_write(
    paths: &SessionPaths,
    variant: WbVariant,
    target_uid: &str,
    is_app_running: &impl Fn(WbVariant) -> bool,
) -> bool {
    if !is_app_running(variant) {
        return false;
    }
    match current_user_uid_at(&paths.auth_file) {
        Some(current) => current == target_uid,
        None => true,
    }
}

/// 操作日志无法解析时的原因前缀（恢复与复制共用，避免漏报后写出第二个副本）。
const UNPARSEABLE_OPERATION_REASON: &str = "操作记录无法解析";

/// 当前认证账号的 uid（该档位认证文件的 account.uid）。
pub fn current_user_uid(variant: WbVariant) -> Option<String> {
    current_user_uid_at(&variant.auth_file_path())
}

/// 从指定登录态文件读取 uid（单测注入临时文件用）。
pub fn current_user_uid_at(auth_file: &Path) -> Option<String> {
    let auth = auth_file::read_auth_file_at(auth_file)?;
    auth.get("account")
        .and_then(|a| a.get("uid"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        == 1
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(iter) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    let names: Vec<String> = iter.flatten().collect();
    names.iter().any(|name| name == column)
}

fn nonempty_text(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 账号对象里的 uid；前后空白视为缺失。
fn account_uid(account: &Value) -> String {
    account
        .get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// WorkBuddy 侧栏展示名：优先 custom_title（用户改名 / 定时任务名），否则 title。
fn session_display_title(title: Option<String>, custom_title: Option<String>) -> String {
    nonempty_text(custom_title)
        .or_else(|| nonempty_text(title))
        .unwrap_or_else(|| "(无标题)".to_string())
}

/// Claw 是账号绑定的 IM 渠道工作区，复制会话行不够，目标账号也用不了。
fn is_claw_workspace(cwd: &str) -> bool {
    cwd.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("claw"))
}

/// 列出某账号未删除的会话（workbuddy.db sessions 表，db 为准）。
///
/// `title` 为 WorkBuddy 侧栏同款展示名；`isPlayground` 对应侧栏「任务」，
/// 其余按 `cwd` 最后一段归入「空间」。
pub fn list_sessions_for_user(variant: WbVariant, uid: &str) -> Value {
    list_sessions_for_user_at(&SessionPaths::for_variant(variant), uid)
}

/// 指定账号名下的会话列表（会话管理页的源账号视角）。
///
/// 与 `GET /api/sessions` 的返回同形（`sessions / current / variant`），但来源是
/// 显式账号而非登录态；账号缺 uid 时返回空列表 + `current: null`（与现有容错一致）。
pub fn list_sessions_for_account(account: &Value) -> Value {
    let variant = account::variant_of(account);
    let uid = account_uid(account);
    if uid.is_empty() {
        return json!({ "sessions": [], "current": Value::Null, "variant": variant.as_str() });
    }
    json!({
        "sessions": list_sessions_for_user(variant, &uid),
        "current": uid,
        "variant": variant.as_str(),
    })
}

fn list_sessions_for_user_at(paths: &SessionPaths, uid: &str) -> Value {
    let db = paths.workbuddy_db();
    if !db.is_file() {
        return json!([]);
    }
    let Some(conn) = open_db(&db, true) else {
        return json!([]);
    };
    if !table_exists(&conn, "sessions") {
        return json!([]);
    }
    let has_custom = column_exists(&conn, "sessions", "custom_title");
    let has_playground = column_exists(&conn, "sessions", "is_playground");
    let sql = match (has_custom, has_playground) {
        (true, true) => {
            "SELECT id, cwd, title, custom_title, updated_at, is_playground FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
        (true, false) => {
            "SELECT id, cwd, title, custom_title, updated_at, 0 FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
        (false, true) => {
            "SELECT id, cwd, title, NULL, updated_at, is_playground FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
        (false, false) => {
            "SELECT id, cwd, title, NULL, updated_at, 0 FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return json!([]),
    };
    let rows = stmt.query_map([uid], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
        ))
    });

    let mut sessions: Vec<Value> = Vec::new();
    if let Ok(iter) = rows {
        for r in iter.flatten() {
            let (cid, cwd, title, custom_title, updated_at, is_playground) = r;
            let cid = cid.unwrap_or_default();
            let cwd = cwd.unwrap_or_default();
            if is_claw_workspace(&cwd) {
                continue;
            }
            sessions.push(json!({
                "id": cid,
                "title": session_display_title(title, custom_title),
                "cwd": cwd,
                "updatedAt": updated_at.unwrap_or(0),
                "hasHistory": find_project_jsonl(paths, &cid).is_some(),
                "isPlayground": is_playground.unwrap_or(0) != 0,
            }));
        }
    }
    json!(sessions)
}

/// 在 `{档位数据根}/projects/{workspace}/{cid}.jsonl` 定位会话正文。
pub(crate) fn find_project_jsonl(paths: &SessionPaths, cid: &str) -> Option<PathBuf> {
    let projects = paths.projects_dir();
    if !projects.is_dir() {
        return None;
    }
    let direct = projects.join(format!("{cid}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let p = entry.path().join(format!("{cid}.jsonl"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 备份 workbuddy.db（含 -wal/-shm），返回主库备份路径。
///
/// 任何一步失败都返回 Err——不能沿用「忽略 copy 错误后仍宣称备份成功」的旧行为，
/// 备份不可信时后续数据库写入必须先停下来（design §1）。
fn backup_workbuddy_db(paths: &SessionPaths, backup_root: &Path) -> Result<PathBuf, String> {
    let db = paths.workbuddy_db();
    if !db.is_file() {
        return Err("会话数据不存在，未复制".to_string());
    }
    std::fs::create_dir_all(backup_root).map_err(|error| format!("备份目录创建失败：{error}"))?;
    for suffix in ["", "-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{}", db.to_string_lossy(), suffix));
        if !src.is_file() {
            continue;
        }
        let dest = backup_root.join(format!("workbuddy.db{suffix}"));
        std::fs::copy(&src, &dest).map_err(|error| format!("备份 {suffix} 失败：{error}"))?;
        let (src_len, dest_len) = (
            std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0),
            std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
        );
        if src_len != dest_len {
            return Err(format!("备份 {suffix} 校验失败：大小不一致，未复制"));
        }
    }
    Ok(backup_root.join("workbuddy.db"))
}

/// 数据库插入结果：`No*` 与 `SourceRowMissing` 都不允许被当成成功。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbCopyOutcome {
    Inserted,
    SourceRowMissing,
    NoSessionsTable,
    NoDb,
}

/// 会话行归属（未删除时）。
fn session_row_owner(paths: &SessionPaths, cid: &str) -> Option<String> {
    let db = paths.workbuddy_db();
    let conn = open_db(&db, true)?;
    conn.query_row(
        "SELECT user_id FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
        [cid],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// 在 workbuddy.db 中把源会话行复制为新 id（动态列，覆盖 id/user_id/updated_at，
/// created_at 透传源值）。
///
/// 跨档时源行与目标行分属两个库：源行从 `source_paths` 读取（跨档用只读连接），
/// 目标行按**目标表列序取交集**写入——目标存在而源没有的列不写，由库默认值补齐，
/// 不写 NULL 破坏约束（design §2.3；旧版客户端列少时靠这条兼容）。
/// 与旧实现不同：db/表/源行缺失都显式返回，不再静默 Ok。
fn insert_session_copy(
    source_paths: &SessionPaths,
    target_paths: &SessionPaths,
    new_cid: &str,
    cid: &str,
    source_uid: &str,
    target_uid: &str,
) -> Result<DbCopyOutcome, String> {
    let dst_db = target_paths.workbuddy_db();
    if !dst_db.is_file() {
        return Ok(DbCopyOutcome::NoDb);
    }
    let src_db = source_paths.workbuddy_db();
    let same_db = src_db == dst_db;
    let source_row = if src_db.is_file() {
        // 同档沿用读写连接（与改造前一致）；跨档对源档只读，不触碰源档 WAL 恢复。
        let Some(conn) = open_db(&src_db, !same_db) else {
            return Err("会话数据无法打开".to_string());
        };
        if !table_exists(&conn, "sessions") {
            return Ok(DbCopyOutcome::NoSessionsTable);
        }
        read_source_session_row(&conn, cid, source_uid)?
    } else {
        None
    };
    let Some(source_row) = source_row else {
        return Ok(DbCopyOutcome::SourceRowMissing);
    };

    let Some(conn) = open_db(&dst_db, false) else {
        return Err("会话数据无法打开".to_string());
    };
    if !table_exists(&conn, "sessions") {
        return Ok(DbCopyOutcome::NoSessionsTable);
    }
    // 写事务的提交必须可靠持久：在本次实际写连接上确认 synchronous ≥ FULL。
    session_backup::ensure_full_synchronous(&conn)?;
    let target_cols = session_table_columns(&conn)?;

    let mut cols: Vec<String> = Vec::with_capacity(target_cols.len());
    let mut vals: Vec<rusqlite::types::Value> = Vec::with_capacity(target_cols.len());
    for col in &target_cols {
        let Some((_, value)) = source_row.iter().find(|(name, _)| name == col) else {
            continue;
        };
        let value = value.clone();
        if col == "cwd" {
            if let rusqlite::types::Value::Text(ref path) = value {
                if is_claw_workspace(path) {
                    return Err("Claw 工作区绑定当前账号渠道，不支持复制".to_string());
                }
            }
        }
        let value = match col.as_str() {
            "id" => rusqlite::types::Value::Text(new_cid.to_string()),
            "user_id" => rusqlite::types::Value::Text(target_uid.to_string()),
            // created_at 是会话历史起点，复制不伪造：源值原样透传，只有源值为 NULL
            // （旧库没记）时才回退到当前时间，避免把 NULL 下传破坏客户端的时间排序。
            "created_at" => match value {
                rusqlite::types::Value::Null => rusqlite::types::Value::Integer(now_ms()),
                source => source,
            },
            // updated_at 恒写当前时间：它驱动「最近更新」排序，副本沿用源值会直接沉底，
            // 而且副本此刻确实刚被创建。
            "updated_at" => rusqlite::types::Value::Integer(now_ms()),
            "deleted_at" => rusqlite::types::Value::Null,
            _ => value,
        };
        cols.push(col.clone());
        vals.push(value);
    }

    let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let colnames = cols.join(", ");
    // 用 INSERT 而不是 INSERT OR REPLACE：新 UUID 撞库时宁可失败，也不能悄悄覆盖既有会话。
    let sql = format!("INSERT INTO sessions ({colnames}) VALUES ({placeholders})");
    let params: Vec<&rusqlite::types::Value> = vals.iter().collect();
    conn.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(|e| format!("会话记录保存失败：{e}"))?;
    Ok(DbCopyOutcome::Inserted)
}

/// 读取一行会话（列名 + 值，按源表列序），供跨库复制按目标列取交集。
fn read_source_session_row(
    conn: &Connection,
    cid: &str,
    uid: &str,
) -> Result<Option<Vec<(String, rusqlite::types::Value)>>, String> {
    let mut stmt = conn
        .prepare("SELECT * FROM sessions WHERE id = ?1 AND user_id = ?2")
        .map_err(|e| e.to_string())?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt
        .query(rusqlite::params![cid, uid])
        .map_err(|e| e.to_string())?;
    let Some(row) = rows.next().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let values = cols
        .into_iter()
        .enumerate()
        .map(|(i, col)| {
            let value = row
                .get::<_, rusqlite::types::Value>(i)
                .unwrap_or(rusqlite::types::Value::Null);
            (col, value)
        })
        .collect();
    Ok(Some(values))
}

/// 目标 sessions 表的列名（按表自身列序）。
fn session_table_columns(conn: &Connection) -> Result<Vec<String>, String> {
    let stmt = conn
        .prepare("SELECT * FROM sessions LIMIT 0")
        .map_err(|e| e.to_string())?;
    Ok(stmt.column_names().iter().map(|s| s.to_string()).collect())
}

/// 写后校验：目标行必须存在、归属目标账号且未删除。
fn verify_session_row(paths: &SessionPaths, new_cid: &str, target_uid: &str) -> Result<(), String> {
    match session_row_owner(paths, new_cid) {
        Some(owner) if owner == target_uid => Ok(()),
        Some(owner) => Err(format!(
            "会话记录归属校验失败：期望 {target_uid}，实际 {owner}"
        )),
        None => Err("会话记录保存后不可见，未按成功处理".to_string()),
    }
}

/// 云端映射登记结果：失败必须上报，不能静默降级成成功。
/// 把勾选的会话复制到目标账号（路径 B）。返回复制报告。
///
/// 档位以**目标账号**自身为准：数据根、数据库、备份目录、认证文件都取该档位。
/// 国际版能力不满足时直接返回明确错误，绝不写半成品（design D6）。
/// App 正在运行时拒绝写入：独立复制 API 与桌面端共用同一条生命周期保护。
pub fn copy_sessions_for_switch(
    target_acc: &Value,
    session_ids: &[String],
) -> Result<Value, String> {
    let variant = account::variant_of(target_acc);
    let paths = SessionPaths::for_variant(variant);
    copy_sessions_for_switch_at(
        &paths,
        variant,
        target_acc,
        session_ids,
        process::is_workbuddy_running,
    )
}

/// 把勾选的会话从**显式源账号**复制到**显式目标账号**（跨档复制入口，design §2.1）。
///
/// 与 [`copy_sessions_for_switch`] 的差异只在来源判定：源 uid 取自 `source_acc`，
/// 不再从目标档登录态读取；源档与目标档可为不同档位。写入侧仍以目标账号为准
/// （数据根、数据库、映射库、备份目录），源档只读。
pub fn copy_sessions_cross(
    source_acc: &Value,
    target_acc: &Value,
    session_ids: &[String],
) -> Result<Value, String> {
    let source_variant = account::variant_of(source_acc);
    let target_variant = account::variant_of(target_acc);
    let source_paths = SessionPaths::for_variant(source_variant);
    let target_paths = SessionPaths::for_variant(target_variant);
    let source_uid = account_uid(source_acc);
    if source_uid.is_empty() {
        return Err("源账号缺少 uid，无法复制会话".to_string());
    }
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法复制会话".to_string());
    }
    let source = CopySide {
        paths: &source_paths,
        variant: source_variant,
        uid: source_uid,
        account_id: nonempty_text(
            source_acc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
    };
    let target = CopySide {
        paths: &target_paths,
        variant: target_variant,
        uid: target_uid,
        account_id: nonempty_text(
            target_acc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
    };
    copy_sessions_cross_at(
        source,
        target,
        session_ids,
        process::is_workbuddy_running,
        SessionPaths::for_variant,
    )
}

/// 可注入路径与「App 是否运行」探针的跨档复制入口（单测注入临时双档目录与假探针，
/// 不触碰真实路径、不探测真实进程）。
fn copy_sessions_cross_at(
    source: CopySide<'_>,
    target: CopySide<'_>,
    session_ids: &[String],
    is_app_running: impl Fn(WbVariant) -> bool,
    resolve_source_paths: impl Fn(WbVariant) -> SessionPaths,
) -> Result<Value, String> {
    // 快拒只针对**目标档**（写侧）：源档只读，运行中的源客户端不影响读取安全；
    // 目标账号不是当前登录账号时免关直接写（实测，见门禁函数注释）。
    if app_running_blocks_target_write(target.paths, target.variant, &target.uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    // 源档也要读（正文与源行），国际版数据根不同构时同样不支持（design D6）。
    ensure_session_copy_supported(source.paths, source.variant)?;
    ensure_session_copy_supported(target.paths, target.variant)?;
    if source.variant == target.variant && source.uid == target.uid {
        return Err("源账号与目标账号相同，无需复制会话".to_string());
    }
    run_copy(
        &source,
        &target,
        session_ids,
        is_app_running,
        resolve_source_paths,
    )
}

/// 可注入路径与「App 是否运行」探针的同档复制入口（单测注入临时目录与假探针，不触碰
/// 真实路径、不探测真实进程）。
///
/// App 运行检查做两次：拿档位操作锁之前先快速拒绝；拿锁之后再复查——锁前到拿锁之间
/// App 可能被启动，只有锁后复查才能保证会话写入发生在 App 停止写入之后（design §4.1）。
fn copy_sessions_for_switch_at(
    paths: &SessionPaths,
    variant: WbVariant,
    target_acc: &Value,
    session_ids: &[String],
    is_app_running: impl Fn(WbVariant) -> bool,
) -> Result<Value, String> {
    // 探测只对国际版生效（design D6 针对的是国际版数据根不同构）。国内版数据根与
    // 改造前同构，保留改造前的路径与返回结构，不让国内版看到「暂不支持」类新文案。
    ensure_session_copy_supported(paths, variant)?;
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法复制会话".to_string());
    }
    let source_uid = current_user_uid_at(&paths.auth_file)
        .ok_or_else(|| "未读取到本机登录态，无法确定来源账号".to_string())?;
    if source_uid == target_uid {
        return Err("当前账号与目标账号相同，无需复制会话".to_string());
    }
    // 同档路径的源恒为当前登录账号、且上面已拦同账号，因此这里对"目标=当前登录"
    // 恒为放行；保留统一门禁只为与其他写入入口共用同一条规则。
    if app_running_blocks_target_write(paths, variant, &target_uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    let source = CopySide {
        paths,
        variant,
        account_id: account_id_for_uid(paths, &source_uid),
        uid: source_uid,
    };
    let target = CopySide {
        paths,
        variant,
        account_id: nonempty_text(
            target_acc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
        uid: target_uid,
    };
    run_copy(
        &source,
        &target,
        session_ids,
        is_app_running,
        SessionPaths::for_variant,
    )
}

/// 国际版支持性探测的统一封装：只有国际版需要数据根同构检查（design D6）。
fn ensure_session_copy_supported(paths: &SessionPaths, variant: WbVariant) -> Result<(), String> {
    if variant == WbVariant::Ai && !session_copy_supported_at(&paths.data_root) {
        return Err(format!(
            "{SESSION_COPY_UNSUPPORTED}（档位 {}）",
            variant.as_str()
        ));
    }
    Ok(())
}

/// 复制的一方：档位数据根、档位名与账号身份（源侧只读、目标侧写入）。
struct CopySide<'a> {
    paths: &'a SessionPaths,
    variant: WbVariant,
    uid: String,
    /// 账号库中的账号 id（仅作展示，身份判定以 uid 为准）。
    account_id: Option<String>,
}

/// 同档与跨档复制的统一流程：双档锁 → 恢复 → 逐会话复制 → 报告。
///
/// `resolve_source_paths` 用于恢复未完成的**跨档**操作时解析其源档数据根；
/// 同档入口传 [`SessionPaths::for_variant`]，跨档入口可注入与两条 side 一致的映射，
/// 单测由此避免触碰真实目录。
fn run_copy(
    source: &CopySide<'_>,
    target: &CopySide<'_>,
    session_ids: &[String],
    is_app_running: impl Fn(WbVariant) -> bool,
    resolve_source_paths: impl Fn(WbVariant) -> SessionPaths,
) -> Result<Value, String> {
    // 档位操作锁覆盖「恢复 → 查询关联 → 写副本 → 提交关联」全过程，
    // 并发请求与中断重试因此不会各自写出第二个副本。
    // 跨档操作同时持有两档锁，获取顺序固定为枚举序（Cn → Ai），任何代码路径不得
    // 反序，避免两个方向的跨档操作互相等待（design §2.5）。
    let mut _ops_locks = Vec::new();
    for variant in WbVariant::ALL {
        if variant == source.variant {
            _ops_locks.push(session_link::try_acquire_variant_ops_lock(
                source.paths,
                variant,
            )?);
        } else if variant == target.variant {
            _ops_locks.push(session_link::try_acquire_variant_ops_lock(
                target.paths,
                variant,
            )?);
        }
    }
    // 复查：锁前未运行、拿锁后目标账号被登录进 App 的情况在这里被拦住，不写入任何产物。
    if app_running_blocks_target_write(target.paths, target.variant, &target.uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    let recovery = recover_pending_session_operations_at_with(
        target.paths,
        target.variant,
        resolve_source_paths,
    );
    let pending = session_link::pending_operations(target.paths, target.variant);

    let context = CopyContext {
        paths: target.paths,
        variant: target.variant,
        source_paths: source.paths,
        source_variant: source.variant,
        source_uid: &source.uid,
        source_account_id: source.account_id.clone(),
        target_uid: &target.uid,
        target_account_id: target.account_id.clone(),
        pending: &pending,
    };

    let mut copied: Vec<Value> = Vec::new();
    let mut already_linked: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    // 解析失败的操作日志无法对应到具体会话：继续复制会绕过 pending 去重，
    // 可能对同一请求再写出第二个副本。
    if let Some(issue) = recovery
        .needs_recovery
        .iter()
        .find(|issue| !issue.retryable && issue.reason.contains(UNPARSEABLE_OPERATION_REASON))
    {
        for cid in session_ids {
            errors.push(json!({"id": cid, "error": issue.reason.clone()}));
        }
    } else {
        for cid in session_ids {
            match copy_one_session(&context, cid) {
                Ok(CopyOutcome::Copied {
                    new_id,
                    group_id,
                    backup,
                    cleanup_state,
                    cleanup_error,
                }) => {
                    let mut item = json!({
                        "id": cid,
                        "newId": new_id,
                        "groupId": group_id,
                        "backup": backup,
                        "cleanupState": cleanup_state,
                    });
                    if let Some(error) = cleanup_error {
                        item["cleanupError"] = json!(error);
                    }
                    copied.push(item);
                }
                Ok(CopyOutcome::AlreadyLinked {
                    session_id,
                    group_id,
                }) => already_linked.push(json!({
                    "id": cid,
                    "sessionId": session_id,
                    "groupId": group_id,
                })),
                Err(error) => errors.push(json!({"id": cid, "error": error})),
            }
        }
    }

    // 本次请求之后仍存在未完成操作（含本次刚留下的）→ 必须提示恢复需求。
    let unfinished_after = session_link::pending_operations(target.paths, target.variant);
    let unusable = !recovery.is_clean() || !unfinished_after.is_empty();
    let mut report = json!({
        "sourceUid": &source.uid,
        "targetUid": &target.uid,
        "copied": copied,
        "alreadyLinked": already_linked,
    });
    // 跨档复制在报告里带两侧档位；同档报告保持原字段（逐字不变）。
    if source.variant != target.variant {
        report["sourceVariant"] = json!(source.variant.as_str());
        report["targetVariant"] = json!(target.variant.as_str());
    }
    if !errors.is_empty() {
        report["errors"] = json!(errors);
    }
    if unusable {
        report["needsRecovery"] = json!(true);
    }
    // 本轮复制之后再扫一遍：当前项的清理失败/保护残留必须出现在报告里，
    // 不能只用请求开始时的维护快照（否则成功项 pending 只在 copied[] 上）。
    // 维护可能补清成功：成功项上的 pending 路径必须改写成 null，避免虚假可还原位置。
    report["temporaryFiles"] = json!(session_backup::maintain(target.paths, target.variant));
    if let Some(items) = report.get_mut("copied").and_then(Value::as_array_mut) {
        reconcile_reported_cleanup(items);
    }
    Ok(report)
}

/// 复制上下文中不变的输入（避免逐会话重复解析）。
///
/// 源 / 目标解耦：`paths` 恒为目标档（写侧），`source_paths` 恒为源档（读侧）；
/// 同档复制时两者相同，跨档复制时指向不同数据根。
struct CopyContext<'a> {
    paths: &'a SessionPaths,
    variant: WbVariant,
    source_paths: &'a SessionPaths,
    source_variant: WbVariant,
    source_uid: &'a str,
    source_account_id: Option<String>,
    target_uid: &'a str,
    target_account_id: Option<String>,
    pending: &'a [Operation],
}

/// 单个会话的一次复制结果。
enum CopyOutcome {
    Copied {
        new_id: String,
        group_id: String,
        /// 待清理位置（已清理为 None）；仅表示待清理，不是可撤销备份。
        backup: Option<String>,
        cleanup_state: String,
        cleanup_error: Option<String>,
    },
    AlreadyLinked {
        session_id: String,
        group_id: String,
    },
}

/// 目标解析：组内目标账号是否已有可复用的有效副本。
struct TargetResolution {
    group_id: Option<String>,
    existing_link: Option<String>,
}

/// 目标账号在关联组内的 active 成员是否真实有效：正文可验证 + 会话行归属正确。
fn member_is_valid(paths: &SessionPaths, member: &LinkMember) -> bool {
    let Some(body) = find_project_jsonl(paths, &member.session_id) else {
        return false;
    };
    match session_link::read_content_snapshot(&body, &member.session_id) {
        ContentState::Ready(_) => {}
        ContentState::Missing | ContentState::Unavailable(_) => return false,
    }
    session_row_owner(paths, &member.session_id).is_some_and(|owner| owner == member.uid)
}

/// 解析（variant, 来源会话）所属组，以及目标账号是否已有有效副本。
fn resolve_target(context: &CopyContext, cid: &str) -> Result<TargetResolution, String> {
    let store = match session_link::load_store(context.paths) {
        StoreState::Missing => {
            return Ok(TargetResolution {
                group_id: None,
                existing_link: None,
            })
        }
        StoreState::Ready(store) => store,
        StoreState::Unavailable(reason) => {
            return Err(format!("{reason}；已阻止复制"));
        }
    };
    // 身份查找按**源档**（跨档组的源成员记源档位，组级 variant 不参与判定）。
    let Some(group) = session_link::find_group_for_identity(
        &store,
        context.source_variant,
        context.source_uid,
        cid,
    ) else {
        return Ok(TargetResolution {
            group_id: None,
            existing_link: None,
        });
    };
    let group_id = Some(group.id.clone());
    let Some(member) = session_link::active_member_for(group, context.target_uid) else {
        return Ok(TargetResolution {
            group_id,
            existing_link: None,
        });
    };
    if member_is_valid(context.paths, member) {
        return Ok(TargetResolution {
            group_id,
            existing_link: Some(member.session_id.clone()),
        });
    }
    // 失效成员保留记录、不自动复活；本次会重建一个新成员。
    Ok(TargetResolution {
        group_id,
        existing_link: None,
    })
}

/// 目标正文路径：目标档 `projects` 下沿用源文件所在的工作区子目录。
///
/// 客户端按会话行的 `cwd` 推导工作区目录名（跨档实验已验证：同名目录下客户端可读），
/// 而 `cwd` 在复制时原样保留，所以跨档时目录名也必须保持一致（design §2.2）。
/// 同档时 `source_paths == target_paths`，结果与源文件同目录，与改造前逐字相同。
fn target_body_path(
    source_path: &Path,
    source_paths: &SessionPaths,
    target_paths: &SessionPaths,
    new_cid: &str,
) -> PathBuf {
    let file_name = format!("{new_cid}.jsonl");
    let relative = source_path
        .parent()
        .and_then(|parent| parent.strip_prefix(source_paths.projects_dir()).ok())
        .filter(|relative| !relative.as_os_str().is_empty());
    match relative {
        Some(relative) => target_paths.projects_dir().join(relative).join(file_name),
        None => target_paths.projects_dir().join(file_name),
    }
}

/// 写入副本正文并做写后校验（复用同一次源快照，避免 TOCTOU）。
fn write_copy_body(
    snapshot: &ContentSnapshot,
    source_path: &Path,
    source_paths: &SessionPaths,
    target_paths: &SessionPaths,
    cid: &str,
    new_cid: &str,
) -> Result<PathBuf, String> {
    let dest = target_body_path(source_path, source_paths, target_paths, new_cid);
    if dest.exists() {
        return Err("目标内容已存在同名文件，已停止复制".to_string());
    }
    // 跨档时目标档可能没有该工作区目录（源目录名跨档一致，见 `target_body_path`）。
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("目标工作区目录创建失败：{error}"))?;
    }
    let text = snapshot.text.replace(cid, new_cid);
    // 正文属于业务完成门禁：会话专用持久化写（sync_all + 父目录持久化）。
    session_backup::durable_write_str(&dest, &text)
        .map_err(|error| format!("复制后的内容保存失败：{error}"))?;
    match session_link::read_content_snapshot(&dest, new_cid) {
        ContentState::Ready(read_back)
            if read_back.normalized.total_digest == snapshot.normalized.total_digest =>
        {
            Ok(dest)
        }
        ContentState::Ready(_) => Err("复制后的内容保存后校验不一致，未按成功处理".to_string()),
        ContentState::Missing => Err("复制后的内容保存后不存在，未按成功处理".to_string()),
        ContentState::Unavailable(reason) => Err(format!("复制后的内容保存后无法确认：{reason}")),
    }
}

/// 首次使用时先落地空的关联存储。
///
/// 保证「操作日志出现」一定晚于「主文件存在」，否则刚预分配的操作日志会被
/// `load_store` 的残留痕迹规则误判成「主文件缺失但残留未完成操作」而自锁。
/// 存储损坏/未知版本/权限失败时同样在这里拒绝，绝不降级成空表。
fn ensure_link_store_ready(paths: &SessionPaths) -> Result<(), String> {
    match session_link::load_store(paths) {
        StoreState::Missing => {
            session_link::with_link_store_write(paths, |_| Ok(()))?;
            Ok(())
        }
        StoreState::Ready(_) => Ok(()),
        StoreState::Unavailable(reason) => Err(format!("{reason}；已阻止复制")),
    }
}

/// 复制单个会话：预分配 UUID → 持久化操作 → 正文 → 数据库 → 映射 → 关联/基线。
fn copy_one_session(context: &CopyContext, cid: &str) -> Result<CopyOutcome, String> {
    let paths = context.paths;
    // 上一次未完成的同一请求：只复用，不新建第二个副本。
    if let Some(operation) = session_link::find_pending_operation(
        context.pending,
        context.source_uid,
        cid,
        context.target_uid,
    ) {
        return Err(format!(
            "上一次复制尚未完成（操作 {}）：{}，这次不会重复创建",
            operation.operation_id,
            operation
                .last_error
                .clone()
                .unwrap_or_else(|| "等待恢复".to_string())
        ));
    }

    // 源正文与源行都在源档（跨档时与目标档不同）。
    let Some(source_path) = find_project_jsonl(context.source_paths, cid) else {
        return Err("会话内容不存在，未复制".to_string());
    };
    let snapshot = match session_link::read_content_snapshot(&source_path, cid) {
        ContentState::Ready(snapshot) => snapshot,
        ContentState::Missing => return Err("会话内容不存在，未复制".to_string()),
        ContentState::Unavailable(reason) => {
            return Err(format!("会话内容无法验证（{reason}），未复制"));
        }
    };
    let source_owner = session_row_owner(context.source_paths, cid);
    match source_owner.as_deref() {
        Some(owner) if owner == context.source_uid => {}
        Some(_) => return Err("源会话不属于当前账号，未复制".to_string()),
        None => return Err("数据库中找不到源会话记录，未复制".to_string()),
    }

    let resolution = resolve_target(context, cid)?;
    if let Some(session_id) = resolution.existing_link {
        return Ok(CopyOutcome::AlreadyLinked {
            session_id,
            group_id: resolution.group_id.unwrap_or_default(),
        });
    }

    // 预分配身份：维护记录（allocating）先于任何备份与业务写入（design §3）。
    // 任何副本写入之前，生命周期身份必须已可靠落盘，恢复/补清理才有依据。
    let new_cid = uuid::Uuid::new_v4().to_string();
    let group_id = resolution
        .group_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut lifecycle = session_backup::begin_operation(
        paths,
        context.variant,
        OPERATION_KIND_COPY,
        Some(new_cid.clone()),
        None,
    )?;
    let backup_dir =
        session_backup::transaction_dir(paths, context.variant, &lifecycle.operation_id)?;
    let backup = match backup_workbuddy_db(paths, &backup_dir) {
        Ok(backup) => backup,
        Err(error) => {
            // 受控失败分支：契约保证 protected 之前不发生业务写入，可安全回收残留。
            session_backup::reclaim_unwritten(paths, context.variant, &mut lifecycle, &error);
            return Err(error);
        }
    };
    // 备份完备先转 protected：此步失败禁止任何业务写入，残留保守保留待下次维护。
    session_backup::mark_protected(paths, &mut lifecycle)?;
    ensure_link_store_ready(paths)?;

    let mut operation = Operation {
        version: OPERATION_VERSION,
        operation_id: lifecycle.operation_id.clone(),
        kind: OPERATION_KIND_COPY.to_string(),
        variant: context.variant,
        // 同档复制写 None（与旧记录逐字兼容）；跨档复制显式记录源档，恢复时据此读源。
        source_variant: (context.source_variant != context.variant)
            .then_some(context.source_variant),
        group_id: group_id.clone(),
        source: OperationMember {
            account_id: context.source_account_id.clone(),
            uid: context.source_uid.to_string(),
            session_id: cid.to_string(),
        },
        target: OperationMember {
            account_id: context.target_account_id.clone(),
            uid: context.target_uid.to_string(),
            session_id: new_cid.clone(),
        },
        expected_content_digest: snapshot.normalized.total_digest.clone(),
        expected_record_count: snapshot.normalized.record_count,
        phase: OpPhase::Prepared,
        backup: Some(backup.to_string_lossy().to_string()),
        lifecycle_version: Some(OPERATION_LIFECYCLE_VERSION),
        cleanup_state: None,
        last_error: None,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    session_link::save_operation(paths, &operation)?;

    if let Err(error) = finish_copy_from_body(
        paths,
        context.variant,
        context.source_paths,
        &mut operation,
        &snapshot,
        Some(&source_path),
    ) {
        fail_operation(paths, &mut operation, &error);
        return Err(error);
    }
    let _ = session_link::prune_operations(
        paths,
        context.variant,
        session_link::KEEP_COMPLETED_OPERATIONS,
    );
    // 业务可靠完成之后才授权清理；清理失败不回滚业务、不报告复制失败。
    let cleanup = finish_backup_cleanup(paths, context.variant, &mut lifecycle);
    let (backup, cleanup_state, cleanup_error) = report_cleanup(&cleanup, &backup_dir);
    Ok(CopyOutcome::Copied {
        new_id: new_cid,
        group_id,
        backup,
        cleanup_state: cleanup_state.to_string(),
        cleanup_error,
    })
}

/// 业务完成后的收尾：可靠转 cleanupPending 再回收；失败只报告，不改业务结果。
fn finish_backup_cleanup(
    paths: &SessionPaths,
    variant: WbVariant,
    lifecycle: &mut BackupLifecycle,
) -> CleanupOutcome {
    if let Err(error) = session_backup::mark_cleanup_pending(paths, lifecycle, None) {
        // 业务已完成、维护记录仍是 protected：下次维护按 Completed 补转后清理。
        return CleanupOutcome::Pending {
            reason: format!("清理状态推进失败（{error}）"),
        };
    }
    session_backup::cleanup_after_success(paths, variant, lifecycle)
}

/// 维护入口补清成功后，把仍写着 pending 路径的成功项改成 cleaned / null。
///
/// `symlink_metadata` 把符号链接也视为「还在」，避免把拒绝删除的链接目标标成已清理。
fn reconcile_reported_cleanup(items: &mut [Value]) {
    for item in items {
        if item.get("cleanupState").and_then(Value::as_str) != Some("pending") {
            continue;
        }
        let still_there = item
            .get("backup")
            .and_then(Value::as_str)
            .is_some_and(|path| std::fs::symlink_metadata(path).is_ok());
        if still_there {
            continue;
        }
        item["backup"] = json!(null);
        if item.get("backupManifest").is_some() {
            item["backupManifest"] = json!(null);
        }
        item["cleanupState"] = json!("cleaned");
        if let Some(object) = item.as_object_mut() {
            object.remove("cleanupError");
        }
    }
}

/// 清理结果到报告字段的投影：已清理不展示路径；待清理保留位置与原因。
fn report_cleanup(
    cleanup: &CleanupOutcome,
    dir: &Path,
) -> (Option<String>, &'static str, Option<String>) {
    match cleanup {
        CleanupOutcome::Cleaned => (None, "cleaned", None),
        CleanupOutcome::Pending { reason } => (
            Some(dir.to_string_lossy().to_string()),
            "pending",
            Some(reason.clone()),
        ),
        CleanupOutcome::Protected { reason } => (
            Some(dir.to_string_lossy().to_string()),
            "pending",
            Some(reason.clone()),
        ),
    }
}

/// 从「源快照已确认」开始推进复制：写正文 → 写数据库行 → 登记映射 → 提交关联与基线。
///
/// `body_source` 为 `Some(source_path)` 时先写正文；为 `None` 表示正文此前已写成
/// （恢复场景），直接继续数据库行与关联。源行读取与目标行写入的档位经
/// `source_paths` 分离（跨档复制时与 `paths` 不同，同档时相同）。
fn finish_copy_from_body(
    paths: &SessionPaths,
    variant: WbVariant,
    source_paths: &SessionPaths,
    operation: &mut Operation,
    snapshot: &ContentSnapshot,
    body_source: Option<&Path>,
) -> Result<(), String> {
    if let Some(source_path) = body_source {
        write_copy_body(
            snapshot,
            source_path,
            source_paths,
            paths,
            &operation.source.session_id,
            &operation.target.session_id,
        )?;
        advance_operation(paths, operation, OpPhase::BodyWritten)?;
    }

    match insert_session_copy(
        source_paths,
        paths,
        &operation.target.session_id,
        &operation.source.session_id,
        &operation.source.uid,
        &operation.target.uid,
    )? {
        DbCopyOutcome::Inserted => {}
        DbCopyOutcome::SourceRowMissing => {
            return Err("数据库中找不到源会话记录，未复制".to_string())
        }
        DbCopyOutcome::NoSessionsTable => return Err("会话数据缺少数据表，未复制".to_string()),
        DbCopyOutcome::NoDb => return Err("会话数据不存在，未复制".to_string()),
    }
    verify_session_row(paths, &operation.target.session_id, &operation.target.uid)?;
    advance_operation(paths, operation, OpPhase::DbWritten)?;

    // 云端登记交接给客户端：不预写 edge_sync_mapping——预写会让 edge-sync 扩展
    // 判定"已迁移"而跳过上传，导致云端没有会话、后续 RENAME/ACTIVITY 404。
    // 副本由客户端下次启动时自行迁移并写入映射行；阶段标记保留，语义为"已交接"。
    advance_operation(paths, operation, OpPhase::MappingWritten)?;

    let group_id = commit_links(paths, variant, operation, &snapshot.normalized)?;
    operation.group_id = group_id;
    advance_operation(paths, operation, OpPhase::LinksCommitted)?;
    advance_operation(paths, operation, OpPhase::Completed)?;
    Ok(())
}

/// 推进操作阶段。阶段只能前进：已经走到更靠后的阶段时不回写（恢复路径不得把
/// `LinksCommitted`/`Completed` 写回 `DbWritten`）。
///
/// 先保存候选副本、成功后才替换内存状态：保存失败时内存阶段不变，随后的
/// `fail_operation` 不会把未落盘的阶段写回磁盘（design §3）。
fn advance_operation(
    paths: &SessionPaths,
    operation: &mut Operation,
    phase: OpPhase,
) -> Result<(), String> {
    if operation.phase >= phase {
        return Ok(());
    }
    let mut candidate = operation.clone();
    candidate.phase = phase;
    candidate.updated_at = now_ms();
    session_link::save_operation(paths, &candidate)?;
    *operation = candidate;
    Ok(())
}

fn fail_operation(paths: &SessionPaths, operation: &mut Operation, error: &str) {
    operation.last_error = Some(error.to_string());
    operation.updated_at = now_ms();
    let _ = session_link::save_operation(paths, operation);
}

/// 原子提交关联与配对基线（含失效成员替换与基线继承）。
///
/// 整个读改写都在关联存储锁内完成；只有全部成功才推进 revision。
fn commit_links(
    paths: &SessionPaths,
    variant: WbVariant,
    operation: &Operation,
    normalized: &NormalizedContent,
) -> Result<String, String> {
    let source = &operation.source;
    let target = &operation.target;
    // 成员级档位：源成员记源档（跨档组的身份基础），目标成员记目标档（参数 variant）。
    let source_variant = operation.source_variant.unwrap_or(variant);
    let group_id = operation.group_id.clone();
    let group_id_out = group_id.clone();
    session_link::with_link_store_write(paths, move |store| {
        let index = match store.groups.iter().position(|group| group.id == group_id) {
            Some(index) => index,
            None => {
                store.groups.push(LinkGroup {
                    id: group_id.clone(),
                    variant,
                    created_at: now_ms(),
                    members: Vec::new(),
                    pair_bases: Vec::new(),
                });
                store.groups.len() - 1
            }
        };
        let group = &mut store.groups[index];

        let source_member_id =
            match session_link::find_member(group, &source.uid, &source.session_id) {
                Some(member) => member.member_id.clone(),
                None => {
                    let member_id = uuid::Uuid::new_v4().to_string();
                    session_link::add_active_member(
                        group,
                        LinkMember {
                            member_id: member_id.clone(),
                            account_id: source.account_id.clone(),
                            uid: source.uid.clone(),
                            session_id: source.session_id.clone(),
                            variant: Some(source_variant),
                            state: MemberState::Active,
                            linked_at: now_ms(),
                            last_synced_at: None,
                        },
                    );
                    member_id
                }
            };

        let target_member_id =
            match session_link::find_member(group, &target.uid, &target.session_id) {
                Some(member) => {
                    let member_id = member.member_id.clone();
                    session_link::set_member_state(group, &member_id, MemberState::Active);
                    member_id
                }
                None => {
                    let member_id = uuid::Uuid::new_v4().to_string();
                    // 同账号的失效 active 成员在这里被显式 supersede，保留记录。
                    session_link::add_active_member(
                        group,
                        LinkMember {
                            member_id: member_id.clone(),
                            account_id: target.account_id.clone(),
                            uid: target.uid.clone(),
                            session_id: target.session_id.clone(),
                            variant: Some(variant),
                            state: MemberState::Active,
                            linked_at: now_ms(),
                            last_synced_at: None,
                        },
                    );
                    member_id
                }
            };

        // 本次复制的正文即来源与目标的共同基线（定向更新，不动其它配对）。
        let pair_baseline_ref = uuid::Uuid::new_v4().to_string();
        session_link::save_baseline(paths, &pair_baseline_ref, normalized)?;
        session_link::set_pair_base(
            group,
            &source_member_id,
            &target_member_id,
            &pair_baseline_ref,
            NORMALIZATION_VERSION,
        );

        // 继承：来源与组内其它成员已有的历史共同基线，只有在「来源内容有序前缀
        // 包含该基线」时才能建立到新成员的基线；已有配对基线不覆盖。
        let others: Vec<String> = group
            .members
            .iter()
            .filter(|member| {
                member.member_id != source_member_id && member.member_id != target_member_id
            })
            .map(|member| member.member_id.clone())
            .collect();
        for other_id in others {
            if session_link::find_pair_base(group, &other_id, &target_member_id).is_some() {
                continue;
            }
            let Some(pair) =
                session_link::find_pair_base(group, &source_member_id, &other_id).cloned()
            else {
                continue;
            };
            if let Some(record) =
                session_link::inheritable_baseline(paths, &pair, &normalized.line_digests)
            {
                session_link::set_pair_base(
                    group,
                    &other_id,
                    &target_member_id,
                    &record.baseline_ref,
                    pair.normalization_version,
                );
            }
        }
        Ok(group_id_out)
    })
}

/// 操作已标成 `LinksCommitted` 时，核验关联组与双方成员仍在（只读，不写存储）。
fn committed_links_present(paths: &SessionPaths, operation: &Operation) -> Result<(), String> {
    match session_link::load_store(paths) {
        StoreState::Ready(store) => {
            let Some(group) = store
                .groups
                .iter()
                .find(|group| group.id == operation.group_id)
            else {
                return Err("会话的关联关系缺失，已停止恢复".to_string());
            };
            let has_source = session_link::find_member(
                group,
                &operation.source.uid,
                &operation.source.session_id,
            )
            .is_some();
            let has_target = session_link::find_member(
                group,
                &operation.target.uid,
                &operation.target.session_id,
            )
            .is_some();
            if !has_source || !has_target {
                return Err("对应的会话缺失，已停止恢复".to_string());
            }
            Ok(())
        }
        StoreState::Missing => Err("同步记录主文件缺失，已停止恢复".to_string()),
        StoreState::Unavailable(reason) => Err(reason),
    }
}

/// 当前账号库中按 uid 找账号 id（成员 accountId 仅作展示，身份判定仍以 uid 为准）。
fn account_id_for_uid(paths: &SessionPaths, uid: &str) -> Option<String> {
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    accounts
        .iter()
        .find(|account| account.get("uid").and_then(Value::as_str) == Some(uid))
        .and_then(|account| account.get("id").and_then(Value::as_str))
        .map(String::from)
}

// ---------------------------------------------------------------------------
// 同步预览与执行契约（design §5 / §6）
// ---------------------------------------------------------------------------

/// 会话同步能力不足时的错误文案。
pub const SESSION_SYNC_UNSUPPORTED: &str = "该档位暂不支持会话同步";
/// 预览凭据过期/失配时的原因码（design §5.2：不继承用户旧选择）。
pub const REASON_PREVIEW_STALE: &str = "previewStale";

/// `sessions.status` 的归档取值。
const SESSION_STATUS_ARCHIVED: &str = "archived";
/// 允许被传导为归档的**目标终态**白名单。
///
/// 排除 `working`（生成中）：正在生成的会话不能被收进归档。精确匹配而非「非
/// working 即允许」——NULL、未知值与缺列都不授予归档权限（真实库存在 `Pending`
/// 这类合法但语义未定的默认值）。
///
/// `active`（当前激活）在列内：切号前客户端被关掉时，目标行常残留一个陈旧的
/// `active`，把它排除会导致该会话的归档状态永远无法与来源收敛。写入侧已有三重防线
/// 兜住真正的活跃会话——`app_running` 门禁拦住 App 在跑的目标档、`planned_status_change`
/// 写前重核、以及事务内按前置状态等值匹配的条件 UPDATE（前置值已变则零行命中）。
const ARCHIVE_TARGET_TERMINAL_STATUSES: [&str; 4] = ["completed", "error", "terminated", "active"];
/// 预览项与预览绑定里的归档动作取值。
const ARCHIVE_ACTION_STATUS_ONLY: &str = "statusOnly";

/// 一条 `syncSelections` 入参（取代只传 `syncLinkIds`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSelection {
    pub group_id: String,
    pub preview_token: String,
    pub mode: SyncMode,
}

impl SyncSelection {
    /// 解析单条入参；缺字段或未知模式直接拒绝（不静默跳过、不回落默认值）。
    pub fn parse(value: &Value) -> Result<Self, String> {
        let group_id = value
            .get("groupId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| "同步选择项缺少 groupId".to_string())?;
        let preview_token = value
            .get("previewToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("同步选择项缺少 previewToken（组 {group_id}）"))?;
        let mode = value
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("同步选择项缺少 mode（组 {group_id}）"))?;
        Ok(Self {
            group_id: group_id.to_string(),
            preview_token: preview_token.to_string(),
            mode: SyncMode::parse(mode)?,
        })
    }
}

/// 解析 `syncSelections`；缺省或 null 视为未勾选同步。
pub fn parse_sync_selections(value: Option<&Value>) -> Result<Vec<SyncSelection>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = value.as_array() else {
        return Err("syncSelections 必须是数组".to_string());
    };
    items.iter().map(SyncSelection::parse).collect()
}

/// 成员当前正文状态；`projects/` 下找不到正文一律按 Missing（不当作空正文）。
pub(crate) fn member_content_state(paths: &SessionPaths, session_id: &str) -> ContentState {
    match find_project_jsonl(paths, session_id) {
        Some(path) => session_link::read_content_snapshot(&path, session_id),
        None => ContentState::Missing,
    }
}

/// 内容快照里的记录数（不可验证时为 0，仅用于展示）。
fn record_count_of(content: &ContentState) -> usize {
    match content {
        ContentState::Ready(snapshot) => snapshot.normalized.record_count,
        _ => 0,
    }
}

/// 会话行的展示名与目录（标题优先 custom_title，与侧栏一致）。
fn session_row_info(paths: &SessionPaths, cid: &str) -> Option<(String, String)> {
    let db = paths.workbuddy_db();
    let conn = open_db(&db, true)?;
    if !table_exists(&conn, "sessions") {
        return None;
    }
    let sql = if column_exists(&conn, "sessions", "custom_title") {
        "SELECT title, custom_title, cwd FROM sessions WHERE id = ?1 AND deleted_at IS NULL"
    } else {
        "SELECT title, NULL, cwd FROM sessions WHERE id = ?1 AND deleted_at IS NULL"
    };
    conn.query_row(sql, [cid], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })
    .ok()
    .map(|(title, custom_title, cwd)| {
        (
            session_display_title(title, custom_title),
            cwd.unwrap_or_default(),
        )
    })
}

/// 会话行是否仍归属该成员账号（行被删除或改归属即成员失效，design §5）。
fn member_row_owned_by(paths: &SessionPaths, member: &LinkMember) -> bool {
    session_row_owner(paths, &member.session_id).is_some_and(|owner| owner == member.uid)
}

/// 会话行 `status` 的读取结果（区分「缺列」与「列为 NULL」）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionStatus {
    /// 会话表没有 `status` 列（旧库）：不支持状态读写，但正文同步照旧可用。
    Unsupported,
    /// 行不存在或已软删除：没有可读写状态的对象。
    Absent,
    /// 行存在且未删除；`None` 表示 `status` 为 NULL。
    Present(Option<String>),
}

/// 读取会话行的 `status`（行必须存在且未软删除）。
///
/// 读失败按 Err 上抛：状态是写入门禁，读不出来就不能声称「不具备归档资格」。
fn read_member_status(paths: &SessionPaths, cid: &str) -> Result<SessionStatus, String> {
    let conn = open_db(&paths.workbuddy_db(), true)
        .ok_or_else(|| "会话数据无法打开，无法读取会话状态".to_string())?;
    if !table_exists(&conn, "sessions") {
        return Err("会话数据缺少数据表，无法读取会话状态".to_string());
    }
    if !column_exists(&conn, "sessions", "status") {
        return Ok(SessionStatus::Unsupported);
    }
    let row: Option<(Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT status, deleted_at FROM sessions WHERE id = ?1",
            [cid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("会话状态读取失败：{error}"))?;
    match row {
        // 行不存在或已软删除：与 Absent 同义。
        None | Some((_, Some(_))) => Ok(SessionStatus::Absent),
        Some((status, None)) => Ok(SessionStatus::Present(status)),
    }
}

/// 单向粘滞归档资格：来源已归档，且目标处于允许被归档的终态。
///
/// 不反向传导取消归档；缺列、NULL、未知值与生成中（`working`）一律不授权。
/// 陈旧 `active` 视作可归档终态，由写入侧门禁兜底（见白名单注释）。
fn archive_qualifies(source: &SessionStatus, target: &SessionStatus) -> bool {
    let SessionStatus::Present(Some(source_status)) = source else {
        return false;
    };
    if source_status != SESSION_STATUS_ARCHIVED {
        return false;
    }
    let SessionStatus::Present(Some(target_status)) = target else {
        return false;
    };
    ARCHIVE_TARGET_TERMINAL_STATUSES.contains(&target_status.as_str())
}

/// 读取双方会话行状态并算出本次的归档动作；读取失败一律按「不授权」处理，
/// 不改变既有正文判定的结果（状态同步是附加能力，不是正文同步的前置条件）。
fn preview_archive_state(
    source_paths: &SessionPaths,
    source_session_id: &str,
    target_paths: &SessionPaths,
    target_session_id: &str,
) -> PreviewArchiveState {
    let source =
        read_member_status(source_paths, source_session_id).unwrap_or(SessionStatus::Absent);
    let target =
        read_member_status(target_paths, target_session_id).unwrap_or(SessionStatus::Absent);
    let source_status = match &source {
        SessionStatus::Present(status) => status.clone(),
        _ => None,
    };
    let target_status = match &target {
        SessionStatus::Present(status) => status.clone(),
        _ => None,
    };
    let action =
        archive_qualifies(&source, &target).then(|| ARCHIVE_ACTION_STATUS_ONLY.to_string());
    PreviewArchiveState {
        source_status,
        target_status,
        action,
    }
}

/// 预览时记入凭据的归档状态（双方 status 与本次授权的归档动作）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PreviewArchiveState {
    source_status: Option<String>,
    target_status: Option<String>,
    /// `Some("statusOnly")` 或 None。
    action: Option<String>,
}

/// 组内是否存在该账号的成员（任意状态）：双方都有成员才谈得上「共同参与」。
fn has_member_for(group: &LinkGroup, uid: &str) -> bool {
    group.members.iter().any(|member| member.uid == uid)
}

/// 成员摘要（只含身份与状态，不含正文）。
fn member_summary(member: Option<&LinkMember>) -> Value {
    match member {
        Some(member) => json!({
            "memberId": member.member_id,
            "uid": member.uid,
            "accountId": member.account_id,
            "sessionId": member.session_id,
            "state": member.state.as_str(),
        }),
        None => Value::Null,
    }
}

/// 成员在预览时刻的内容绑定（不可验证时摘要留空，判定必然为 unknown）。
fn member_binding(member: &LinkMember, content: &ContentState) -> PreviewMemberBinding {
    let (raw_digest, normalized_digest, record_count) = match content {
        ContentState::Ready(snapshot) => (
            snapshot.full_digest.clone(),
            snapshot.normalized.total_digest.clone(),
            snapshot.normalized.record_count,
        ),
        _ => (String::new(), String::new(), 0),
    };
    PreviewMemberBinding {
        member_id: member.member_id.clone(),
        account_id: member.account_id.clone(),
        uid: member.uid.clone(),
        session_id: member.session_id.clone(),
        raw_digest,
        normalized_digest,
        record_count,
    }
}

/// 由实时状态组装预览绑定（预览与执行前核对共用同一份装配逻辑）。
// 参数都是本次绑定的显式输入（含归档状态），与复制侧同口径不做结构装箱。
#[allow(clippy::too_many_arguments)]
fn live_preview_binding(
    group: &LinkGroup,
    source_member: &LinkMember,
    target_member: &LinkMember,
    source_content: &ContentState,
    target_content: &ContentState,
    baseline: &BaselineState,
    verdict: SyncVerdict,
    archive: &PreviewArchiveState,
) -> PreviewBinding {
    PreviewBinding {
        variant: group.variant,
        group_id: group.id.clone(),
        group_fingerprint: session_link::group_fingerprint(group),
        source: member_binding(source_member, source_content),
        target: member_binding(target_member, target_content),
        baseline_ref: session_link::find_pair_base(
            group,
            &source_member.member_id,
            &target_member.member_id,
        )
        .map(|pair| pair.baseline_ref.clone()),
        baseline_total_digest: baseline.ready().map(|record| record.total_digest.clone()),
        baseline_record_count: baseline.ready().map(|record| record.record_count),
        verdict,
        source_status: archive.source_status.clone(),
        target_status: archive.target_status.clone(),
        archive_action: archive.action.clone(),
    }
}

/// 预览「当前账号 → 目标账号」可同步的关联组（design §6）。
///
/// 只处理双方共同参与的组，不涉及第三方账号；只读，不写任何会话正文。
/// 来源身份一律取该档位登录态文件，不接受前端传入。
pub fn session_links_preview(variant: WbVariant, target_acc: &Value) -> Result<Value, String> {
    session_links_preview_at(&SessionPaths::for_variant(variant), variant, target_acc)
}

fn session_links_preview_at(
    paths: &SessionPaths,
    variant: WbVariant,
    target_acc: &Value,
) -> Result<Value, String> {
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法同步会话".to_string());
    }
    let source_uid = current_user_uid_at(&paths.auth_file)
        .ok_or_else(|| "未读取到本机登录态，无法确定来源账号".to_string())?;
    if source_uid == target_uid {
        return Err("当前账号与目标账号相同，无需同步会话".to_string());
    }
    // 能力探测与复制同口径：只对国际版生效（国内版数据根与改造前同构）。
    let supported = variant != WbVariant::Ai || session_copy_supported_at(&paths.data_root);

    let mut report = json!({
        "supported": supported,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "groups": [],
    });
    if !supported {
        report["storeStatus"] = json!("unsupported");
        return Ok(report);
    }
    match session_link::load_store(paths) {
        StoreState::Missing => report["storeStatus"] = json!("missing"),
        StoreState::Unavailable(reason) => {
            report["storeStatus"] = json!("unavailable");
            report["storeError"] = json!(reason);
        }
        StoreState::Ready(store) => {
            report["storeStatus"] = json!("ready");
            let groups: Vec<Value> = store
                .groups
                .iter()
                .filter(|group| {
                    group.variant == variant
                        && has_member_for(group, &source_uid)
                        && has_member_for(group, &target_uid)
                })
                .map(|group| {
                    let fixed_paths = |_: &LinkGroup, _: &LinkMember| paths.clone();
                    preview_group_item(paths, &fixed_paths, group, &source_uid, &target_uid)
                })
                .collect();
            report["groups"] = json!(groups);
        }
    }
    Ok(report)
}

/// 预览「显式来源账号 → 显式目标账号」可同步的关联组（会话管理页用；跨档支持）。
///
/// 与 [`session_links_preview`] 的差异只在来源判定：源 uid 取自 `source_acc`，
/// 不再从登录态读取；成员内容与行按**成员自身档位**读取（跨档组：源读源档、
/// 目标读目标档），关联存储与基线共享。
pub fn session_links_preview_cross(
    source_acc: &Value,
    target_acc: &Value,
) -> Result<Value, String> {
    let source_variant = account::variant_of(source_acc);
    let target_variant = account::variant_of(target_acc);
    session_links_preview_cross_at(
        &SessionPaths::for_variant(source_variant),
        source_variant,
        source_acc,
        &SessionPaths::for_variant(target_variant),
        target_variant,
        target_acc,
        SessionPaths::for_variant,
    )
}

/// [`session_links_preview_cross`] 的可注入版本（单测注入临时双档目录与解析器）。
fn session_links_preview_cross_at(
    source_paths: &SessionPaths,
    source_variant: WbVariant,
    source_acc: &Value,
    target_paths: &SessionPaths,
    target_variant: WbVariant,
    target_acc: &Value,
    resolve_source_paths: impl Fn(WbVariant) -> SessionPaths,
) -> Result<Value, String> {
    let source_uid = account_uid(source_acc);
    if source_uid.is_empty() {
        return Err("来源账号缺少 uid，无法预览会话".to_string());
    }
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法同步会话".to_string());
    }
    if source_variant == target_variant && source_uid == target_uid {
        return Err("来源账号与目标账号相同，无需同步会话".to_string());
    }
    // 能力探测与复制同口径：成员数据根只有国际版需要同构检查（读也依赖）。
    let mut supported = true;
    for (paths, variant) in [
        (source_paths, source_variant),
        (target_paths, target_variant),
    ] {
        if variant == WbVariant::Ai && !session_copy_supported_at(&paths.data_root) {
            supported = false;
        }
    }

    let mut report = json!({
        "supported": supported,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "groups": [],
    });
    // 跨档预览在报告里带两侧档位；同档报告保持原字段（与现有入口逐字兼容）。
    if source_variant != target_variant {
        report["sourceVariant"] = json!(source_variant.as_str());
        report["targetVariant"] = json!(target_variant.as_str());
    }
    if !supported {
        report["storeStatus"] = json!("unsupported");
        return Ok(report);
    }
    // 关联存储与基线共享；用目标档路径承载（同档时两者相同）。
    match session_link::load_store(target_paths) {
        StoreState::Missing => report["storeStatus"] = json!("missing"),
        StoreState::Unavailable(reason) => {
            report["storeStatus"] = json!("unavailable");
            report["storeError"] = json!(reason);
        }
        StoreState::Ready(store) => {
            report["storeStatus"] = json!("ready");
            // 成员数据根：与请求两侧同档的成员直接用请求路径（含单测注入），
            // 其它档位（历史组）按解析器解析，避免误读。
            let member_paths = |group: &LinkGroup, member: &LinkMember| -> SessionPaths {
                let variant = session_link::member_variant(group, member);
                if variant == source_variant {
                    source_paths.clone()
                } else if variant == target_variant {
                    target_paths.clone()
                } else {
                    resolve_source_paths(variant)
                }
            };
            let groups: Vec<Value> = store
                .groups
                .iter()
                .filter(|group| {
                    has_member_for(group, &source_uid) && has_member_for(group, &target_uid)
                })
                .map(|group| {
                    preview_group_item(target_paths, &member_paths, group, &source_uid, &target_uid)
                })
                .collect();
            report["groups"] = json!(groups);
        }
    }
    Ok(report)
}

/// 单个关联组的预览项。
///
/// 记录数与差集只用于向用户解释；能否勾选只由判定结果决定（design §3.2）。
/// `member_paths` 解析成员所在档位的数据根：同档恒为同一份，跨档按成员档位
/// （源读源档、目标读目标档）；关联存储与基线与档位无关，统一走 `store_paths`。
fn preview_group_item(
    store_paths: &SessionPaths,
    member_paths: &dyn Fn(&LinkGroup, &LinkMember) -> SessionPaths,
    group: &LinkGroup,
    source_uid: &str,
    target_uid: &str,
) -> Value {
    let source_any = group.members.iter().find(|member| member.uid == source_uid);
    let target_any = group.members.iter().find(|member| member.uid == target_uid);
    // 组展示名取来源会话（同步保留目标的 sessionId、标题与自定义标题）。
    let (title, cwd) = source_any
        .and_then(|member| {
            let paths = member_paths(group, member);
            session_row_info(&paths, &member.session_id)
        })
        .unwrap_or_else(|| ("(无标题)".to_string(), String::new()));
    let source_summary = member_summary(source_any);
    let target_summary = member_summary(target_any);

    let (Some(source_member), Some(target_member)) = (
        session_link::active_member_for(group, source_uid),
        session_link::active_member_for(group, target_uid),
    ) else {
        // 任一方的有效成员缺失（失效/已被替换）→ 关联不确定，不提供任何写入动作。
        return json!({
            "groupId": group.id,
            "title": title,
            "cwd": cwd,
            "verdict": SyncVerdict::Unknown.as_str(),
            "extraA": 0,
            "extraB": 0,
            "common": 0,
            "defaultChecked": false,
            "availableModes": [],
            "reason": "对应的会话已失效或已被替换，需手动处理",
            // 记录数契约与其它不可验证路径一致：source/target 为 0、baseline 为 null
            // （前端的 `recordCount` 类型按此声明，不能发 null）。
            "recordCount": {"source": 0, "target": 0, "baseline": null},
            "source": source_summary,
            "target": target_summary,
        });
    };

    let source_paths = member_paths(group, source_member);
    let target_paths = member_paths(group, target_member);
    let baseline = session_link::load_pair_baseline(
        store_paths,
        group,
        &source_member.member_id,
        &target_member.member_id,
    );
    let source_content = member_content_state(&source_paths, &source_member.session_id);
    let target_content = member_content_state(&target_paths, &target_member.session_id);
    let rows_match = member_row_owned_by(&source_paths, source_member)
        && member_row_owned_by(&target_paths, target_member);
    let decision = if rows_match {
        session_link::decide_sync(&source_content, &target_content, &baseline)
    } else {
        // 会话行缺失/归属异常：成员实际已失效，与内容不可验证同等对待。
        SyncDecision::unknown("会话记录缺失或归属异常，对应的会话已失效")
    };
    let reason = decision.reason.clone();
    let modes: Vec<&str> = decision
        .verdict
        .available_modes()
        .iter()
        .map(|mode| mode.as_str())
        .collect();
    // 归档资格：独立于正文判定，只看双方会话行的状态。来源已归档 + 目标处于终态白名单
    // 才授权；缺列/NULL/生成中（working）一律不授权，陈旧 active 放行（写入侧门禁兜底），
    // 也不因此影响既有正文判定的结果。
    // 不可验证的判定不提供任何动作：此时既没有正文模式也不会下发凭据，若仍写出
    // archiveAction，前端摘要会显示「仅同步归档」而用户实际勾不了，等于误导。
    let mut archive = preview_archive_state(
        &source_paths,
        &source_member.session_id,
        &target_paths,
        &target_member.session_id,
    );
    if decision.verdict == SyncVerdict::Unknown {
        archive.action = None;
    }
    let archive_action = archive.action.clone();
    // 归档状态同步作为可选操作，默认不勾选（防误操作），由用户主动勾选授权；正文动作沿用自身默认值。
    let default_checked = decision.default_checked;
    // Keep ordinary sync unavailable for Ahead, but bind its verified snapshot so the
    // session-group "use this copy" flow can explicitly request UnifyOverwrite.
    // 内容一致本身不可勾选，但具备归档资格时要发凭据，否则归档动作无从授权。
    let actionable = !modes.is_empty()
        || decision.verdict == SyncVerdict::Ahead
        || (archive_action.is_some() && decision.verdict == SyncVerdict::Identical);

    let mut item = json!({
        "groupId": group.id,
        "title": title,
        "cwd": cwd,
        "verdict": decision.verdict.as_str(),
        "extraA": decision.extra_a,
        "extraB": decision.extra_b,
        "common": decision.common,
        "defaultChecked": default_checked,
        "availableModes": modes,
        "reason": reason.clone(),
        "recordCount": {
            "source": record_count_of(&source_content),
            "target": record_count_of(&target_content),
            "baseline": baseline.ready().map(|record| record.record_count),
        },
        "source": source_summary,
        "target": target_summary,
    });
    if let Some(action) = archive_action.as_deref() {
        item["archiveAction"] = json!(action);
    }
    if actionable {
        let binding = live_preview_binding(
            group,
            source_member,
            target_member,
            &source_content,
            &target_content,
            &baseline,
            decision.verdict,
            &archive,
        );
        match session_link::save_preview_token(store_paths, binding) {
            Ok(preview_id) => item["previewToken"] = json!(preview_id),
            Err(error) => {
                // 绑定存不下来就不能让用户勾选：不给出可执行动作，并说明原因。
                item["availableModes"] = json!([]);
                item["defaultChecked"] = json!(false);
                item["reason"] = json!(format!("{reason}（检查结果无法保存：{error}）"));
            }
        }
    }
    item
}

/// 校验通过后的执行计划：写入阶段与报告所需的全部已核对信息。
///
/// 正文快照来自同一次读取（在档位锁内、App 已停止写入之后），执行时不再回读来源，
/// 避免执行与校验之间的 TOCTOU。
struct SyncWritePlan {
    group_id: String,
    mode: SyncMode,
    verdict: SyncVerdict,
    source_member: LinkMember,
    target_member: LinkMember,
    source_snapshot: ContentSnapshot,
    target_snapshot: ContentSnapshot,
    /// 待写入目标的新正文：来源正文按本副本 sessionId 替换为目标 sessionId。
    incoming_text: String,
    /// 待写入正文的归一化内容（执行后据此复算摘要核验）。
    incoming: NormalizedContent,
    /// 提交前该成员对的基线引用（清单里记录，便于人工比对）。
    old_baseline_ref: Option<String>,
    /// 本次是否把目标对齐为 `archived`：状态模式下是唯一动作，正文模式下是副作用。
    archive_target: bool,
    /// 目标行在写入前的 `status`：事务内条件 UPDATE 的前置值；缺列时为 None。
    target_status_before: Option<String>,
    reason: String,
}

impl SyncWritePlan {
    fn source_records(&self) -> usize {
        self.source_snapshot.normalized.record_count
    }
}

/// 单条同步选择的校验结果。
enum SyncItemOutcome {
    /// 全部前置条件通过，可进入写入阶段（计划体较大，装箱避免枚举体积失衡）。
    Validated {
        mode: SyncMode,
        verdict: SyncVerdict,
        plan: Box<SyncWritePlan>,
    },
    /// 版本变化/前置条件不满足：跳过该项，不沿用用户旧选择。
    Skipped {
        reason: &'static str,
        message: String,
        verdict: Option<SyncVerdict>,
    },
    /// 入参或凭据非法、模式越权：拒绝，不静默执行。
    Rejected { message: String },
}

/// 重新校验单条选择：凭据、身份、成员、基线、正文逐项核对（design §5.2）。
fn plan_sync_selection(
    context: &SyncContext,
    store: Option<&LinkStore>,
    selection: &SyncSelection,
) -> SyncItemOutcome {
    let paths = context.paths;
    let source_paths = context.source_paths;
    let (source_uid, target_uid) = (context.source_uid, context.target_uid);
    // 凭据必须是我们服务端保存过的：伪造的 id 读不到，直接拒绝。
    let Some(token) = session_link::load_preview_token(paths, &selection.preview_token) else {
        return SyncItemOutcome::Rejected {
            message: "检查结果不存在或已失效，请重新检查后再操作".to_string(),
        };
    };
    let binding = &token.binding;
    // 组级 variant 是创建档位，不作为过滤（跨档组两方向都可同步）；
    // 身份由凭据的组成员绑定与指纹校验（verify_preview）逐项兜底。
    if binding.group_id != selection.group_id {
        return SyncItemOutcome::Rejected {
            message: "检查结果与所选会话不匹配，已拒绝".to_string(),
        };
    }
    let skip = |message: String| SyncItemOutcome::Skipped {
        reason: REASON_PREVIEW_STALE,
        message,
        verdict: Some(binding.verdict),
    };
    // 来源身份取登录态、目标身份取入参：与预览不一致说明账号已经变了。
    if binding.source.uid != source_uid || binding.target.uid != target_uid {
        return skip("账号已变化，检查结果已失效".to_string());
    }
    let Some(store) = store else {
        return skip("同步记录不存在或不可用，检查结果已失效".to_string());
    };
    // 组查找只按 id：跨档组的组级 variant 是创建档位，不作为身份过滤；
    // 两侧成员身份由 active_member_for 与凭据中的成员绑定逐项校验。
    let Some(group) = store
        .groups
        .iter()
        .find(|group| group.id == selection.group_id)
    else {
        return skip("会话的关联关系已不存在，检查结果已失效".to_string());
    };
    let (Some(source_member), Some(target_member)) = (
        session_link::active_member_for(group, source_uid),
        session_link::active_member_for(group, target_uid),
    ) else {
        return skip("对应的会话已失效，检查结果已失效".to_string());
    };
    // 会话行缺失/归属异常：成员实际已失效（与预览的判定口径一致），提前拦下不写。
    // 源行在源档读、目标行在目标档读（跨档时不同数据库）。
    if !member_row_owned_by(source_paths, source_member)
        || !member_row_owned_by(paths, target_member)
    {
        return skip("会话记录缺失或归属异常，检查结果已失效".to_string());
    }
    // 重新加载正文、基线与判定，再与凭据逐项核对；任一变化都跳过（含显式覆盖）。
    let source_content = member_content_state(source_paths, &source_member.session_id);
    let target_content = member_content_state(paths, &target_member.session_id);
    let baseline = session_link::load_pair_baseline(
        paths,
        group,
        &source_member.member_id,
        &target_member.member_id,
    );
    let decision = session_link::decide_sync(&source_content, &target_content, &baseline);
    let live = live_preview_binding(
        group,
        source_member,
        target_member,
        &source_content,
        &target_content,
        &baseline,
        decision.verdict,
        &preview_archive_state(
            source_paths,
            &source_member.session_id,
            paths,
            &target_member.session_id,
        ),
    );
    let mismatches = session_link::verify_preview(&token, &live);
    if !mismatches.is_empty() {
        return skip(format!("检查结果已失效：{}", mismatches.join("；")));
    }
    // 本次是否随写入把目标对齐为归档：凭据里授权过才允许，且必须在本次重新核验。
    let archive_authorized = binding.archive_action.as_deref() == Some(ARCHIVE_ACTION_STATUS_ONLY);
    // 判定已按当前内容重算：mode 必须仍然成立，unknown 不得被覆盖绕过。
    // `StatusOnly` 不属于任何正文判定（`allows` 恒为 false），走独立的归档资格门禁。
    if selection.mode == SyncMode::StatusOnly {
        if !archive_authorized {
            return SyncItemOutcome::Rejected {
                message: "该组本次不提供仅同步归档的操作，已拒绝".to_string(),
            };
        }
        if !matches!(
            decision.verdict,
            SyncVerdict::Identical | SyncVerdict::Ahead
        ) {
            return SyncItemOutcome::Rejected {
                message: format!("该组判定为 {}，不允许仅同步归档", decision.verdict.as_str()),
            };
        }
    } else if !decision.verdict.allows(selection.mode) {
        return SyncItemOutcome::Rejected {
            message: format!(
                "该组判定为 {}，不允许以 {} 模式同步",
                decision.verdict.as_str(),
                selection.mode.as_str()
            ),
        };
    }
    // 判定非 unknown 必然双方正文可验证（decide_sync 的前置条件），这里仍然显式兜底。
    let (ContentState::Ready(source_snapshot), ContentState::Ready(target_snapshot)) =
        (&source_content, &target_content)
    else {
        return skip("双方内容不可验证，检查结果已失效".to_string());
    };
    // 归档资格在写入前重新核验（来源仍已归档、目标仍在终态白名单）；任一项不满足
    // 整体判为预览失效，不静默执行另一组副作用。
    let target_status_before = if archive_authorized {
        let source_status = match read_member_status(source_paths, &source_member.session_id) {
            Ok(status) => status,
            Err(error) => return SyncItemOutcome::Rejected { message: error },
        };
        let target_status = match read_member_status(paths, &target_member.session_id) {
            Ok(status) => status,
            Err(error) => return SyncItemOutcome::Rejected { message: error },
        };
        if !archive_qualifies(&source_status, &target_status) {
            return skip("会话的归档状态已变化，检查结果已失效".to_string());
        }
        match target_status {
            SessionStatus::Present(status) => status,
            _ => return skip("会话的归档状态已变化，检查结果已失效".to_string()),
        }
    } else {
        None
    };
    // 正文模式：目标正文 = 来源正文，只把本副本 sessionId 换成目标 sessionId。
    // 状态模式：正文零写入，待写入内容就是目标自己的快照（恢复据此核验「正文没被改」）。
    let (incoming_text, incoming) = if selection.mode == SyncMode::StatusOnly {
        (
            target_snapshot.text.clone(),
            target_snapshot.normalized.clone(),
        )
    } else {
        let incoming_text = source_snapshot
            .text
            .replace(&source_member.session_id, &target_member.session_id);
        let incoming =
            match session_link::normalize_jsonl(&incoming_text, &target_member.session_id) {
                Ok(normalized) => normalized,
                Err(reason) => {
                    return SyncItemOutcome::Rejected {
                        message: format!("目标内容无法按来源内容生成（{reason}），已拒绝"),
                    }
                }
            };
        (incoming_text, incoming)
    };
    SyncItemOutcome::Validated {
        mode: selection.mode,
        verdict: decision.verdict,
        plan: Box::new(SyncWritePlan {
            group_id: selection.group_id.clone(),
            mode: selection.mode,
            verdict: decision.verdict,
            source_member: source_member.clone(),
            target_member: target_member.clone(),
            source_snapshot: source_snapshot.clone(),
            target_snapshot: target_snapshot.clone(),
            incoming_text,
            incoming,
            old_baseline_ref: live.baseline_ref,
            archive_target: archive_authorized,
            target_status_before,
            reason: decision.reason,
        }),
    }
}

/// 重新校验勾选的同步项并返回 `sessionSync` 报告（design §5 / §6）。
///
/// 每项走「解析 → 重新加载身份/成员/基线/正文 → 核对预览凭据 → 重新判定 → 备份 →
/// 阶段化写入」：任一版本变化都跳过该项（原因码 [`REASON_PREVIEW_STALE`]），
/// 只在全部校验通过后才写目标；写入到 `completed` 才算 `synced`，
/// 中断的操作保留记录并置 `needsRecovery`，绝不报成成功。
///
/// 调用方需保证 App 已停止写入：本函数自行获取档位操作锁，并在加锁后复查一次。
pub fn sync_sessions_for_switch(
    target_acc: &Value,
    selections: &[SyncSelection],
) -> Result<Value, String> {
    let variant = account::variant_of(target_acc);
    let paths = SessionPaths::for_variant(variant);
    sync_sessions_for_switch_at(
        &paths,
        variant,
        target_acc,
        selections,
        process::is_workbuddy_running,
    )
}

/// 同步上下文中不变的输入（避免逐项重复解析身份）。
///
/// 源 / 目标解耦：`paths` 恒为目标档（写侧），`source_paths` 恒为源档（读侧）；
/// 同档同步时两者相同，跨档同步时指向不同数据根。
struct SyncContext<'a> {
    paths: &'a SessionPaths,
    variant: WbVariant,
    source_paths: &'a SessionPaths,
    source_variant: WbVariant,
    source_uid: &'a str,
    source_account_id: Option<String>,
    target_uid: &'a str,
    target_account_id: Option<String>,
    /// 本次进入写入前仍未完成的操作（同一目标上不得再写一次）。
    pending: &'a [Operation],
}

fn sync_sessions_for_switch_at(
    paths: &SessionPaths,
    variant: WbVariant,
    target_acc: &Value,
    selections: &[SyncSelection],
    is_app_running: impl Fn(WbVariant) -> bool,
) -> Result<Value, String> {
    let mut synced: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    if selections.is_empty() {
        return Ok(json!({ "synced": synced, "skipped": skipped, "errors": errors }));
    }
    if variant == WbVariant::Ai && !session_copy_supported_at(&paths.data_root) {
        return Err(format!(
            "{SESSION_SYNC_UNSUPPORTED}（档位 {}）",
            variant.as_str()
        ));
    }
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法同步会话".to_string());
    }
    let source_uid = current_user_uid_at(&paths.auth_file)
        .ok_or_else(|| "未读取到本机登录态，无法确定来源账号".to_string())?;
    if source_uid == target_uid {
        return Err("当前账号与目标账号相同，无需同步会话".to_string());
    }
    // 与复制同一条门禁：同档路径的源恒为当前登录账号，这里恒为放行（统一规则）。
    if app_running_blocks_target_write(paths, variant, &target_uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }

    // 档位操作锁与复制共用：预览后的校验与写入都不得与并发复制交错。
    let _ops_lock = session_link::try_acquire_variant_ops_lock(paths, variant)?;
    // 复查：锁前未运行、拿锁后目标账号被登录进 App 的情况在这里被拦住。
    if app_running_blocks_target_write(paths, variant, &target_uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    let recovery = recover_pending_session_operations_at(paths, variant);
    let pending = session_link::pending_operations(paths, variant);
    let (store, store_unavailable) = match session_link::load_store(paths) {
        StoreState::Ready(store) => (Some(store), None),
        StoreState::Missing => (None, None),
        StoreState::Unavailable(reason) => (None, Some(reason)),
    };
    // 关系表损坏/未知版本，或存在解析不出来的操作日志：不能当成没有关联继续校验。
    let store_broken = store_unavailable.is_some();
    let blocked = store_unavailable.or_else(|| {
        recovery
            .needs_recovery
            .iter()
            .find(|issue| !issue.retryable && issue.reason.contains(UNPARSEABLE_OPERATION_REASON))
            .map(|issue| issue.reason.clone())
    });
    let context = SyncContext {
        paths,
        variant,
        source_paths: paths,
        source_variant: variant,
        source_uid: &source_uid,
        source_account_id: account_id_for_uid(paths, &source_uid),
        target_uid: &target_uid,
        target_account_id: nonempty_text(
            target_acc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
        pending: &pending,
    };
    match blocked {
        Some(reason) => {
            for selection in selections {
                errors.push(json!({ "groupId": selection.group_id, "error": reason }));
            }
        }
        None => {
            for selection in selections {
                match plan_sync_selection(&context, store.as_ref(), selection) {
                    SyncItemOutcome::Validated {
                        mode,
                        verdict,
                        plan,
                    } => match execute_sync_item(&context, &plan, mode, verdict) {
                        Ok(item) => synced.push(item),
                        Err(error) => {
                            errors.push(json!({ "groupId": selection.group_id, "error": error }))
                        }
                    },
                    SyncItemOutcome::Skipped {
                        reason,
                        message,
                        verdict,
                    } => skipped.push(json!({
                        "groupId": selection.group_id,
                        "status": "skipped",
                        "reasonCode": reason,
                        "message": message,
                        "verdict": verdict.map(SyncVerdict::as_str),
                    })),
                    SyncItemOutcome::Rejected { message } => {
                        errors.push(json!({ "groupId": selection.group_id, "error": message }))
                    }
                }
            }
        }
    }

    let unfinished_after = session_link::pending_operations(paths, variant);
    let needs_recovery = store_broken || !recovery.is_clean() || !unfinished_after.is_empty();
    let mut report = json!({ "synced": synced, "skipped": skipped, "errors": errors });
    if needs_recovery {
        report["needsRecovery"] = json!(true);
    }
    // 本轮同步之后再扫一遍：当前项的清理失败/保护残留必须出现在报告里。
    report["temporaryFiles"] = json!(session_backup::maintain(paths, variant));
    if let Some(items) = report.get_mut("synced").and_then(Value::as_array_mut) {
        reconcile_reported_cleanup(items);
    }
    Ok(report)
}

/// 把**显式来源账号**的新增内容同步到**显式目标账号**的关联会话（会话管理页；跨档支持）。
///
/// 与 [`sync_sessions_for_switch`] 的差异只在来源判定：源 uid 取自 `source_acc`，
/// 不再从登录态读取；源正文与源行按源档读取，写入侧仍以目标账号为准
/// （目标档数据根 + 备份 + 基线提交）。现有切号入口不受影响（同档语义逐字保留）。
pub fn sync_sessions_cross(
    source_acc: &Value,
    target_acc: &Value,
    selections: &[SyncSelection],
) -> Result<Value, String> {
    let source_variant = account::variant_of(source_acc);
    let target_variant = account::variant_of(target_acc);
    let source_paths = SessionPaths::for_variant(source_variant);
    let target_paths = SessionPaths::for_variant(target_variant);
    let source_uid = account_uid(source_acc);
    if source_uid.is_empty() {
        return Err("源账号缺少 uid，无法同步会话".to_string());
    }
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法同步会话".to_string());
    }
    let source = CopySide {
        paths: &source_paths,
        variant: source_variant,
        uid: source_uid,
        account_id: nonempty_text(
            source_acc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
    };
    let target = CopySide {
        paths: &target_paths,
        variant: target_variant,
        uid: target_uid,
        account_id: nonempty_text(
            target_acc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
    };
    sync_sessions_cross_at(
        &source,
        &target,
        selections,
        process::is_workbuddy_running,
        SessionPaths::for_variant,
    )
}

/// 可注入路径与「App 是否运行」探针的跨档同步入口（单测注入临时双档目录与假探针）。
fn sync_sessions_cross_at(
    source: &CopySide<'_>,
    target: &CopySide<'_>,
    selections: &[SyncSelection],
    is_app_running: impl Fn(WbVariant) -> bool,
    resolve_source_paths: impl Fn(WbVariant) -> SessionPaths,
) -> Result<Value, String> {
    let variant = target.variant;
    let mut synced: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    if selections.is_empty() {
        return Ok(json!({ "synced": synced, "skipped": skipped, "errors": errors }));
    }
    // 与复制同一条门禁：仅当目标账号是目标档当前登录账号时才要求关闭客户端
    // （源档只读，运行中的源客户端不影响读取安全）。
    if app_running_blocks_target_write(target.paths, target.variant, &target.uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    // 源档也要读（源正文与源行），国际版数据根不同构时同样不支持（与复制同口径）。
    if source.variant == WbVariant::Ai && !session_copy_supported_at(&source.paths.data_root) {
        return Err(format!(
            "{SESSION_SYNC_UNSUPPORTED}（档位 {}）",
            source.variant.as_str()
        ));
    }
    if target.variant == WbVariant::Ai && !session_copy_supported_at(&target.paths.data_root) {
        return Err(format!(
            "{SESSION_SYNC_UNSUPPORTED}（档位 {}）",
            target.variant.as_str()
        ));
    }
    if source.variant == target.variant && source.uid == target.uid {
        return Err("源账号与目标账号相同，无需同步会话".to_string());
    }

    // 双档操作锁：获取顺序固定为枚举序（Cn → Ai），任何代码路径不得反序（design §2.5）。
    let mut _ops_locks = Vec::new();
    for lock_variant in WbVariant::ALL {
        if lock_variant == source.variant {
            _ops_locks.push(session_link::try_acquire_variant_ops_lock(
                source.paths,
                lock_variant,
            )?);
        } else if lock_variant == target.variant {
            _ops_locks.push(session_link::try_acquire_variant_ops_lock(
                target.paths,
                lock_variant,
            )?);
        }
    }
    // 复查：锁前未运行、拿锁后目标账号被登录进 App 的情况在这里被拦住，不写入任何产物。
    if app_running_blocks_target_write(target.paths, target.variant, &target.uid, &is_app_running) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    let recovery =
        recover_pending_session_operations_at_with(target.paths, variant, resolve_source_paths);
    let pending = session_link::pending_operations(target.paths, variant);
    let (store, store_unavailable) = match session_link::load_store(target.paths) {
        StoreState::Ready(store) => (Some(store), None),
        StoreState::Missing => (None, None),
        StoreState::Unavailable(reason) => (None, Some(reason)),
    };
    // 关系表损坏/未知版本，或存在解析不出来的操作日志：不能当成没有关联继续校验。
    let store_broken = store_unavailable.is_some();
    let blocked = store_unavailable.or_else(|| {
        recovery
            .needs_recovery
            .iter()
            .find(|issue| !issue.retryable && issue.reason.contains(UNPARSEABLE_OPERATION_REASON))
            .map(|issue| issue.reason.clone())
    });
    let context = SyncContext {
        paths: target.paths,
        variant: target.variant,
        source_paths: source.paths,
        source_variant: source.variant,
        source_uid: &source.uid,
        source_account_id: source.account_id.clone(),
        target_uid: &target.uid,
        target_account_id: target.account_id.clone(),
        pending: &pending,
    };
    match blocked {
        Some(reason) => {
            for selection in selections {
                errors.push(json!({ "groupId": selection.group_id, "error": reason }));
            }
        }
        None => {
            for selection in selections {
                match plan_sync_selection(&context, store.as_ref(), selection) {
                    SyncItemOutcome::Validated {
                        mode,
                        verdict,
                        plan,
                    } => match execute_sync_item(&context, &plan, mode, verdict) {
                        Ok(item) => synced.push(item),
                        Err(error) => {
                            errors.push(json!({ "groupId": selection.group_id, "error": error }))
                        }
                    },
                    SyncItemOutcome::Skipped {
                        reason,
                        message,
                        verdict,
                    } => skipped.push(json!({
                        "groupId": selection.group_id,
                        "status": "skipped",
                        "reasonCode": reason,
                        "message": message,
                        "verdict": verdict.map(SyncVerdict::as_str),
                    })),
                    SyncItemOutcome::Rejected { message } => {
                        errors.push(json!({ "groupId": selection.group_id, "error": message }))
                    }
                }
            }
        }
    }

    let unfinished_after = session_link::pending_operations(target.paths, variant);
    let needs_recovery = store_broken || !recovery.is_clean() || !unfinished_after.is_empty();
    let mut report = json!({ "synced": synced, "skipped": skipped, "errors": errors });
    if needs_recovery {
        report["needsRecovery"] = json!(true);
    }
    report["temporaryFiles"] = json!(session_backup::maintain(target.paths, variant));
    if let Some(items) = report.get_mut("synced").and_then(Value::as_array_mut) {
        reconcile_reported_cleanup(items);
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// 同步执行与备份（design §5.4 / §5.5）
// ---------------------------------------------------------------------------

/// 同步备份清单格式版本；读到其它版本一律视为不可用。
pub const SYNC_BACKUP_VERSION: u32 = 1;
/// 数据库快照方法（清单里如实记录，恢复方据此选择恢复方法）。
const DB_SNAPSHOT_METHOD: &str = "sqliteBackupApi";
/// 数据库快照的分页步长与超时：App 已关闭，超时说明库被其它进程占用。
const DB_BACKUP_PAGES_PER_STEP: i32 = 1024;
const DB_BACKUP_TIMEOUT: Duration = Duration::from_secs(30);
/// 操作日志的 kind 取值。
const OPERATION_KIND_COPY: &str = "copy";
const OPERATION_KIND_SYNC: &str = "sync";

/// 一次同步的备份目录：`backups/session-transactions/{variant}/{operationId}`。
///
/// 身份由调用方（生命周期记录）预分配，目录用 `create_dir` 拒绝复用。
fn sync_backup_dir(
    paths: &SessionPaths,
    variant: WbVariant,
    operation_id: &str,
) -> Result<PathBuf, String> {
    session_backup::transaction_dir(paths, variant, operation_id)
}

fn sync_manifest_file(dir: &Path) -> PathBuf {
    dir.join("manifest.json")
}

/// 清单里记录的单个成员：身份、覆盖前正文摘要与备份位置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncBackupMember {
    member_id: String,
    uid: String,
    session_id: String,
    /// 覆盖前正文的备份位置（相对备份目录）；来源侧不写目标，为 None。
    body_file: Option<String>,
    body_raw_digest: String,
    body_normalized_digest: String,
    record_count: usize,
}

/// 本次待写入目标的新正文：恢复补写按这份内容重放，不回读可能已变化的来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncBackupPayload {
    body_file: String,
    body_raw_digest: String,
    body_normalized_digest: String,
    record_count: usize,
}

/// 目标会话行的覆盖前快照（恢复「目标行」用，design §5.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncBackupRow {
    session_id: String,
    user_id: String,
    title: Option<String>,
    custom_title: Option<String>,
    /// 覆盖前的 `status`；旧清单或旧库缺列时缺省为 None（恢复不得据此改状态）。
    #[serde(default)]
    status: Option<String>,
    updated_at: Option<i64>,
    deleted_at: Option<i64>,
}

/// 数据库备份：一致性快照位置、目标行覆盖前快照与本次写入的时间戳。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncBackupDb {
    snapshot_file: String,
    method: String,
    target_row: Option<SyncBackupRow>,
    /// 本次写入目标行的 updated_at（恢复时据此判断这一步是否已应用）。
    new_updated_at: i64,
    /// 本次要写入的 `status`；旧清单与不做状态同步的操作缺省为 None。
    #[serde(default)]
    new_status: Option<String>,
}

/// 同步备份清单：路径、目标行、备份位置与恢复方法（design §5.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncBackupManifest {
    version: u32,
    operation_id: String,
    variant: WbVariant,
    group_id: String,
    mode: SyncMode,
    verdict: SyncVerdict,
    created_at: i64,
    /// 目标正文的绝对路径（恢复补写用；使用时必须校验落在本项目录内）。
    target_body_path: String,
    source: SyncBackupMember,
    target: SyncBackupMember,
    incoming: SyncBackupPayload,
    db: SyncBackupDb,
    /// 本次要提交的新配对基线引用（恢复补完复用同一个引用，不重复新建）。
    /// 仅同步归档不产生新基线，为 None；旧清单的字符串仍读为 Some。
    #[serde(default)]
    new_baseline_ref: Option<String>,
    old_baseline_ref: Option<String>,
    /// 提交成功后目标成员的 lastSyncedAt。
    last_synced_at: i64,
    /// 恢复方法（人类可读；清单只在未完成/待清理期间保留，成功清理后随目录删除）。
    restore_steps: Vec<String>,
}

/// 一次同步的备份结果：目录、清单位置与清单本体。
struct SyncBackup {
    dir: PathBuf,
    manifest_file: PathBuf,
    manifest: SyncBackupManifest,
}

/// 备份单个文件并按摘要核验（不一致即报错，不宣称备份成功）。
fn backup_file_with_digest(
    source: &Path,
    dest: &Path,
    expected_raw_digest: &str,
) -> Result<(), String> {
    let bytes = std::fs::read(source).map_err(|error| format!("备份读取失败：{error}"))?;
    if full_digest_of(&bytes) != expected_raw_digest {
        return Err("备份内容与读取时不一致（内容在读取后被改动），已停止保存".to_string());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("备份目录创建失败：{error}"))?;
    }
    session_backup::durable_write(dest, &bytes)
        .map_err(|error| format!("备份保存失败：{error}"))?;
    let read_back = std::fs::read(dest).map_err(|error| format!("备份回读失败：{error}"))?;
    if full_digest_of(&read_back) != expected_raw_digest {
        return Err("备份保存后核验不一致，未按备份成功处理".to_string());
    }
    Ok(())
}

/// 用 SQLite 在线备份 API 生成一致性快照（design §5.4：不直接 cp 活动库）。
///
/// 活动库可能带 WAL/SHM，直接复制会拿到半写状态；backup API 产出的是自洽的独立
/// 数据库文件（同时自动带上未 checkpoint 的 WAL 内容），并做写后核验。
fn snapshot_workbuddy_db(source: &Path, dest: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err("会话数据不存在，无法备份".to_string());
    }
    if dest.exists() {
        return Err("备份数据库已存在同名文件，未覆盖".to_string());
    }
    // 源连接用读写打开：WAL 库在缺 -shm 时无法只读打开，而备份是写入门禁，
    // 不能因此失败。App 已关闭且持有档位锁，读写打开不会改动会话内容。
    let src = open_db(source, false).ok_or_else(|| "会话数据无法打开，未同步".to_string())?;
    {
        let mut dst =
            Connection::open(dest).map_err(|error| format!("备份数据库创建失败：{error}"))?;
        let backup = Backup::new(&src, &mut dst)
            .map_err(|error| format!("数据库快照初始化失败：{error}"))?;
        let deadline = Instant::now() + DB_BACKUP_TIMEOUT;
        loop {
            match backup.step(DB_BACKUP_PAGES_PER_STEP) {
                Ok(StepResult::Done) => break,
                Ok(StepResult::Busy) | Ok(StepResult::Locked) => {
                    if Instant::now() >= deadline {
                        return Err("数据库快照超时（数据库被占用），未同步".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(_) => {}
                Err(error) => return Err(format!("数据库快照失败：{error}")),
            }
        }
    }
    verify_db_snapshot(dest)
}

/// 快照核验：可回读、完整性检查通过、会话表存在。
fn verify_db_snapshot(path: &Path) -> Result<(), String> {
    let conn =
        open_db(path, true).ok_or_else(|| "备份数据库无法回读，未按备份成功处理".to_string())?;
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| format!("备份数据库完整性校验失败：{error}"))?;
    if check != "ok" {
        return Err(format!("备份数据库完整性校验未通过：{check}"));
    }
    if !table_exists(&conn, "sessions") {
        return Err("备份数据库缺少数据表，未按备份成功处理".to_string());
    }
    Ok(())
}

/// 读取一行会话的覆盖前快照（含标题类列）。
fn read_session_row(conn: &Connection, cid: &str) -> Result<Option<SyncBackupRow>, String> {
    // Schema errors are not evidence that the target was deleted. Keep custom_title optional
    // for older databases, but propagate every failure while inspecting the schema.
    let mut statement = conn
        .prepare("PRAGMA table_info(sessions)")
        .map_err(|error| format!("会话表结构读取失败：{error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("会话表结构读取失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("会话表结构读取失败：{error}"))?;
    if columns.is_empty() {
        return Err("会话数据缺少数据表，无法读取目标会话记录".to_string());
    }
    // `custom_title` 与 `status` 都可能缺失：缺列时在 SELECT 里用 NULL 占位保持列数，
    // 避免旧库（含旧测试用例）抛「列名不存在」，也避免 row.get 索引错位。
    let has_custom_title = columns.iter().any(|column| column == "custom_title");
    let has_status = columns.iter().any(|column| column == "status");
    let sql = match (has_custom_title, has_status) {
        (true, true) => {
            "SELECT id, user_id, title, custom_title, status, updated_at, deleted_at \
             FROM sessions WHERE id = ?1"
        }
        (true, false) => {
            "SELECT id, user_id, title, custom_title, NULL, updated_at, deleted_at \
             FROM sessions WHERE id = ?1"
        }
        (false, true) => {
            "SELECT id, user_id, title, NULL, status, updated_at, deleted_at \
             FROM sessions WHERE id = ?1"
        }
        (false, false) => {
            "SELECT id, user_id, title, NULL, NULL, updated_at, deleted_at \
             FROM sessions WHERE id = ?1"
        }
    };
    conn.query_row(sql, [cid], |row| {
        Ok(SyncBackupRow {
            session_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            user_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            title: row.get(2)?,
            custom_title: row.get(3)?,
            status: row.get(4)?,
            updated_at: row.get(5)?,
            deleted_at: row.get(6)?,
        })
    })
    .optional()
    .map_err(|error| format!("目标会话记录读取失败：{error}"))
}

/// 会话行的覆盖前快照；只有成功查询且没有命中时才返回 None。
fn session_row_snapshot(paths: &SessionPaths, cid: &str) -> Result<Option<SyncBackupRow>, String> {
    let conn = open_db(&paths.workbuddy_db(), true)
        .ok_or_else(|| "会话数据无法打开，无法读取目标会话记录".to_string())?;
    read_session_row(&conn, cid)
}

/// 读取备份清单；缺失/损坏/版本不符一律返回 None（视为没有恢复依据）。
fn load_sync_manifest(dir: &Path) -> Option<SyncBackupManifest> {
    let text = std::fs::read_to_string(sync_manifest_file(dir)).ok()?;
    let manifest: SyncBackupManifest = serde_json::from_str(&text).ok()?;
    (manifest.version == SYNC_BACKUP_VERSION).then_some(manifest)
}

/// 备份完整性核验：覆盖前正文与待写入正文都能按清单摘要读回，数据库快照可回读。
fn verify_sync_backup(dir: &Path, manifest: &SyncBackupManifest) -> Result<(), String> {
    let Some(target_body_file) = manifest.target.body_file.as_deref() else {
        return Err("备份清单缺少目标内容备份位置".to_string());
    };
    for (relative, digest) in [
        (target_body_file, manifest.target.body_raw_digest.as_str()),
        (
            manifest.incoming.body_file.as_str(),
            manifest.incoming.body_raw_digest.as_str(),
        ),
    ] {
        let bytes = std::fs::read(dir.join(relative))
            .map_err(|error| format!("备份文件缺失或不可读（{relative}）：{error}"))?;
        if full_digest_of(&bytes) != digest {
            return Err(format!("备份文件摘要不一致（{relative}），已停止恢复"));
        }
    }
    let snapshot = dir.join(&manifest.db.snapshot_file);
    if !snapshot.is_file() {
        return Err("数据库快照缺失，已停止恢复".to_string());
    }
    let conn =
        open_db(&snapshot, true).ok_or_else(|| "数据库快照无法打开，已停止恢复".to_string())?;
    if !table_exists(&conn, "sessions") {
        return Err("数据库快照缺少数据表，已停止恢复".to_string());
    }
    Ok(())
}

/// 目标正文路径：必须是档位 `projects/` 目录下 `{目标 sessionId}.jsonl`。
///
/// 清单可能被外部改动，恢复写入前必须重新确认路径落在本项目录内，不能按清单原样写。
fn validated_target_body_path(
    paths: &SessionPaths,
    manifest: &SyncBackupManifest,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(&manifest.target_body_path);
    let expected_name = format!("{}.jsonl", manifest.target.session_id);
    let name_matches = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy() == expected_name);
    if !path.starts_with(paths.projects_dir()) || !name_matches {
        return Err("备份清单记录的目标内容路径不合法，已停止保存".to_string());
    }
    Ok(path)
}

/// 创建本次同步的备份：唯一目录 + 覆盖前正文 + 待写入正文 + 数据库一致性快照 + 清单。
///
/// 全过程只读目标、只写备份目录；任何一步失败都返回 Err，调用方必须零写入
/// （不碰目标正文、不改数据库、不提交基线）。`operation_id` 由生命周期记录预分配。
#[allow(clippy::too_many_arguments)] // 与 rotate.rs 同口径：参数都是本次备份的显式输入
fn create_sync_backup(
    paths: &SessionPaths,
    variant: WbVariant,
    operation_id: &str,
    plan: &SyncWritePlan,
    target_body_path: &Path,
    new_updated_at: i64,
    new_baseline_ref: Option<&str>,
    last_synced_at: i64,
) -> Result<SyncBackup, String> {
    let dir = sync_backup_dir(paths, variant, operation_id)?;
    if !dir.is_dir() {
        return Err("操作专属目录不存在，未创建备份".to_string());
    }
    std::fs::create_dir_all(dir.join("bodies"))
        .map_err(|error| format!("同步备份目录创建失败：{error}"))?;

    // 1) 覆盖前正文备份 + 摘要核验（恢复/回滚的唯一依据）。
    let original_rel = format!("bodies/original-{}.jsonl", plan.target_member.session_id);
    backup_file_with_digest(
        target_body_path,
        &dir.join(&original_rel),
        &plan.target_snapshot.full_digest,
    )?;
    // 2) 待写入正文备份 + 摘要核验（写入与恢复都只写这份字节）。
    let incoming_rel = format!("bodies/incoming-{}.jsonl", plan.target_member.session_id);
    let incoming_raw = plan.incoming_text.as_bytes();
    std::fs::write(dir.join(&incoming_rel), incoming_raw)
        .map_err(|error| format!("待保存内容备份失败：{error}"))?;
    let incoming_raw_digest = full_digest_of(incoming_raw);
    if full_digest_of(
        &std::fs::read(dir.join(&incoming_rel))
            .map_err(|error| format!("待保存内容回读失败：{error}"))?,
    ) != incoming_raw_digest
    {
        return Err("待保存内容备份保存后核验不一致，未按备份成功处理".to_string());
    }
    // 3) 数据库一致性快照。
    let db_rel = "workbuddy.db".to_string();
    snapshot_workbuddy_db(&paths.workbuddy_db(), &dir.join(&db_rel))?;
    let target_row = session_row_snapshot(paths, &plan.target_member.session_id)?;
    // 规划与备份之间状态被改动就不按「可归档」继续：条件 UPDATE 的前置值必须来自
    // 与规划时一致的现场，不能拿一份已经过期的前置值去写库。
    if plan.archive_target
        && target_row.as_ref().and_then(|row| row.status.clone()) != plan.target_status_before
    {
        return Err("目标会话的状态在校验后发生变化，已停止保存".to_string());
    }

    let manifest = SyncBackupManifest {
        version: SYNC_BACKUP_VERSION,
        operation_id: operation_id.to_string(),
        variant,
        group_id: plan.group_id.clone(),
        mode: plan.mode,
        verdict: plan.verdict,
        created_at: now_ms(),
        target_body_path: target_body_path.to_string_lossy().to_string(),
        source: SyncBackupMember {
            member_id: plan.source_member.member_id.clone(),
            uid: plan.source_member.uid.clone(),
            session_id: plan.source_member.session_id.clone(),
            body_file: None,
            body_raw_digest: plan.source_snapshot.full_digest.clone(),
            body_normalized_digest: plan.source_snapshot.normalized.total_digest.clone(),
            record_count: plan.source_records(),
        },
        target: SyncBackupMember {
            member_id: plan.target_member.member_id.clone(),
            uid: plan.target_member.uid.clone(),
            session_id: plan.target_member.session_id.clone(),
            body_file: Some(original_rel),
            body_raw_digest: plan.target_snapshot.full_digest.clone(),
            body_normalized_digest: plan.target_snapshot.normalized.total_digest.clone(),
            record_count: plan.target_snapshot.normalized.record_count,
        },
        incoming: SyncBackupPayload {
            body_file: incoming_rel,
            body_raw_digest: incoming_raw_digest,
            body_normalized_digest: plan.incoming.total_digest.clone(),
            record_count: plan.incoming.record_count,
        },
        db: SyncBackupDb {
            snapshot_file: db_rel,
            method: DB_SNAPSHOT_METHOD.to_string(),
            target_row,
            new_updated_at,
            new_status: plan
                .archive_target
                .then(|| SESSION_STATUS_ARCHIVED.to_string()),
        },
        new_baseline_ref: new_baseline_ref.map(str::to_string),
        old_baseline_ref: plan.old_baseline_ref.clone(),
        last_synced_at,
        restore_steps: vec![
            "目标内容：从 bodies/ 下 original-*.jsonl 写回目标路径（先校验当前内容是否为本次保存的内容）"
                .to_string(),
            "目标会话记录：把 sessions.updated_at 还原为清单 db.targetRow.updatedAt（先校验归属与当前值）"
                .to_string(),
            "数据库：workbuddy.db 为本次保存前的一致性快照，可用 SQLite 打开核对；不要整库覆盖当前库"
                .to_string(),
            "同步记录：把 manifest.newBaselineRef 对应的成员间同步记录还原为 oldBaselineRef（若未改动则无需处理）"
                .to_string(),
        ],
    };
    // 4) 清单落盘并回读核验：备份必须可验证恢复，读不回来的清单不算备份成功。
    let content = serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
    let manifest_file = sync_manifest_file(&dir);
    session_backup::durable_write_str(&manifest_file, &content)
        .map_err(|error| format!("同步备份清单保存失败：{error}"))?;
    match load_sync_manifest(&dir) {
        Some(read_back) if read_back == manifest => {}
        _ => return Err("同步备份清单保存后核验不一致，未按备份成功处理".to_string()),
    }
    verify_sync_backup(&dir, &manifest)?;
    Ok(SyncBackup {
        dir,
        manifest_file,
        manifest,
    })
}

/// 目标正文相对本次操作的状态。
enum SyncBodyState {
    /// 已经是本次写入的内容。
    Ours,
    /// 仍是覆盖前的内容（尚未写入，或已被还原）。
    PreSync,
    /// 正文不存在。
    Gone,
    /// 两者都不是：可能被其它程序改动，停止恢复，不覆盖未知内容。
    Unknown(String),
}

fn classify_sync_body(
    target_body_path: &Path,
    target_session_id: &str,
    pre_sync_raw_digest: &str,
    expected_digest: &str,
) -> SyncBodyState {
    match session_link::read_content_snapshot(target_body_path, target_session_id) {
        ContentState::Ready(content) if content.normalized.total_digest == expected_digest => {
            SyncBodyState::Ours
        }
        ContentState::Ready(content) if content.full_digest == pre_sync_raw_digest => {
            SyncBodyState::PreSync
        }
        ContentState::Ready(_) => SyncBodyState::Unknown(
            "目标内容与本次保存及覆盖前版本都不一致（可能被其它程序改动），已停止保存，不覆盖未知内容"
                .to_string(),
        ),
        ContentState::Missing => SyncBodyState::Gone,
        ContentState::Unavailable(reason) => SyncBodyState::Unknown(format!(
            "目标内容无法验证（{reason}），已停止保存"
        )),
    }
}

/// 原子替换目标正文（同目录临时文件 + rename），写后复算摘要核验。
fn write_sync_body(
    target_body_path: &Path,
    target_session_id: &str,
    text: &str,
    expected_digest: &str,
) -> Result<NormalizedContent, String> {
    // 正文属于业务完成门禁：会话专用持久化写（sync_all + 父目录持久化）。
    session_backup::durable_write_str(target_body_path, text)
        .map_err(|error| format!("同步内容保存失败：{error}"))?;
    match session_link::read_content_snapshot(target_body_path, target_session_id) {
        ContentState::Ready(read_back) if read_back.normalized.total_digest == expected_digest => {
            Ok(read_back.normalized)
        }
        ContentState::Ready(_) => Err("同步内容保存后校验不一致，未按成功处理".to_string()),
        ContentState::Missing => Err("同步内容保存后不存在，未按成功处理".to_string()),
        ContentState::Unavailable(reason) => Err(format!("同步内容保存后无法确认：{reason}")),
    }
}

/// 从备份目录取待写入正文并原子替换目标正文：写入的字节等于备份的字节。
fn apply_sync_body(
    paths: &SessionPaths,
    backup_dir: &Path,
    manifest: &SyncBackupManifest,
    expected_digest: &str,
) -> Result<NormalizedContent, String> {
    let target_body_path = validated_target_body_path(paths, manifest)?;
    let bytes = std::fs::read(backup_dir.join(&manifest.incoming.body_file))
        .map_err(|error| format!("待保存内容备份读取失败：{error}"))?;
    if full_digest_of(&bytes) != manifest.incoming.body_raw_digest {
        return Err("待保存内容备份与清单不一致，已停止保存".to_string());
    }
    let text =
        String::from_utf8(bytes).map_err(|_| "待保存内容不是合法 UTF-8，未保存".to_string())?;
    write_sync_body(
        &target_body_path,
        &manifest.target.session_id,
        &text,
        expected_digest,
    )
}

/// 事务内更新目标行 updated_at：命中归属（owner = 目标 uid）且未删除。
///
/// 目标行状态的条件写入要求：事务内按前置状态等值匹配，避免覆盖并发改动。
struct StatusChange<'a> {
    /// 覆盖前的 `status`（事务内 WHERE 条件）。
    before: String,
    /// 本次要写入的状态。
    after: &'a str,
}

/// 更新目标会话行：`updated_at` 恒写，状态只在本次获得授权时按前置值条件写入。
///
/// 只改 updated_at（与本次状态）；sessionId、标题与 custom_title 必须与覆盖前一致（R4）。
fn update_target_session_row(
    paths: &SessionPaths,
    target: &OperationMember,
    new_updated_at: i64,
    before: Option<&SyncBackupRow>,
    status_change: Option<&StatusChange<'_>>,
) -> Result<(), String> {
    let mut conn = open_db(&paths.workbuddy_db(), false)
        .ok_or_else(|| "会话数据无法打开，未同步".to_string())?;
    if !table_exists(&conn, "sessions") {
        return Err("会话数据缺少数据表，未同步".to_string());
    }
    // 写事务的提交必须可靠持久：在本次实际写连接上确认 synchronous ≥ FULL。
    session_backup::ensure_full_synchronous(&conn)?;
    let tx = conn
        .transaction()
        .map_err(|error| format!("会话数据事务开启失败：{error}"))?;
    // 状态写入用条件 UPDATE：目标在事务内被改为活跃态/被取消归档时 affected 为 0，
    // 事务外的一次读取不能替代事务内的条件。
    let affected = match status_change {
        // `status = after` 这一支是幂等重放：数据库已提交但阶段标记未推进时，恢复会
        // 再跑一次本函数。前置值不匹配就不写——活跃态、未知值均被排除在外。
        Some(change) => tx.execute(
            "UPDATE sessions SET updated_at = ?1, status = ?2 \
             WHERE id = ?3 AND user_id = ?4 AND deleted_at IS NULL \
             AND (status = ?5 OR status = ?6)",
            rusqlite::params![
                new_updated_at,
                change.after,
                target.session_id,
                target.uid,
                change.before,
                change.after
            ],
        ),
        None => tx.execute(
            "UPDATE sessions SET updated_at = ?1 \
             WHERE id = ?2 AND user_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![new_updated_at, target.session_id, target.uid],
        ),
    }
    .map_err(|error| format!("目标会话记录更新失败：{error}"))?;
    if affected != 1 {
        return Err(
            "目标会话记录归属校验失败：会话不存在、不属于目标账号或已被删除，未按成功处理"
                .to_string(),
        );
    }
    match (read_session_row(&tx, &target.session_id)?, before) {
        (None, _) => return Err("目标会话记录保存后不可见，未按成功处理".to_string()),
        (Some(after), Some(before)) => {
            if after.session_id != before.session_id
                || after.user_id != before.user_id
                || after.title != before.title
                || after.custom_title != before.custom_title
            {
                return Err(
                    "目标会话记录的归属或标题在保存期间发生变化，已回滚本次更新".to_string()
                );
            }
            if after.updated_at != Some(new_updated_at) {
                return Err("目标会话记录更新时间未按本次保存生效，未按成功处理".to_string());
            }
            // 只在本次确实写状态时才校验状态。不写状态的正文同步里，目标被第三方
            // 从 completed 改成 active 属于与本次无关的変化，保留当前状态继续补完
            // 正文，不能把一次正常的正文恢复升级成人工处理。
            // （旧清单同样记不到 status：此处若拿 None 去比对，真实库上恒不相等。）
            if let Some(change) = status_change {
                if after.status.as_deref() != Some(change.after) {
                    return Err("目标会话记录的状态在保存期间发生变化，已回滚本次更新".to_string());
                }
            }
        }
        (Some(_), None) => {}
    }
    tx.commit()
        .map_err(|error| format!("会话数据提交失败：{error}"))?;
    Ok(())
}

/// 提交 A/B 新基线与目标成员 lastSyncedAt（定向更新，不触碰其它配对）。
///
/// 新基线用新的 ref，避免覆盖被其它成员对继承的历史基线（A/B 同步不代表 C 也同步）。
/// 组查找只按 id（跨档组的组级 variant 是创建档位，不作过滤）；成员身份由下面的
/// member_id 校验兜底。
fn commit_sync_baseline(
    paths: &SessionPaths,
    manifest: &SyncBackupManifest,
    normalized: &NormalizedContent,
) -> Result<(), String> {
    // 仅同步归档不产生新基线：缺引用即拒绝提交，不能用空引用或假引用蒙混。
    let Some(new_baseline_ref) = manifest.new_baseline_ref.as_deref() else {
        return Err("同步备份清单缺少新基线引用，未提交同步结果".to_string());
    };
    let source_member_id = manifest.source.member_id.as_str();
    let target_member_id = manifest.target.member_id.as_str();
    session_link::with_link_store_write(paths, |store| {
        let Some(group) = store
            .groups
            .iter_mut()
            .find(|group| group.id == manifest.group_id)
        else {
            return Err("会话的关联关系已不存在，未提交同步结果".to_string());
        };
        if !group
            .members
            .iter()
            .any(|member| member.member_id == source_member_id)
        {
            return Err("当前账号的会话已不存在，未提交同步结果".to_string());
        }
        {
            let Some(target) = group
                .members
                .iter_mut()
                .find(|member| member.member_id == target_member_id)
            else {
                return Err("目标账号的会话已不存在，未提交同步结果".to_string());
            };
            target.last_synced_at = Some(manifest.last_synced_at);
        }
        session_link::save_baseline(paths, new_baseline_ref, normalized)?;
        session_link::set_pair_base(
            group,
            source_member_id,
            target_member_id,
            new_baseline_ref,
            NORMALIZATION_VERSION,
        );
        Ok(())
    })
}

/// 核验 A/B 配对基线已提交为本次的新引用（已越过该阶段时不重放，只核验）。
fn verify_sync_baseline_committed(
    paths: &SessionPaths,
    manifest: &SyncBackupManifest,
) -> Result<(), String> {
    // 仅同步归档没有新基线：这类操作不会走到本函数，缺引用即为清单异常。
    let Some(new_baseline_ref) = manifest.new_baseline_ref.as_deref() else {
        return Err("同步备份清单缺少新基线引用，已停止恢复".to_string());
    };
    match session_link::load_store(paths) {
        StoreState::Ready(store) => {
            let Some(group) = store
                .groups
                .iter()
                .find(|group| group.id == manifest.group_id)
            else {
                return Err("会话的关联关系缺失，已停止恢复".to_string());
            };
            let Some(pair) = session_link::find_pair_base(
                group,
                &manifest.source.member_id,
                &manifest.target.member_id,
            ) else {
                return Err("同步记录缺失，已停止恢复".to_string());
            };
            if Some(pair.baseline_ref.as_str()) != manifest.new_baseline_ref.as_deref() {
                return Err("同步记录与本次保存不一致，已停止恢复".to_string());
            }
            if session_link::load_baseline(paths, new_baseline_ref).is_none() {
                return Err("同步记录缺失或内容不一致，已停止恢复".to_string());
            }
            Ok(())
        }
        StoreState::Missing => Err("同步记录主文件缺失，已停止恢复".to_string()),
        StoreState::Unavailable(reason) => Err(reason),
    }
}

/// 用备份里的覆盖前正文回滚目标正文（只在当前内容确为本次写入时调用）。
fn restore_sync_backup_body(
    paths: &SessionPaths,
    dir: &Path,
    manifest: &SyncBackupManifest,
) -> Result<(), String> {
    let Some(relative) = manifest.target.body_file.as_deref() else {
        return Err("备份清单缺少目标内容备份位置".to_string());
    };
    let bytes =
        std::fs::read(dir.join(relative)).map_err(|error| format!("备份内容读取失败：{error}"))?;
    if full_digest_of(&bytes) != manifest.target.body_raw_digest {
        return Err("备份内容摘要不一致，已停止回滚".to_string());
    }
    let text =
        String::from_utf8(bytes).map_err(|_| "备份内容不是合法 UTF-8，未回滚".to_string())?;
    let target_body_path = validated_target_body_path(paths, manifest)?;
    session_backup::durable_write_str(&target_body_path, &text)
        .map_err(|error| format!("目标内容回滚失败：{error}"))
}

/// 清单里的状态写入要求（新状态 + 覆盖前状态）；缺覆盖前状态即不可核验。
fn status_change_of(manifest: &SyncBackupManifest) -> Result<Option<StatusChange<'_>>, String> {
    let Some(after) = manifest.db.new_status.as_deref() else {
        return Ok(None);
    };
    let Some(before) = manifest
        .db
        .target_row
        .as_ref()
        .and_then(|row| row.status.clone())
    else {
        return Err("备份清单缺少目标会话的覆盖前状态，已停止保存".to_string());
    };
    Ok(Some(StatusChange { before, after }))
}

/// 正文模式下本次实际执行的状态写入。
///
/// 正文模式的归档是副作用：目标已不在可归档的状态（例如被打开后变成 active）时，
/// 不能为了归档把正文恢复卡成人工处理——跳过状态写入，正文照常补完。
/// 仅同步归档里状态就是全部动作，必须保留要求，由事务内条件 UPDATE 兜底。
fn planned_status_change<'a>(
    paths: &SessionPaths,
    operation: &Operation,
    manifest: &'a SyncBackupManifest,
) -> Result<Option<StatusChange<'a>>, String> {
    let Some(change) = status_change_of(manifest)? else {
        return Ok(None);
    };
    if manifest.mode == SyncMode::StatusOnly {
        return Ok(Some(change));
    }
    let current =
        session_row_snapshot(paths, &operation.target.session_id)?.and_then(|row| row.status);
    let still_archivable = current.as_deref() == Some(change.after)
        || current.as_deref() == Some(change.before.as_str());
    Ok(still_archivable.then_some(change))
}

/// 仅同步归档的阶段推进：`Prepared → DbWritten → Completed`。
///
/// 跳过 `BodyWritten` 与 `LinksCommitted`：本次没有正文写入，也不产生新基线，
/// 不能伪造「正文已写入」或「基线已提交」的含义。
fn run_status_only_phases(
    paths: &SessionPaths,
    operation: &mut Operation,
    manifest: &SyncBackupManifest,
    body: SyncBodyState,
) -> Result<(), String> {
    // 1) 正文：只核验仍是本次读取的目标原文；任何写入（含补写与回滚）都不允许。
    match body {
        SyncBodyState::Ours | SyncBodyState::PreSync => {
            let target_body_path = validated_target_body_path(paths, manifest)?;
            match session_link::read_content_snapshot(
                &target_body_path,
                &manifest.target.session_id,
            ) {
                ContentState::Ready(content)
                    if content.normalized.total_digest == operation.expected_content_digest => {}
                ContentState::Ready(_) => {
                    return Err("目标内容与本次保存不一致，状态同步未执行".to_string())
                }
                ContentState::Missing => return Err("目标内容丢失，状态同步未执行".to_string()),
                ContentState::Unavailable(reason) => {
                    return Err(format!("目标内容无法验证（{reason}），状态同步未执行"))
                }
            }
        }
        SyncBodyState::Gone => return Err("目标内容不存在，状态同步未执行".to_string()),
        SyncBodyState::Unknown(reason) => return Err(reason),
    }
    // 2) 状态：归档是本次唯一动作，必须带可核验的覆盖前状态。
    let Some(status_change) = status_change_of(manifest)? else {
        return Err("备份清单未记录本次归档状态，状态同步未执行".to_string());
    };
    update_target_session_row(
        paths,
        &operation.target,
        manifest.db.new_updated_at,
        manifest.db.target_row.as_ref(),
        Some(&status_change),
    )?;
    advance_operation(paths, operation, OpPhase::DbWritten)?;
    // 3) 不提交基线、不推进目标成员 lastSyncedAt：正文与配对基线保持原值。
    advance_operation(paths, operation, OpPhase::Completed)?;
    Ok(())
}

/// 阶段化写入：正文 → 数据库 → 组表 → completed；每步先落阶段再推进，可恢复。
///
/// 正常执行与恢复共用同一段代码：阶段只前进不回退，已越过的阶段不重放（只核验产物），
/// 因此恢复不会产生第二份写入。仅同步归档走 [`run_status_only_phases`]，不进正文分支。
fn run_sync_phases(
    paths: &SessionPaths,
    operation: &mut Operation,
    backup_dir: &Path,
    manifest: &SyncBackupManifest,
    body: SyncBodyState,
) -> Result<(), String> {
    // 仅同步归档：正文零写入，也不提交新基线。必须在这里分流——复用正文分支时
    // `Gone` 会把备份里的目标原文写回业务正文，等于补写正文。
    if manifest.mode == SyncMode::StatusOnly {
        return run_status_only_phases(paths, operation, manifest, body);
    }
    let normalized = match body {
        // 已写入：只核验现场，不重写。
        SyncBodyState::Ours => {
            let target_body_path = validated_target_body_path(paths, manifest)?;
            match session_link::read_content_snapshot(
                &target_body_path,
                &manifest.target.session_id,
            ) {
                ContentState::Ready(content)
                    if content.normalized.total_digest == operation.expected_content_digest =>
                {
                    content.normalized
                }
                ContentState::Ready(_) => {
                    return Err("目标内容与本次保存不一致，已停止恢复".to_string())
                }
                ContentState::Missing => return Err("目标内容丢失，已停止恢复".to_string()),
                ContentState::Unavailable(reason) => {
                    return Err(format!("目标内容无法验证（{reason}），已停止恢复"))
                }
            }
        }
        SyncBodyState::PreSync | SyncBodyState::Gone => {
            let normalized = apply_sync_body(
                paths,
                backup_dir,
                manifest,
                &operation.expected_content_digest,
            )?;
            advance_operation(paths, operation, OpPhase::BodyWritten)?;
            normalized
        }
        SyncBodyState::Unknown(reason) => return Err(reason),
    };

    // 数据库步骤必须**恰好执行一次**：本次时间戳已落库说明这一步已由本操作提交过，
    // 重跑会把首次执行时合法跳过的归档副作用补上（当时目标是 active，现在回到
    // completed 又变成了「可归档」）——等于在恢复里执行了用户当次没同意的动作。
    // 仅同步归档不受影响：它的 `db_applied` 已确认状态就是本次新值，重跑等价。
    let db_already_applied = manifest.mode != SyncMode::StatusOnly
        && session_row_snapshot(paths, &operation.target.session_id)?
            .and_then(|row| row.updated_at)
            == Some(manifest.db.new_updated_at);
    if db_already_applied {
        // 行仍归属目标账号且未删除即可视为本操作的产物，不写库、不改状态。
        let owner = session_row_owner(paths, &operation.target.session_id)
            .ok_or_else(|| "目标会话记录不存在或已被删除，已停止恢复".to_string())?;
        if owner != operation.target.uid {
            return Err("目标会话记录归属异常，已停止恢复".to_string());
        }
    } else {
        let status_change = planned_status_change(paths, operation, manifest)?;
        update_target_session_row(
            paths,
            &operation.target,
            manifest.db.new_updated_at,
            manifest.db.target_row.as_ref(),
            status_change.as_ref(),
        )?;
    }
    advance_operation(paths, operation, OpPhase::DbWritten)?;

    if operation.phase < OpPhase::LinksCommitted {
        commit_sync_baseline(paths, manifest, &normalized)?;
        advance_operation(paths, operation, OpPhase::LinksCommitted)?;
    } else {
        verify_sync_baseline_committed(paths, manifest)?;
    }
    advance_operation(paths, operation, OpPhase::Completed)?;
    Ok(())
}

/// 执行单条同步：备份 → 阶段化写入 → completed。
///
/// 任一阶段失败都保留未完成操作（同一目标 UUID 与新基线引用，下次切换按清单补完），
/// 绝不把未完成的写入报告成 `synced`。
fn execute_sync_item(
    context: &SyncContext,
    plan: &SyncWritePlan,
    mode: SyncMode,
    verdict: SyncVerdict,
) -> Result<Value, String> {
    let paths = context.paths;
    // 同一目标上仍有未完成写入：本轮不得再写一次（等恢复完成）。
    if let Some(operation) = context.pending.iter().find(|operation| {
        operation.phase.is_unfinished()
            && operation.target.session_id == plan.target_member.session_id
    }) {
        return Err(format!(
            "上一次会话保存尚未完成（操作 {}）：{}，本次未保存",
            operation.operation_id,
            operation
                .last_error
                .clone()
                .unwrap_or_else(|| "等待恢复".to_string())
        ));
    }
    let target_body_path = find_project_jsonl(paths, &plan.target_member.session_id)
        .ok_or_else(|| "目标内容不存在，未同步".to_string())?;
    let new_updated_at = now_ms();
    let last_synced_at = now_ms();
    // 仅同步归档不产生新正文，也就没有新配对基线：显式记为 None，不用空引用占位。
    let new_baseline_ref = (mode != SyncMode::StatusOnly).then(|| uuid::Uuid::new_v4().to_string());
    // 预分配身份：维护记录（allocating）先于备份与业务写入（design §3）。
    let target_title =
        session_row_info(paths, &plan.target_member.session_id).map(|(title, _)| title);
    let mut lifecycle = session_backup::begin_operation(
        paths,
        context.variant,
        OPERATION_KIND_SYNC,
        Some(plan.target_member.session_id.clone()),
        target_title,
    )?;
    // 备份是一道门禁：备份不可信就零写入（不碰正文、不改数据库、不提交基线）。
    let backup = match create_sync_backup(
        paths,
        context.variant,
        &lifecycle.operation_id,
        plan,
        &target_body_path,
        new_updated_at,
        new_baseline_ref.as_deref(),
        last_synced_at,
    ) {
        Ok(backup) => backup,
        Err(error) => {
            // 受控失败分支：protected 之前未写业务，可安全回收准备残留。
            session_backup::reclaim_unwritten(paths, context.variant, &mut lifecycle, &error);
            return Err(error);
        }
    };
    // 备份完备先转 protected：此步失败禁止业务写入，残留保守保留待下次维护。
    session_backup::mark_protected(paths, &mut lifecycle)?;

    let mut operation = Operation {
        version: OPERATION_VERSION,
        operation_id: lifecycle.operation_id.clone(),
        kind: OPERATION_KIND_SYNC.to_string(),
        variant: context.variant,
        // 同档同步写 None（与旧记录逐字兼容）；跨档同步显式记录源档（审计与恢复依据）。
        source_variant: (context.source_variant != context.variant)
            .then_some(context.source_variant),
        group_id: plan.group_id.clone(),
        source: OperationMember {
            account_id: context.source_account_id.clone(),
            uid: plan.source_member.uid.clone(),
            session_id: plan.source_member.session_id.clone(),
        },
        target: OperationMember {
            account_id: context.target_account_id.clone(),
            uid: plan.target_member.uid.clone(),
            session_id: plan.target_member.session_id.clone(),
        },
        expected_content_digest: plan.incoming.total_digest.clone(),
        expected_record_count: plan.incoming.record_count,
        phase: OpPhase::Prepared,
        backup: Some(backup.manifest_file.to_string_lossy().to_string()),
        lifecycle_version: Some(OPERATION_LIFECYCLE_VERSION),
        cleanup_state: None,
        last_error: None,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    session_link::save_operation(paths, &operation)?;

    let body = classify_sync_body(
        &target_body_path,
        &plan.target_member.session_id,
        &plan.target_snapshot.full_digest,
        &operation.expected_content_digest,
    );
    if let Err(error) = run_sync_phases(paths, &mut operation, &backup.dir, &backup.manifest, body)
    {
        fail_operation(paths, &mut operation, &error);
        return Err(error);
    }
    let _ = session_link::prune_operations(
        paths,
        context.variant,
        session_link::KEEP_COMPLETED_OPERATIONS,
    );
    // 业务可靠完成后才授权清理；清理失败不回滚业务、不报告同步失败。
    let cleanup = finish_backup_cleanup(paths, context.variant, &mut lifecycle);
    let (backup_path, cleanup_state, cleanup_error) = report_cleanup(&cleanup, &backup.dir);
    let manifest_path = backup_path
        .as_ref()
        .map(|path| format!("{path}/manifest.json"));
    let mut item = json!({
        "groupId": plan.group_id,
        "status": "synced",
        "verdict": verdict.as_str(),
        "mode": mode.as_str(),
        "sourceSessionId": plan.source_member.session_id,
        "targetSessionId": plan.target_member.session_id,
        "recordCount": {
            "source": plan.source_records(),
            "targetBefore": plan.target_snapshot.normalized.record_count,
            "target": plan.incoming.record_count,
        },
        "updatedAt": backup.manifest.db.new_updated_at,
        "backup": backup_path,
        "backupManifest": manifest_path,
        "cleanupState": cleanup_state,
        "message": plan.reason,
    });
    if let Some(error) = cleanup_error {
        item["cleanupError"] = json!(error);
    }
    Ok(item)
}

// ---------------------------------------------------------------------------
// 未完成操作恢复（design §4.2 / §4.5）
// ---------------------------------------------------------------------------

/// 恢复某档位全部未完成操作（需已持有档位操作锁）。
///
/// 恢复先检查实际状态再决定下一步，不盲目重放；中间产物被改动或丢失时只上报
/// needsRecovery，不覆盖未知内容。
///
/// 跨档操作的源档数据根按 [`SessionPaths::for_variant`] 解析；单测与需要注入
/// 临时目录的宿主用 [`recover_pending_session_operations_at_with`]。
pub fn recover_pending_session_operations_at(
    paths: &SessionPaths,
    variant: WbVariant,
) -> RecoveryReport {
    recover_pending_session_operations_at_with(paths, variant, SessionPaths::for_variant)
}

/// [`recover_pending_session_operations_at`] 的可注入版本：`resolve_source_paths`
/// 用于解析跨档操作（`sourceVariant` 与操作档不同）的源档数据根。
pub fn recover_pending_session_operations_at_with<F>(
    paths: &SessionPaths,
    variant: WbVariant,
    resolve_source_paths: F,
) -> RecoveryReport
where
    F: Fn(WbVariant) -> SessionPaths,
{
    let mut report = RecoveryReport::default();
    let scan = session_link::scan_operations(paths);
    // 扫描不完整（目录不可读/枚举失败）不能按「没有未完成操作」继续写：与解析失败
    // 同口径阻断，避免绕过 pending 去重后写出第二个副本。
    if !scan.complete {
        report.needs_recovery.push(RecoveryIssue {
            operation_id: "operation-scan".to_string(),
            reason: format!(
                "{UNPARSEABLE_OPERATION_REASON}（操作记录无法读取），已停止恢复以免产生重复复制"
            ),
            retryable: false,
        });
    }
    for problem in scan.problems {
        report.needs_recovery.push(RecoveryIssue {
            operation_id: problem.clone(),
            reason: format!(
                "{UNPARSEABLE_OPERATION_REASON}（{problem}），已停止恢复以免产生重复复制"
            ),
            retryable: false,
        });
    }
    for operation in scan
        .operations
        .into_iter()
        .filter(|operation| operation.variant == variant && operation.phase.is_unfinished())
    {
        // 跨档操作（源档与操作档不同）按源档解析读侧数据根；同档与旧记录视为同档。
        let source_paths = operation
            .source_variant
            .filter(|source_variant| *source_variant != operation.variant)
            .map(&resolve_source_paths);
        match recover_operation(paths, variant, operation, source_paths.as_ref()) {
            RecoverOutcome::Recovered(id) => report.recovered.push(id),
            RecoverOutcome::Abandoned(id) => report.abandoned.push(id),
            RecoverOutcome::NeedsRecovery {
                id,
                reason,
                retryable,
            } => {
                report.needs_recovery.push(RecoveryIssue {
                    operation_id: id,
                    reason,
                    retryable,
                });
            }
        }
    }
    // 恢复之后补清理：回收可安全回收的临时备份残留，保护其余并上报（design §6）。
    report.temporary_files = session_backup::maintain(paths, variant);
    report
}

/// 生产入口：自行获取档位操作锁后恢复（被占用时返回明确错误，不排队）。
pub fn recover_pending_session_operations(variant: WbVariant) -> Result<RecoveryReport, String> {
    let paths = SessionPaths::for_variant(variant);
    let _lock = session_link::try_acquire_variant_ops_lock(&paths, variant)?;
    Ok(recover_pending_session_operations_at_with(
        &paths,
        variant,
        SessionPaths::for_variant,
    ))
}

enum RecoverOutcome {
    Recovered(String),
    Abandoned(String),
    NeedsRecovery {
        id: String,
        reason: String,
        /// 既有宿主契约：true 允许账号切换和 App 启动，不能仅表示故障可重试。
        /// 同步尚未恢复一致时必须为 false，即使稍后重试可能成功。
        retryable: bool,
    },
}

/// 目标正文现状判定（恢复的第一步）。
enum BodyCheck {
    /// 正文与操作记录一致。
    Verified(NormalizedContent),
    /// 尚未写入（操作停在 Prepared 阶段）。
    Absent,
    /// 中间产物被改动/丢失：停止恢复。
    NeedsRecovery(String),
}

fn check_target_body(paths: &SessionPaths, operation: &Operation) -> BodyCheck {
    match find_project_jsonl(paths, &operation.target.session_id)
        .as_deref()
        .map(|path| session_link::read_content_snapshot(path, &operation.target.session_id))
    {
        Some(ContentState::Ready(content)) => {
            if content.normalized.total_digest == operation.expected_content_digest {
                BodyCheck::Verified(content.normalized)
            } else {
                BodyCheck::NeedsRecovery(
                    "目标内容与操作记录不一致（可能被其它程序改动），已停止恢复".to_string(),
                )
            }
        }
        Some(ContentState::Unavailable(reason)) => {
            BodyCheck::NeedsRecovery(format!("目标内容不可验证（{reason}），已停止恢复"))
        }
        Some(ContentState::Missing) | None => {
            if operation.phase >= OpPhase::BodyWritten {
                BodyCheck::NeedsRecovery("目标内容丢失，已停止恢复".to_string())
            } else {
                BodyCheck::Absent
            }
        }
    }
}

/// 恢复单个操作：按「持久化阶段 + 实际状态」逐阶段判断，已经越过的阶段不重放
/// （不重复登记映射、不重写关联存储与基线）；校验不因跳过重放而放松。
fn recover_operation(
    paths: &SessionPaths,
    variant: WbVariant,
    operation: Operation,
    source_paths: Option<&SessionPaths>,
) -> RecoverOutcome {
    if operation.kind == OPERATION_KIND_SYNC {
        return recover_sync_operation(paths, operation);
    }
    recover_copy_operation(paths, variant, operation, source_paths)
}

/// 恢复一次同步（design §5.6）：先校验现场，再按清单补完，绝不覆盖未知内容。
///
/// 顺序：备份必须完好 → 目标正文只能是「本次写入的」或「覆盖前的」→ 目标行归属与
/// 更新时间必须可安全识别 → 复用同一段阶段代码补完。任一项不满足即停止并上报
/// needsRecovery（阻断启动，宿主必须等待恢复一致后才能启动 App）。
fn recover_sync_operation(paths: &SessionPaths, mut operation: Operation) -> RecoverOutcome {
    let operation_id = operation.operation_id.clone();
    let needs = |reason: String, retryable: bool| RecoverOutcome::NeedsRecovery {
        id: operation_id.clone(),
        reason,
        retryable,
    };

    // 1) 备份必须完好：没有可验证的备份就没有恢复依据，也不得盲目重放。
    // 操作日志里记录的是本次的备份清单路径，备份目录是它的父目录。
    let Some(backup_dir) = operation
        .backup
        .clone()
        .map(PathBuf::from)
        .and_then(|manifest_file| manifest_file.parent().map(Path::to_path_buf))
    else {
        return needs(
            "同步备份位置缺失，已停止恢复，请手动处理".to_string(),
            false,
        );
    };
    let Some(manifest) = load_sync_manifest(&backup_dir) else {
        return needs(
            "同步备份清单缺失或损坏，已停止恢复，请手动处理".to_string(),
            false,
        );
    };
    if let Err(reason) = verify_sync_backup(&backup_dir, &manifest) {
        return needs(reason, false);
    }
    let target_body_path = match validated_target_body_path(paths, &manifest) {
        Ok(path) => path,
        Err(reason) => return needs(reason, false),
    };

    // 2) 目标正文：只接受「本次写入的内容」或「覆盖前的内容」。
    let body = classify_sync_body(
        &target_body_path,
        &operation.target.session_id,
        &manifest.target.body_raw_digest,
        &operation.expected_content_digest,
    );
    match &body {
        // 后续无关修改（用户/官方 App 追加过内容）或内容不可验证：停止恢复，不覆盖。
        SyncBodyState::Unknown(reason) => return needs(reason.clone(), false),
        // 状态同步不写正文，也永远不会推进到 BodyWritten：正文丢失后若按正文口径
        // 拦截，每次启动都会重跑并失败，等于永久阻断启动。已提交的状态予以保留，
        // 只放弃本次归档意图（不写库、不补正文、不重建文件）。
        // 必须排在「已写正文却丢失」的通用拦截之前——DbWritten 晚于 BodyWritten，
        // 否则归档已提交的现场会先被通用分支截走，永远走不到这里。
        SyncBodyState::Gone if manifest.mode == SyncMode::StatusOnly => {
            abandon_operation_with(
                paths,
                &mut operation,
                "目标内容已不存在，本次仅同步归档已放弃，已提交的状态保持不变",
            );
            return RecoverOutcome::Abandoned(operation_id);
        }
        SyncBodyState::Gone if operation.phase >= OpPhase::BodyWritten => {
            return needs("目标内容丢失，已停止恢复".to_string(), false);
        }
        _ => {}
    }

    // 3) 目标行：归属与更新时间必须可安全识别。
    let row = match session_row_snapshot(paths, &operation.target.session_id) {
        Ok(row) => row,
        Err(reason) => return needs(reason, false),
    };
    let row_absent = match &row {
        None => true,
        // 已删除的会话（软删除）同样没有可更新的行：会话在 App 里已经不存在。
        Some(row) => row.deleted_at.is_some(),
    };
    if row_absent {
        // 没有可更新的行就无法补完。若残留的是本次写入的正文，按清单回滚成覆盖前
        // 内容，不留无行的半成品；这不算成功，记为放弃（无需人工处理，不阻断启动）。
        // 仅同步归档不写正文，也就没有可回滚的正文：写回即等于补写业务文件。
        if matches!(body, SyncBodyState::Ours) && manifest.mode != SyncMode::StatusOnly {
            if let Err(error) = restore_sync_backup_body(paths, &backup_dir, &manifest) {
                return needs(
                    format!("目标会话记录已不存在且内容回滚失败（{error}），请手动处理"),
                    false,
                );
            }
        }
        abandon_operation_with(
            paths,
            &mut operation,
            "目标会话记录已不存在（或已删除），本次同步已从备份回滚，未保存会话内容",
        );
        return RecoverOutcome::Abandoned(operation_id);
    }
    let Some(row) = row else {
        // row_absent 已经把 None 分支处理掉，这里只是形式上的兜底。
        return needs("目标会话记录无法读取，已停止恢复".to_string(), false);
    };
    if row.user_id != operation.target.uid {
        return needs("目标会话记录归属异常，已停止恢复".to_string(), false);
    }
    let before_row = manifest.db.target_row.as_ref();
    let before_updated_at = before_row.and_then(|row| row.updated_at);
    let applied = row.updated_at == Some(manifest.db.new_updated_at);
    // 状态相等只在「状态就是全部动作」时才要求（仅同步归档）。正文模式的归档是
    // 副作用：目标被打开变成 active 属于与本次正文同步无关的変化，不能因此把一次
    // 正常的正文恢复升级成人工处理——由下面的状态写入决策决定是否跳过归档。
    let status_matters = manifest.mode == SyncMode::StatusOnly;
    let untouched = before_row.is_some_and(|before| {
        before.updated_at == row.updated_at && (!status_matters || before.status == row.status)
    });
    // 状态到底有没有写上去，不能只看时间戳：用户取消归档或改为活跃态时时间戳可能
    // 仍是本次值。但正文模式里归档只是副作用，允许被合法跳过（见
    // planned_status_change：目标变为活跃态时跳过），跳过与否每次都由现场状态重新
    // 推导、结果一致，因此只要本次时间戳已落库，这一步就算完成。
    // 仅同步归档里状态是全部动作，必须额外确认状态也等于本次新值。
    let db_applied = match (
        manifest.mode == SyncMode::StatusOnly,
        manifest.db.new_status.as_deref(),
    ) {
        (true, Some(expected)) => applied && row.status.as_deref() == Some(expected),
        _ => applied,
    };
    if !(db_applied || untouched) && before_updated_at.is_some() {
        // 既不是本次写入的值、也不是覆盖前的值：被其它程序改动过，不覆盖。
        return needs(
            "目标会话记录的状态或更新时间与本次保存及覆盖前值都不一致（可能被其它程序改动），已停止恢复"
                .to_string(),
            false,
        );
    }

    // 4) 复用同一段阶段代码补完（阶段只前进，已越过的阶段只核验不重放）。
    match run_sync_phases(paths, &mut operation, &backup_dir, &manifest, body) {
        Ok(()) => RecoverOutcome::Recovered(operation_id),
        Err(error) => {
            fail_operation(paths, &mut operation, &error);
            // Retry may succeed later, but the unfinished sync must block startup now.
            needs(error, false)
        }
    }
}

/// 恢复一次复制（第一步的原有逻辑，保持不变）。
fn recover_copy_operation(
    paths: &SessionPaths,
    variant: WbVariant,
    mut operation: Operation,
    source_paths: Option<&SessionPaths>,
) -> RecoverOutcome {
    let operation_id = operation.operation_id.clone();
    let needs = |reason: String, retryable: bool| RecoverOutcome::NeedsRecovery {
        id: operation_id.clone(),
        reason,
        retryable,
    };
    // 读侧数据根：跨档操作由调用方解析出的源档；同档与旧记录用目标档。
    let read_paths = source_paths.unwrap_or(paths);

    // 1) 目标正文：已写成则直接复用；未写成则用当前源内容补写；被改动则停止。
    let normalized = match check_target_body(paths, &operation) {
        BodyCheck::Verified(normalized) => normalized,
        BodyCheck::NeedsRecovery(reason) => return needs(reason, false),
        BodyCheck::Absent => {
            let Some(source_path) = find_project_jsonl(read_paths, &operation.source.session_id)
            else {
                abandon_operation(paths, &mut operation);
                return RecoverOutcome::Abandoned(operation_id);
            };
            let source = match session_link::read_content_snapshot(
                &source_path,
                &operation.source.session_id,
            ) {
                ContentState::Ready(snapshot) => snapshot,
                _ => {
                    abandon_operation(paths, &mut operation);
                    return RecoverOutcome::Abandoned(operation_id);
                }
            };
            // 源内容在本机发生了变化：按当前内容继续（副本是快照复制，不是同步）。
            if source.normalized.total_digest != operation.expected_content_digest {
                operation.expected_content_digest = source.normalized.total_digest.clone();
                operation.expected_record_count = source.normalized.record_count;
            }
            if let Err(error) = write_copy_body(
                &source,
                &source_path,
                read_paths,
                paths,
                &operation.source.session_id,
                &operation.target.session_id,
            ) {
                fail_operation(paths, &mut operation, &error);
                return needs(error, true);
            }
            if let Err(error) = advance_operation(paths, &mut operation, OpPhase::BodyWritten) {
                return needs(error, true);
            }
            source.normalized
        }
    };

    // 2) 数据库行：缺失则补写，归属异常则停止。
    match session_row_owner(paths, &operation.target.session_id) {
        Some(owner) if owner == operation.target.uid => {}
        Some(_) => {
            return needs("目标会话记录归属异常，已停止恢复".to_string(), false);
        }
        None => {
            if operation.phase >= OpPhase::DbWritten {
                return needs("目标会话记录丢失，已停止恢复".to_string(), false);
            }
            match insert_session_copy(
                read_paths,
                paths,
                &operation.target.session_id,
                &operation.source.session_id,
                &operation.source.uid,
                &operation.target.uid,
            ) {
                Ok(DbCopyOutcome::Inserted) => {}
                Ok(outcome) => {
                    let error = format!("会话记录保存失败（{outcome:?}），保留操作待重试");
                    fail_operation(paths, &mut operation, &error);
                    return needs(error, false);
                }
                Err(error) => {
                    fail_operation(paths, &mut operation, &error);
                    return needs(error, true);
                }
            }
            if let Err(error) =
                verify_session_row(paths, &operation.target.session_id, &operation.target.uid)
            {
                return needs(error, false);
            }
        }
    }
    if let Err(error) = advance_operation(paths, &mut operation, OpPhase::DbWritten) {
        return needs(error, true);
    }

    // 3) 云端登记已交接给客户端（不预写 edge_sync_mapping）：这里只推进阶段，
    // 不写入、不核验映射行——该行由 edge-sync 扩展在客户端下次启动时写入，
    // 工具恢复时不能要求它已存在，否则会误报 needsRecovery。
    if operation.phase < OpPhase::MappingWritten {
        if let Err(error) = advance_operation(paths, &mut operation, OpPhase::MappingWritten) {
            return needs(error, true);
        }
    }

    // 4) 关联与基线：已提交过就不再 commit_links，避免重复写关联存储与基线；
    // 同样先核验组与成员仍在，不能把「跳过重放」当成「产物一定还在」。
    if operation.phase < OpPhase::LinksCommitted {
        match commit_links(paths, variant, &operation, &normalized) {
            Ok(group_id) => {
                operation.group_id = group_id;
            }
            Err(error) => {
                fail_operation(paths, &mut operation, &error);
                return needs(error, true);
            }
        }
        if let Err(error) = advance_operation(paths, &mut operation, OpPhase::LinksCommitted) {
            return needs(error, true);
        }
    } else if let Err(error) = committed_links_present(paths, &operation) {
        return needs(error, false);
    }
    if let Err(error) = advance_operation(paths, &mut operation, OpPhase::Completed) {
        return needs(error, true);
    }
    cleanup_finished_operation(paths, variant, &operation);
    RecoverOutcome::Recovered(operation_id)
}

fn abandon_operation(paths: &SessionPaths, operation: &mut Operation) {
    abandon_operation_with(
        paths,
        operation,
        "源会话已不可用，且没有复制出任何会话，已放弃该操作",
    );
}

/// 放弃一个未写入任何会话内容的操作（阶段置 Abandoned，不算成功）。
///
/// `cleanupState = safeTerminated` 是「已验证安全终止」的持久标记：只有带标记的
/// Abandoned 才允许维护入口回收对应临时备份（design §3）。
fn abandon_operation_with(paths: &SessionPaths, operation: &mut Operation, reason: &str) {
    operation.phase = OpPhase::Abandoned;
    operation.cleanup_state = Some(CLEANUP_STATE_SAFE_TERMINATED.to_string());
    operation.last_error = Some(reason.to_string());
    operation.updated_at = now_ms();
    let _ = session_link::save_operation(paths, operation);
}

/// 恢复/放弃完成后立即回收该操作的临时备份（待清理推进失败时留给下次维护补转）。
fn cleanup_finished_operation(paths: &SessionPaths, variant: WbVariant, operation: &Operation) {
    let Ok(Some(mut record)) =
        session_backup::load_lifecycle(paths, variant, &operation.operation_id)
    else {
        // 没有维护记录（旧操作/记录损坏）：维护扫描会按自身口径上报，这里不猜测。
        return;
    };
    if session_backup::mark_cleanup_pending(paths, &mut record, None).is_ok() {
        let _ = session_backup::cleanup_after_success(paths, variant, &record);
    }
}

#[cfg(test)]
mod tests {
    //! 会话复制/关联/恢复的端到端单测。
    //!
    //! 所有用例都在临时目录里构造数据根与存储根，绝不读写真实的 `~/.wb-switch`
    //! 或 WorkBuddy 数据目录。

    use super::*;
    use crate::modules::session_link::{LinkStore, MemberState, Operation, StoreState};
    use serde_json::json;

    struct Env {
        root: PathBuf,
        paths: SessionPaths,
    }

    impl Env {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "wb_switch_copy_test_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            let paths = SessionPaths {
                store_root: root.join("store"),
                data_root: root.join("data"),
                auth_file: root.join("auth.info"),
                link_namespace: LinkNamespace::WorkBuddy,
            };
            std::fs::create_dir_all(paths.projects_dir().join("ws-a")).unwrap();
            Env { root, paths }
        }

        fn paths(&self) -> SessionPaths {
            self.paths.clone()
        }

        fn set_login(&self, uid: &str) {
            std::fs::write(
                &self.paths.auth_file,
                json!({"account": {"uid": uid}}).to_string(),
            )
            .unwrap();
        }

        fn create_db(&self) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    title TEXT,
                    custom_title TEXT,
                    cwd TEXT,
                    created_at INTEGER,
                    updated_at INTEGER,
                    deleted_at INTEGER,
                    is_playground INTEGER
                );",
            )
            .unwrap();
        }

        fn create_edge_db(&self, variant: WbVariant) {
            let conn = Connection::open(self.paths.edge_sync_db(variant)).unwrap();
            conn.execute_batch(
                "CREATE TABLE edge_sync_mapping (
                    session_id TEXT,
                    conversation_id TEXT,
                    msg_channel TEXT,
                    created_at INTEGER
                );",
            )
            .unwrap();
        }

        /// 带 `sessions.status` 列的表：真实库有该列，默认建表没有。
        ///
        /// 默认值与真实库一致（`Pending`，不在终态白名单内），避免「缺省即可归档」。
        fn create_db_with_status(&self) {
            self.create_db_with_status_column("status TEXT NOT NULL DEFAULT 'Pending'");
        }

        /// `status` 可空的历史形态：验证「NULL 不授予归档权限」时必须能真的写出 NULL。
        fn create_db_with_nullable_status(&self) {
            self.create_db_with_status_column("status TEXT");
        }

        fn create_db_with_status_column(&self, status_column: &str) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute_batch(&format!(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    title TEXT,
                    custom_title TEXT,
                    {status_column},
                    cwd TEXT,
                    created_at INTEGER,
                    updated_at INTEGER,
                    deleted_at INTEGER,
                    is_playground INTEGER
                );"
            ))
            .unwrap();
        }

        fn add_session(&self, id: &str, uid: &str, title: &str) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute(
                "INSERT INTO sessions (id, user_id, title, custom_title, cwd, created_at, updated_at, deleted_at, is_playground)
                 VALUES (?1, ?2, ?3, NULL, '/ws/a', 1000, 2000, NULL, 0)",
                rusqlite::params![id, uid, title],
            )
            .unwrap();
        }

        fn add_body(&self, cid: &str, text: &str) -> PathBuf {
            let path = self
                .paths
                .projects_dir()
                .join("ws-a")
                .join(format!("{cid}.jsonl"));
            std::fs::write(&path, text).unwrap();
            path
        }

        fn body_path(&self, cid: &str) -> PathBuf {
            self.paths
                .projects_dir()
                .join("ws-a")
                .join(format!("{cid}.jsonl"))
        }

        fn delete_row(&self, id: &str) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute("DELETE FROM sessions WHERE id = ?1", [id])
                .unwrap();
        }

        /// 设置会话状态（模拟用户归档/取消归档、App 写入活跃态）。
        fn set_status(&self, id: &str, status: &str) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute(
                "UPDATE sessions SET status = ?1 WHERE id = ?2",
                rusqlite::params![status, id],
            )
            .unwrap();
        }

        /// 把状态置为 NULL（旧库/NULL 语义），验证白名单不授予 NULL 归档权限。
        fn set_status_null(&self, id: &str) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute("UPDATE sessions SET status = NULL WHERE id = ?1", [id])
                .unwrap();
        }

        /// 设置更新时间戳（构造「本次时间戳已落库」的中断现场）。
        fn set_updated_at(&self, id: &str, updated_at: i64) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                rusqlite::params![updated_at, id],
            )
            .unwrap();
        }

        /// 设置历史起点时间戳（`None` 造 NULL：旧库没记下创建时间的形态）。
        fn set_created_at(&self, id: &str, created_at: Option<i64>) {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![created_at, id],
            )
            .unwrap();
        }

        fn created_at_of(&self, id: &str) -> Option<i64> {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.query_row(
                "SELECT created_at FROM sessions WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
        }

        fn updated_at_of(&self, id: &str) -> Option<i64> {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.query_row(
                "SELECT updated_at FROM sessions WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
        }

        fn status_of(&self, id: &str) -> Option<String> {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            conn.query_row("SELECT status FROM sessions WHERE id = ?1", [id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
        }

        fn target(&self, uid: &str) -> Value {
            json!({"id": format!("acc-{uid}"), "uid": uid, "variant": "cn"})
        }

        fn store(&self) -> LinkStore {
            match session_link::load_store(&self.paths) {
                StoreState::Ready(store) => store,
                other => panic!("同步记录应为 Ready，实际 {other:?}"),
            }
        }

        fn body_files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.paths.projects_dir().join("ws-a"))
                .unwrap()
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    name.ends_with(".jsonl").then_some(name)
                })
                .collect();
            names.sort();
            names
        }

        /// 基线目录里的 `*.json` 数量（判断恢复是否新增基线）。
        fn baseline_files(&self) -> usize {
            std::fs::read_dir(self.paths.baselines_dir())
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                        .count()
                })
                .unwrap_or(0)
        }

        /// 云端映射表行数（判断恢复是否重复登记）。
        fn mapping_rows(&self) -> usize {
            let conn = Connection::open(self.paths.edge_sync_db(WbVariant::Cn)).unwrap();
            conn.query_row("SELECT COUNT(*) FROM edge_sync_mapping", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap() as usize
        }

        fn rows_for(&self, uid: &str) -> Vec<String> {
            let conn = Connection::open(self.paths.workbuddy_db()).unwrap();
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE user_id = ?1 AND deleted_at IS NULL")
                .unwrap();
            let mut rows: Vec<String> = stmt
                .query_map([uid], |row| row.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect();
            rows.sort();
            rows
        }

        fn first_copy_id(&self, report: &Value) -> String {
            report["copied"][0]["newId"].as_str().unwrap().to_string()
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn body_text(cid: &str) -> String {
        format!(
            "{}\n{}\n",
            json!({"type": "user", "sessionId": cid, "text": "你好"}),
            json!({"type": "assistant", "sessionId": cid, "text": "hi"})
        )
    }

    /// 一个可用的国内版环境：源账号 uid-a 有一个带正文的会话 sess-1。
    fn ready_env(name: &str) -> Env {
        let env = Env::new(name);
        env.create_db();
        env.create_edge_db(WbVariant::Cn);
        env.set_login("uid-a");
        env.add_session("sess-1", "uid-a", "标题一");
        env.add_body("sess-1", &body_text("sess-1"));
        env
    }

    fn copy(env: &Env, target_uid: &str, ids: &[&str]) -> Value {
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target(target_uid),
            &ids,
            |_| false,
        )
        .unwrap()
    }

    // ---------------------------------------------------------------------------
    // 基础路径与能力探测
    // ---------------------------------------------------------------------------

    /// A1 防回归：WorkBuddy 命名空间下三条关联路径必须与改造前**逐字相同**
    /// （存量关联表 / 基线 / 锁都在这些名字上，改名即等于数据丢失）。
    #[test]
    fn workbuddy_link_paths_are_byte_identical_to_before() {
        let paths = SessionPaths::for_variant(WbVariant::Cn);
        assert!(paths.session_links_file().ends_with("session_links.json"));
        assert!(paths.session_links_dir().ends_with("session-links"));
        assert!(paths.baselines_dir().ends_with("session-links/baselines"));
        assert!(paths
            .preview_tokens_dir()
            .ends_with("session-links/previews"));
        assert!(paths.operations_dir().ends_with("session-links/operations"));
        assert!(paths
            .link_store_lock_file()
            .ends_with("locks/session-links.lock"));
        // 相对工具存储根逐段比对：写死分隔符会在 Windows 上反转断言方向。
        let root = std::env::temp_dir().join("wb-switch-store");
        let at_root = SessionPaths {
            store_root: root.clone(),
            data_root: PathBuf::new(),
            auth_file: PathBuf::new(),
            link_namespace: LinkNamespace::WorkBuddy,
        };
        assert_eq!(
            at_root.session_links_file(),
            root.join("session_links.json")
        );
        assert_eq!(at_root.session_links_dir(), root.join("session-links"));
        assert_eq!(
            at_root.baselines_dir(),
            root.join("session-links").join("baselines")
        );
        assert_eq!(
            at_root.link_store_lock_file(),
            root.join("locks").join("session-links.lock")
        );
    }

    /// 命名空间隔离：VS Code 侧的关联表 / 目录 / 锁与 WorkBuddy 名字不同，
    /// 且两者的存储根相同（同一 `~/.wb-switch` 下并存而不互相污染）。
    #[test]
    fn vscode_link_paths_are_isolated_from_workbuddy() {
        let root = std::env::temp_dir().join("wb-switch-store");
        let workbuddy = SessionPaths {
            store_root: root.clone(),
            data_root: PathBuf::new(),
            auth_file: PathBuf::new(),
            link_namespace: LinkNamespace::WorkBuddy,
        };
        let vscode = SessionPaths::for_vscode_ext_at(root.clone());
        assert_eq!(vscode.store_root, workbuddy.store_root);
        assert_eq!(
            vscode.session_links_file(),
            root.join("vscode_session_links.json")
        );
        assert_eq!(
            vscode.session_links_dir(),
            root.join("vscode-session-links")
        );
        assert_eq!(
            vscode.baselines_dir(),
            root.join("vscode-session-links").join("baselines")
        );
        assert_eq!(
            vscode.link_store_lock_file(),
            root.join("locks").join("vscode-session-links.lock")
        );
        assert_eq!(
            vscode.preview_tokens_dir(),
            root.join("vscode-session-links").join("previews")
        );
        assert_eq!(
            vscode.operations_dir(),
            root.join("vscode-session-links").join("operations")
        );
        assert_ne!(vscode.session_links_file(), workbuddy.session_links_file());
        assert_ne!(vscode.session_links_dir(), workbuddy.session_links_dir());
        assert_ne!(vscode.baselines_dir(), workbuddy.baselines_dir());
        assert_ne!(vscode.preview_tokens_dir(), workbuddy.preview_tokens_dir());
        assert_ne!(vscode.operations_dir(), workbuddy.operations_dir());
        assert_ne!(
            vscode.link_store_lock_file(),
            workbuddy.link_store_lock_file()
        );
        // 默认命名空间是 WorkBuddy：`SessionPaths::for_variant` 之外的历史构造点
        // 不会因为新增字段而漂移到 VS Code 名字上。
        assert_eq!(LinkNamespace::default(), LinkNamespace::WorkBuddy);
    }

    /// 命名空间隔离：CodeBuddy IDE 侧的关联表 / 目录 / 锁与 WorkBuddy、VS Code 都不同，
    /// 三个目标在同一 `~/.wb-switch` 下并存而不互相污染。
    #[test]
    fn codebuddy_ide_link_paths_are_isolated_from_other_namespaces() {
        let root = std::env::temp_dir().join("wb-switch-store");
        let workbuddy = SessionPaths {
            store_root: root.clone(),
            data_root: PathBuf::new(),
            auth_file: PathBuf::new(),
            link_namespace: LinkNamespace::WorkBuddy,
        };
        let vscode = SessionPaths::for_vscode_ext_at(root.clone());
        let ide = SessionPaths::for_codebuddy_ide_at(root.clone());
        assert_eq!(ide.store_root, workbuddy.store_root);
        assert_eq!(
            ide.session_links_file(),
            root.join("codebuddy_ide_session_links.json")
        );
        assert_eq!(
            ide.session_links_dir(),
            root.join("codebuddy-ide-session-links")
        );
        assert_eq!(
            ide.baselines_dir(),
            root.join("codebuddy-ide-session-links").join("baselines")
        );
        assert_eq!(
            ide.link_store_lock_file(),
            root.join("locks").join("codebuddy-ide-session-links.lock")
        );
        assert_eq!(
            ide.preview_tokens_dir(),
            root.join("codebuddy-ide-session-links").join("previews")
        );
        assert_eq!(
            ide.operations_dir(),
            root.join("codebuddy-ide-session-links").join("operations")
        );
        for other in [&workbuddy, &vscode] {
            assert_ne!(ide.session_links_file(), other.session_links_file());
            assert_ne!(ide.session_links_dir(), other.session_links_dir());
            assert_ne!(ide.baselines_dir(), other.baselines_dir());
            assert_ne!(ide.preview_tokens_dir(), other.preview_tokens_dir());
            assert_ne!(ide.operations_dir(), other.operations_dir());
            assert_ne!(ide.link_store_lock_file(), other.link_store_lock_file());
        }
    }

    #[test]
    fn db_paths_follow_variant_data_root() {
        let cn = SessionPaths::for_variant(WbVariant::Cn);
        // Path::ends_with 按路径分量比较，Windows 上 `\` 与 `/` 等价；
        // 不要用 to_string_lossy().ends_with()——那会把分隔符写进断言。
        assert!(cn.workbuddy_db().ends_with(".workbuddy/workbuddy.db"));
        // 映射库文件名交给解析器：真实数据根可能是任意版本（WorkBuddy 5.6 已迁移到
        // v4 且 v2/v3 残留并存），这里只断言落在国内版数据根下的 edge-sync-mapping-*.db，
        // 具体发现规则由 edge_sync_db_picks_largest_discovered_version 用临时目录覆盖。
        let cn_edge = cn.edge_sync_db(WbVariant::Cn);
        assert_eq!(cn_edge.parent(), Some(cn.data_root.as_path()));
        assert!(cn_edge.to_string_lossy().contains("edge-sync-mapping-"));

        let ai = SessionPaths::for_variant(WbVariant::Ai);
        assert_eq!(ai.workbuddy_db().parent(), Some(ai.data_root.as_path()));
        assert_ne!(cn.workbuddy_db(), ai.workbuddy_db());
        // 国际版数据根与国内版不同构，默认文件名为 v4，且同样走动态发现。
        assert!(ai
            .edge_sync_db(WbVariant::Ai)
            .to_string_lossy()
            .ends_with("edge-sync-mapping-v4.db"));
        assert_ne!(
            cn.edge_sync_db(WbVariant::Cn),
            ai.edge_sync_db(WbVariant::Ai)
        );
        // 锁与关联存储都挂在工具存储根下，顺序固定为「档位锁 → 存储锁」。
        assert!(cn
            .variant_ops_lock_file(WbVariant::Cn)
            .ends_with("locks/session-ops-cn.lock"));
        assert_ne!(
            cn.variant_ops_lock_file(WbVariant::Cn),
            cn.variant_ops_lock_file(WbVariant::Ai)
        );
        assert!(cn
            .link_store_lock_file()
            .ends_with("locks/session-links.lock"));
    }

    /// 在临时数据根里放好给定文件，返回国内版（或指定档位）解析出的映射库文件名。
    fn edge_sync_pick(variant: WbVariant, files: &[&str]) -> String {
        let root = temp_root("edge_sync_pick");
        std::fs::create_dir_all(&root).unwrap();
        for name in files {
            std::fs::write(root.join(name), b"stub").unwrap();
        }
        let paths = SessionPaths {
            data_root: root.clone(),
            ..SessionPaths::for_variant(variant)
        };
        let picked = paths
            .edge_sync_db(variant)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        std::fs::remove_dir_all(&root).unwrap();
        picked
    }

    #[test]
    fn edge_sync_db_picks_largest_discovered_version() {
        // 回归：WorkBuddy 客户端自行演进映射库文件名（本机实测 v2 迁移残留、v3、v4
        // 并存，v3 从未出现在本工具代码里）。路径必须动态发现最大版本号，写死任何
        // 名字都会再次失效——写死 v2 会把登记写进迁移残留库，云端归属随之丢失。
        // 六种组合：空 / 仅无后缀 / 仅 v2 / v2+v4 / v2+v3+v4 / 仅 v4。
        let cn = WbVariant::Cn;
        assert_eq!(edge_sync_pick(cn, &[]), "edge-sync-mapping-v2.db");
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping.db"]),
            "edge-sync-mapping.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v0.db"]),
            "edge-sync-mapping-v0.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v2.db"]),
            "edge-sync-mapping-v2.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v2.db", "edge-sync-mapping-v4.db"]),
            "edge-sync-mapping-v4.db"
        );
        assert_eq!(
            edge_sync_pick(
                cn,
                &[
                    "edge-sync-mapping-v2.db",
                    "edge-sync-mapping-v3.db",
                    "edge-sync-mapping-v4.db",
                ]
            ),
            "edge-sync-mapping-v4.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v4.db"]),
            "edge-sync-mapping-v4.db"
        );

        // 伴生文件（-shm / -wal）不是候选；更高版本出现时自动适配。
        assert_eq!(
            edge_sync_pick(
                cn,
                &[
                    "edge-sync-mapping-v4.db",
                    "edge-sync-mapping-v4.db-shm",
                    "edge-sync-mapping-v4.db-wal",
                ]
            ),
            "edge-sync-mapping-v4.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v4.db", "edge-sync-mapping-v5.db"]),
            "edge-sync-mapping-v5.db"
        );

        // 版本号只接受十进制数字：前导零仍按数值比较；非数字、溢出、大小写差异和
        // 额外前后缀都静默忽略，避免把相似但非 WorkBuddy 文件误当成映射库。
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v01.db"]),
            "edge-sync-mapping-v01.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v01.db", "edge-sync-mapping-v1.db"]),
            "edge-sync-mapping-v1.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v+9.db"]),
            "edge-sync-mapping-v2.db"
        );
        assert_eq!(
            edge_sync_pick(cn, &["edge-sync-mapping-v18446744073709551616.db"]),
            "edge-sync-mapping-v2.db"
        );
        assert_eq!(
            edge_sync_pick(
                cn,
                &[
                    "edge-sync-mapping-vx.db",
                    "Edge-sync-mapping-v9.db",
                    "prefix-edge-sync-mapping-v9.db",
                    "edge-sync-mapping-v9.db.bak",
                    "edge-sync-mapping-v9-extra.db",
                ]
            ),
            "edge-sync-mapping-v2.db"
        );

        // 国际版走同一套发现逻辑（不再写死 v4）。
        assert_eq!(
            edge_sync_pick(WbVariant::Ai, &[]),
            "edge-sync-mapping-v4.db"
        );
        assert_eq!(
            edge_sync_pick(
                WbVariant::Ai,
                &["edge-sync-mapping-v4.db", "edge-sync-mapping-v6.db"]
            ),
            "edge-sync-mapping-v6.db"
        );

        // 目录不存在：安全回落到默认文件名，不 panic（调用方据此报「云端映射库不存在」）。
        let missing = temp_root("edge_sync_missing");
        assert_eq!(
            edge_sync_db_path(&missing, cn),
            missing.join("edge-sync-mapping-v2.db")
        );
        assert_eq!(
            edge_sync_db_path(&missing, WbVariant::Ai),
            missing.join("edge-sync-mapping-v4.db")
        );
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_switch_session_root_{}_{name}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn create_sessions_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT, title TEXT);",
        )
        .unwrap();
    }

    /// 能力探测：`projects/` 目录 + `workbuddy.db` 的 `sessions` 表同时存在才可用。
    #[test]
    fn session_copy_capability_probe() {
        let bare = temp_root("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(!session_copy_supported_at(&bare));

        let only_projects = temp_root("only-projects");
        std::fs::create_dir_all(only_projects.join("projects")).unwrap();
        assert!(!session_copy_supported_at(&only_projects));

        let empty_db = temp_root("empty-db");
        std::fs::create_dir_all(&empty_db).unwrap();
        let conn = Connection::open(empty_db.join("workbuddy.db")).unwrap();
        conn.execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        drop(conn);
        assert!(!session_copy_supported_at(&empty_db));

        let db_only = temp_root("db-only");
        std::fs::create_dir_all(&db_only).unwrap();
        create_sessions_db(&db_only.join("workbuddy.db"));
        assert!(!session_copy_supported_at(&db_only));

        let ready = temp_root("ready");
        std::fs::create_dir_all(ready.join("projects")).unwrap();
        create_sessions_db(&ready.join("workbuddy.db"));
        assert!(session_copy_supported_at(&ready));

        for dir in [bare, only_projects, empty_db, db_only, ready] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 能力不满足时返回明确错误，且不写任何文件。
    #[test]
    fn copy_sessions_for_switch_rejects_unsupported_root() {
        let env = Env::new("unsupported");
        let bare = Env {
            root: env.root.clone(),
            paths: SessionPaths {
                store_root: env.root.join("bare-store"),
                data_root: env.root.join("bare-data"),
                auth_file: env.root.join("auth.info"),
                link_namespace: LinkNamespace::WorkBuddy,
            },
        };
        std::fs::create_dir_all(bare.paths.data_root.clone()).unwrap();
        let err = copy_sessions_for_switch_at(
            &bare.paths(),
            WbVariant::Ai,
            &json!({"id": "ai-1", "uid": "u-ai", "variant": "ai"}),
            &["cid-1".to_string()],
            |_| false,
        )
        .expect_err("不支持的档位必须返回错误");
        assert!(err.contains(SESSION_COPY_UNSUPPORTED), "{err}");
        assert_eq!(std::fs::read_dir(&bare.paths.data_root).unwrap().count(), 0);
    }

    /// 能力探测只对国际版生效：国内版在探测不通过的数据根上仍走改造前的路径。
    #[test]
    fn session_copy_probe_only_gates_ai() {
        let env = Env::new("cn-no-probe");
        let bare = SessionPaths {
            store_root: env.root.join("bare-store"),
            data_root: env.root.join("bare-data"),
            auth_file: env.root.join("auth.info"),
            link_namespace: LinkNamespace::WorkBuddy,
        };
        std::fs::create_dir_all(&bare.data_root).unwrap();

        let cn_err = copy_sessions_for_switch_at(
            &bare,
            WbVariant::Cn,
            &json!({"id": "cn-1", "variant": "cn", "uid": "   "}),
            &["cid-1".to_string()],
            |_| false,
        )
        .expect_err("缺 uid 仍必须返回错误");
        assert_eq!(cn_err, "目标账号缺少 uid，无法复制会话");
        assert!(
            !cn_err.contains(SESSION_COPY_UNSUPPORTED),
            "国内版不得被能力探测拦截: {cn_err}"
        );

        let ai_err = copy_sessions_for_switch_at(
            &bare,
            WbVariant::Ai,
            &json!({"id": "ai-1", "uid": "u-ai", "variant": "ai"}),
            &["cid-1".to_string()],
            |_| false,
        )
        .expect_err("国际版不满足能力探测必须返回错误");
        assert!(ai_err.contains(SESSION_COPY_UNSUPPORTED), "{ai_err}");
    }

    /// 能力可用时继续走 uid 校验（证明探测不会误短路）。
    #[test]
    fn copy_sessions_for_switch_requires_target_uid() {
        let env = ready_env("requires-uid");
        let err = copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &json!({"id": "a-1", "variant": "cn", "uid": "   "}),
            &["sess-1".to_string()],
            |_| false,
        )
        .expect_err("缺 uid 必须返回错误");
        assert_eq!(err, "目标账号缺少 uid，无法复制会话");
    }

    /// 未登录 / 目标即当前账号：拒绝且不写任何东西。
    #[test]
    fn copy_rejects_missing_login_and_same_account() {
        let env = ready_env("login-checks");
        std::fs::remove_file(&env.paths.auth_file).unwrap();
        let err = copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &["sess-1".to_string()],
            |_| false,
        )
        .expect_err("缺登录态必须报错");
        assert!(err.contains("未读取到本机登录态"), "{err}");

        env.set_login("uid-b");
        let err = copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &["sess-1".to_string()],
            |_| false,
        )
        .expect_err("同一账号必须报错");
        assert!(err.contains("当前账号与目标账号相同"), "{err}");
        assert_eq!(env.rows_for("uid-b").len(), 0);
    }

    // ---------------------------------------------------------------------------
    // 幂等复制与关联组（R1）
    // ---------------------------------------------------------------------------

    #[test]
    fn copy_writes_body_row_and_link_group() {
        let env = ready_env("happy");
        let report = copy(&env, "uid-b", &["sess-1"]);

        assert_eq!(report["sourceUid"], "uid-a");
        assert_eq!(report["targetUid"], "uid-b");
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert_eq!(report["alreadyLinked"].as_array().unwrap().len(), 0);
        assert!(report.get("errors").is_none());
        assert!(report.get("needsRecovery").is_none());

        let new_id = env.first_copy_id(&report);
        assert_ne!(new_id, "sess-1");
        assert_eq!(report["copied"][0]["id"], "sess-1");

        // 正文：新 id 文件存在、旧 id 引用已替换，源文件不动。
        let copied_body = std::fs::read_to_string(env.body_path(&new_id)).unwrap();
        assert!(copied_body.contains(&new_id));
        assert!(!copied_body.contains("sess-1"));
        assert_eq!(
            std::fs::read_to_string(env.body_path("sess-1")).unwrap(),
            body_text("sess-1")
        );

        // 数据库行归属目标账号。
        assert_eq!(env.rows_for("uid-b"), vec![new_id.clone()]);
        assert_eq!(env.rows_for("uid-a"), vec!["sess-1".to_string()]);

        // 不预写云端映射：登记交接给客户端（edge-sync 扩展迁移时自行写入）。
        assert_eq!(env.mapping_rows(), 0, "复制不得预写映射行");

        // 关联组：同一逻辑会话、两个账号各一个 active 成员、一对基线。
        let store = env.store();
        assert!(
            store.revision >= 1,
            "首次落地空存储 + 关联提交都会推进 revision"
        );
        assert_eq!(store.groups.len(), 1);
        let group = &store.groups[0];
        assert_eq!(group.variant, WbVariant::Cn);
        assert_eq!(group.members.len(), 2);
        assert!(group
            .members
            .iter()
            .all(|member| member.state == MemberState::Active));
        assert_eq!(group.pair_bases.len(), 1);
        assert!(env
            .paths
            .baselines_dir()
            .join(format!("{}.json", group.pair_bases[0].baseline_ref))
            .exists());
        // 来源账号的成员带上了 accountId（账号库缺失时为 None，不影响身份判定）。
        assert!(group
            .members
            .iter()
            .any(|member| member.uid == "uid-a" && member.session_id == "sess-1"));

        // 成功清理：临时备份已回收，报告不展示可还原路径，也没有维护记录残留。
        assert!(report["copied"][0]["backup"].is_null(), "{report}");
        assert_eq!(report["copied"][0]["cleanupState"], "cleaned", "{report}");
        assert_eq!(report["temporaryFiles"], json!([]), "{report}");
        let operation_id = session_link::scan_operations(&env.paths)
            .operations
            .first()
            .expect("操作日志必须保留")
            .operation_id
            .clone();
        assert!(
            !session_backup::transaction_dir(&env.paths, WbVariant::Cn, &operation_id)
                .unwrap()
                .exists(),
            "成功路径必须回收本次操作专属目录"
        );
        assert!(session_backup::scan_lifecycle(&env.paths)
            .records
            .is_empty());
    }

    /// 复制保留源 `created_at`（会话历史起点不伪造），`updated_at` 仍写当前时间。
    ///
    /// `updated_at` 驱动客户端「最近更新」排序：副本沿用源的旧值会直接沉底，而副本
    /// 此刻确实刚被创建，写当前时间才符合事实。
    #[test]
    fn copy_preserves_source_created_at_and_refreshes_updated_at() {
        let env = ready_env("copy-created-at");
        // 源行由 add_session 写成 created_at=1000 / updated_at=2000。
        let report = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&report);

        assert_eq!(env.created_at_of("sess-1"), Some(1000), "源行 created_at");
        assert_eq!(
            env.created_at_of(&new_id),
            Some(1000),
            "副本必须沿用源的 created_at"
        );
        let copied_updated_at = env.updated_at_of(&new_id).expect("副本必须有 updated_at");
        assert!(
            copied_updated_at > 2000,
            "副本 updated_at 必须是本次时间而不是源的旧值：{copied_updated_at}"
        );
        assert_eq!(
            env.updated_at_of("sess-1"),
            Some(2000),
            "源行时间戳不得被复制改动"
        );
    }

    /// 源行 `created_at` 为 NULL（旧库没记）时回退当前时间：NULL 下传会破坏客户端排序。
    #[test]
    fn copy_falls_back_to_now_when_source_created_at_is_null() {
        let env = ready_env("copy-created-at-null");
        env.add_session("sess-2", "uid-a", "标题二");
        env.add_body("sess-2", &body_text("sess-2"));
        env.set_created_at("sess-2", None);
        assert_eq!(
            env.created_at_of("sess-2"),
            None,
            "源行 created_at 必须是 NULL"
        );

        let before = now_ms();
        let report = copy(&env, "uid-b", &["sess-2"]);
        let new_id = env.first_copy_id(&report);
        let after = now_ms();

        let copied_created_at = env
            .created_at_of(&new_id)
            .expect("源值为 NULL 时必须回退为具体时间，不得下传 NULL");
        assert!(
            (before..=after).contains(&copied_created_at),
            "回退值必须是本次时间：{copied_created_at} 不在 [{before}, {after}]"
        );
    }

    /// 批量复制逐项清理：进入下一项之前，上一成功项的临时目录已经消失（不累积备份）。
    #[test]
    fn batch_copy_cleans_each_backup_before_next_item() {
        let env = ready_env("batch-cleanup");
        env.add_session("sess-2", "uid-a", "标题二");
        env.add_body("sess-2", &body_text("sess-2"));

        let report = copy(&env, "uid-b", &["sess-1", "sess-2"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 2, "{report}");
        for item in report["copied"].as_array().unwrap() {
            assert!(item["backup"].is_null(), "{report}");
            assert_eq!(item["cleanupState"], "cleaned", "{report}");
        }
        // 两个成功项都不留目录与维护记录：连续操作不累积成功备份。
        let transactions = env
            .paths
            .backup_root()
            .join(session_backup::TRANSACTIONS_DIR_NAME)
            .join(WbVariant::Cn.as_str());
        let leftovers = std::fs::read_dir(&transactions)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "成功批次不得累积操作专属目录");
        assert!(session_backup::scan_lifecycle(&env.paths)
            .records
            .is_empty());
        assert_eq!(
            env.body_files().len(),
            4,
            "两条来源内容与两个复制后的内容都在"
        );
    }

    /// 归属不可验证（临时目录路径被替换为符号链接）：业务成功保持不变、材料保留并上报，
    /// 解除异常后由维护入口补清理。
    #[cfg(unix)]
    #[test]
    fn cleanup_protects_business_success_when_transaction_path_is_symlinked() {
        let env = ready_env("cleanup-symlink");
        let transactions = env
            .paths
            .backup_root()
            .join(session_backup::TRANSACTIONS_DIR_NAME);
        std::fs::create_dir_all(&transactions).unwrap();
        let target = env.root.join("redirected-transactions");
        std::fs::create_dir_all(&target).unwrap();
        // 档位目录被替换为符号链接：删除前校验必须拒绝，且不得跟随链接删除。
        std::os::unix::fs::symlink(&target, transactions.join(WbVariant::Cn.as_str())).unwrap();

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 1, "{report}");
        let new_id = env.first_copy_id(&report);
        // 业务成功不变：正文与数据库行都在，报告仍报成功，只是临时文件待处理。
        assert!(env.body_path(&new_id).exists());
        assert_eq!(env.rows_for("uid-b"), vec![new_id.clone()]);
        assert_eq!(report["copied"][0]["cleanupState"], "pending", "{report}");
        assert!(
            report["copied"][0]["backup"].as_str().is_some(),
            "待清理必须保留位置：{report}"
        );
        assert!(
            report["copied"][0]["cleanupError"]
                .as_str()
                .unwrap()
                .contains("符号链接"),
            "{report}"
        );
        let temporary_files = report["temporaryFiles"].as_array().expect("temporaryFiles");
        assert!(
            !temporary_files.is_empty(),
            "本轮清理受阻必须出现在报告级 temporaryFiles：{report}"
        );
        assert!(
            temporary_files.iter().any(|item| item["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("符号链接"))),
            "{report}"
        );
        assert_eq!(session_backup::scan_lifecycle(&env.paths).records.len(), 1);

        // 解除异常后：维护入口补清理，业务结果不变；链接目标不被当作本操作材料删除。
        std::fs::remove_file(transactions.join(WbVariant::Cn.as_str())).unwrap();
        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(recovery.is_clean(), "{:?}", recovery.needs_recovery);
        assert!(session_backup::scan_lifecycle(&env.paths)
            .records
            .is_empty());
        assert!(env.body_path(&new_id).exists());
        assert_eq!(
            session_link::scan_operations(&env.paths).operations[0]
                .cleanup_state
                .as_deref(),
            Some(session_backup::CLEANUP_STATE_CLEANED)
        );
    }

    #[test]
    fn copy_retry_reuses_member_without_second_copy() {
        let env = ready_env("retry");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);
        let revision_before_retry = env.store().revision;

        let second = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(second["copied"].as_array().unwrap().len(), 0);
        assert_eq!(second["alreadyLinked"].as_array().unwrap().len(), 1);
        assert_eq!(second["alreadyLinked"][0]["sessionId"], new_id);
        assert!(second.get("errors").is_none());

        assert_eq!(env.body_files().len(), 2, "重试不得产生第二个副本");
        assert_eq!(env.rows_for("uid-b").len(), 1);
        assert_eq!(
            env.store().revision,
            revision_before_retry,
            "幂等复用不写同步记录"
        );
        assert_eq!(env.store().groups[0].members.len(), 2);
    }

    #[test]
    fn copy_back_reuses_original_session() {
        let env = ready_env("back");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);

        // 目标账号成为当前账号，把副本复制回原账号：必须复用原件。
        env.set_login("uid-b");
        let back = copy(&env, "uid-a", &[&new_id]);
        assert_eq!(back["copied"].as_array().unwrap().len(), 0);
        assert_eq!(back["alreadyLinked"].as_array().unwrap().len(), 1);
        assert_eq!(back["alreadyLinked"][0]["sessionId"], "sess-1");

        assert_eq!(env.body_files().len(), 2, "B→A 不得新建副本");
        assert_eq!(env.rows_for("uid-a"), vec!["sess-1".to_string()]);
        assert_eq!(env.store().groups[0].members.len(), 2);
    }

    #[test]
    fn chain_a_to_b_then_a_to_c_then_b_to_c_reuses_existing_copy() {
        let env = ready_env("chain");
        let to_b = copy(&env, "uid-b", &["sess-1"]);
        let b_id = env.first_copy_id(&to_b);

        // 同一来源再复制给 C：同组内新增一个成员，不是新组。
        let to_c = copy(&env, "uid-c", &["sess-1"]);
        let c_id = env.first_copy_id(&to_c);
        assert_eq!(env.store().groups.len(), 1);
        assert_eq!(env.store().groups[0].members.len(), 3);

        // B→C：组内已有 C 的有效副本，复用而不重复复制。
        env.set_login("uid-b");
        let b_to_c = copy(&env, "uid-c", &[&b_id]);
        assert_eq!(b_to_c["copied"].as_array().unwrap().len(), 0);
        assert_eq!(b_to_c["alreadyLinked"][0]["sessionId"], c_id);
        assert_eq!(env.body_files().len(), 3);

        // 配对基线按成员对保存：A/B、A/C 各有基线；C 加入时按 A/B 基线继承出 B/C。
        let group = &env.store().groups[0];
        assert_eq!(group.pair_bases.len(), 3);
        let member_id = |uid: &str| {
            group
                .members
                .iter()
                .find(|member| member.uid == uid)
                .unwrap()
                .member_id
                .clone()
        };
        let (a, b, c) = (member_id("uid-a"), member_id("uid-b"), member_id("uid-c"));
        let pair = |left: &str, right: &str| {
            session_link::find_pair_base(group, left, right)
                .unwrap_or_else(|| panic!("缺少成员对基线 {left}/{right}"))
                .baseline_ref
                .clone()
        };
        assert_eq!(pair(&a, &b), pair(&b, &c), "B/C 继承自 A/B 的共同基线");
        assert_ne!(pair(&a, &c), pair(&a, &b), "A/C 是各自新建的基线");
    }

    #[test]
    fn rename_does_not_break_link() {
        let env = ready_env("rename");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);

        // 用户在目标账号改名：关联不依赖标题。
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET title = '改过的标题', custom_title = '自定义名' WHERE id = ?1",
            [&new_id],
        )
        .unwrap();
        drop(conn);

        let again = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(again["alreadyLinked"][0]["sessionId"], new_id);
        assert_eq!(env.body_files().len(), 2);
        assert_eq!(env.store().groups[0].members.len(), 2);
    }

    #[test]
    fn same_title_independent_sessions_stay_separate() {
        let env = ready_env("same-title");
        env.add_session("sess-2", "uid-a", "标题一");
        env.add_body("sess-2", &body_text("sess-2"));

        let report = copy(&env, "uid-b", &["sess-1", "sess-2"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 2);
        let ids: Vec<String> = report["copied"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["newId"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);

        // 同标题不建立关联：第二个会话各成一个组，且各自独立幂等。
        assert_eq!(env.store().groups.len(), 2);
        assert_eq!(env.rows_for("uid-b").len(), 2);
        assert_eq!(env.body_files().len(), 4);

        let again = copy(&env, "uid-b", &["sess-1", "sess-2"]);
        assert_eq!(again["alreadyLinked"].as_array().unwrap().len(), 2);
        assert_eq!(env.body_files().len(), 4);
    }

    #[test]
    fn invalid_target_member_is_superseded_and_rebuilt_without_resurrection() {
        let env = ready_env("rebuild");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let old_id = env.first_copy_id(&first);

        // 目标副本的正文丢失 → 旧成员失效，重建新成员。
        std::fs::remove_file(env.body_path(&old_id)).unwrap();
        let rebuilt = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(rebuilt["copied"].as_array().unwrap().len(), 1);
        let new_id = env.first_copy_id(&rebuilt);
        assert_ne!(new_id, old_id);
        assert_eq!(
            env.body_files().len(),
            2,
            "旧复制后的内容已删，只剩源与新建副本"
        );

        let group = &env.store().groups[0];
        assert_eq!(group.members.len(), 3);
        let actives: Vec<&str> = group
            .members
            .iter()
            .filter(|member| member.uid == "uid-b" && member.state == MemberState::Active)
            .map(|member| member.session_id.as_str())
            .collect();
        assert_eq!(actives, vec![new_id.as_str()], "每账号唯一 active");
        assert_eq!(
            group
                .members
                .iter()
                .find(|member| member.session_id == old_id)
                .unwrap()
                .state,
            MemberState::Superseded
        );

        // 旧正文恢复后不得自动争夺有效位置：仍是重建后的成员有效。
        env.add_body(&old_id, &body_text(&old_id));
        let after_restore = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(after_restore["alreadyLinked"][0]["sessionId"], new_id);
        assert_eq!(env.store().groups[0].members.len(), 3, "不新增成员");
    }

    #[test]
    fn identity_uses_uid_and_never_rebinds_by_account_id() {
        let env = ready_env("identity");

        // 手工写入一个组：目标成员 uid=uid-b，accountId 是旧账号 id（重新导入前的 id）。
        let paths = env.paths();
        session_link::with_link_store_write(&paths, |store| {
            store.groups.push(LinkGroup {
                id: "g-1".to_string(),
                variant: WbVariant::Cn,
                created_at: 1,
                members: vec![
                    LinkMember {
                        member_id: "m-a".to_string(),
                        account_id: Some("acc-uid-a".to_string()),
                        uid: "uid-a".to_string(),
                        session_id: "sess-1".to_string(),
                        variant: None,
                        state: MemberState::Active,
                        linked_at: 1,
                        last_synced_at: None,
                    },
                    LinkMember {
                        member_id: "m-b".to_string(),
                        account_id: Some("old-account-id".to_string()),
                        uid: "uid-b".to_string(),
                        session_id: "sess-b".to_string(),
                        variant: None,
                        state: MemberState::Active,
                        linked_at: 1,
                        last_synced_at: None,
                    },
                ],
                pair_bases: Vec::new(),
            });
            Ok(())
        })
        .unwrap();
        env.add_body("sess-b", &body_text("sess-b"));
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, created_at, updated_at, deleted_at, is_playground)
             VALUES ('sess-b', 'uid-b', '旧副本', '/ws/a', 1, 2, NULL, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        // uid 相同、accountId 变了 → 仍复用（身份以 uid 为准）。
        let reused = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(reused["alreadyLinked"][0]["sessionId"], "sess-b");
        assert_eq!(env.body_files().len(), 2);

        // accountId 相同但 uid 不同 → 不得错误绑定，必须新建。
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET user_id = 'uid-x' WHERE id = 'sess-b'",
            [],
        )
        .unwrap();
        drop(conn);
        let fresh = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(fresh["copied"].as_array().unwrap().len(), 1);
        assert_eq!(env.store().groups[0].members.len(), 3);
    }

    #[test]
    fn variant_isolation_keeps_groups_separate() {
        let env = ready_env("variants");
        let cn = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(cn["copied"].as_str(), None);
        assert_eq!(cn["copied"].as_array().unwrap().len(), 1);

        // 国际版数据根：单独一套数据（同 store 根），身份字符串相同但档位不同。
        let mut ai_paths = env.paths();
        ai_paths.data_root = env.root.join("data-ai");
        std::fs::create_dir_all(ai_paths.projects_dir().join("ws-a")).unwrap();
        let conn = Connection::open(ai_paths.workbuddy_db()).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, title TEXT, custom_title TEXT, cwd TEXT, created_at INTEGER, updated_at INTEGER, deleted_at INTEGER, is_playground INTEGER);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, created_at, updated_at, deleted_at, is_playground)
             VALUES ('ai-sess-1', 'uid-a', 'AI 会话', '/ws/a', 1, 2, NULL, 0)",
            [],
        )
        .unwrap();
        drop(conn);
        std::fs::write(
            ai_paths.projects_dir().join("ws-a").join("ai-sess-1.jsonl"),
            body_text("ai-sess-1"),
        )
        .unwrap();
        let conn = Connection::open(ai_paths.edge_sync_db(WbVariant::Ai)).unwrap();
        conn.execute_batch(
            "CREATE TABLE edge_sync_mapping (session_id TEXT, conversation_id TEXT, msg_channel TEXT, created_at INTEGER);",
        )
        .unwrap();
        drop(conn);

        let ai_report = copy_sessions_for_switch_at(
            &ai_paths,
            WbVariant::Ai,
            &json!({"id": "acc-uid-b", "uid": "uid-b", "variant": "ai"}),
            &["ai-sess-1".to_string()],
            |_| false,
        )
        .unwrap();
        assert_eq!(
            ai_report["copied"].as_array().unwrap().len(),
            1,
            "同档位身份不同，必须新复制"
        );

        let store = env.store();
        assert_eq!(store.groups.len(), 2);
        let variants: Vec<&str> = store
            .groups
            .iter()
            .map(|group| group.variant.as_str())
            .collect();
        assert!(
            variants.contains(&"cn") && variants.contains(&"ai"),
            "{variants:?}"
        );
        // 两档位不串数据：成员会话 id 不相交，国际版组里带着国际版来源会话。
        let mut seen = std::collections::HashSet::new();
        for group in &store.groups {
            for member in &group.members {
                assert!(
                    seen.insert(member.session_id.clone()),
                    "会话 {} 同时出现在两个档位的组里",
                    member.session_id
                );
            }
        }
        let ai_group = store
            .groups
            .iter()
            .find(|group| group.variant == WbVariant::Ai)
            .unwrap();
        assert!(ai_group
            .members
            .iter()
            .any(|member| member.session_id == "ai-sess-1"));
    }

    /// 报告契约：copied / alreadyLinked / errors 同时出现时字段完整（桌面与 webui 同形）。
    #[test]
    fn copy_report_carries_copied_already_linked_and_errors_together() {
        let env = ready_env("contract");
        env.add_session("sess-2", "uid-a", "标题二");
        env.add_body("sess-2", &body_text("sess-2"));

        let first = copy(&env, "uid-b", &["sess-1", "sess-2"]);
        assert_eq!(first["copied"].as_array().unwrap().len(), 2);

        // 第二个会话的正文丢失：本次一个复用、一个失败。
        std::fs::remove_file(env.body_path("sess-2")).unwrap();
        let mixed = copy(&env, "uid-b", &["sess-1", "sess-2"]);
        assert_eq!(mixed["sourceUid"], "uid-a");
        assert_eq!(mixed["targetUid"], "uid-b");
        assert_eq!(mixed["copied"].as_array().unwrap().len(), 0);
        assert_eq!(mixed["alreadyLinked"].as_array().unwrap().len(), 1);
        assert_eq!(mixed["alreadyLinked"][0]["id"], "sess-1");
        assert_eq!(mixed["errors"].as_array().unwrap().len(), 1);
        assert_eq!(mixed["errors"][0]["id"], "sess-2");
        assert_eq!(mixed["errors"][0]["error"], "会话内容不存在，未复制");
        // 复用/失败都不算未完成写入。
        assert!(mixed.get("needsRecovery").is_none());
    }

    // ---------------------------------------------------------------------------
    // 失败必须可见、可恢复（R1 / R5）
    // ---------------------------------------------------------------------------

    #[test]
    fn missing_body_is_not_reported_as_success() {
        let env = ready_env("no-body");
        std::fs::remove_file(env.body_path("sess-1")).unwrap();

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        assert_eq!(report["errors"][0]["error"], "会话内容不存在，未复制");
        assert_eq!(env.rows_for("uid-b").len(), 0, "不得写出半成品会话记录");
        assert_eq!(env.body_files().len(), 0);
    }

    #[test]
    fn truncated_body_is_not_reported_as_success() {
        let env = ready_env("truncated");
        env.add_body("sess-1", "{\"sessionId\":\"sess-1\"\n");

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("会话内容无法验证"), "{error}");
        assert_eq!(env.rows_for("uid-b").len(), 0);
    }

    #[test]
    fn missing_source_row_is_not_reported_as_success() {
        let env = ready_env("no-row");
        env.delete_row("sess-1");

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        assert_eq!(
            report["errors"][0]["error"],
            "数据库中找不到源会话记录，未复制"
        );
        assert_eq!(env.body_files().len(), 1, "不得写出无数据库行的内容");
    }

    #[test]
    fn source_row_of_another_account_is_rejected() {
        let env = ready_env("other-owner");
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET user_id = 'uid-x' WHERE id = 'sess-1'",
            [],
        )
        .unwrap();
        drop(conn);

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        assert_eq!(report["errors"][0]["error"], "源会话不属于当前账号，未复制");
    }

    /// 复制不再依赖云端映射库：库缺失也照常成功，不重建、不写行。
    #[test]
    fn copy_succeeds_without_edge_sync_db() {
        let env = ready_env("mapping-absent");
        std::fs::remove_file(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert!(report.get("errors").is_none(), "{report}");
        assert_eq!(env.body_files().len(), 2);
        assert_eq!(env.rows_for("uid-b").len(), 1);
        assert!(
            !env.paths().edge_sync_db(WbVariant::Cn).exists(),
            "复制不得创建或写入映射库"
        );
        assert!(session_link::pending_operations(&env.paths(), WbVariant::Cn).is_empty());
    }

    /// 关联提交失败（存储目录不可写）→ 不报成功；恢复后同一 UUID 完成。
    #[cfg(unix)]
    #[test]
    fn link_commit_failure_keeps_pending_then_recovers_with_same_uuid() {
        use std::os::unix::fs::PermissionsExt;

        let env = ready_env("link-commit");
        // 先跑一次成功，建立锁文件与存储文件，避免把「无法加锁」当成关联提交失败。
        copy(&env, "uid-c", &["sess-1"]);
        if std::fs::write(env.paths.store_root.join(".probe"), b"x").is_err() {
            return;
        }

        std::fs::set_permissions(
            &env.paths.store_root,
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        if std::fs::write(env.paths.store_root.join(".probe"), b"x").is_ok() {
            // root / 特殊 ACL 环境写保护无效：跳过（不误报）。
            std::fs::set_permissions(
                &env.paths.store_root,
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            return;
        }

        let failed = copy(&env, "uid-b", &["sess-1"]);
        std::fs::set_permissions(
            &env.paths.store_root,
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(failed["copied"].as_array().unwrap().len(), 0);
        let error = failed["errors"][0]["error"].as_str().unwrap();
        assert!(
            error.contains("保存失败") || error.contains("同步记录"),
            "{error}"
        );
        assert_eq!(failed["needsRecovery"], true);

        let pending = session_link::pending_operations(&env.paths(), WbVariant::Cn);
        let target = pending
            .iter()
            .find(|operation| operation.target.uid == "uid-b")
            .expect("应保留未完成操作");
        let new_id = target.target.session_id.clone();

        let retry = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(retry["copied"].as_array().unwrap().len(), 0);
        assert_eq!(retry["alreadyLinked"][0]["sessionId"], new_id);
        assert_eq!(
            env.body_files().len(),
            4,
            "两个目标各一份副本，重试不得新增"
        );
        assert_eq!(env.rows_for("uid-b"), vec![new_id]);
    }

    /// 正文写入失败：不得写出数据库行，也不报成功。
    #[cfg(unix)]
    #[test]
    fn body_write_failure_reports_error_without_db_row() {
        use std::os::unix::fs::PermissionsExt;

        let env = ready_env("body-write");
        let ws = env.paths.projects_dir().join("ws-a");
        std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::write(ws.join(".probe"), b"x").is_ok();
        let report = copy(&env, "uid-b", &["sess-1"]);
        std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o755)).unwrap();
        if writable {
            // root / 特殊 ACL 环境：写保护无效，跳过断言。
            return;
        }
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("复制后的内容保存失败"), "{error}");
        assert_eq!(env.rows_for("uid-b").len(), 0);
        assert_eq!(env.body_files().len(), 1);
    }

    #[test]
    fn corrupt_store_blocks_copy_and_preserves_original() {
        let env = ready_env("corrupt-store");
        std::fs::create_dir_all(&env.paths.store_root).unwrap();
        std::fs::write(env.paths.session_links_file(), "not-json").unwrap();

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(
            error.contains("同步记录") && error.contains("已阻止复制"),
            "{error}"
        );
        assert_eq!(env.rows_for("uid-b").len(), 0);
        assert_eq!(env.body_files().len(), 1);
        assert_eq!(
            std::fs::read_to_string(env.paths.session_links_file()).unwrap(),
            "not-json",
            "必须保留现场，不得当空表覆盖"
        );
    }

    #[test]
    fn unknown_store_version_blocks_copy() {
        let env = ready_env("unknown-version");
        std::fs::create_dir_all(&env.paths.store_root).unwrap();
        std::fs::write(
            env.paths.session_links_file(),
            json!({"version": 99, "revision": 1, "groups": []}).to_string(),
        )
        .unwrap();

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("版本"), "{error}");
        assert_eq!(env.rows_for("uid-b").len(), 0);
    }

    /// 主文件被删但基线文件仍在 → 检测到痕迹，不得当首次使用重建空表，复制被阻止。
    #[test]
    fn copy_blocked_when_store_file_missing_but_baselines_remain() {
        let env = ready_env("baseline-trace");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);
        assert!(env.baseline_files() > 0, "首次复制应留下基线文件");

        std::fs::remove_file(env.paths.session_links_file()).unwrap();
        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        assert_eq!(report["alreadyLinked"].as_array().unwrap().len(), 0);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("保留现场"), "{error}");
        assert!(
            !env.paths.session_links_file().exists(),
            "不得重建空表覆盖现场"
        );
        assert_eq!(env.body_files().len(), 2, "不得产生第二个副本");
        assert_eq!(env.rows_for("uid-b"), vec![new_id]);
    }

    /// 并发请求：档位操作锁被占用时直接拒绝，不产生第二个副本。
    #[test]
    fn concurrent_request_is_rejected_without_second_copy() {
        let env = ready_env("concurrent");
        let held = session_link::try_acquire_variant_ops_lock(&env.paths(), WbVariant::Cn).unwrap();

        let err = copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &["sess-1".to_string()],
            |_| false,
        )
        .expect_err("持锁期间必须拒绝");
        assert!(err.contains("会话操作"), "{err}");
        assert_eq!(env.body_files().len(), 1);

        drop(held);
        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
    }

    /// 目标账号不是当前登录账号（auth=uid-a）时，App 运行中不再拦截写入（S5 实测口径）；
    /// 快拒与锁后复查仍各执行一次，且复查必须发生在持锁之后、写入之前。
    #[test]
    fn copy_runs_while_app_running_for_non_current_account() {
        let env = ready_env("app-running-non-current");
        let probes = std::cell::Cell::new(0usize);
        let lock_held_on_recheck = std::cell::Cell::new(false);
        let lock_path = env.paths.variant_ops_lock_file(WbVariant::Cn);
        let probe = |_: WbVariant| {
            let n = probes.get() + 1;
            probes.set(n);
            if n == 1 {
                true
            } else {
                // 第二次必须发生在持锁之后：此时再抢同一把锁应为 Busy。
                match session_link::try_lock_file(&lock_path) {
                    Err(session_link::LockError::Busy) => lock_held_on_recheck.set(true),
                    Err(session_link::LockError::Unavailable(reason)) => {
                        panic!("第二次探针时期望档位锁已被持有，实际 Unavailable: {reason}")
                    }
                    Ok(_) => panic!("第二次探针时期望档位锁已被持有，实际拿到了锁"),
                }
                true
            }
        };
        let report = copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &["sess-1".to_string()],
            probe,
        )
        .expect("目标非当前登录账号：运行中必须放行");
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert_eq!(probes.get(), 2, "快拒与锁后复查各检查一次");
        assert!(
            lock_held_on_recheck.get(),
            "复查必须发生在已经拿到档位锁之后、任何写入之前"
        );
        assert_eq!(env.rows_for("uid-b").len(), 1, "副本行已写入");
        assert!(env.paths.session_links_file().exists(), "关联记录已建立");
    }

    // ---------------------------------------------------------------------------
    // 恢复
    // ---------------------------------------------------------------------------

    #[test]
    fn recovery_abandons_prepared_operation_when_source_is_gone() {
        let env = ready_env("abandon");
        let paths = env.paths();
        session_link::save_operation(
            &paths,
            &Operation {
                version: crate::modules::session_link::OPERATION_VERSION,
                operation_id: "op-gone".to_string(),
                kind: "copy".to_string(),
                variant: WbVariant::Cn,
                source_variant: None,
                group_id: "g-gone".to_string(),
                source: OperationMember {
                    account_id: None,
                    uid: "uid-a".to_string(),
                    session_id: "sess-gone".to_string(),
                },
                target: OperationMember {
                    account_id: None,
                    uid: "uid-b".to_string(),
                    session_id: "new-gone".to_string(),
                },
                expected_content_digest: "d".to_string(),
                expected_record_count: 1,
                phase: crate::modules::session_link::OpPhase::Prepared,
                backup: None,
                lifecycle_version: None,
                cleanup_state: None,
                last_error: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();

        let report = recover_pending_session_operations_at(&paths, WbVariant::Cn);
        assert_eq!(report.abandoned, vec!["op-gone".to_string()]);
        assert!(report.is_clean());
        assert!(session_link::pending_operations(&paths, WbVariant::Cn).is_empty());
        assert_eq!(env.body_files().len(), 1);
    }

    #[test]
    fn recovery_stops_when_intermediate_body_was_modified() {
        let env = ready_env("recovery-stop");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);
        let paths = env.paths();

        // 模拟"正文已写、后续阶段中断"：把已完成的操作日志回退到 BodyWritten。
        let mut operation = session_link::scan_operations(&paths)
            .operations
            .into_iter()
            .find(|operation| operation.target.session_id == new_id)
            .expect("应能找到该副本的操作记录");
        operation.phase = OpPhase::BodyWritten;
        session_link::save_operation(&paths, &operation).unwrap();

        // 中间产物被其它程序改动 → 停止恢复，不覆盖。
        let tampered = format!(
            "{}\n",
            json!({"type": "user", "sessionId": new_id, "text": "别人改的"})
        );
        std::fs::write(env.body_path(&new_id), &tampered).unwrap();

        let report = recover_pending_session_operations_at(&paths, WbVariant::Cn);
        assert!(!report.is_clean());
        let issue = &report.needs_recovery[0];
        assert!(!issue.retryable);
        assert!(
            issue.reason.contains("目标内容与操作记录不一致"),
            "{}",
            issue.reason
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&new_id)).unwrap(),
            tampered
        );

        // 该会话再次请求时不新建副本，而是报告未完成。
        let again = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(again["copied"].as_array().unwrap().len(), 0);
        assert_eq!(again["errors"].as_array().unwrap().len(), 1);
        assert!(again["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("上一次复制尚未完成"));
        assert_eq!(env.body_files().len(), 2, "不得产生第二个副本");
    }

    /// 复制完成后再次请求同一会话：报告 alreadyLinked，不新建副本。
    #[test]
    fn retry_after_completed_copy_does_not_duplicate() {
        let env = ready_env("partial-resume");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);
        let body_count = env.body_files().len();
        assert_eq!(body_count, 2);

        let again = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(again["copied"].as_array().unwrap().len(), 0);
        assert_eq!(again["alreadyLinked"][0]["sessionId"], new_id);
        assert_eq!(env.body_files().len(), body_count, "不得产生第二个副本");
        assert_eq!(env.rows_for("uid-b").len(), 1);
    }

    /// 恢复不重放已完成的阶段：产物全部就位、只剩 Completed 未写时，只补写阶段标记，
    /// 不重写关联存储与基线、不重复登记映射（design §5）。
    #[test]
    fn recovery_completes_without_replaying_finished_stages() {
        let env = ready_env("recover-no-replay");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);
        let paths = env.paths();

        // 模拟「关联已提交、Completed 写入失败」：把已完成的操作日志回退到 LinksCommitted。
        let mut operation = session_link::scan_operations(&paths)
            .operations
            .into_iter()
            .find(|operation| operation.target.session_id == new_id)
            .expect("应能找到该副本的操作记录");
        operation.phase = OpPhase::LinksCommitted;
        session_link::save_operation(&paths, &operation).unwrap();
        let operation_id = operation.operation_id.clone();

        let before = env.store();
        let revision_before = before.revision;
        let pair_bases_before = before.groups[0].pair_bases.clone();
        let baselines_before = env.baseline_files();
        let members_before = before.groups[0].members.len();

        let report = recover_pending_session_operations_at(&paths, WbVariant::Cn);
        assert_eq!(report.recovered, vec![operation_id]);
        assert!(report.is_clean(), "{:?}", report.needs_recovery);

        let after = env.store();
        assert_eq!(after.revision, revision_before, "恢复不得重写同步记录");
        assert_eq!(after.groups[0].pair_bases.len(), pair_bases_before.len());
        assert_eq!(
            after.groups[0].pair_bases[0].baseline_ref, pair_bases_before[0].baseline_ref,
            "不得重写配对基线"
        );
        assert_eq!(after.groups[0].members.len(), members_before);
        assert_eq!(env.baseline_files(), baselines_before, "不得新增基线文件");
        assert_eq!(
            env.mapping_rows(),
            0,
            "复制不再预写云端映射，恢复也不得补写"
        );
        assert_eq!(env.body_files().len(), 2, "不得产生第二个副本");
        assert_eq!(env.rows_for("uid-b"), vec![new_id]);
        assert!(session_link::pending_operations(&paths, WbVariant::Cn).is_empty());
    }

    /// phase 已是 LinksCommitted，但关联主文件被删：不得跳过核验后标 Completed。
    #[test]
    fn recovery_stops_when_committed_links_are_missing() {
        let env = ready_env("recover-links-gone");
        let first = copy(&env, "uid-b", &["sess-1"]);
        let new_id = env.first_copy_id(&first);
        let paths = env.paths();

        let mut operation = session_link::scan_operations(&paths)
            .operations
            .into_iter()
            .find(|operation| operation.target.session_id == new_id)
            .expect("应能找到该副本的操作记录");
        operation.phase = OpPhase::LinksCommitted;
        session_link::save_operation(&paths, &operation).unwrap();
        std::fs::remove_file(paths.session_links_file()).unwrap();

        let report = recover_pending_session_operations_at(&paths, WbVariant::Cn);
        assert!(report.recovered.is_empty(), "{:?}", report.recovered);
        assert_eq!(report.needs_recovery.len(), 1);
        assert!(!report.needs_recovery[0].retryable);
        assert!(
            report.needs_recovery[0].reason.contains("同步记录"),
            "{}",
            report.needs_recovery[0].reason
        );
        assert!(
            !paths.session_links_file().exists(),
            "不得把缺失的主文件当成空表重建"
        );
        assert_eq!(env.body_files().len(), 2, "不得产生第二个副本");
        assert_eq!(
            session_link::pending_operations(&paths, WbVariant::Cn).len(),
            1,
            "必须保留未完成操作，不能标 Completed"
        );
    }

    // ---------------------------------------------------------------------------
    // 账户/环境辅助
    // ---------------------------------------------------------------------------

    #[test]
    fn unparseable_operation_log_blocks_copy_without_second_replica() {
        let env = ready_env("bad-op-json");
        std::fs::create_dir_all(env.paths.operations_dir()).unwrap();
        std::fs::write(env.paths.operations_dir().join("broken.json"), "not-json").unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths(), WbVariant::Cn);
        assert!(!recovery.is_clean());
        assert_eq!(recovery.needs_recovery.len(), 1);
        assert!(!recovery.needs_recovery[0].retryable);
        assert!(
            recovery.needs_recovery[0]
                .reason
                .contains(UNPARSEABLE_OPERATION_REASON),
            "{}",
            recovery.needs_recovery[0].reason
        );

        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        assert_eq!(report["alreadyLinked"].as_array().unwrap().len(), 0);
        assert_eq!(report["needsRecovery"], true);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains(UNPARSEABLE_OPERATION_REASON), "{error}");
        assert_eq!(
            env.body_files().len(),
            1,
            "不得绕过损坏的操作记录写出第二个副本"
        );
        assert_eq!(env.rows_for("uid-b").len(), 0);
    }

    #[test]
    fn recovery_reports_store_unavailable_instead_of_writing() {
        let env = ready_env("store-broken-recovery");
        std::fs::create_dir_all(&env.paths.store_root).unwrap();
        std::fs::write(env.paths.session_links_file(), "not-json").unwrap();

        let report = recover_pending_session_operations_at(&env.paths(), WbVariant::Cn);
        assert!(report.is_clean(), "没有未完成操作时不做任何事");
        assert_eq!(
            std::fs::read_to_string(env.paths.session_links_file()).unwrap(),
            "not-json"
        );
    }

    #[test]
    fn session_display_title_prefers_custom_title() {
        assert_eq!(
            session_display_title(Some("自动标题".into()), Some("美团每日自动领券".into())),
            "美团每日自动领券"
        );
        assert_eq!(
            session_display_title(None, Some("美团每日自动领券".into())),
            "美团每日自动领券"
        );
        assert_eq!(
            session_display_title(Some("汉字详情页".into()), None),
            "汉字详情页"
        );
        assert_eq!(session_display_title(None, None), "(无标题)");
        assert_eq!(
            session_display_title(Some("  ".into()), Some("".into())),
            "(无标题)"
        );
    }

    #[test]
    fn claw_workspace_detected_by_folder_name() {
        assert!(is_claw_workspace("/Users/apple/WorkBuddy/Claw"));
        assert!(is_claw_workspace("/Users/apple/WorkBuddy/claw/"));
        assert!(is_claw_workspace(r"C:\Users\me\WorkBuddy\Claw"));
        assert!(!is_claw_workspace("/Users/apple/WorkBuddy/ClawBot"));
        assert!(!is_claw_workspace(
            "/Users/apple/Documents/AI-PROJECT/LetterTotTown"
        ));
    }

    #[test]
    fn list_sessions_marks_has_history() {
        let env = ready_env("list-sessions");
        let sessions = list_sessions_for_user_at(&env.paths(), "uid-a");
        assert_eq!(sessions.as_array().unwrap().len(), 1);
        assert_eq!(sessions[0]["id"], "sess-1");
        assert_eq!(sessions[0]["hasHistory"], true);

        std::fs::remove_file(env.body_path("sess-1")).unwrap();
        let sessions = list_sessions_for_user_at(&env.paths(), "uid-a");
        assert_eq!(sessions[0]["hasHistory"], false);
    }

    #[test]
    fn insert_session_copy_duplicates_row_with_target_uid() {
        let env = ready_env("insert");
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, created_at, updated_at, deleted_at, is_playground)
             VALUES ('src-1', 'uid-a', '旧标题', '/ws', 1000, 2000, NULL, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let outcome = insert_session_copy(
            &env.paths(),
            &env.paths(),
            "new-uuid-1",
            "src-1",
            "uid-a",
            "uid-b",
        )
        .unwrap();
        assert_eq!(outcome, DbCopyOutcome::Inserted);

        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        let (id, user_id, title, deleted_at, is_playground, created_at, updated_at): (
            String,
            String,
            String,
            Option<i64>,
            i64,
            Option<i64>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT id, user_id, title, deleted_at, is_playground, created_at, updated_at \
                 FROM sessions WHERE id = 'new-uuid-1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(id, "new-uuid-1");
        assert_eq!(user_id, "uid-b");
        assert_eq!(title, "旧标题"); // 普通列原样保留
        assert_eq!(deleted_at, None);
        assert_eq!(is_playground, 0);
        assert_eq!(created_at, Some(1000), "created_at 必须是源的会话起点");
        assert!(
            updated_at.is_some_and(|value| value > 2000),
            "updated_at 必须刷新为本次时间：{updated_at:?}"
        );
    }

    #[test]
    fn insert_session_copy_reports_missing_source_and_db() {
        let env = ready_env("insert-missing");
        assert_eq!(
            insert_session_copy(
                &env.paths(),
                &env.paths(),
                "new-1",
                "missing",
                "uid-a",
                "uid-b"
            )
            .unwrap(),
            DbCopyOutcome::SourceRowMissing,
            "源行缺失必须显式上报，不能当成功（旧实现的假成功）"
        );

        std::fs::remove_file(env.paths.workbuddy_db()).unwrap();
        assert_eq!(
            insert_session_copy(
                &env.paths(),
                &env.paths(),
                "new-1",
                "sess-1",
                "uid-a",
                "uid-b"
            )
            .unwrap(),
            DbCopyOutcome::NoDb
        );
    }

    #[test]
    fn backup_failure_is_propagated_instead_of_claimed_success() {
        let env = ready_env("backup-fail");
        std::fs::remove_file(env.paths.workbuddy_db()).unwrap();
        let err = backup_workbuddy_db(&env.paths(), &env.paths.backup_root()).unwrap_err();
        assert!(err.contains("会话数据不存在"), "{err}");

        // 正常备份返回主库路径且大小一致。
        env.create_db();
        env.add_session("sess-1", "uid-a", "标题一");
        let backup = backup_workbuddy_db(&env.paths(), &env.paths.backup_root()).unwrap();
        assert!(backup.ends_with("workbuddy.db"));
        assert_eq!(
            std::fs::metadata(&backup).unwrap().len(),
            std::fs::metadata(env.paths.workbuddy_db()).unwrap().len()
        );
    }

    #[test]
    fn session_row_owner_reads_target_uid() {
        let env = ready_env("row-owner");
        assert_eq!(
            session_row_owner(&env.paths(), "sess-1").as_deref(),
            Some("uid-a")
        );
        assert_eq!(session_row_owner(&env.paths(), "missing"), None);
        env.delete_row("sess-1");
        assert_eq!(session_row_owner(&env.paths(), "sess-1"), None);
    }

    // ---------------------------------------------------------------------------
    // 跨档复制（WorkBuddy 国内版 ↔ 国际版）
    // ---------------------------------------------------------------------------

    /// 双档测试环境：共享工具存储根（关联表/锁/备份）+ 两个档位数据根（CN 与 AI）。
    struct CrossEnv {
        root: PathBuf,
        store_root: PathBuf,
        cn_data: PathBuf,
        ai_data: PathBuf,
    }

    impl CrossEnv {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "wb_switch_cross_test_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            let store_root = root.join("store");
            let cn_data = root.join("cn-data");
            let ai_data = root.join("ai-data");
            for data in [&cn_data, &ai_data] {
                std::fs::create_dir_all(data.join("projects").join("ws-a")).unwrap();
            }
            CrossEnv {
                root,
                store_root,
                cn_data,
                ai_data,
            }
        }

        fn paths(&self, variant: WbVariant) -> SessionPaths {
            SessionPaths {
                store_root: self.store_root.clone(),
                data_root: match variant {
                    WbVariant::Cn => self.cn_data.clone(),
                    WbVariant::Ai => self.ai_data.clone(),
                },
                // 跨档入口不读登录态（源 uid 由调用方显式传入）；门禁用它判断
                // 目标账号是否为该档当前登录账号。
                auth_file: self
                    .store_root
                    .join(format!("auth-{}.info", variant.as_str())),
                link_namespace: LinkNamespace::WorkBuddy,
            }
        }

        /// 写入指定档位的登录态（门禁读 `account.uid` 判定目标是否当前登录账号）。
        fn set_login(&self, variant: WbVariant, uid: &str) {
            let path = self.paths(variant).auth_file;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, json!({"account": {"uid": uid}}).to_string()).unwrap();
        }

        /// 建 sessions 表；`extra` 追加一列，用于模拟两档列集合差异。
        fn create_db(&self, variant: WbVariant, extra: &str) {
            let conn = Connection::open(self.paths(variant).workbuddy_db()).unwrap();
            conn.execute_batch(&format!(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    title TEXT,
                    custom_title TEXT,
                    cwd TEXT,
                    created_at INTEGER,
                    updated_at INTEGER,
                    deleted_at INTEGER,
                    is_playground INTEGER{extra}
                );"
            ))
            .unwrap();
        }

        fn create_edge_db(&self, variant: WbVariant) {
            let conn = Connection::open(self.paths(variant).edge_sync_db(variant)).unwrap();
            conn.execute_batch(
                "CREATE TABLE edge_sync_mapping (
                    session_id TEXT,
                    conversation_id TEXT,
                    msg_channel TEXT,
                    created_at INTEGER
                );",
            )
            .unwrap();
        }

        fn add_session(&self, variant: WbVariant, id: &str, uid: &str, title: &str) {
            let conn = Connection::open(self.paths(variant).workbuddy_db()).unwrap();
            conn.execute(
                "INSERT INTO sessions (id, user_id, title, custom_title, cwd, created_at, updated_at, deleted_at, is_playground)
                 VALUES (?1, ?2, ?3, NULL, '/ws/a', 1000, 2000, NULL, 0)",
                rusqlite::params![id, uid, title],
            )
            .unwrap();
        }

        fn add_body(&self, variant: WbVariant, cid: &str, text: &str) {
            std::fs::write(
                self.paths(variant)
                    .projects_dir()
                    .join("ws-a")
                    .join(format!("{cid}.jsonl")),
                text,
            )
            .unwrap();
        }

        fn body_path(&self, variant: WbVariant, cid: &str) -> PathBuf {
            self.paths(variant)
                .projects_dir()
                .join("ws-a")
                .join(format!("{cid}.jsonl"))
        }

        fn rows_for(&self, variant: WbVariant, uid: &str) -> Vec<String> {
            let conn = Connection::open(self.paths(variant).workbuddy_db()).unwrap();
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE user_id = ?1 AND deleted_at IS NULL")
                .unwrap();
            let mut rows: Vec<String> = stmt
                .query_map([uid], |row| row.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect();
            rows.sort();
            rows
        }

        fn mapping_rows(&self, variant: WbVariant) -> Vec<(String, String)> {
            let conn = Connection::open(self.paths(variant).edge_sync_db(variant)).unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, msg_channel FROM edge_sync_mapping ORDER BY session_id",
                )
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .flatten()
                .collect()
        }

        fn delete_session(&self, variant: WbVariant, id: &str) {
            let conn = Connection::open(self.paths(variant).workbuddy_db()).unwrap();
            conn.execute("DELETE FROM sessions WHERE id = ?1", [id])
                .unwrap();
        }
    }

    impl Drop for CrossEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn cross_side<'a>(paths: &'a SessionPaths, variant: WbVariant, uid: &str) -> CopySide<'a> {
        CopySide {
            paths,
            variant,
            uid: uid.to_string(),
            account_id: Some(format!("acc-{uid}")),
        }
    }

    /// 可注入双档临时目录的复制调用（解析器与两条 side 一致，不触碰真实路径）。
    fn cross_copy(
        env: &CrossEnv,
        source_variant: WbVariant,
        source_uid: &str,
        target_variant: WbVariant,
        target_uid: &str,
        ids: &[&str],
    ) -> Value {
        let source_paths = env.paths(source_variant);
        let target_paths = env.paths(target_variant);
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        copy_sessions_cross_at(
            cross_side(&source_paths, source_variant, source_uid),
            cross_side(&target_paths, target_variant, target_uid),
            &ids,
            |_| false,
            |variant| env.paths(variant),
        )
        .unwrap()
    }

    /// 与 `cross_copy` 相同，但注入「App 是否运行」探针（门禁测试用）。
    fn cross_copy_with_probe(
        env: &CrossEnv,
        source_variant: WbVariant,
        source_uid: &str,
        target_variant: WbVariant,
        target_uid: &str,
        ids: &[&str],
        is_app_running: impl Fn(WbVariant) -> bool,
    ) -> Result<Value, String> {
        let source_paths = env.paths(source_variant);
        let target_paths = env.paths(target_variant);
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        copy_sessions_cross_at(
            cross_side(&source_paths, source_variant, source_uid),
            cross_side(&target_paths, target_variant, target_uid),
            &ids,
            is_app_running,
            |variant| env.paths(variant),
        )
    }

    /// 跨档门禁只拦「目标账号 = 目标档当前登录账号」：App 运行中
    /// 目标≠当前登录 → 放行；目标=当前登录 → 拒绝；读不到登录态 → 保守拒绝。
    #[test]
    fn cross_copy_gate_only_blocks_current_login_target() {
        let env = CrossEnv::new("cross-gate");
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        let run = |uid: &str| {
            cross_copy_with_probe(
                &env,
                WbVariant::Cn,
                "uid-a",
                WbVariant::Ai,
                uid,
                &["sess-1"],
                |_| true,
            )
        };

        // 目标档登录的是别人（uid-x）：运行中放行，副本落库。
        env.set_login(WbVariant::Ai, "uid-x");
        let report = run("uid-b").expect("目标≠当前登录：运行中必须放行");
        let new_id = report["copied"][0]["newId"].as_str().unwrap().to_string();
        assert_eq!(env.rows_for(WbVariant::Ai, "uid-b"), vec![new_id]);

        // 目标账号正是目标档当前登录账号：运行中拒绝，且不再写入。
        env.set_login(WbVariant::Ai, "uid-b");
        let err = run("uid-b").unwrap_err();
        assert_eq!(err, SESSION_COPY_APP_RUNNING);
        assert_eq!(
            env.rows_for(WbVariant::Ai, "uid-b").len(),
            1,
            "未新增副本行"
        );

        // 读不到登录态（认证文件缺失）：保守拒绝。
        std::fs::remove_file(env.paths(WbVariant::Ai).auth_file).unwrap();
        let err = run("uid-b").unwrap_err();
        assert_eq!(err, SESSION_COPY_APP_RUNNING);

        // App 未运行：任何目标都放行（登录态缺失也不再拦截）；此时源会话已有
        // 有效副本，返回 alreadyLinked 而非再次复制。
        let report = cross_copy_with_probe(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
            |_| false,
        )
        .unwrap();
        assert!(
            !report["alreadyLinked"].as_array().unwrap().is_empty(),
            "{report}"
        );
        assert!(report.get("errors").is_none(), "{report}");
    }

    /// 基础跨档复制（CN → AI）：正文按源工作区同名目录落到目标档，行写目标档、
    /// 映射行交给客户端，关联组两个成员各记自己的档位。
    #[test]
    fn cross_copy_lands_body_row_and_member_variants() {
        let env = CrossEnv::new("cn-to-ai");
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        let report = cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
        );
        assert_eq!(report["sourceVariant"], "cn");
        assert_eq!(report["targetVariant"], "ai");
        assert_eq!(report["sourceUid"], "uid-a");
        assert_eq!(report["targetUid"], "uid-b");
        let new_id = report["copied"][0]["newId"].as_str().unwrap().to_string();
        assert_ne!(new_id, "sess-1");

        // 正文：目标档同名工作区目录，id 全量替换；源档不新增文件。
        let target_body = env.body_path(WbVariant::Ai, &new_id);
        assert!(target_body.is_file());
        let text = std::fs::read_to_string(&target_body).unwrap();
        assert!(text.contains(&new_id));
        assert!(!text.contains("sess-1"));
        assert!(!env.body_path(WbVariant::Cn, &new_id).exists());

        assert_eq!(env.rows_for(WbVariant::Ai, "uid-b"), vec![new_id.clone()]);
        assert_eq!(
            env.rows_for(WbVariant::Cn, "uid-a"),
            vec!["sess-1".to_string()]
        );

        // 不预写映射行：云端登记交接给客户端，两档映射表都不应出现工具写入。
        assert!(env.mapping_rows(WbVariant::Ai).is_empty());
        assert!(env.mapping_rows(WbVariant::Cn).is_empty());

        // 关联组：跨档组，两成员各记自己的档位。
        let store = match session_link::load_store(&env.paths(WbVariant::Ai)) {
            StoreState::Ready(store) => store,
            other => panic!("同步记录应为 Ready，实际 {other:?}"),
        };
        assert_eq!(store.groups.len(), 1);
        let group = &store.groups[0];
        let source_member = session_link::find_member(group, "uid-a", "sess-1").unwrap();
        assert_eq!(source_member.variant, Some(WbVariant::Cn));
        let target_member = session_link::find_member(group, "uid-b", &new_id).unwrap();
        assert_eq!(target_member.variant, Some(WbVariant::Ai));
    }

    /// 目标档列集合与源档不同（双向缺列）时按目标列取交集：不写 NULL、不报错。
    #[test]
    fn cross_copy_uses_target_columns_intersection() {
        let env = CrossEnv::new("columns");
        // 源多一列 transport；目标多一列 unread。
        env.create_db(WbVariant::Cn, ", transport TEXT");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, ", unread INTEGER");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        let report = cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
        );
        let new_id = report["copied"][0]["newId"].as_str().unwrap().to_string();

        let conn = Connection::open(env.paths(WbVariant::Ai).workbuddy_db()).unwrap();
        let (title, cwd, unread): (String, String, Option<i64>) = conn
            .query_row(
                "SELECT title, cwd, unread FROM sessions WHERE id = ?1",
                [&new_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "标题一");
        assert_eq!(cwd, "/ws/a");
        // 目标独有列由库默认值补齐（无默认值时 NULL），不因源列结构报错。
        assert_eq!(unread, None);
    }

    /// 反向（AI → CN）同路径可用：正文/行/映射落 CN，成员档位正确。
    #[test]
    fn cross_copy_ai_to_cn_uses_same_kernel() {
        let env = CrossEnv::new("ai-to-cn");
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.add_session(WbVariant::Ai, "sess-ai", "uid-ai", "AI 标题");
        env.add_body(WbVariant::Ai, "sess-ai", &body_text("sess-ai"));

        let report = cross_copy(
            &env,
            WbVariant::Ai,
            "uid-ai",
            WbVariant::Cn,
            "uid-cn",
            &["sess-ai"],
        );
        assert_eq!(report["sourceVariant"], "ai");
        assert_eq!(report["targetVariant"], "cn");
        let new_id = report["copied"][0]["newId"].as_str().unwrap().to_string();
        assert!(env.body_path(WbVariant::Cn, &new_id).is_file());
        assert_eq!(env.rows_for(WbVariant::Cn, "uid-cn"), vec![new_id.clone()]);
        assert!(
            env.mapping_rows(WbVariant::Cn).is_empty(),
            "复制不得预写映射行"
        );

        let store = match session_link::load_store(&env.paths(WbVariant::Cn)) {
            StoreState::Ready(store) => store,
            other => panic!("同步记录应为 Ready，实际 {other:?}"),
        };
        let group = &store.groups[0];
        assert_eq!(
            session_link::find_member(group, "uid-ai", "sess-ai")
                .unwrap()
                .variant,
            Some(WbVariant::Ai)
        );
        assert_eq!(
            session_link::find_member(group, "uid-cn", &new_id)
                .unwrap()
                .variant,
            Some(WbVariant::Cn)
        );
    }

    /// 同一跨档请求重复执行：第二次复用已有有效副本（alreadyLinked），不写第二个副本。
    #[test]
    fn cross_copy_reuses_existing_target_link() {
        let env = CrossEnv::new("already");
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        let first = cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
        );
        let new_id = first["copied"][0]["newId"].as_str().unwrap().to_string();
        let second = cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
        );
        assert!(second["copied"].as_array().unwrap().is_empty(), "{second}");
        assert_eq!(
            second["alreadyLinked"][0]["sessionId"],
            new_id.as_str(),
            "{second}"
        );
        assert_eq!(env.rows_for(WbVariant::Ai, "uid-b"), vec![new_id]);
    }

    /// 跨档操作中断后恢复：源正文与源行从源档（sourceVariant）读取，产物落目标档。
    #[test]
    fn cross_recovery_reads_source_from_operation_variant() {
        let env = CrossEnv::new("recovery");
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        let ai_paths = env.paths(WbVariant::Ai);
        // 与生产一致：操作产生于一次复制，关联存储主文件必然已存在（先建空表）。
        session_link::with_link_store_write(&ai_paths, |_| Ok(())).unwrap();
        // 手工留下一条 Prepared 阶段的跨档操作（CN → AI），模拟正文写入前中断。
        session_link::save_operation(
            &ai_paths,
            &Operation {
                version: crate::modules::session_link::OPERATION_VERSION,
                operation_id: "op-cross".to_string(),
                kind: "copy".to_string(),
                variant: WbVariant::Ai,
                source_variant: Some(WbVariant::Cn),
                group_id: "g-cross".to_string(),
                source: OperationMember {
                    account_id: None,
                    uid: "uid-a".to_string(),
                    session_id: "sess-1".to_string(),
                },
                target: OperationMember {
                    account_id: None,
                    uid: "uid-b".to_string(),
                    session_id: "new-cross".to_string(),
                },
                expected_content_digest: "将在恢复时按源内容刷新".to_string(),
                expected_record_count: 0,
                phase: crate::modules::session_link::OpPhase::Prepared,
                backup: None,
                lifecycle_version: None,
                cleanup_state: None,
                last_error: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();

        let report =
            recover_pending_session_operations_at_with(&ai_paths, WbVariant::Ai, |variant| {
                env.paths(variant)
            });
        assert!(report.needs_recovery.is_empty(), "{report:?}");
        assert_eq!(report.recovered, vec!["op-cross".to_string()]);

        // 产物全部落到 AI（目标档）。
        assert!(env.body_path(WbVariant::Ai, "new-cross").is_file());
        assert_eq!(
            env.rows_for(WbVariant::Ai, "uid-b"),
            vec!["new-cross".to_string()]
        );
        assert!(
            env.mapping_rows(WbVariant::Ai).is_empty(),
            "恢复不得补写映射行"
        );
        let store = match session_link::load_store(&ai_paths) {
            StoreState::Ready(store) => store,
            other => panic!("同步记录应为 Ready，实际 {other:?}"),
        };
        let group = store
            .groups
            .iter()
            .find(|group| group.id == "g-cross")
            .expect("恢复后应提交关联组");
        assert_eq!(
            session_link::find_member(group, "uid-a", "sess-1")
                .unwrap()
                .variant,
            Some(WbVariant::Cn)
        );
    }

    /// 跨档预览：复制建组后判 identical；源追加 → fastForward；两边分别追加 → diverge；
    /// 源行删除 → unknown。内容与标题按成员档位读取（源读 CN、目标读 AI）。
    #[test]
    fn cross_preview_verdicts_follow_member_content() {
        let env = CrossEnv::new("preview");
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        let source_paths = env.paths(WbVariant::Cn);
        let target_paths = env.paths(WbVariant::Ai);
        let source_acc = json!({"id": "acc-uid-a", "uid": "uid-a", "variant": "cn"});
        let target_acc = json!({"id": "acc-uid-b", "uid": "uid-b", "variant": "ai"});
        let preview = || {
            session_links_preview_cross_at(
                &source_paths,
                WbVariant::Cn,
                &source_acc,
                &target_paths,
                WbVariant::Ai,
                &target_acc,
                |variant| env.paths(variant),
            )
            .unwrap()
        };

        // 复制建组：CN → AI（正文落在目标档同名工作区目录）。
        let report = cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
        );
        let new_id = report["copied"][0]["newId"].as_str().unwrap().to_string();

        // 复制完成即一致；报告带两侧档位，标题取自源档。
        let first = preview();
        assert_eq!(first["sourceVariant"], "cn");
        assert_eq!(first["targetVariant"], "ai");
        assert_eq!(first["storeStatus"], "ready");
        assert_eq!(first["groups"][0]["verdict"], "identical");
        assert_eq!(first["groups"][0]["title"], "标题一");
        assert_eq!(first["groups"][0]["source"]["sessionId"], "sess-1");
        assert_eq!(first["groups"][0]["target"]["sessionId"], new_id.as_str());

        // 源追加 3 条 → fastForward（extraA=3），给出可执行的 previewToken。
        append_records(&env.body_path(WbVariant::Cn, "sess-1"), "sess-1", 2, 3);
        let second = preview();
        let item = &second["groups"][0];
        assert_eq!(item["verdict"], "fastForward");
        assert_eq!(item["extraA"], 3);
        assert_eq!(item["extraB"], 0);
        assert_eq!(item["defaultChecked"], true);
        assert_eq!(item["recordCount"]["source"], 5);
        assert_eq!(item["recordCount"]["target"], 2);
        assert!(item["previewToken"]
            .as_str()
            .is_some_and(|token| !token.is_empty()));

        // 目标追加自己独有的 2 条 → diverge（两边各有独有内容，只能显式覆盖）。
        append_records(&env.body_path(WbVariant::Ai, &new_id), &new_id, 100, 2);
        let third = preview();
        let item = &third["groups"][0];
        assert_eq!(item["verdict"], "diverge");
        assert_eq!(item["extraA"], 3);
        assert_eq!(item["extraB"], 2);
        assert_eq!(item["availableModes"][0], "overwrite");

        // 源行删除 → 成员失效，判定 unknown 且不可执行。
        env.delete_session(WbVariant::Cn, "sess-1");
        let fourth = preview();
        let item = &fourth["groups"][0];
        assert_eq!(item["verdict"], "unknown");
        assert_eq!(item["defaultChecked"], false);
        assert!(item["availableModes"].as_array().unwrap().is_empty());
        assert!(item.get("previewToken").is_none());
    }

    /// 跨档预览入口也覆盖同档组（两个国内版账号）：判定路径一致，报告不带档位字段。
    #[test]
    fn cross_preview_supports_same_variant_groups() {
        let env = CrossEnv::new("preview-same");
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));

        cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Cn,
            "uid-b",
            &["sess-1"],
        );

        let paths = env.paths(WbVariant::Cn);
        let source_acc = json!({"id": "acc-uid-a", "uid": "uid-a", "variant": "cn"});
        let target_acc = json!({"id": "acc-uid-b", "uid": "uid-b", "variant": "cn"});
        let report = session_links_preview_cross_at(
            &paths,
            WbVariant::Cn,
            &source_acc,
            &paths,
            WbVariant::Cn,
            &target_acc,
            |variant| env.paths(variant),
        )
        .unwrap();
        assert!(report.get("sourceVariant").is_none());
        assert_eq!(report["groups"][0]["verdict"], "identical");
    }

    /// 跨档同步的公共准备：CN 源 sess-1 → AI 目标复制建组，返回（环境、源/目标路径、newId）。
    fn cross_sync_env(name: &str) -> (CrossEnv, SessionPaths, SessionPaths, String) {
        let env = CrossEnv::new(name);
        env.create_db(WbVariant::Cn, "");
        env.create_edge_db(WbVariant::Cn);
        env.create_db(WbVariant::Ai, "");
        env.create_edge_db(WbVariant::Ai);
        env.add_session(WbVariant::Cn, "sess-1", "uid-a", "标题一");
        env.add_body(WbVariant::Cn, "sess-1", &body_text("sess-1"));
        let report = cross_copy(
            &env,
            WbVariant::Cn,
            "uid-a",
            WbVariant::Ai,
            "uid-b",
            &["sess-1"],
        );
        let new_id = report["copied"][0]["newId"].as_str().unwrap().to_string();
        let cn_paths = env.paths(WbVariant::Cn);
        let ai_paths = env.paths(WbVariant::Ai);
        (env, cn_paths, ai_paths, new_id)
    }

    /// 跨档同步（fastForward）：源追加后把新增同步到目标，判定回到 identical。
    #[test]
    fn cross_sync_fast_forward_catches_target_up() {
        let (env, cn_paths, ai_paths, new_id) = cross_sync_env("sync-ff");
        let source_acc = json!({"id": "acc-uid-a", "uid": "uid-a", "variant": "cn"});
        let target_acc = json!({"id": "acc-uid-b", "uid": "uid-b", "variant": "ai"});

        // 源产生 3 条新增 → 预览给 fastForward + 可执行凭据。
        append_records(&env.body_path(WbVariant::Cn, "sess-1"), "sess-1", 2, 3);
        let before = session_links_preview_cross_at(
            &cn_paths,
            WbVariant::Cn,
            &source_acc,
            &ai_paths,
            WbVariant::Ai,
            &target_acc,
            |variant| env.paths(variant),
        )
        .unwrap();
        let item = &before["groups"][0];
        assert_eq!(item["verdict"], "fastForward");
        let selection = SyncSelection {
            group_id: item["groupId"].as_str().unwrap().to_string(),
            preview_token: item["previewToken"].as_str().unwrap().to_string(),
            mode: SyncMode::FastForward,
        };

        let report = sync_sessions_cross_at(
            &cross_side(&cn_paths, WbVariant::Cn, "uid-a"),
            &cross_side(&ai_paths, WbVariant::Ai, "uid-b"),
            &[selection],
            |_| false,
            |variant| env.paths(variant),
        )
        .unwrap();
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["synced"][0]["status"], "synced");
        assert_eq!(report["synced"][0]["mode"], "fastForward");

        // 目标正文追平源（保留自己的 sessionId）；再判定为 identical。
        let source_text = std::fs::read_to_string(env.body_path(WbVariant::Cn, "sess-1")).unwrap();
        let target_text = std::fs::read_to_string(env.body_path(WbVariant::Ai, &new_id)).unwrap();
        assert_eq!(target_text, source_text.replace("sess-1", &new_id));

        let after = session_links_preview_cross_at(
            &cn_paths,
            WbVariant::Cn,
            &source_acc,
            &ai_paths,
            WbVariant::Ai,
            &target_acc,
            |variant| env.paths(variant),
        )
        .unwrap();
        assert_eq!(after["groups"][0]["verdict"], "identical");
    }

    /// 跨档同步（overwrite）：冲突经显式覆盖后目标 = 源（目标独有内容被替换）。
    #[test]
    fn cross_sync_overwrite_resolves_diverge() {
        let (env, cn_paths, ai_paths, new_id) = cross_sync_env("sync-overwrite");
        let source_acc = json!({"id": "acc-uid-a", "uid": "uid-a", "variant": "cn"});
        let target_acc = json!({"id": "acc-uid-b", "uid": "uid-b", "variant": "ai"});

        append_records(&env.body_path(WbVariant::Cn, "sess-1"), "sess-1", 2, 3);
        append_records(&env.body_path(WbVariant::Ai, &new_id), &new_id, 100, 2);

        let before = session_links_preview_cross_at(
            &cn_paths,
            WbVariant::Cn,
            &source_acc,
            &ai_paths,
            WbVariant::Ai,
            &target_acc,
            |variant| env.paths(variant),
        )
        .unwrap();
        let item = &before["groups"][0];
        assert_eq!(item["verdict"], "diverge");
        assert_eq!(item["availableModes"][0], "overwrite");
        let selection = SyncSelection {
            group_id: item["groupId"].as_str().unwrap().to_string(),
            preview_token: item["previewToken"].as_str().unwrap().to_string(),
            mode: SyncMode::Overwrite,
        };

        let report = sync_sessions_cross_at(
            &cross_side(&cn_paths, WbVariant::Cn, "uid-a"),
            &cross_side(&ai_paths, WbVariant::Ai, "uid-b"),
            &[selection],
            |_| false,
            |variant| env.paths(variant),
        )
        .unwrap();
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["synced"][0]["mode"], "overwrite");

        let source_text = std::fs::read_to_string(env.body_path(WbVariant::Cn, "sess-1")).unwrap();
        let target_text = std::fs::read_to_string(env.body_path(WbVariant::Ai, &new_id)).unwrap();
        assert_eq!(target_text, source_text.replace("sess-1", &new_id));
        assert!(
            !target_text.contains("\"index\":100"),
            "目标独有内容应被覆盖"
        );
    }

    /// 跨档同步：预览后源又变化 → 跳过（previewStale），目标零写入。
    #[test]
    fn cross_sync_skips_stale_preview() {
        let (env, cn_paths, ai_paths, new_id) = cross_sync_env("sync-stale");
        let source_acc = json!({"id": "acc-uid-a", "uid": "uid-a", "variant": "cn"});
        let target_acc = json!({"id": "acc-uid-b", "uid": "uid-b", "variant": "ai"});

        append_records(&env.body_path(WbVariant::Cn, "sess-1"), "sess-1", 2, 1);
        let before = session_links_preview_cross_at(
            &cn_paths,
            WbVariant::Cn,
            &source_acc,
            &ai_paths,
            WbVariant::Ai,
            &target_acc,
            |variant| env.paths(variant),
        )
        .unwrap();
        let item = &before["groups"][0];
        assert_eq!(item["verdict"], "fastForward");
        let selection = SyncSelection {
            group_id: item["groupId"].as_str().unwrap().to_string(),
            preview_token: item["previewToken"].as_str().unwrap().to_string(),
            mode: SyncMode::FastForward,
        };

        // 预览之后源再变化：凭据失效，执行必须跳过并保持目标不变。
        let target_before = std::fs::read_to_string(env.body_path(WbVariant::Ai, &new_id)).unwrap();
        append_records(&env.body_path(WbVariant::Cn, "sess-1"), "sess-1", 3, 1);
        let report = sync_sessions_cross_at(
            &cross_side(&cn_paths, WbVariant::Cn, "uid-a"),
            &cross_side(&ai_paths, WbVariant::Ai, "uid-b"),
            &[selection],
            |_| false,
            |variant| env.paths(variant),
        )
        .unwrap();
        assert!(report["synced"].as_array().unwrap().is_empty());
        assert_eq!(report["skipped"][0]["reasonCode"], "previewStale");
        let target_after = std::fs::read_to_string(env.body_path(WbVariant::Ai, &new_id)).unwrap();
        assert_eq!(target_after, target_before);
    }

    // ---------------------------------------------------------------------------
    // 同步预览与执行契约（S5）
    // ---------------------------------------------------------------------------

    /// 追加 `count` 条有序记录（模拟用户在来源账号继续对话）。
    fn append_records(path: &Path, cid: &str, from: usize, count: usize) {
        let mut text = std::fs::read_to_string(path).unwrap();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        for index in from..from + count {
            text.push_str(
                &json!({"type": "assistant", "sessionId": cid, "index": index}).to_string(),
            );
            text.push('\n');
        }
        std::fs::write(path, &text).unwrap();
    }

    fn preview(env: &Env, target_uid: &str) -> Value {
        session_links_preview_at(&env.paths(), WbVariant::Cn, &env.target(target_uid)).unwrap()
    }

    fn selection(group_id: &str, preview_token: &str, mode: SyncMode) -> SyncSelection {
        SyncSelection {
            group_id: group_id.to_string(),
            preview_token: preview_token.to_string(),
            mode,
        }
    }

    fn sync(env: &Env, target_uid: &str, selections: &[SyncSelection]) -> Value {
        sync_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target(target_uid),
            selections,
            |_| false,
        )
        .unwrap()
    }

    /// 组内所有配对基线的快照：同步不得推进任何关联版本（含第三方成员的配对）。
    fn pair_snapshot(env: &Env) -> Vec<String> {
        let mut items: Vec<String> = env
            .store()
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .pair_bases
                    .iter()
                    .map(|pair| format!("{}/{}", pair.member_ids.join("+"), pair.baseline_ref))
            })
            .collect();
        items.sort();
        items
    }

    /// 造一个可快进的场景：A→B 复制后来源追加 3 条记录，返回 (groupId, 预览凭据, 目标会话 id)。
    fn fast_forward_scene(env: &Env) -> (String, String, String) {
        let report = copy(env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path("sess-1"), "sess-1", 0, 3);

        let preview = preview(env, "uid-b");
        assert_eq!(preview["groups"].as_array().unwrap().len(), 1, "{preview}");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward", "{preview}");
        (
            group["groupId"].as_str().unwrap().to_string(),
            group["previewToken"].as_str().unwrap().to_string(),
            target_id,
        )
    }

    /// 预览报告 fastForward 与默认勾选；预览本身是只读的（不改正文与关联版本）。
    #[test]
    fn preview_reports_fast_forward_and_stays_read_only() {
        let env = ready_env("sync-preview-ff");
        let (_, _, target_id) = fast_forward_scene(&env);
        let target_body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        let revision_before = env.store().revision;
        let baselines_before = env.baseline_files();

        let preview = preview(&env, "uid-b");
        assert_eq!(preview["supported"], true);
        assert_eq!(preview["storeStatus"], "ready");
        assert_eq!(preview["sourceUid"], "uid-a");
        assert_eq!(preview["targetUid"], "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["title"], "标题一");
        assert_eq!(group["cwd"], "/ws/a");
        assert_eq!(group["verdict"], "fastForward");
        assert_eq!(group["defaultChecked"], true);
        assert_eq!(group["extraA"], 3);
        assert_eq!(group["extraB"], 0);
        assert_eq!(group["common"], 2);
        assert_eq!(group["availableModes"], json!(["fastForward"]));
        assert_eq!(group["recordCount"]["source"], 5);
        assert_eq!(group["recordCount"]["target"], 2);
        assert_eq!(group["recordCount"]["baseline"], 2);
        assert_eq!(group["source"]["uid"], "uid-a");
        assert_eq!(group["target"]["sessionId"], target_id);
        assert_eq!(group["target"]["state"], "active");
        assert!(group["previewToken"].as_str().is_some());
        assert!(group["reason"].as_str().unwrap().contains("可以直接同步"));
        // 预览是只读的：目标正文与关联版本都不变。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
        );
        assert_eq!(env.store().revision, revision_before);
        assert_eq!(env.baseline_files(), baselines_before);
    }

    /// 仅目标变化 → ahead：普通同步不可勾选，但为显式整组统一保留校验凭据。
    #[test]
    fn preview_reports_ahead_when_only_target_changed() {
        let env = ready_env("sync-preview-ahead");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path(&target_id), &target_id, 0, 5);
        let target_body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        let revision_before = env.store().revision;

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "ahead");
        assert_eq!(group["defaultChecked"], false);
        assert_eq!(group["extraA"], 0);
        assert_eq!(group["extraB"], 5);
        assert_eq!(group["availableModes"], json!([]));
        assert!(
            group["previewToken"].as_str().is_some(),
            "显式整组统一需要绑定当前内容"
        );
        assert!(group["reason"]
            .as_str()
            .unwrap()
            .contains("只有目标账号新增"));

        // 预览不写目标：正文与关联版本都不变。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
        );
        assert_eq!(env.store().revision, revision_before);
    }

    #[test]
    fn explicit_group_unify_can_replace_ahead_but_ordinary_overwrite_cannot() {
        let env = ready_env("sync-unify-ahead");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path(&target_id), &target_id, 0, 5);
        let group = preview(&env, "uid-b")["groups"][0].clone();
        assert_eq!(group["verdict"], "ahead");
        assert_eq!(group["availableModes"], json!([]));
        let group_id = group["groupId"].as_str().unwrap();
        let token = group["previewToken"].as_str().unwrap();

        let rejected = sync(
            &env,
            "uid-b",
            &[selection(group_id, token, SyncMode::Overwrite)],
        );
        assert!(rejected["synced"].as_array().unwrap().is_empty());
        assert_eq!(rejected["errors"].as_array().unwrap().len(), 1);

        let applied = sync(
            &env,
            "uid-b",
            &[selection(group_id, token, SyncMode::UnifyOverwrite)],
        );
        assert_eq!(applied["synced"].as_array().unwrap().len(), 1, "{applied}");
        let source_text = std::fs::read_to_string(env.body_path("sess-1")).unwrap();
        let target_text = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        assert_eq!(
            session_link::normalize_jsonl(&source_text, "sess-1").unwrap(),
            session_link::normalize_jsonl(&target_text, &target_id).unwrap(),
        );
    }

    /// 一方有效成员缺失（失效/被替换）→ 不提供写入动作，记录数按契约给 0 而不是 null。
    #[test]
    fn preview_reports_invalid_member_as_unknown_without_null_record_counts() {
        let env = ready_env("sync-preview-invalid-member");
        let (group_id, _token, target_id) = fast_forward_scene(&env);
        session_link::with_link_store_write(&env.paths(), |store| {
            let group = store
                .groups
                .iter_mut()
                .find(|group| group.id == group_id)
                .expect("组必须存在");
            let member_id = session_link::active_member_for(group, "uid-b")
                .expect("目标成员原本有效")
                .member_id
                .clone();
            assert!(session_link::set_member_state(
                group,
                &member_id,
                MemberState::Stale
            ));
            Ok(())
        })
        .unwrap();

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "unknown", "{preview}");
        assert_eq!(group["defaultChecked"], false);
        assert_eq!(group["availableModes"], json!([]));
        assert!(group.get("previewToken").is_none(), "不可执行的组不发凭据");
        assert!(
            group["reason"]
                .as_str()
                .unwrap()
                .contains("对应的会话已失效"),
            "{preview}"
        );
        // 契约：不可验证时 source/target 为 0、baseline 为 null（前端类型据此声明）。
        assert_eq!(
            group["recordCount"],
            json!({"source": 0, "target": 0, "baseline": null}),
            "{preview}"
        );
        // 两侧成员状态仍要展示（用户据此判断是哪一侧失效）。
        assert_eq!(group["target"]["sessionId"], target_id);
        assert_eq!(group["target"]["state"], "stale");
        assert_eq!(group["source"]["state"], "active");
    }

    /// 预览后来源追加 → 执行时跳过该组（previewStale），不继承用户旧选择。
    #[test]
    fn sync_skips_when_source_changed_after_preview() {
        let env = ready_env("sync-stale-source");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let target_body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();

        append_records(&env.body_path("sess-1"), "sess-1", 3, 1);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty());
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        let skipped = &report["skipped"][0];
        assert_eq!(skipped["reasonCode"], REASON_PREVIEW_STALE);
        assert!(
            skipped["message"]
                .as_str()
                .unwrap()
                .contains("来源账号的内容已变化"),
            "{skipped}"
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
        );
    }

    /// 预览后目标被改动 → 执行时跳过（显式覆盖也不能绕过版本校验）。
    #[test]
    fn sync_skips_when_target_changed_after_preview_even_with_overwrite() {
        let env = ready_env("sync-stale-target");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        append_records(&env.body_path(&target_id), &target_id, 0, 1);
        let target_body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();

        for mode in [SyncMode::FastForward, SyncMode::Overwrite] {
            let report = sync(&env, "uid-b", &[selection(&group_id, &token, mode)]);
            assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
            let skipped = &report["skipped"][0];
            assert_eq!(skipped["reasonCode"], REASON_PREVIEW_STALE, "{report}");
            assert!(
                skipped["message"]
                    .as_str()
                    .unwrap()
                    .contains("目标账号的内容已变化"),
                "{skipped}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
        );
    }

    /// 预览后换了账号 → 执行时跳过（身份由后端从登录态读取，不沿用令牌里的身份）。
    #[test]
    fn sync_skips_when_account_changed_after_preview() {
        let env = ready_env("sync-stale-account");
        let (group_id, token, _) = fast_forward_scene(&env);
        let revision_before = env.store().revision;

        env.set_login("uid-c");
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        let skipped = &report["skipped"][0];
        assert_eq!(skipped["reasonCode"], REASON_PREVIEW_STALE);
        assert!(
            skipped["message"].as_str().unwrap().contains("账号已变化"),
            "{skipped}"
        );
        assert_eq!(env.store().revision, revision_before);
    }

    /// 预览后关联组或基线变化 → 执行时跳过。
    #[test]
    fn sync_skips_when_group_or_baseline_changed_after_preview() {
        // 组变化：同组新增成员（例如并发复制把第三方账号加进来）。
        let env = ready_env("sync-stale-group");
        let (group_id, token, _) = fast_forward_scene(&env);
        let paths = env.paths();
        session_link::with_link_store_write(&paths, |store| {
            let group = store
                .groups
                .iter_mut()
                .find(|group| group.id == group_id)
                .expect("组必须存在");
            session_link::add_active_member(
                group,
                LinkMember {
                    member_id: "m-uid-c".to_string(),
                    account_id: None,
                    uid: "uid-c".to_string(),
                    session_id: "sess-c".to_string(),
                    variant: None,
                    state: MemberState::Active,
                    linked_at: 1,
                    last_synced_at: None,
                },
            );
            Ok(())
        })
        .unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        let skipped = &report["skipped"][0];
        assert_eq!(skipped["reasonCode"], REASON_PREVIEW_STALE, "{report}");
        assert!(
            skipped["message"]
                .as_str()
                .unwrap()
                .contains("会话的关联关系或同步记录已变化"),
            "{skipped}"
        );

        // 基线变化：同一个基线引用被改写成另一份内容。
        let env = ready_env("sync-stale-baseline");
        let (group_id, token, _) = fast_forward_scene(&env);
        let baseline_ref = env.store().groups[0].pair_bases[0].baseline_ref.clone();
        let drifted = session_link::BaselineRecord {
            version: session_link::BASELINE_VERSION,
            baseline_ref: baseline_ref.clone(),
            normalization_version: session_link::NORMALIZATION_VERSION,
            created_at: 1,
            record_count: 1,
            total_digest: session_link::total_digest_of(&["00".to_string()]),
            line_digests: vec!["00".to_string()],
        };
        std::fs::write(
            env.paths
                .baselines_dir()
                .join(format!("{baseline_ref}.json")),
            serde_json::to_string(&drifted).unwrap(),
        )
        .unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        let skipped = &report["skipped"][0];
        assert_eq!(skipped["reasonCode"], REASON_PREVIEW_STALE, "{report}");
        assert!(
            skipped["message"]
                .as_str()
                .unwrap()
                .contains("上次同步的内容已变化"),
            "{skipped}"
        );
    }

    /// 伪造 token / 张冠李戴的组 → 拒绝，不静默执行。
    #[test]
    fn sync_rejects_forged_or_mismatched_preview_token() {
        let env = ready_env("sync-forged");
        let (group_id, token, target_id) = fast_forward_scene(&env);

        for forged in [
            "11111111-2222-3333-4444-555555555555",
            "../../../../etc/passwd",
            "sess-1.json",
        ] {
            let report = sync(
                &env,
                "uid-b",
                &[selection(&group_id, forged, SyncMode::FastForward)],
            );
            assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
            let errors = report["errors"].as_array().unwrap();
            assert_eq!(errors.len(), 1, "{report}");
            assert!(
                errors[0]["error"]
                    .as_str()
                    .unwrap()
                    .contains("检查结果不存在"),
                "{report}"
            );
        }

        // 真实凭据 + 别的组 id：张冠李戴同样拒绝。
        let report = sync(
            &env,
            "uid-b",
            &[selection("g-other", &token, SyncMode::FastForward)],
        );
        assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
        assert!(
            report["errors"][0]["error"]
                .as_str()
                .unwrap()
                .contains("不匹配"),
            "{report}"
        );

        // 未知模式在解析阶段就拒绝。
        assert!(parse_sync_selections(Some(
            &json!([{"groupId": group_id, "previewToken": token, "mode": "force"}])
        ))
        .unwrap_err()
        .contains("未知的同步模式"));

        // 全程没有任何写入。
        assert_eq!(env.body_files().len(), 2);
        assert_eq!(
            session_row_owner(&env.paths(), &target_id).as_deref(),
            Some("uid-b")
        );
    }

    /// 判定为 unknown 时强制覆盖 → 拒绝（不得绕过版本校验）。
    #[test]
    fn sync_rejects_forced_overwrite_when_verdict_is_unknown() {
        let env = ready_env("sync-unknown-overwrite");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        // 目标加入独有内容：双方互不为前缀，删掉基线后无法用内容关系判定 → unknown。
        append_records(&env.body_path(&target_id), &target_id, 100, 2);
        let target_body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        let revision_before = env.store().revision;

        // 基线文件被删除：共同基线不可验证 → 重新判定必然 unknown。
        let store = env.store();
        let group = store
            .groups
            .iter()
            .find(|group| group.id == group_id)
            .unwrap();
        let baseline_ref = group.pair_bases[0].baseline_ref.clone();
        std::fs::remove_file(
            env.paths
                .baselines_dir()
                .join(format!("{baseline_ref}.json")),
        )
        .unwrap();

        let paths = env.paths();
        let source_member = session_link::active_member_for(group, "uid-a").unwrap();
        let target_member = session_link::active_member_for(group, "uid-b").unwrap();
        let source_content = member_content_state(&paths, &source_member.session_id);
        let target_content = member_content_state(&paths, &target_member.session_id);
        let baseline = session_link::load_pair_baseline(
            &paths,
            group,
            &source_member.member_id,
            &target_member.member_id,
        );
        assert_eq!(
            session_link::decide_sync(&source_content, &target_content, &baseline).verdict,
            SyncVerdict::Unknown
        );
        // 前端只能回传凭据 id；这里直接构造一份「unknown + 空可选模式」的服务端凭据，
        // 模拟强行以覆盖模式执行。
        let forged = session_link::save_preview_token(
            &paths,
            live_preview_binding(
                group,
                source_member,
                target_member,
                &source_content,
                &target_content,
                &baseline,
                SyncVerdict::Unknown,
                &PreviewArchiveState::default(),
            ),
        )
        .unwrap();

        for mode in [SyncMode::Overwrite, SyncMode::FastForward] {
            let report = sync(&env, "uid-b", &[selection(&group_id, &forged, mode)]);
            assert!(
                report["skipped"].as_array().unwrap().is_empty(),
                "unknown 不得进入校验通过：{report}"
            );
            let errors = report["errors"].as_array().unwrap();
            assert_eq!(errors.len(), 1, "{report}");
            assert!(
                errors[0]["error"].as_str().unwrap().contains("不允许以"),
                "{report}"
            );
        }

        // 以原凭据 + 覆盖模式 → 版本校验先拦下（判定已变），同样不写。
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::Overwrite)],
        );
        assert_eq!(report["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);

        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
        );
        assert_eq!(env.store().revision, revision_before);
    }

    /// 无配对基线时，目标内容被来源完整包含 → 预览照常发放可勾选凭据，执行成功。
    ///
    /// 覆盖轮换链的最后一跳（C 切回 A）：隔跳配对没有基线记录，但目标内容是来源内容的
    /// 严格有序前缀，追加同步零覆盖，因此无需基线佐证（对齐 git fast-forward）。
    #[test]
    fn sync_fast_forward_without_baseline_when_target_is_ordered_prefix() {
        let env = ready_env("sync-ff-no-baseline");
        let (group_id, _, target_id) = fast_forward_scene(&env);
        let target_body_before = body_bytes(&env, &target_id);

        // 删除基线文件：等价于「隔跳配对从未登记过共同基线」的不可验证状态。
        let group = group_snapshot(&env, &group_id);
        let baseline_ref = group.pair_bases[0].baseline_ref.clone();
        std::fs::remove_file(
            env.paths
                .baselines_dir()
                .join(format!("{baseline_ref}.json")),
        )
        .unwrap();

        // 预览：没有可验证基线也照常判快进，并发放可勾选凭据。
        let preview = preview(&env, "uid-b");
        let item = &preview["groups"][0];
        assert_eq!(item["verdict"], "fastForward", "{preview}");
        assert_eq!(item["defaultChecked"], true, "{preview}");
        assert_eq!(item["availableModes"], json!(["fastForward"]), "{preview}");
        assert!(item["recordCount"]["baseline"].is_null(), "{preview}");
        assert!(
            item["reason"].as_str().unwrap().contains("新增 3 条"),
            "{preview}"
        );
        let token = item["previewToken"]
            .as_str()
            .expect("可勾选项必须发放凭据")
            .to_string();

        // 执行：无基线不阻塞写入，目标收敛到来源内容，并补建配对基线。
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_ne!(body_bytes(&env, &target_id), target_body_before);
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id)
        );
        let group = group_snapshot(&env, &group_id);
        let baseline = session_link::load_pair_baseline(
            &env.paths(),
            &group,
            &member_id_of(&group, "uid-a"),
            &member_id_of(&group, "uid-b"),
        );
        assert!(baseline.ready().is_some(), "执行后必须补建可验证的配对基线");
    }

    /// 第三方账号：只处理 A 与 B 共同参与的组，同步只推进 A/B 的基线与正文。
    #[test]
    fn sync_never_touches_third_account_member_or_baseline() {
        let env = ready_env("sync-third-account");
        let a_to_b = copy(&env, "uid-b", &["sess-1"]);
        let b_id = env.first_copy_id(&a_to_b);
        let a_to_c = copy(&env, "uid-c", &["sess-1"]);
        let c_id = env.first_copy_id(&a_to_c);
        append_records(&env.body_path("sess-1"), "sess-1", 0, 2);
        append_records(&env.body_path(&c_id), &c_id, 0, 7);

        let pairs_before = pair_snapshot(&env);
        let baselines_before = env.baseline_files();
        let c_body_before = std::fs::read_to_string(env.body_path(&c_id)).unwrap();
        let b_body_before = std::fs::read_to_string(env.body_path(&b_id)).unwrap();

        // 只列出 A 与 B 共同参与的组；C 的改动不影响 A/B 的判定。
        let preview = preview(&env, "uid-b");
        let groups = preview["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{preview}");
        assert_eq!(groups[0]["verdict"], "fastForward", "{preview}");
        assert_eq!(groups[0]["extraB"], 0);
        let group_id = groups[0]["groupId"].as_str().unwrap().to_string();
        let token = groups[0]["previewToken"].as_str().unwrap().to_string();

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert!(report.get("needsRecovery").is_none(), "{report}");

        // 只有 A/B 的配对基线被推进（旧引用消失、新引用出现），A/C 与 B/C 原样保留。
        let store = env.store();
        let group = store
            .groups
            .iter()
            .find(|group| group.id == group_id)
            .unwrap();
        let pairs_after = pair_snapshot(&env);
        let removed: Vec<&String> = pairs_before
            .iter()
            .filter(|entry| !pairs_after.contains(entry))
            .collect();
        let added: Vec<&String> = pairs_after
            .iter()
            .filter(|entry| !pairs_before.contains(entry))
            .collect();
        assert_eq!(removed.len(), 1, "只有 A/B 的基线被改写：{pairs_after:?}");
        let (a_member, b_member) = (member_id_of(group, "uid-a"), member_id_of(group, "uid-b"));
        assert!(
            removed[0].starts_with(&format!("{a_member}+{b_member}"))
                || removed[0].starts_with(&format!("{b_member}+{a_member}")),
            "被改写的必须是 A/B 成员对：{}",
            removed[0]
        );
        assert_eq!(added.len(), 1);
        assert_eq!(env.baseline_files(), baselines_before + 1, "只新增一份基线");

        // C 的正文、基线与同步时间都不动；来源账号也不被标成已同步。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&c_id)).unwrap(),
            c_body_before,
            "C 的内容不得被同步改写"
        );
        assert_ne!(
            std::fs::read_to_string(env.body_path(&b_id)).unwrap(),
            b_body_before,
            "B 的内容应被同步替换"
        );
        let group = env
            .store()
            .groups
            .into_iter()
            .find(|group| group.id == group_id)
            .unwrap();
        assert!(group
            .members
            .iter()
            .find(|member| member.uid == "uid-c")
            .unwrap()
            .last_synced_at
            .is_none());
        assert!(session_link::active_member_for(&group, "uid-a")
            .unwrap()
            .last_synced_at
            .is_none());
        assert!(session_link::active_member_for(&group, "uid-b")
            .unwrap()
            .last_synced_at
            .is_some());
    }

    fn member_id_of(group: &LinkGroup, uid: &str) -> String {
        group
            .members
            .iter()
            .find(|member| member.uid == uid)
            .unwrap()
            .member_id
            .clone()
    }

    /// 关联存储不可用/缺失 → 不把校验当成通过。
    #[test]
    fn sync_reports_store_problems_instead_of_passing_checks() {
        // 损坏：保留现场，全部选择项报错并要求恢复。
        let env = ready_env("sync-store-broken");
        let (group_id, token, _) = fast_forward_scene(&env);
        std::fs::write(env.paths.session_links_file(), "not-json").unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
        assert!(
            report["errors"][0]["error"]
                .as_str()
                .unwrap()
                .contains("同步记录"),
            "{report}"
        );
        assert_eq!(report["needsRecovery"], true, "关系表损坏必须要求恢复");

        // 主文件与基线都被清掉（首次使用）：凭据无从校验 → 跳过。
        let env = ready_env("sync-store-missing");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        std::fs::remove_file(env.paths.session_links_file()).unwrap();
        std::fs::remove_dir_all(env.paths.baselines_dir()).unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(env.body_files().len(), 2);
        assert!(env.body_path(&target_id).exists());
    }

    /// 预览的能力与状态上报：档位不支持、关联存储未初始化、入参错误。
    #[test]
    fn preview_reports_supported_and_store_status() {
        let env = Env::new("sync-preview-status");
        env.set_login("uid-a");

        // 关联存储还没建立 → missing，不是错误。
        let report = preview(&env, "uid-b");
        assert_eq!(report["supported"], true);
        assert_eq!(report["storeStatus"], "missing");
        assert_eq!(report["groups"].as_array().unwrap().len(), 0);

        // 国际版数据根不同构 → 不支持，前端据此隐藏同步区块。
        let ai_paths = SessionPaths {
            store_root: env.root.join("store-ai"),
            data_root: env.root.join("data-ai"),
            auth_file: env.paths.auth_file.clone(),
            link_namespace: LinkNamespace::WorkBuddy,
        };
        std::fs::create_dir_all(&ai_paths.data_root).unwrap();
        let report = session_links_preview_at(
            &ai_paths,
            WbVariant::Ai,
            &json!({"id": "ai-1", "uid": "uid-b", "variant": "ai"}),
        )
        .unwrap();
        assert_eq!(report["supported"], false);
        assert_eq!(report["storeStatus"], "unsupported");
        assert_eq!(report["groups"].as_array().unwrap().len(), 0);

        // 入参错误：缺 uid、同账号。
        assert!(
            session_links_preview_at(&env.paths(), WbVariant::Cn, &json!({"uid": " "}))
                .unwrap_err()
                .contains("缺少 uid")
        );
        assert!(
            session_links_preview_at(&env.paths(), WbVariant::Cn, &env.target("uid-a"))
                .unwrap_err()
                .contains("当前账号与目标账号相同")
        );
    }

    /// 执行入口的生命周期与入参保护。
    #[test]
    fn sync_requires_app_stopped_and_valid_target() {
        let env = ready_env("sync-lifecycle");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let target_body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        let one = [selection(&group_id, &token, SyncMode::FastForward)];

        // 目标账号不是当前登录账号（auth=uid-a）：App 运行中不再拦截，正常同步（S5 实测口径）。
        let report = sync_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &one,
            |_| true,
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1);
        let target_body_after = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        assert_ne!(target_body_before, target_body_after, "目标正文已追平");

        // 空选择项 → 什么都不做，也不要求关闭 App。
        let report = sync_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &[],
            |_| true,
        )
        .unwrap();
        assert!(report["skipped"].as_array().unwrap().is_empty());
        assert!(report["errors"].as_array().unwrap().is_empty());
        assert!(report["synced"].as_array().unwrap().is_empty());

        // 缺 uid / 同账号。
        let err = sync_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &json!({"uid": "  "}),
            &one,
            |_| false,
        )
        .unwrap_err();
        assert_eq!(err, "目标账号缺少 uid，无法同步会话");
        let err = sync_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-a"),
            &one,
            |_| false,
        )
        .unwrap_err();
        assert!(err.contains("当前账号与目标账号相同"), "{err}");

        // 失败的两条（缺 uid / 同账号）都不应再触碰目标正文。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_after
        );
    }

    /// syncSelections 解析：缺字段/未知模式/非数组一律拒绝。
    #[test]
    fn sync_selection_parsing_rejects_invalid_input() {
        assert!(parse_sync_selections(None).unwrap().is_empty());
        assert!(parse_sync_selections(Some(&json!(null)))
            .unwrap()
            .is_empty());
        assert!(parse_sync_selections(Some(&json!("x")))
            .unwrap_err()
            .contains("必须是数组"));

        let parsed = parse_sync_selections(Some(&json!([
            {"groupId": "g-1", "previewToken": "11111111-2222-3333-4444-555555555555", "mode": "overwrite"}
        ])))
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].group_id, "g-1");
        assert_eq!(parsed[0].mode, SyncMode::Overwrite);

        for (name, item) in [
            (
                "缺 groupId",
                json!({"previewToken": "t", "mode": "fastForward"}),
            ),
            (
                "缺 previewToken",
                json!({"groupId": "g-1", "mode": "fastForward"}),
            ),
            ("缺 mode", json!({"groupId": "g-1", "previewToken": "t"})),
            (
                "空白 groupId",
                json!({"groupId": "  ", "previewToken": "t", "mode": "fastForward"}),
            ),
        ] {
            let error = parse_sync_selections(Some(&json!([item]))).unwrap_err();
            assert!(!error.is_empty(), "{name}");
        }
    }

    // ---------------------------------------------------------------------------
    // 备份、写入与中断恢复（S6）
    // ---------------------------------------------------------------------------

    /// 目标会话行的可观测状态（用户 id、标题、自定义标题、更新时间、删除时间）。
    type RowView = (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );

    fn try_session_row(env: &Env, cid: &str) -> Option<RowView> {
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.query_row(
            "SELECT user_id, title, custom_title, updated_at, deleted_at FROM sessions WHERE id = ?1",
            [cid],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .ok()
    }

    fn session_row(env: &Env, cid: &str) -> RowView {
        try_session_row(env, cid).expect("会话记录必须存在")
    }

    /// 改标题与自定义标题（模拟用户在目标账号改名；标题不是内容身份，不使预览失效）。
    fn set_row_meta(env: &Env, cid: &str, title: &str, custom_title: Option<&str>) {
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET title = ?1, custom_title = ?2 WHERE id = ?3",
            rusqlite::params![title, custom_title, cid],
        )
        .unwrap();
    }

    fn group_snapshot(env: &Env, group_id: &str) -> LinkGroup {
        env.store()
            .groups
            .into_iter()
            .find(|group| group.id == group_id)
            .expect("同步关系必须存在")
    }

    fn pair_ref_of(group: &LinkGroup, left_uid: &str, right_uid: &str) -> String {
        session_link::find_pair_base(
            group,
            &member_id_of(group, left_uid),
            &member_id_of(group, right_uid),
        )
        .expect("成员对基线必须存在")
        .baseline_ref
        .clone()
    }

    /// 同步后目标正文的预期内容：来源正文 + 目标 sessionId。
    fn incoming_text(env: &Env, source_id: &str, target_id: &str) -> String {
        std::fs::read_to_string(env.body_path(source_id))
            .unwrap()
            .replace(source_id, target_id)
    }

    /// 让 `sessions` 的 UPDATE 在事务内失败（模拟数据库写入阶段中断）。
    fn install_block_update_trigger(env: &Env) {
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER block_sync_update BEFORE UPDATE ON sessions
             BEGIN SELECT RAISE(ABORT, 'sessions 更新被测试拦截'); END;",
        )
        .unwrap();
    }

    fn drop_block_update_trigger(env: &Env) {
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute_batch("DROP TRIGGER block_sync_update;")
            .unwrap();
    }

    fn body_bytes(env: &Env, cid: &str) -> Vec<u8> {
        std::fs::read(env.body_path(cid)).unwrap()
    }

    fn sync_operations(env: &Env) -> Vec<Operation> {
        session_link::scan_operations(&env.paths)
            .operations
            .into_iter()
            .filter(|operation| operation.kind == OPERATION_KIND_SYNC)
            .collect()
    }

    /// 成功路径：正文替换为来源内容，SID/标题/custom_title/归属保留，updated_at 更新，
    /// A/B 基线推进、目标成员 lastSyncedAt 更新、不触碰映射库、备份可查看。
    #[test]
    fn sync_fast_forward_replaces_body_and_advances_pair_baseline() {
        let env = ready_env("sync-ff-write");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let source_body = std::fs::read_to_string(env.body_path("sess-1")).unwrap();
        let source_digest_before = full_digest_of(source_body.as_bytes());
        let target_body_before = body_bytes(&env, &target_id);
        // 目标账号已改过名：同步必须原样保留标题与自定义标题。
        set_row_meta(&env, &target_id, "改名后的标题", Some("自定义名"));
        let row_before = session_row(&env, &target_id);
        let pair_ref_before = pair_ref_of(&group_snapshot(&env, &group_id), "uid-a", "uid-b");
        let baselines_before = env.baseline_files();
        let edge_db_before = std::fs::read(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
        assert!(report.get("needsRecovery").is_none(), "{report}");
        let synced = report["synced"].as_array().unwrap();
        assert_eq!(synced.len(), 1, "{report}");
        assert_eq!(synced[0]["status"], "synced");
        assert_eq!(synced[0]["groupId"], group_id);
        assert_eq!(synced[0]["mode"], "fastForward");
        assert_eq!(synced[0]["verdict"], "fastForward");
        assert_eq!(synced[0]["sourceSessionId"], "sess-1");
        assert_eq!(synced[0]["targetSessionId"], target_id);
        assert_eq!(synced[0]["recordCount"]["source"], 5);
        assert_eq!(synced[0]["recordCount"]["targetBefore"], 2);
        assert_eq!(synced[0]["recordCount"]["target"], 5);

        // 成功清理：不展示可还原路径，本次临时目录与维护记录都已回收。
        assert!(synced[0]["backup"].is_null(), "{report}");
        assert!(synced[0]["backupManifest"].is_null(), "{report}");
        assert_eq!(synced[0]["cleanupState"], "cleaned", "{report}");
        assert_ne!(body_bytes(&env, &target_id), target_body_before);
        let operations = sync_operations(&env);
        assert_eq!(operations.len(), 1);
        let operation_id = operations[0].operation_id.clone();
        assert!(
            !session_backup::transaction_dir(&env.paths, WbVariant::Cn, &operation_id)
                .unwrap()
                .exists(),
            "成功路径必须回收本次操作专属目录"
        );
        assert!(
            session_backup::scan_lifecycle(&env.paths)
                .records
                .is_empty(),
            "成功路径不得残留维护记录"
        );
        assert_eq!(
            operations[0].cleanup_state.as_deref(),
            Some(session_backup::CLEANUP_STATE_CLEANED),
            "业务日志标注备份已清理"
        );
        assert!(
            operations[0].backup.is_none(),
            "已清理的操作不再展示备份位置"
        );

        // 目标正文被替换为来源内容（本副本 sessionId 换成目标 id）；来源正文不动。
        let expected = incoming_text(&env, "sess-1", &target_id);
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_eq!(
            full_digest_of(std::fs::read(env.body_path("sess-1")).unwrap().as_slice()),
            source_digest_before
        );

        // 数据库：只更新 updated_at，SID/归属/标题/custom_title 原样保留。
        let row_after = session_row(&env, &target_id);
        assert_eq!(row_after.0, row_before.0);
        assert_eq!(row_after.1, row_before.1);
        assert_eq!(row_after.2, row_before.2);
        assert_eq!(row_after.3, Some(synced[0]["updatedAt"].as_i64().unwrap()));
        assert!(row_after.3.unwrap() > row_before.3.unwrap());
        assert_eq!(row_after.4, None);

        // 组表：A/B 基线推进到本次写入内容，目标成员 lastSyncedAt 更新。
        let incoming_normalized =
            session_link::normalize_jsonl(&expected, &target_id).expect("内容可归一化");
        let group = group_snapshot(&env, &group_id);
        let pair_ref_after = pair_ref_of(&group, "uid-a", "uid-b");
        assert_ne!(pair_ref_after, pair_ref_before);
        assert_eq!(env.baseline_files(), baselines_before + 1);
        let baseline = session_link::load_baseline(&env.paths, &pair_ref_after).unwrap();
        assert_eq!(baseline.total_digest, incoming_normalized.total_digest);
        assert_eq!(baseline.record_count, 5);
        let last_synced = session_link::active_member_for(&group, "uid-b")
            .unwrap()
            .last_synced_at
            .expect("目标成员必须记录本次同步时间");
        assert!(last_synced >= row_after.3.unwrap());
        assert!(session_link::active_member_for(&group, "uid-a")
            .unwrap()
            .last_synced_at
            .is_none());

        // 不修改 edge-sync-mapping 库；不产生第二个副本；操作日志只有一条且已完成。
        assert_eq!(
            std::fs::read(env.paths.edge_sync_db(WbVariant::Cn)).unwrap(),
            edge_db_before
        );
        assert_eq!(env.body_files().len(), 2);
        assert_eq!(operations[0].phase, OpPhase::Completed);
        assert_eq!(operations[0].target.session_id, target_id);
        assert!(session_link::pending_operations(&env.paths, WbVariant::Cn).is_empty());
    }

    /// 故障状态下的备份必须保持完整可核验：清单、一致性快照、覆盖前/待写入正文与摘要
    /// 齐全，位于本版专属临时目录根，且维护记录仍能追踪（design §8：安全断言保留在
    /// 准备/故障阶段验证，而不是在成功路径上）。
    #[test]
    fn pending_sync_backup_stays_verifiable_until_recovery() {
        let env = ready_env("sync-backup-verify-on-failure");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        set_row_meta(&env, &target_id, "改名后的标题", Some("自定义名"));
        let row_before = session_row(&env, &target_id);
        let target_body_before = body_bytes(&env, &target_id);

        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["needsRecovery"], true, "{report}");
        drop_block_update_trigger(&env);

        let operation = session_link::pending_operations(&env.paths, WbVariant::Cn)
            .into_iter()
            .next()
            .expect("故障后必须保留未完成操作");
        let manifest_file = PathBuf::from(operation.backup.clone().expect("必须记录备份位置"));
        let backup_dir = manifest_file.parent().unwrap().to_path_buf();
        assert_eq!(
            manifest_file,
            backup_dir.join("manifest.json"),
            "清单是恢复的唯一依据"
        );
        let manifest = load_sync_manifest(&backup_dir).expect("备份清单必须可读");
        assert_eq!(
            backup_dir.file_name().unwrap().to_string_lossy(),
            manifest.operation_id,
            "备份目录按 operationId 唯一"
        );
        assert_eq!(manifest.group_id, group_id);
        assert_eq!(manifest.variant, WbVariant::Cn);
        assert_eq!(manifest.mode, SyncMode::FastForward);
        assert_eq!(manifest.verdict, SyncVerdict::FastForward);
        assert_eq!(manifest.source.uid, "uid-a");
        assert_eq!(manifest.source.session_id, "sess-1");
        assert_eq!(manifest.source.body_file, None);
        assert_eq!(manifest.target.uid, "uid-b");
        assert_eq!(manifest.target.session_id, target_id);
        assert_eq!(
            manifest.target_body_path,
            env.body_path(&target_id).to_string_lossy()
        );
        assert!(backup_dir.join(&manifest.db.snapshot_file).is_file());
        assert_eq!(manifest.db.method, DB_SNAPSHOT_METHOD);
        let before_row = manifest.db.target_row.as_ref().expect("必须记录目标行");
        assert_eq!(before_row.session_id, target_id);
        assert_eq!(before_row.user_id, "uid-b");
        assert_eq!(before_row.title.as_deref(), Some("改名后的标题"));
        assert_eq!(before_row.custom_title.as_deref(), Some("自定义名"));
        assert_eq!(before_row.updated_at, row_before.3);
        let original_backup = backup_dir.join(manifest.target.body_file.clone().unwrap());
        assert_eq!(std::fs::read(&original_backup).unwrap(), target_body_before);
        assert_eq!(
            full_digest_of(&std::fs::read(&original_backup).unwrap()),
            manifest.target.body_raw_digest
        );
        assert_eq!(manifest.target.record_count, 2);
        assert_eq!(manifest.incoming.record_count, 5);
        assert!(manifest.restore_steps.len() >= 4);
        verify_sync_backup(&backup_dir, &manifest).expect("备份必须可核验");
        // 备份位于本版专属临时目录根，维护记录仍能追踪同一 operationId。
        assert!(backup_dir.starts_with(
            env.paths
                .backup_root()
                .join(session_backup::TRANSACTIONS_DIR_NAME)
        ));
        assert!(session_backup::scan_lifecycle(&env.paths)
            .records
            .iter()
            .any(|record| record.operation_id == manifest.operation_id));
    }

    /// 显式覆盖：目标全文被替换为来源内容（目标独有记录不再保留），仍然保留 SID/标题。
    #[test]
    fn sync_overwrite_replaces_target_full_text() {
        let env = ready_env("sync-overwrite");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        // 双方都变化：来源追加 2 条，目标也追加 3 条 → diverge。
        append_records(&env.body_path("sess-1"), "sess-1", 0, 2);
        append_records(&env.body_path(&target_id), &target_id, 10, 3);
        set_row_meta(&env, &target_id, "目标标题", Some("目标自定义名"));
        let row_before = session_row(&env, &target_id);

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "diverge", "{preview}");
        assert_eq!(group["defaultChecked"], false);
        assert_eq!(group["availableModes"], json!(["overwrite"]));
        assert_eq!(group["extraB"], 3);
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::Overwrite)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        let expected = incoming_text(&env, "sess-1", &target_id);
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected,
            "覆盖必须替换目标全文"
        );
        let row_after = session_row(&env, &target_id);
        assert_eq!(row_after.1.as_deref(), Some("目标标题"));
        assert_eq!(row_after.2.as_deref(), Some("目标自定义名"));
        assert_ne!(row_after.3, row_before.3);
    }

    /// 双方一致（identical）不写正文：既不发凭据，强行执行也会被拒绝。
    #[test]
    fn sync_identical_verdict_never_writes_body() {
        let env = ready_env("sync-identical");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        let target_before = body_bytes(&env, &target_id);
        let revision_before = env.store().revision;

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "identical", "{preview}");
        assert_eq!(group["defaultChecked"], false);
        assert_eq!(group["availableModes"], json!([]));
        assert!(group.get("previewToken").is_none(), "不可执行的组不发凭据");
        let group_id = group["groupId"].as_str().unwrap().to_string();

        // 伪造一份 identical 的服务端凭据强行覆盖：判定不允许该模式 → 拒绝，不写正文。
        let paths = env.paths();
        let store = env.store();
        let g = store
            .groups
            .iter()
            .find(|candidate| candidate.id == group_id)
            .unwrap();
        let source_member = session_link::active_member_for(g, "uid-a").unwrap();
        let target_member = session_link::active_member_for(g, "uid-b").unwrap();
        let source_content = member_content_state(&paths, &source_member.session_id);
        let target_content = member_content_state(&paths, &target_member.session_id);
        let baseline = session_link::load_pair_baseline(
            &paths,
            g,
            &source_member.member_id,
            &target_member.member_id,
        );
        assert_eq!(
            session_link::decide_sync(&source_content, &target_content, &baseline).verdict,
            SyncVerdict::Identical
        );
        let forged = session_link::save_preview_token(
            &paths,
            live_preview_binding(
                g,
                source_member,
                target_member,
                &source_content,
                &target_content,
                &baseline,
                SyncVerdict::Identical,
                &PreviewArchiveState::default(),
            ),
        )
        .unwrap();
        for mode in [SyncMode::Overwrite, SyncMode::FastForward] {
            let report = sync(&env, "uid-b", &[selection(&group_id, &forged, mode)]);
            assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
            assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
            assert!(
                report["errors"][0]["error"]
                    .as_str()
                    .unwrap()
                    .contains("不允许以"),
                "{report}"
            );
        }
        assert_eq!(body_bytes(&env, &target_id), target_before);
        assert_eq!(env.store().revision, revision_before);
        assert_eq!(pair_snapshot(&env).len(), 1, "基线没有被推进");
    }

    /// 备份失败 → 零覆盖：正文、数据库、基线、关联版本都没有变化，也不留未完成操作。
    #[test]
    fn sync_backup_failure_leaves_target_and_store_untouched() {
        let env = ready_env("sync-backup-fail");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let target_before = body_bytes(&env, &target_id);
        let row_before = session_row(&env, &target_id);
        let revision_before = env.store().revision;
        let baselines_before = env.baseline_files();
        let pairs_before = pair_snapshot(&env);

        // 临时目录根被占成普通文件 → 操作专属目录创建失败。
        std::fs::create_dir_all(env.paths.backup_root()).unwrap();
        let transactions_root = env
            .paths
            .backup_root()
            .join(session_backup::TRANSACTIONS_DIR_NAME);
        let _ = std::fs::remove_dir_all(&transactions_root);
        std::fs::write(&transactions_root, b"occupied").unwrap();

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("临时目录创建失败"), "{error}");

        // 零写入：正文、数据库行、基线、关联版本原样保留。
        assert_eq!(body_bytes(&env, &target_id), target_before);
        assert_eq!(session_row(&env, &target_id), row_before);
        assert_eq!(env.store().revision, revision_before);
        assert_eq!(env.baseline_files(), baselines_before);
        assert_eq!(pair_snapshot(&env), pairs_before);
        // 备份没成功就什么都不能留下：没有未完成操作，也不需要恢复。
        assert!(session_link::pending_operations(&env.paths, WbVariant::Cn).is_empty());
        assert!(sync_operations(&env).is_empty());
        assert!(report.get("needsRecovery").is_none(), "{report}");
    }

    /// 正文写入阶段中断：不报成功、保留未完成操作、恢复后补完且不产生第二份。
    #[cfg(unix)]
    #[test]
    fn sync_body_write_failure_keeps_pending_then_recovers() {
        use std::os::unix::fs::PermissionsExt;

        let env = ready_env("sync-body-fail");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let row_before = session_row(&env, &target_id);
        let target_before = body_bytes(&env, &target_id);

        let ws = env.paths.projects_dir().join("ws-a");
        std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::write(ws.join(".probe"), b"x").is_ok();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o755)).unwrap();
        if writable {
            // root / 特殊 ACL 环境：写保护无效，跳过断言。
            return;
        }
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("同步内容保存失败"), "{error}");
        assert_eq!(report["needsRecovery"], true);

        // 正文与数据库都没变；备份已生成并留下未完成操作（阶段停在 Prepared）。
        assert_eq!(body_bytes(&env, &target_id), target_before);
        assert_eq!(session_row(&env, &target_id), row_before);
        let pending = session_link::pending_operations(&env.paths, WbVariant::Cn);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].phase, OpPhase::Prepared);
        let operation_id = pending[0].operation_id.clone();
        let backup_dir = PathBuf::from(pending[0].backup.clone().unwrap())
            .parent()
            .unwrap()
            .to_path_buf();
        let manifest = load_sync_manifest(&backup_dir).expect("备份清单必须可读");

        // 恢复：按同一份清单补完，复用同一个目标 UUID 与新基线引用。
        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert_eq!(recovery.recovered, vec![operation_id.clone()]);
        assert!(recovery.is_clean(), "{:?}", recovery.needs_recovery);
        let expected = incoming_text(&env, "sess-1", &target_id);
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_ne!(session_row(&env, &target_id).3, row_before.3);
        assert_eq!(env.body_files().len(), 2, "恢复不得产生第二份副本");
        assert_eq!(
            Some(pair_ref_of(
                &group_snapshot(&env, &group_id),
                "uid-a",
                "uid-b"
            )),
            manifest.new_baseline_ref,
            "恢复复用同一个新基线引用"
        );
        assert!(session_link::pending_operations(&env.paths, WbVariant::Cn).is_empty());
        let operations = sync_operations(&env);
        assert_eq!(operations.len(), 1, "不得新增第二条操作记录");
        assert_eq!(operations[0].operation_id, operation_id);
        assert_eq!(operations[0].phase, OpPhase::Completed);
        assert_eq!(operations[0].target.session_id, target_id);
    }

    /// 数据库更新阶段中断：不报成功；解除故障后恢复补完，不产生第二份写入。
    #[test]
    fn sync_db_update_failure_is_not_reported_as_success_then_recovers() {
        let env = ready_env("sync-db-fail");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let row_before = session_row(&env, &target_id);
        let expected = incoming_text(&env, "sess-1", &target_id);
        let baselines_before = env.baseline_files();
        let pairs_before = pair_snapshot(&env);

        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("目标会话记录更新失败"), "{error}");
        assert_eq!(report["needsRecovery"], true);

        // 正文已写入，但数据库未更新：绝不报成功；组表也未提交。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_eq!(session_row(&env, &target_id), row_before);
        assert_eq!(env.baseline_files(), baselines_before);
        assert_eq!(pair_snapshot(&env), pairs_before);
        let pending = session_link::pending_operations(&env.paths, WbVariant::Cn);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].phase, OpPhase::BodyWritten);
        let manifest_dir = PathBuf::from(pending[0].backup.clone().unwrap())
            .parent()
            .unwrap()
            .to_path_buf();
        let manifest = load_sync_manifest(&manifest_dir).unwrap();

        // 同一组再次同步（故障未解除）：预览已过期，不得写入，也不得新建第二份。
        let again = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(again["synced"].as_array().unwrap().is_empty(), "{again}");
        assert!(again.get("needsRecovery").is_some(), "{again}");
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_eq!(env.body_files().len(), 2);

        // 历史同步恢复仍失败时也必须阻断启动，不能仅依赖本轮新增 pending 差集。
        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert_eq!(recovery.needs_recovery.len(), 1);
        assert!(recovery.recovered.is_empty());
        assert!(recovery.abandoned.is_empty());
        assert!(crate::modules::switch::recovery_blocks_startup(&recovery));
        assert_eq!(sync_operations(&env)[0].phase, OpPhase::BodyWritten);
        assert_eq!(session_row(&env, &target_id), row_before);
        assert_eq!(pair_snapshot(&env), pairs_before);

        // 解除故障后恢复：补完数据库与组表，仍复用同一份清单与同一条操作记录。
        drop_block_update_trigger(&env);
        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery.needs_recovery);
        assert!(recovery.is_clean(), "{:?}", recovery.needs_recovery);
        assert_eq!(
            session_row(&env, &target_id).3,
            Some(manifest.db.new_updated_at)
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_eq!(env.body_files().len(), 2);
        assert_eq!(env.baseline_files(), baselines_before + 1);
        let group = group_snapshot(&env, &group_id);
        assert_eq!(
            Some(pair_ref_of(&group, "uid-a", "uid-b")),
            manifest.new_baseline_ref
        );
        assert_eq!(
            session_link::active_member_for(&group, "uid-b")
                .unwrap()
                .last_synced_at,
            Some(manifest.last_synced_at)
        );
        let operations = sync_operations(&env);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].phase, OpPhase::Completed);
        assert_eq!(operations[0].target.session_id, target_id);
    }

    /// 组表提交阶段中断：正文与数据库已写、不报成功；恢复只补组表，不重放已完成阶段。
    #[test]
    fn sync_link_commit_failure_keeps_pending_then_recovers() {
        let env = ready_env("sync-link-fail");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let row_before = session_row(&env, &target_id);
        let expected = incoming_text(&env, "sess-1", &target_id);
        let baselines_before = env.baseline_files();
        let revision_before = env.store().revision;

        // 关联存储锁被占用：只有组表提交这一步会失败。
        let held = session_link::try_lock_file(&env.paths.link_store_lock_file()).unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        drop(held);

        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["needsRecovery"], true);
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("同步记录"), "{error}");

        // 正文与数据库都已写入，但组表未提交 → 阶段停在 DbWritten。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_ne!(session_row(&env, &target_id).3, row_before.3);
        assert_eq!(
            env.baseline_files(),
            baselines_before,
            "组表未提交，基线文件也未落盘"
        );
        assert_eq!(env.store().revision, revision_before, "组表未写");
        let pending = session_link::pending_operations(&env.paths, WbVariant::Cn);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].phase, OpPhase::DbWritten);
        let manifest_dir = PathBuf::from(pending[0].backup.clone().unwrap())
            .parent()
            .unwrap()
            .to_path_buf();
        let manifest = load_sync_manifest(&manifest_dir).unwrap();

        // 恢复：只补组表（复用清单里的新基线引用），不重写正文、不重复登记第二份。
        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery.needs_recovery);
        assert!(recovery.is_clean(), "{:?}", recovery.needs_recovery);
        assert_eq!(env.store().revision, revision_before + 1, "组表只提交一次");
        assert_eq!(env.baseline_files(), baselines_before + 1, "不新增重复基线");
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_eq!(
            session_row(&env, &target_id).3,
            Some(manifest.db.new_updated_at)
        );
        let group = group_snapshot(&env, &group_id);
        assert_eq!(
            Some(pair_ref_of(&group, "uid-a", "uid-b")),
            manifest.new_baseline_ref
        );
        assert_eq!(
            session_link::active_member_for(&group, "uid-b")
                .unwrap()
                .last_synced_at,
            Some(manifest.last_synced_at)
        );
        let operations = sync_operations(&env);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].phase, OpPhase::Completed);
        assert_eq!(env.body_files().len(), 2);
    }

    // ---------------------------------------------------------------------------
    // 归档状态同步（statusOnly，#143）
    // ---------------------------------------------------------------------------

    /// 带状态列的可同步环境（会话表含 `status`）。
    fn ready_env_with_status(name: &str) -> Env {
        let env = Env::new(name);
        env.create_db_with_status();
        env.create_edge_db(WbVariant::Cn);
        env.set_login("uid-a");
        env.add_session("sess-1", "uid-a", "标题一");
        env.add_body("sess-1", &body_text("sess-1"));
        env
    }

    /// 同上的历史形态：`status` 列可空（用来验证 NULL 不授予归档权限）。
    fn ready_env_with_nullable_status(name: &str) -> Env {
        let env = Env::new(name);
        env.create_db_with_nullable_status();
        env.create_edge_db(WbVariant::Cn);
        env.set_login("uid-a");
        env.add_session("sess-1", "uid-a", "标题一");
        env.add_body("sess-1", &body_text("sess-1"));
        env
    }

    fn member_last_synced_at(env: &Env, group_id: &str, uid: &str) -> Option<i64> {
        group_snapshot(env, group_id)
            .members
            .into_iter()
            .find(|member| member.uid == uid)
            .and_then(|member| member.last_synced_at)
    }

    /// 造一个「正文一致 + 来源已归档 + 目标为指定状态」的场景（不要求可勾选）。
    fn archive_row_scene(env: &Env, target_status: &str) -> (String, String) {
        let report = copy(env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        env.set_status("sess-1", "archived");
        env.set_status(&target_id, target_status);
        let preview = preview(env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "identical", "{preview}");
        (group["groupId"].as_str().unwrap().to_string(), target_id)
    }

    /// 造一个「正文一致 + 来源已归档 + 目标在终态」的可勾选场景。
    fn archive_scene(env: &Env, target_status: &str) -> (String, String, String) {
        let (group_id, target_id) = archive_row_scene(env, target_status);
        let preview = preview(env, "uid-b");
        let group = &preview["groups"][0];
        (
            group_id,
            group["previewToken"].as_str().unwrap().to_string(),
            target_id,
        )
    }

    /// 造一个「可快进 + 来源已归档」的场景。
    fn fast_forward_archive_scene(env: &Env, target_status: &str) -> (String, String, String) {
        let report = copy(env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path("sess-1"), "sess-1", 2, 3);
        env.set_status("sess-1", "archived");
        env.set_status(&target_id, target_status);
        let preview = preview(env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward", "{preview}");
        (
            group["groupId"].as_str().unwrap().to_string(),
            group["previewToken"].as_str().unwrap().to_string(),
            target_id,
        )
    }

    /// 目标已归档且来源有可快进的新记录：正文照写、时间照更新，状态保持 archived。
    #[test]
    fn fast_forward_keeps_archived_target_archived() {
        let env = ready_env_with_status("status-ff-already-archived");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path("sess-1"), "sess-1", 2, 3);
        env.set_status("sess-1", "completed");
        env.set_status(&target_id, "archived");
        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward", "{preview}");
        assert!(group["archiveAction"].is_null(), "{group}");
        let token = group["previewToken"].as_str().unwrap().to_string();
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let updated_before = session_row(&env, &target_id).3;

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id)
        );
        assert_ne!(
            session_row(&env, &target_id).3,
            updated_before,
            "正文写入照旧更新时间"
        );
    }

    /// 造一个「双方都有改动（diverge）+ 来源已归档」的场景。
    fn diverge_archive_scene(env: &Env, target_status: &str) -> (String, String, String) {
        let report = copy(env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        // 目标追加一条来源没有的记录（index 100 与来源的 2/3 都不同）→ 双方互不为前缀。
        append_records(&env.body_path("sess-1"), "sess-1", 2, 2);
        append_records(&env.body_path(&target_id), &target_id, 100, 1);
        env.set_status("sess-1", "archived");
        env.set_status(&target_id, target_status);
        let preview = preview(env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "diverge", "{preview}");
        (
            group["groupId"].as_str().unwrap().to_string(),
            group["previewToken"].as_str().unwrap().to_string(),
            target_id,
        )
    }

    /// 正文一致且归档资格成立：只改状态，正文字节、配对基线与 lastSyncedAt 全不动。
    #[test]
    fn status_sync_archives_target_without_touching_body() {
        let env = ready_env_with_status("status-identical");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(
            group["availableModes"],
            json!([]),
            "正文模式仍为空：归档不是正文覆盖"
        );
        assert_eq!(group["archiveAction"], "statusOnly");
        assert_eq!(group["defaultChecked"], false);

        let body_before = body_bytes(&env, &target_id);
        let pairs_before = pair_snapshot(&env);
        let baselines_before = env.baseline_files();
        let synced_before = member_last_synced_at(&env, &group_id, "uid-b");

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        let item = &report["synced"][0];
        assert_eq!(item["mode"], "statusOnly");
        assert_eq!(item["verdict"], "identical");
        assert_eq!(item["recordCount"]["target"], 2, "记录数来自目标原正文");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(
            body_bytes(&env, &target_id),
            body_before,
            "正文字节不得变化"
        );
        assert_eq!(pair_snapshot(&env), pairs_before, "配对基线不得推进");
        assert_eq!(env.baseline_files(), baselines_before, "不得产生新基线");
        assert_eq!(
            member_last_synced_at(&env, &group_id, "uid-b"),
            synced_before,
            "正文 lastSyncedAt 保持原值"
        );
        assert_eq!(env.body_files().len(), 2, "不得产生第二份正文");
    }

    /// 终态白名单：completed / error / terminated / active 可归档；`working`、`Pending`、
    /// NULL 与未知值一律不授权。
    ///
    /// `active` 在列内（issue #204）：切号前客户端被关掉常留下陈旧 `active`，排除它会让
    /// 归档状态永远无法与来源收敛；真正的活跃会话由写入侧三重门禁兜底。
    #[test]
    fn status_sync_accepts_only_terminal_target_statuses() {
        for allowed in ["completed", "error", "terminated", "active"] {
            let env = ready_env_with_status(&format!("status-allow-{allowed}"));
            let (group_id, token, target_id) = archive_scene(&env, allowed);
            let report = sync(
                &env,
                "uid-b",
                &[selection(&group_id, &token, SyncMode::StatusOnly)],
            );
            assert_eq!(
                report["synced"].as_array().unwrap().len(),
                1,
                "{allowed}: {report}"
            );
            assert_eq!(
                env.status_of(&target_id).as_deref(),
                Some("archived"),
                "{allowed}"
            );
        }
        for denied in ["working", "Pending", "unknown-value"] {
            let env = ready_env_with_status(&format!("status-deny-{denied}"));
            let (_, target_id) = archive_row_scene(&env, denied);
            let preview_report = preview(&env, "uid-b");
            let group = &preview_report["groups"][0];
            assert!(group["archiveAction"].is_null(), "{denied}: {group}");
            assert!(
                group["previewToken"].is_null(),
                "{denied}: 无动作即无凭据 {group}"
            );
            assert_eq!(
                env.status_of(&target_id).as_deref(),
                Some(denied),
                "{denied}: 不得写入状态"
            );
        }
        // status 为 NULL：非终态，不授权（列可空的旧库才写得进 NULL）。
        let env = ready_env_with_nullable_status("status-deny-null");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        env.set_status("sess-1", "archived");
        env.set_status_null(&target_id);
        let preview_report = preview(&env, "uid-b");
        let group = &preview_report["groups"][0];
        assert_eq!(group["verdict"], "identical", "{preview_report}");
        assert!(group["archiveAction"].is_null(), "NULL: {group}");
        assert!(
            group["previewToken"].is_null(),
            "NULL: 无动作即无凭据 {group}"
        );
        assert_eq!(env.status_of(&target_id), None, "NULL: 不得写入状态");
    }

    /// issue #204：目标停在切号前残留的陈旧 `active` 时，归档必须仍能收敛。
    ///
    /// 场景＝上次切号后客户端被关掉/崩掉，目标行停在 `active` 且没有在跑的生成；
    /// 旧白名单把它当活跃态排除，会让该会话的归档状态永久卡住。
    #[test]
    fn status_sync_archives_stale_active_target() {
        let env = ready_env_with_status("status-stale-active");
        let (group_id, token, target_id) = archive_scene(&env, "active");
        let before = preview(&env, "uid-b");
        let group = &before["groups"][0];
        assert_eq!(group["archiveAction"], "statusOnly", "{before}");
        assert_eq!(group["availableModes"], json!([]), "归档不是正文覆盖");
        assert_eq!(group["defaultChecked"], false, "归档默认不勾选");
        let body_before = body_bytes(&env, &target_id);

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(report["synced"][0]["mode"], "statusOnly");
        assert_eq!(report["synced"][0]["verdict"], "identical");
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("archived"),
            "陈旧 active 目标必须被归档"
        );
        assert_eq!(
            body_bytes(&env, &target_id),
            body_before,
            "只改状态，正文不得变化"
        );
    }

    /// 陈旧的 `active` 目标一旦真的开始生成（`working`），归档立刻重新被拒绝。
    ///
    /// 放行 `active` 靠的是「生成中的会话不会停在 active」这个前提：客户端在生成时
    /// 先把状态写成 `working`，所以 `working` 仍在白名单外，仍是硬拦截。
    #[test]
    fn status_sync_denies_when_stale_active_target_starts_generating() {
        let env = ready_env_with_status("status-active-to-working");
        let (group_id, token, target_id) = archive_scene(&env, "active");
        env.set_status(&target_id, "working");

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("working"),
            "生成中的会话不得被收进归档"
        );
    }

    /// 单向粘滞：来源未归档、目标已归档时，绝不反向取消归档。
    #[test]
    fn status_sync_never_unarchives_target() {
        let env = ready_env_with_status("status-no-unarchive");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        env.set_status("sess-1", "completed");
        env.set_status(&target_id, "archived");

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "identical", "{preview}");
        assert!(group["archiveAction"].is_null(), "{group}");
        assert!(group["previewToken"].is_null(), "{group}");
        let updated_before = session_row(&env, &target_id).3;
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(session_row(&env, &target_id).3, updated_before);
    }

    /// 双方都已归档：无动作、不写库、不抬时间戳。
    #[test]
    fn status_sync_is_noop_when_both_already_archived() {
        let env = ready_env_with_status("status-both-archived");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        env.set_status("sess-1", "archived");
        env.set_status(&target_id, "archived");
        let updated_before = session_row(&env, &target_id).3;

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert!(group["archiveAction"].is_null(), "{group}");
        assert!(group["previewToken"].is_null(), "{group}");
        assert_eq!(
            session_row(&env, &target_id).3,
            updated_before,
            "不得刷新时间"
        );
    }

    /// Ahead 只传导归档：目标独有内容完整保留，后续判定不因状态操作虚假变成一致。
    #[test]
    fn status_sync_preserves_ahead_target_content() {
        let env = ready_env_with_status("status-ahead");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path(&target_id), &target_id, 2, 2);
        env.set_status("sess-1", "archived");
        env.set_status(&target_id, "completed");

        let preview_report = preview(&env, "uid-b");
        let group = &preview_report["groups"][0];
        assert_eq!(group["verdict"], "ahead", "{preview_report}");
        assert_eq!(group["availableModes"], json!([]), "切号预览仍不给正文模式");
        assert_eq!(group["archiveAction"], "statusOnly");
        assert_eq!(group["defaultChecked"], false);
        let token = group["previewToken"].as_str().unwrap().to_string();
        let group_id = group["groupId"].as_str().unwrap().to_string();

        let body_before = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            body_before,
            "目标独有内容必须保留"
        );
        // 状态操作不得让后续判定虚假变成 identical。
        let again = preview(&env, "uid-b");
        assert_eq!(again["groups"][0]["verdict"], "ahead", "{again}");
    }

    /// 快进且来源已归档：提交的是 fastForward，归档作为副作用随正文写入一并生效。
    #[test]
    fn fast_forward_archives_target_as_side_effect() {
        let env = ready_env_with_status("status-ff");
        let (group_id, token, target_id) = fast_forward_archive_scene(&env, "completed");
        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["availableModes"], json!(["fastForward"]));
        assert_eq!(group["archiveAction"], "statusOnly");

        let updated_before = session_row(&env, &target_id).3;
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(report["synced"][0]["mode"], "fastForward");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id),
            "正文必须含来源新记录"
        );
        assert_ne!(
            session_row(&env, &target_id).3,
            updated_before,
            "正文写入照旧更新时间"
        );
    }

    /// 陈旧 `active` 目标 + 正文快进：归档作为正文写入的副作用一并生效。
    ///
    /// 正文模式的状态写入走 `planned_status_change`（写前重核，不查白名单）与事务内
    /// 按前置状态等值匹配的条件 UPDATE，因此 active 目标既能被归档、现场被改成
    /// `working` 时又会被跳过。
    #[test]
    fn fast_forward_archives_stale_active_target_as_side_effect() {
        let env = ready_env_with_status("status-ff-stale-active");
        let (group_id, token, target_id) = fast_forward_archive_scene(&env, "active");
        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["availableModes"], json!(["fastForward"]));
        assert_eq!(group["archiveAction"], "statusOnly");

        let updated_before = session_row(&env, &target_id).3;
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(report["synced"][0]["mode"], "fastForward");
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("archived"),
            "陈旧 active 目标必须随正文一并归档"
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id),
            "正文必须含来源新记录"
        );
        assert_ne!(
            session_row(&env, &target_id).3,
            updated_before,
            "正文写入照旧更新时间"
        );
    }

    /// 冲突：归档资格成立也不默认勾选，只有显式提交 overwrite 才随正文归档。
    #[test]
    fn diverge_still_requires_explicit_overwrite_then_archives() {
        let env = ready_env_with_status("status-diverge");
        let (group_id, token, target_id) = diverge_archive_scene(&env, "completed");
        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["availableModes"], json!(["overwrite"]));
        assert_eq!(
            group["archiveAction"], "statusOnly",
            "预览时即固定，不随勾选变化"
        );
        assert_eq!(group["defaultChecked"], false, "冲突仍不默认勾选");

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::Overwrite)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id)
        );
    }

    /// 预览后来源取消归档：整体判为预览失效，零状态写入。
    #[test]
    fn status_sync_skips_when_source_unarchived_after_preview() {
        let env = ready_env_with_status("status-stale-source");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        env.set_status("sess-1", "completed");

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(env.status_of(&target_id).as_deref(), Some("completed"));
    }

    /// 预览后目标变为活跃：条件 UPDATE 拒绝，不得把活会话收进归档。
    #[test]
    fn status_sync_skips_when_target_becomes_active_after_preview() {
        let env = ready_env_with_status("status-stale-target");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        env.set_status(&target_id, "working");

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(env.status_of(&target_id).as_deref(), Some("working"));
    }

    /// 伪造 statusOnly 提交：凭据里没有授权即拒绝，不静默执行。
    #[test]
    fn status_sync_rejects_unauthorized_status_only() {
        // 不带 status 列的库：双方状态读数恒为 None，与伪造凭据一致，
        // 因此走的是「凭据未授权归档」的拒绝路径，而不是预览过期。
        let env = ready_env("status-unauthorized");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "identical", "{preview}");
        assert!(group["previewToken"].is_null(), "无动作即无凭据 {group}");

        // 手工伪造一份「identical + statusOnly」凭据：执行必须拒绝。
        let store = env.store();
        let group_id = store.groups[0].id.clone();
        let source_member = store.groups[0]
            .members
            .iter()
            .find(|member| member.uid == "uid-a")
            .unwrap()
            .clone();
        let target_member = store.groups[0]
            .members
            .iter()
            .find(|member| member.uid == "uid-b")
            .unwrap()
            .clone();
        let source_content = member_content_state(&env.paths(), &source_member.session_id);
        let target_content = member_content_state(&env.paths(), &target_member.session_id);
        let baseline = session_link::load_pair_baseline(
            &env.paths(),
            &store.groups[0],
            &source_member.member_id,
            &target_member.member_id,
        );
        let forged = session_link::save_preview_token(
            &env.paths(),
            live_preview_binding(
                &store.groups[0],
                &source_member,
                &target_member,
                &source_content,
                &target_content,
                &baseline,
                SyncVerdict::Identical,
                &PreviewArchiveState::default(),
            ),
        )
        .unwrap();
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &forged, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report}");
        assert!(
            report["errors"][0]["error"]
                .as_str()
                .unwrap()
                .contains("不提供仅同步归档"),
            "{report}"
        );
        let _ = target_id;
    }

    /// 数据库写入中断后恢复：补完归档，无正文写入、无新基线；重复恢复不二次写入。
    #[test]
    fn status_sync_recovers_after_interrupted_db_write() {
        let env = ready_env_with_status("status-recover");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        let body_before = body_bytes(&env, &target_id);
        let baselines_before = env.baseline_files();
        let pairs_before = pair_snapshot(&env);

        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("completed"));
        drop_block_update_trigger(&env);

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery);
        assert_eq!(env.status_of(&target_id).as_deref(), Some("archived"));
        assert_eq!(body_bytes(&env, &target_id), body_before, "恢复不得写正文");
        assert_eq!(env.baseline_files(), baselines_before, "恢复不得产生基线");
        assert_eq!(pair_snapshot(&env), pairs_before);

        // 重复恢复：已完成的不再重放，也不再刷新时间。
        let updated = session_row(&env, &target_id).3;
        let again = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            again.needs_recovery.is_empty(),
            "{:?}",
            again.needs_recovery
        );
        assert!(again.recovered.is_empty(), "{:?}", again);
        assert_eq!(session_row(&env, &target_id).3, updated);
    }

    /// 中断后目标正文被追加：保留并报告人工处理，不补写正文、不越权改状态。
    #[test]
    fn status_sync_reports_manual_handling_when_body_changed() {
        let env = ready_env_with_status("status-recover-tampered");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);

        append_records(&env.body_path(&target_id), &target_id, 100, 1);
        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(recovery.recovered.is_empty(), "{:?}", recovery);
        assert_eq!(recovery.needs_recovery.len(), 1, "{:?}", recovery);
        assert!(!recovery.needs_recovery[0].retryable);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("completed"),
            "正文已变即不得补写归档"
        );
    }

    /// 旧清单缺 status / newStatus：正常解析并按旧行为恢复，不自动归档。
    #[test]
    fn legacy_manifest_without_status_fields_parses_with_old_behavior() {
        let env = ready_env_with_status("legacy-manifest");
        let (group_id, token, _target_id) = fast_forward_archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);
        let operation = sync_operations(&env)[0].clone();
        let manifest_path = operation.backup.clone().unwrap();
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        {
            let db = value.get_mut("db").unwrap().as_object_mut().unwrap();
            db.remove("newStatus");
            if let Some(row) = db.get_mut("targetRow").and_then(Value::as_object_mut) {
                row.remove("status");
            }
        }
        let dir = env.root.join("legacy-manifest-dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
        let parsed = load_sync_manifest(&dir).expect("旧清单必须能解析");
        assert_eq!(parsed.db.new_status, None, "旧清单不得被当成归档意图");
        assert!(parsed
            .db
            .target_row
            .as_ref()
            .and_then(|row| row.status.clone())
            .is_none());
        assert!(
            parsed.new_baseline_ref.is_some(),
            "旧清单的字符串基线引用仍读为 Some"
        );
        assert_eq!(parsed.mode, SyncMode::FastForward);
    }

    /// 仅同步归档的清单不落新基线引用；正文模式必须有。
    #[test]
    fn status_only_manifest_has_no_new_baseline_ref() {
        let env = ready_env_with_status("status-manifest-baseline");
        let (group_id, token, _target_id) = archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let _ = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        drop_block_update_trigger(&env);
        let operation = sync_operations(&env)[0].clone();
        let manifest = load_sync_manifest(
            Path::new(&operation.backup.clone().unwrap())
                .parent()
                .unwrap(),
        )
        .expect("清单必须可读");
        assert_eq!(manifest.mode, SyncMode::StatusOnly);
        assert_eq!(manifest.new_baseline_ref, None, "状态同步不得写假基线引用");
        assert_eq!(manifest.db.new_status.as_deref(), Some("archived"));
        assert_eq!(
            manifest
                .db
                .target_row
                .as_ref()
                .and_then(|row| row.status.clone()),
            Some("completed".to_string())
        );
    }

    /// 会话表缺 status 列：不归档、不报列名错误，正文同步照旧可用。
    #[test]
    fn status_sync_degrades_when_status_column_missing() {
        let env = ready_env("status-no-column");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        append_records(&env.body_path("sess-1"), "sess-1", 2, 3);

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward", "{preview}");
        assert!(
            group["archiveAction"].is_null(),
            "缺列即不提供归档动作 {group}"
        );
        let token = group["previewToken"].as_str().unwrap().to_string();
        let group_id = group["groupId"].as_str().unwrap().to_string();

        // 正文同步不受影响。
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id)
        );
    }

    /// `read_session_row` 在 custom_title × status 四种列组合下索引都正确。
    #[test]
    fn read_session_row_handles_every_column_combination() {
        for (has_custom_title, has_status) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let env = Env::new(&format!("row-cols-{has_custom_title}-{has_status}"));
            let mut columns = vec![
                "id TEXT PRIMARY KEY".to_string(),
                "user_id TEXT NOT NULL".to_string(),
                "title TEXT".to_string(),
            ];
            if has_custom_title {
                columns.push("custom_title TEXT".to_string());
            }
            if has_status {
                columns.push("status TEXT".to_string());
            }
            columns.push("updated_at INTEGER".to_string());
            columns.push("deleted_at INTEGER".to_string());
            let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
            conn.execute_batch(&format!("CREATE TABLE sessions ({});", columns.join(", ")))
                .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, user_id, title, updated_at, deleted_at) \
                 VALUES ('s1', 'uid-a', '标题', 2000, NULL)",
                [],
            )
            .unwrap();
            if has_custom_title {
                conn.execute(
                    "UPDATE sessions SET custom_title = '自定义' WHERE id = 's1'",
                    [],
                )
                .unwrap();
            }
            if has_status {
                conn.execute(
                    "UPDATE sessions SET status = 'completed' WHERE id = 's1'",
                    [],
                )
                .unwrap();
            }
            let row = read_session_row(&conn, "s1").unwrap().expect("行必须可读");
            assert_eq!(row.session_id, "s1");
            assert_eq!(row.user_id, "uid-a");
            assert_eq!(row.title.as_deref(), Some("标题"));
            assert_eq!(
                row.custom_title.as_deref(),
                has_custom_title.then_some("自定义"),
                "{has_custom_title}/{has_status}"
            );
            assert_eq!(
                row.status.as_deref(),
                has_status.then_some("completed"),
                "{has_custom_title}/{has_status}"
            );
            assert_eq!(
                row.updated_at,
                Some(2000),
                "{has_custom_title}/{has_status}"
            );
            assert_eq!(row.deleted_at, None);
        }
    }

    /// `statusOnly` 只能由归档资格授权，不属于任何正文判定的权限。
    #[test]
    fn status_only_mode_is_not_a_body_permission() {
        assert_eq!(SyncMode::parse("statusOnly").unwrap(), SyncMode::StatusOnly);
        assert_eq!(SyncMode::StatusOnly.as_str(), "statusOnly");
        for verdict in [
            SyncVerdict::Identical,
            SyncVerdict::FastForward,
            SyncVerdict::Ahead,
            SyncVerdict::Diverge,
            SyncVerdict::Unknown,
        ] {
            assert!(!verdict.allows(SyncMode::StatusOnly), "{verdict:?}");
            assert!(
                !verdict.available_modes().contains(&SyncMode::StatusOnly),
                "{verdict:?} 的正文模式里不得出现 statusOnly"
            );
        }
        // 原有模式权限不变。
        assert!(SyncVerdict::FastForward.allows(SyncMode::FastForward));
        assert!(SyncVerdict::Diverge.allows(SyncMode::Overwrite));
        assert!(SyncVerdict::Ahead.allows(SyncMode::UnifyOverwrite));
    }

    /// 预览绑定的归档字段：旧凭据可反序列化，状态或动作变化必须报过期。
    #[test]
    fn preview_binding_archive_fields_round_trip_and_are_verified() {
        let env = ready_env_with_status("preview-binding-archive");
        let (group_id, token, _target_id) = archive_scene(&env, "completed");
        let stored = session_link::load_preview_token(&env.paths(), &token).expect("凭据必须存在");
        assert_eq!(stored.version, session_link::PREVIEW_TOKEN_VERSION);
        assert_eq!(stored.binding.source_status.as_deref(), Some("archived"));
        assert_eq!(stored.binding.target_status.as_deref(), Some("completed"));
        assert_eq!(stored.binding.archive_action.as_deref(), Some("statusOnly"));

        // 旧格式（无归档字段）仍能反序列化，且一律按「未授权」处理。
        let mut value = serde_json::to_value(&stored.binding).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("sourceStatus");
        object.remove("targetStatus");
        object.remove("archiveAction");
        let legacy: PreviewBinding = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.source_status, None);
        assert_eq!(legacy.target_status, None);
        assert_eq!(legacy.archive_action, None);

        // 状态或动作任一项变化都必须报过期。
        let mut drifted_status = stored.binding.clone();
        drifted_status.target_status = Some("active".to_string());
        let stale = session_link::verify_preview(&stored, &drifted_status);
        assert!(
            stale.iter().any(|reason| reason.contains("归档状态")),
            "{stale:?}"
        );
        let mut drifted_action = stored.binding.clone();
        drifted_action.archive_action = None;
        let stale = session_link::verify_preview(&stored, &drifted_action);
        assert!(
            stale.iter().any(|reason| reason.contains("归档")),
            "{stale:?}"
        );
        let _ = group_id;
    }

    /// 旧清单在**带 status 列的库**上必须仍能按旧行为恢复完成。
    ///
    /// 回归 guarding：旧清单反序列化后 `targetRow.status` 为 None，若拿它去和现场读出的
    /// `Some("completed")` 比对，会恒不相等并把正常恢复误判成「状态被改动」。
    #[test]
    fn legacy_manifest_recovers_on_database_with_status_column() {
        let env = ready_env_with_status("legacy-manifest-status-db");
        let (group_id, token, target_id) = fast_forward_archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);

        // 把清单改写成改动之前的旧格式：去掉 status 与 newStatus。
        let operation = sync_operations(&env)[0].clone();
        let manifest_path = operation.backup.clone().unwrap();
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        {
            let db = value.get_mut("db").unwrap().as_object_mut().unwrap();
            db.remove("newStatus");
            if let Some(row) = db.get_mut("targetRow").and_then(Value::as_object_mut) {
                row.remove("status");
            }
        }
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "旧清单必须能恢复完成：{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery);
        // 旧清单不含归档意图：正文补完即可，不得顺手归档。
        assert_eq!(env.status_of(&target_id).as_deref(), Some("completed"));
    }

    /// 只归档遇到正文文件被删：放弃本次归档，不写库、不补正文，也不永久阻断启动。
    #[test]
    fn status_sync_abandons_when_target_body_gone() {
        let env = ready_env_with_status("status-body-gone");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);
        std::fs::remove_file(env.body_path(&target_id)).unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "正文缺失不得变成需要人工处理：{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.abandoned.len(), 1, "{:?}", recovery);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("completed"),
            "放弃时不得写状态"
        );
        assert!(!env.body_path(&target_id).exists(), "不得重建正文文件");
    }

    /// 不可验证（unknown）时不下发 archiveAction：没有凭据就勾不了，发了只会误导。
    #[test]
    fn unknown_verdict_never_offers_archive_action() {
        let env = ready_env_with_status("status-unknown");
        let report = copy(&env, "uid-b", &["sess-1"]);
        let target_id = env.first_copy_id(&report);
        env.set_status("sess-1", "archived");
        env.set_status(&target_id, "completed");
        // 删掉目标正文：内容不可验证 → unknown。
        std::fs::remove_file(env.body_path(&target_id)).unwrap();

        let preview = preview(&env, "uid-b");
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "unknown", "{preview}");
        assert!(group["archiveAction"].is_null(), "{group}");
        assert!(group["previewToken"].is_null(), "{group}");
        assert_eq!(env.status_of(&target_id).as_deref(), Some("completed"));
    }

    /// 归档已提交、完成标记未落盘，之后正文丢失：放弃本次，保留已提交的状态。
    ///
    /// `DbWritten` 晚于 `BodyWritten`，若放弃分支排在「已写正文却丢失」的通用拦截
    /// 之后，这个现场会先被通用分支截走并永久阻断启动。
    #[test]
    fn status_sync_abandons_when_body_lost_after_db_commit() {
        let env = ready_env_with_status("status-gone-after-commit");
        let (group_id, token, target_id) = archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::StatusOnly)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);

        // 手工推进到「数据库已提交、完成标记未落盘」的现场：状态置为本次新值，
        // 操作阶段停在 DbWritten。
        env.set_status(&target_id, "archived");
        let mut operation = sync_operations(&env)[0].clone();
        operation.phase = OpPhase::DbWritten;
        session_link::save_operation(&env.paths, &operation).unwrap();
        std::fs::remove_file(env.body_path(&target_id)).unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "归档已提交后正文丢失不得变成人工处理：{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.abandoned.len(), 1, "{:?}", recovery);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("archived"),
            "已提交的状态必须保留"
        );
        assert!(!env.body_path(&target_id).exists(), "不得补写或重建正文");
    }

    /// 不写状态的正文恢复：目标状态被改成 active 也照常补完，不被状态变化阻断。
    #[test]
    fn body_only_recovery_tolerates_unrelated_status_change() {
        let env = ready_env_with_status("body-recovery-status-changed");
        let (group_id, token, target_id) = fast_forward_archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);
        // 中断期间目标被打开并开始生成：状态变为 active（与本次正文同步无关）。
        env.set_status(&target_id, "active");

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "与状态无关的正文恢复不得被阻断：{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("active"),
            "本次不写状态，必须保留当前状态"
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id),
            "正文必须补完"
        );
    }

    /// 跳过归档副作用后「提交数据库 → 完成标记落盘前」再次崩溃：恢复必须能补完，
    /// 不能把自己合法产生的「本次时间戳 + active」判成外部修改。
    #[test]
    fn body_mode_recovery_survives_second_crash_after_archive_skipped() {
        let env = ready_env_with_status("body-skip-second-crash");
        let (group_id, token, target_id) = fast_forward_archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);
        // 目标被打开并开始生成：归档副作用将被合法跳过。
        env.set_status(&target_id, "active");

        // 手工构造「已跳过归档并提交数据库、完成标记未落盘」的现场。
        let mut operation = sync_operations(&env)[0].clone();
        let manifest = load_sync_manifest(
            Path::new(operation.backup.as_ref().unwrap())
                .parent()
                .unwrap(),
        )
        .expect("清单必须可读");
        env.set_updated_at(&target_id, manifest.db.new_updated_at);
        operation.phase = OpPhase::DbWritten;
        session_link::save_operation(&env.paths, &operation).unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "跳过归档后的二次崩溃不得阻断恢复：{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("active"),
            "活跃会话绝不能被归档"
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id)
        );
    }

    /// 正文模式下「数据库已提交、归档曾被跳过、目标又回到终态」：恢复不得重做归档。
    ///
    /// 跳过与否是现场状态的纯函数，但**纯函数不等于跨次恢复幂等**——输入状态会变，
    /// 重新推导就可能从「跳过」翻成「可归档」。数据库步骤必须恰好执行一次。
    #[test]
    fn body_mode_recovery_does_not_reapply_skipped_archive() {
        let env = ready_env_with_status("body-no-rearchive");
        let (group_id, token, target_id) = fast_forward_archive_scene(&env, "completed");
        install_block_update_trigger(&env);
        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        drop_block_update_trigger(&env);

        // 手工构造「数据库已提交且归档被跳过、完成标记未落盘」的现场：时间戳是本次值，
        // 状态仍是 completed（归档跳过后回到终态，或用户取消了归档）。
        let mut operation = sync_operations(&env)[0].clone();
        let manifest = load_sync_manifest(
            Path::new(operation.backup.as_ref().unwrap())
                .parent()
                .unwrap(),
        )
        .expect("清单必须可读");
        env.set_updated_at(&target_id, manifest.db.new_updated_at);
        operation.phase = OpPhase::DbWritten;
        session_link::save_operation(&env.paths, &operation).unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(
            recovery.needs_recovery.is_empty(),
            "数据库已提交后恢复不得再被阻断：{:?}",
            recovery.needs_recovery
        );
        assert_eq!(recovery.recovered.len(), 1, "{:?}", recovery);
        assert_eq!(
            env.status_of(&target_id).as_deref(),
            Some("completed"),
            "恢复不得重做已跳过的归档"
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            incoming_text(&env, "sess-1", &target_id),
            "正文必须已补完"
        );
    }

    /// 造一个「正文已写入、数据库未更新」的中断现场：返回 (目标会话 id, 覆盖前正文, 本次写入正文)。
    fn interrupted_sync_scene(env: &Env) -> (String, Vec<u8>, String) {
        let (group_id, token, target_id) = fast_forward_scene(env);
        let target_before = body_bytes(env, &target_id);
        install_block_update_trigger(env);
        let report = sync(
            env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert_eq!(report["needsRecovery"], true, "{report}");
        drop_block_update_trigger(env);
        let written = std::fs::read_to_string(env.body_path(&target_id)).unwrap();
        assert_eq!(written, incoming_text(env, "sess-1", &target_id));
        assert_eq!(
            session_link::pending_operations(&env.paths, WbVariant::Cn)[0].phase,
            OpPhase::BodyWritten
        );
        (target_id, target_before, written)
    }

    /// 恢复不覆盖后续无关修改：目标正文被改动过 → 停止恢复并要求人工处理。
    #[test]
    fn sync_recovery_stops_when_target_changed_after_interrupted_write() {
        let env = ready_env("sync-recovery-unknown");
        let (target_id, _, _) = interrupted_sync_scene(&env);
        let baselines_before = env.baseline_files();
        let pairs_before = pair_snapshot(&env);

        // 官方 App / 用户对中间产物继续追加了内容：属于「未知内容」，不得覆盖。
        append_records(&env.body_path(&target_id), &target_id, 100, 1);
        let tampered = std::fs::read_to_string(env.body_path(&target_id)).unwrap();

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(recovery.recovered.is_empty(), "{:?}", recovery.recovered);
        assert_eq!(recovery.needs_recovery.len(), 1);
        assert!(
            !recovery.needs_recovery[0].retryable,
            "未知内容必须暂停启动：{:?}",
            recovery.needs_recovery[0]
        );
        assert!(
            recovery.needs_recovery[0].reason.contains("不一致")
                || recovery.needs_recovery[0].reason.contains("无法验证"),
            "{}",
            recovery.needs_recovery[0].reason
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            tampered,
            "不得覆盖后续无关修改"
        );
        // 数据库与组表都保持原样，未完成操作保留待人工处理。
        assert_eq!(env.baseline_files(), baselines_before);
        assert_eq!(pair_snapshot(&env), pairs_before);
        assert_eq!(
            session_link::pending_operations(&env.paths, WbVariant::Cn).len(),
            1
        );
        // 编排层据此暂停切换与启动 App。
        assert!(crate::modules::switch::recovery_blocks_startup(&recovery));
    }

    /// 目标行更新时间被其它程序改动 → 停止恢复（不覆盖未知修改）。
    #[test]
    fn sync_recovery_stops_when_target_row_changed_after_write() {
        let env = ready_env("sync-recovery-row-drift");
        let (target_id, _, written) = interrupted_sync_scene(&env);

        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = 987654321 WHERE id = ?1",
            [&target_id],
        )
        .unwrap();
        drop(conn);

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(recovery.recovered.is_empty(), "{:?}", recovery.recovered);
        assert_eq!(recovery.needs_recovery.len(), 1);
        assert!(!recovery.needs_recovery[0].retryable);
        assert!(
            recovery.needs_recovery[0].reason.contains("更新时间"),
            "{}",
            recovery.needs_recovery[0].reason
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            written
        );
    }

    /// 目标行归属不符（被改到别的账号）→ 停止恢复，不报成功。
    #[test]
    fn sync_recovery_stops_when_target_row_owner_changed() {
        let env = ready_env("sync-recovery-owner");
        let (target_id, _, written) = interrupted_sync_scene(&env);

        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET user_id = 'uid-x' WHERE id = ?1",
            [&target_id],
        )
        .unwrap();
        drop(conn);

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert!(recovery.recovered.is_empty(), "{:?}", recovery.recovered);
        assert_eq!(recovery.needs_recovery.len(), 1);
        assert!(!recovery.needs_recovery[0].retryable);
        assert!(
            recovery.needs_recovery[0].reason.contains("归属"),
            "{}",
            recovery.needs_recovery[0].reason
        );
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            written,
            "归属异常时不得继续改写内容"
        );
        assert_eq!(
            session_link::pending_operations(&env.paths, WbVariant::Cn).len(),
            1
        );
    }

    /// 恢复期间数据库不可读：不覆盖正文/行/基线，保留 pending 并暂停启动；解除后恢复成功。
    #[test]
    fn sync_recovery_preserves_committed_state_on_database_read_failure() {
        for fault in ["open", "schema", "query"] {
            let env = ready_env(&format!("sync-read-failure-{fault}"));
            // 写入阶段被故障中断：操作未完成、临时备份按契约保留（只在完成后才清理）。
            let (target_id, _, _) = interrupted_sync_scene(&env);
            let operation = session_link::pending_operations(&env.paths, WbVariant::Cn)
                .into_iter()
                .next()
                .expect("故障后必须保留未完成操作");
            let body_before = body_bytes(&env, &target_id);
            let row_before = session_row(&env, &target_id);
            let pairs_before = pair_snapshot(&env);
            let baselines_before = env.baseline_files();
            let db = env.paths.workbuddy_db();
            let saved_db = db.with_extension("saved");
            match fault {
                "open" => std::fs::rename(&db, &saved_db).unwrap(),
                "schema" => Connection::open(&db)
                    .unwrap()
                    .execute_batch("ALTER TABLE sessions RENAME TO unavailable_sessions")
                    .unwrap(),
                "query" => Connection::open(&db)
                    .unwrap()
                    .execute_batch(
                        "ALTER TABLE sessions RENAME COLUMN updated_at TO unavailable_updated_at",
                    )
                    .unwrap(),
                _ => unreachable!(),
            }
            let existing_db = if fault == "open" { &saved_db } else { &db };
            let db_before = std::fs::read(existing_db).unwrap();
            let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
            assert!(recovery.recovered.is_empty(), "{fault}");
            assert!(recovery.abandoned.is_empty(), "{fault}");
            assert_eq!(recovery.needs_recovery.len(), 1, "{fault}");
            assert!(
                crate::modules::switch::recovery_blocks_startup(&recovery),
                "{fault}"
            );
            assert_eq!(body_bytes(&env, &target_id), body_before, "{fault}");
            assert_eq!(std::fs::read(existing_db).unwrap(), db_before, "{fault}");
            assert_eq!(pair_snapshot(&env), pairs_before, "{fault}");
            assert_eq!(env.baseline_files(), baselines_before, "{fault}");
            let pending = session_link::pending_operations(&env.paths, WbVariant::Cn);
            assert_eq!(pending.len(), 1, "{fault}");
            assert_eq!(pending[0].operation_id, operation.operation_id);
            assert_eq!(pending[0].phase, OpPhase::BodyWritten);

            match fault {
                "open" => std::fs::rename(&saved_db, &db).unwrap(),
                "schema" => Connection::open(&db)
                    .unwrap()
                    .execute_batch("ALTER TABLE unavailable_sessions RENAME TO sessions")
                    .unwrap(),
                "query" => Connection::open(&db)
                    .unwrap()
                    .execute_batch(
                        "ALTER TABLE sessions RENAME COLUMN unavailable_updated_at TO updated_at",
                    )
                    .unwrap(),
                _ => unreachable!(),
            }
            let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
            assert!(
                recovery.is_clean(),
                "{fault}: {:?}",
                recovery.needs_recovery
            );
            assert_eq!(recovery.recovered, vec![operation.operation_id.clone()]);
            assert_eq!(body_bytes(&env, &target_id), body_before);
            // 补完之后才更新目标行与基线：只改 updated_at，身份/标题不动。
            let row_after = session_row(&env, &target_id);
            assert_eq!(row_after.0, row_before.0);
            assert_eq!(row_after.1, row_before.1);
            assert_eq!(row_after.2, row_before.2);
            assert!(row_after.3.unwrap() > row_before.3.unwrap());
            assert_eq!(row_after.4, None);
            assert_eq!(env.baseline_files(), baselines_before + 1);
            assert_ne!(pair_snapshot(&env), pairs_before);
            assert_eq!(sync_operations(&env)[0].phase, OpPhase::Completed);
            assert!(
                !session_backup::transaction_dir(
                    &env.paths,
                    WbVariant::Cn,
                    &operation.operation_id
                )
                .unwrap()
                .exists(),
                "{fault}: 恢复完成后必须回收临时备份"
            );
        }
    }

    #[test]
    fn sync_row_snapshot_supports_legacy_schema_and_distinguishes_missing_row() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(read_session_row(&conn, "session").is_err());
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT, user_id TEXT, title TEXT, updated_at INTEGER, deleted_at INTEGER);
             INSERT INTO sessions VALUES ('session', 'uid', 'title', 123, NULL);",
        ).unwrap();
        let row = read_session_row(&conn, "session").unwrap().unwrap();
        assert_eq!(row.custom_title, None);
        assert_eq!(row.title.as_deref(), Some("title"));
        assert!(read_session_row(&conn, "absent").unwrap().is_none());
        conn.execute_batch("UPDATE sessions SET updated_at = 'invalid'")
            .unwrap();
        assert!(read_session_row(&conn, "session").is_err());
    }

    /// 目标行已不存在（硬删除）→ 无法补完，按备份回滚正文，不留无行的半成品。
    #[test]
    fn sync_recovery_rolls_back_body_when_target_row_is_gone() {
        let env = ready_env("sync-recovery-row-gone");
        let (target_id, target_before, _) = interrupted_sync_scene(&env);
        let baselines_before = env.baseline_files();
        env.delete_row(&target_id);

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert_eq!(recovery.abandoned.len(), 1, "{:?}", recovery.recovered);
        assert!(recovery.is_clean(), "{:?}", recovery.needs_recovery);
        assert_eq!(
            body_bytes(&env, &target_id),
            target_before,
            "必须按备份回滚成覆盖前内容"
        );
        assert_eq!(env.baseline_files(), baselines_before, "基线未被提交");
        assert!(session_link::pending_operations(&env.paths, WbVariant::Cn).is_empty());
        assert!(try_session_row(&env, &target_id).is_none(), "目标行已删除");
    }

    /// 目标行被软删除（deleted_at 非空）→ 同样无法补完，回滚正文并记为放弃。
    #[test]
    fn sync_recovery_rolls_back_body_when_target_row_is_soft_deleted() {
        let env = ready_env("sync-recovery-row-deleted");
        let (target_id, target_before, _) = interrupted_sync_scene(&env);
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "UPDATE sessions SET deleted_at = 123456 WHERE id = ?1",
            [&target_id],
        )
        .unwrap();
        drop(conn);

        let recovery = recover_pending_session_operations_at(&env.paths, WbVariant::Cn);
        assert_eq!(recovery.abandoned.len(), 1, "{:?}", recovery.recovered);
        assert!(recovery.is_clean(), "{:?}", recovery.needs_recovery);
        assert_eq!(body_bytes(&env, &target_id), target_before);
        assert!(session_link::pending_operations(&env.paths, WbVariant::Cn).is_empty());
    }

    /// 同一请求里重复勾选同一组：第一次写入后凭据即失效，第二次跳过，不重复写。
    #[test]
    fn sync_duplicate_selection_in_one_request_writes_once() {
        let env = ready_env("sync-duplicate-selection");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let baselines_before = env.baseline_files();
        let expected = incoming_text(&env, "sess-1", &target_id);
        let once = selection(&group_id, &token, SyncMode::FastForward);

        let report = sync(&env, "uid-b", &[once.clone(), once]);
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        let skipped = report["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1, "{report}");
        assert_eq!(skipped[0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            expected
        );
        assert_eq!(env.body_files().len(), 2, "不得产生第二份副本");
        assert_eq!(env.baseline_files(), baselines_before + 1, "只提交一次基线");
        assert_eq!(sync_operations(&env).len(), 1, "只留一条同步操作");
    }

    /// 同一目标仍有未完成写入时，本轮不得再写一次（等恢复完成）。
    #[test]
    fn sync_refuses_to_write_while_previous_operation_is_unfinished() {
        let env = ready_env("sync-pending-guard");
        let (group_id, token, target_id) = fast_forward_scene(&env);
        let target_before = body_bytes(&env, &target_id);
        let paths = env.paths();
        // 手工留下一条缺备份清单的未完成同步操作：恢复无法完成它，只能保持 pending。
        session_link::save_operation(
            &paths,
            &Operation {
                version: OPERATION_VERSION,
                operation_id: "op-sync-pending".to_string(),
                kind: OPERATION_KIND_SYNC.to_string(),
                variant: WbVariant::Cn,
                source_variant: None,
                group_id: group_id.clone(),
                source: OperationMember {
                    account_id: None,
                    uid: "uid-a".to_string(),
                    session_id: "sess-1".to_string(),
                },
                target: OperationMember {
                    account_id: None,
                    uid: "uid-b".to_string(),
                    session_id: target_id.clone(),
                },
                expected_content_digest: "d".to_string(),
                expected_record_count: 1,
                phase: OpPhase::Prepared,
                backup: None,
                lifecycle_version: None,
                cleanup_state: None,
                last_error: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();

        let report = sync(
            &env,
            "uid-b",
            &[selection(&group_id, &token, SyncMode::FastForward)],
        );
        assert!(report["synced"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["needsRecovery"], true, "{report}");
        assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("上一次会话保存尚未完成"), "{error}");
        assert_eq!(body_bytes(&env, &target_id), target_before);
        assert_eq!(env.body_files().len(), 2);
        assert_eq!(
            session_link::pending_operations(&paths, WbVariant::Cn).len(),
            1,
            "旧操作保留待恢复，不新增待写操作"
        );
    }

    /// 同步的备份清单被删/为空目录时不得盲目重放。
    #[test]
    fn sync_recovery_stops_without_usable_backup() {
        let env = ready_env("sync-recovery-no-backup");
        let (group_id, _token, target_id) = fast_forward_scene(&env);
        let target_before = body_bytes(&env, &target_id);
        let paths = env.paths();
        session_link::save_operation(
            &paths,
            &Operation {
                version: OPERATION_VERSION,
                operation_id: "op-sync-no-backup".to_string(),
                kind: OPERATION_KIND_SYNC.to_string(),
                variant: WbVariant::Cn,
                source_variant: None,
                group_id,
                source: OperationMember {
                    account_id: None,
                    uid: "uid-a".to_string(),
                    session_id: "sess-1".to_string(),
                },
                target: OperationMember {
                    account_id: None,
                    uid: "uid-b".to_string(),
                    session_id: target_id.clone(),
                },
                expected_content_digest: "d".to_string(),
                expected_record_count: 1,
                phase: OpPhase::BodyWritten,
                backup: Some(
                    env.paths
                        .backup_root()
                        .join("session-transactions/cn/does-not-exist/manifest.json")
                        .to_string_lossy()
                        .to_string(),
                ),
                lifecycle_version: Some(OPERATION_LIFECYCLE_VERSION),
                cleanup_state: None,
                last_error: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .unwrap();

        let recovery = recover_pending_session_operations_at(&paths, WbVariant::Cn);
        assert!(recovery.recovered.is_empty(), "{:?}", recovery.recovered);
        assert_eq!(recovery.needs_recovery.len(), 1);
        assert!(!recovery.needs_recovery[0].retryable);
        assert!(
            recovery.needs_recovery[0].reason.contains("备份"),
            "{}",
            recovery.needs_recovery[0].reason
        );
        assert_eq!(body_bytes(&env, &target_id), target_before);
    }
}
