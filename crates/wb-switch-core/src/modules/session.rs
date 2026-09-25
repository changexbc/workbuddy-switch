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
use crate::modules::config::{now_ms, now_secs, store_dir};
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
/// 两个宿主（WorkBuddy 桌面版与 VS Code CodeBuddy 插件）共用同一份内核
/// （[`crate::modules::session_link`]），但各自的关联关系互不可见：
/// 同一工具存储根下按命名空间取不同的文件名与目录名，避免互相污染。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkNamespace {
    /// WorkBuddy 桌面版（默认，路径与改造前逐字相同）。
    #[default]
    WorkBuddy,
    /// VS Code CodeBuddy 插件的会话（独立文件名与目录）。
    VscodeExt,
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

    /// 关联组主表：WorkBuddy 与 VS Code 插件各一份，互不可见（design §2）。
    pub fn session_links_file(&self) -> PathBuf {
        match self.link_namespace {
            LinkNamespace::WorkBuddy => self.store_root.join("session_links.json"),
            LinkNamespace::VscodeExt => self.store_root.join("vscode_session_links.json"),
        }
    }

    /// 关联存储目录（基线 / 凭据 / 操作日志的父目录）。
    pub fn session_links_dir(&self) -> PathBuf {
        match self.link_namespace {
            LinkNamespace::WorkBuddy => self.store_root.join("session-links"),
            LinkNamespace::VscodeExt => self.store_root.join("vscode-session-links"),
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

    /// 关联存储的短时全局锁文件：与主表同生命周期，按命名空间分开。
    pub fn link_store_lock_file(&self) -> PathBuf {
        match self.link_namespace {
            LinkNamespace::WorkBuddy => self.locks_dir().join("session-links.lock"),
            LinkNamespace::VscodeExt => self.locks_dir().join("vscode-session-links.lock"),
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

/// WorkBuddy 正在运行时的统一错误文案：会话写入必须在 App 停止写入之后。
pub const SESSION_COPY_APP_RUNNING: &str =
    "WorkBuddy 正在运行，已阻止修改会话数据；请先退出 WorkBuddy 后重试";

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
fn find_project_jsonl(paths: &SessionPaths, cid: &str) -> Option<PathBuf> {
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

/// 在 workbuddy.db 中把源会话行复制为新 id（动态列，覆盖 id/user_id/时间戳）。
///
/// 与旧实现不同：db/表/源行缺失都显式返回，不再静默 Ok。
fn insert_session_copy(
    paths: &SessionPaths,
    new_cid: &str,
    cid: &str,
    source_uid: &str,
    target_uid: &str,
) -> Result<DbCopyOutcome, String> {
    let db_path = paths.workbuddy_db();
    if !db_path.is_file() {
        return Ok(DbCopyOutcome::NoDb);
    }
    let Some(conn) = open_db(&db_path, false) else {
        return Err("会话数据无法打开".to_string());
    };
    if !table_exists(&conn, "sessions") {
        return Ok(DbCopyOutcome::NoSessionsTable);
    }
    // 写事务的提交必须可靠持久：在本次实际写连接上确认 synchronous ≥ FULL。
    session_backup::ensure_full_synchronous(&conn)?;
    let mut src_stmt = conn
        .prepare("SELECT * FROM sessions WHERE id = ?1 AND user_id = ?2")
        .map_err(|e| e.to_string())?;
    let cols: Vec<String> = src_stmt
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut rows = src_stmt
        .query(rusqlite::params![cid, source_uid])
        .map_err(|e| e.to_string())?;
    let Some(row) = rows.next().map_err(|e| e.to_string())? else {
        return Ok(DbCopyOutcome::SourceRowMissing);
    };
    let mut vals: Vec<rusqlite::types::Value> = Vec::with_capacity(cols.len());
    for (i, col) in cols.iter().enumerate() {
        let v = row
            .get::<_, rusqlite::types::Value>(i)
            .unwrap_or(rusqlite::types::Value::Null);
        if col == "cwd" {
            if let rusqlite::types::Value::Text(ref path) = v {
                if is_claw_workspace(path) {
                    return Err("Claw 工作区绑定当前账号渠道，不支持复制".to_string());
                }
            }
        }
        match col.as_str() {
            "id" => vals.push(rusqlite::types::Value::Text(new_cid.to_string())),
            "user_id" => vals.push(rusqlite::types::Value::Text(target_uid.to_string())),
            // 继承源会话的时间戳，而不是落成复制时刻：
            // 客户端会话列表按 created_at/updated_at 降序排列，写当前时间会把批量复制的
            // 历史会话顶到列表最上方，把用户正在进行的对话挤下去。源值为空（脏数据）时
            // 才回退到当前时刻，保证排序列始终可比较。
            "created_at" | "updated_at" => {
                let inherited = matches!(&v, rusqlite::types::Value::Integer(ts) if *ts > 0);
                vals.push(if inherited {
                    v
                } else {
                    rusqlite::types::Value::Integer(now_ms())
                });
            }
            "deleted_at" => vals.push(rusqlite::types::Value::Null),
            _ => vals.push(v),
        }
    }
    drop(rows);
    drop(src_stmt);

    let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let colnames = cols.join(", ");
    // 用 INSERT 而不是 INSERT OR REPLACE：新 UUID 撞库时宁可失败，也不能悄悄覆盖既有会话。
    let sql = format!("INSERT INTO sessions ({colnames}) VALUES ({placeholders})");
    let params: Vec<&rusqlite::types::Value> = vals.iter().collect();
    conn.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(|e| format!("会话记录保存失败：{e}"))?;
    Ok(DbCopyOutcome::Inserted)
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
#[derive(Debug, Clone)]
enum MappingOutcome {
    Registered,
    Unavailable(String),
}

/// 把新会话注册进 edge_sync_mapping（云端归属关键）。沿用既有登记方式，不扩大作用。
fn register_edge_sync_mapping(
    paths: &SessionPaths,
    variant: WbVariant,
    new_cid: &str,
    target_uid: &str,
) -> MappingOutcome {
    let db_path = paths.edge_sync_db(variant);
    if !db_path.is_file() {
        return MappingOutcome::Unavailable(format!(
            "云端映射库 {} 不存在",
            db_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        ));
    }
    let Some(conn) = open_db(&db_path, false) else {
        return MappingOutcome::Unavailable("云端映射库无法打开".to_string());
    };
    if !table_exists(&conn, "edge_sync_mapping") {
        return MappingOutcome::Unavailable("云端映射库缺少 edge_sync_mapping 表".to_string());
    }
    if let Err(reason) = session_backup::ensure_full_synchronous(&conn) {
        return MappingOutcome::Unavailable(reason);
    }
    let result = conn.execute(
        "INSERT OR REPLACE INTO edge_sync_mapping \
         (session_id, conversation_id, msg_channel, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            new_cid,
            new_cid,
            format!("convmsg:{target_uid}"),
            now_secs()
        ],
    );
    match result {
        Ok(_) => MappingOutcome::Registered,
        Err(error) => MappingOutcome::Unavailable(format!("云端映射登记失败：{error}")),
    }
}

/// 目标会话是否已按预期登记在云端映射表（恢复跳过重放时只核验，不 INSERT）。
fn mapping_row_matches(
    paths: &SessionPaths,
    variant: WbVariant,
    session_id: &str,
    target_uid: &str,
) -> bool {
    let db_path = paths.edge_sync_db(variant);
    if !db_path.is_file() {
        return false;
    }
    let Some(conn) = open_db(&db_path, true) else {
        return false;
    };
    if !table_exists(&conn, "edge_sync_mapping") {
        return false;
    }
    let expected = format!("convmsg:{target_uid}");
    conn.query_row(
        "SELECT msg_channel FROM edge_sync_mapping WHERE session_id = ?1",
        [session_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .is_some_and(|channel| channel == expected)
}

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

/// 可注入路径与「App 是否运行」探针的复制入口（单测注入临时目录与假探针，不触碰
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
    if is_app_running(variant) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    // 探测只对国际版生效（design D6 针对的是国际版数据根不同构）。国内版数据根与
    // 改造前同构，保留改造前的路径与返回结构，不让国内版看到「暂不支持」类新文案。
    if variant == WbVariant::Ai && !session_copy_supported_at(&paths.data_root) {
        return Err(format!(
            "{SESSION_COPY_UNSUPPORTED}（档位 {}）",
            variant.as_str()
        ));
    }
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() {
        return Err("目标账号缺少 uid，无法复制会话".to_string());
    }
    let source_uid = current_user_uid_at(&paths.auth_file)
        .ok_or_else(|| "未读取到本机登录态，无法确定来源账号".to_string())?;
    if source_uid == target_uid {
        return Err("当前账号与目标账号相同，无需复制会话".to_string());
    }

    // 档位操作锁覆盖「恢复 → 查询关联 → 写副本 → 提交关联」全过程，
    // 并发请求与中断重试因此不会各自写出第二个副本。
    let _ops_lock = session_link::try_acquire_variant_ops_lock(paths, variant)?;
    // 复查：锁前未运行、拿锁后 App 已被启动的情况在这里被拦住，不写入任何产物。
    if is_app_running(variant) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
    }
    let recovery = recover_pending_session_operations_at(paths, variant);
    let pending = session_link::pending_operations(paths, variant);

    let context = CopyContext {
        paths,
        variant,
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
    let unfinished_after = session_link::pending_operations(paths, variant);
    let unusable = !recovery.is_clean() || !unfinished_after.is_empty();
    let mut report = json!({
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "copied": copied,
        "alreadyLinked": already_linked,
    });
    if !errors.is_empty() {
        report["errors"] = json!(errors);
    }
    if unusable {
        report["needsRecovery"] = json!(true);
    }
    // 本轮复制之后再扫一遍：当前项的清理失败/保护残留必须出现在报告里，
    // 不能只用请求开始时的维护快照（否则成功项 pending 只在 copied[] 上）。
    // 维护可能补清成功：成功项上的 pending 路径必须改写成 null，避免虚假可还原位置。
    report["temporaryFiles"] = json!(session_backup::maintain(paths, variant));
    if let Some(items) = report.get_mut("copied").and_then(Value::as_array_mut) {
        reconcile_reported_cleanup(items);
    }
    Ok(report)
}

/// 复制上下文中不变的输入（避免逐会话重复解析）。
struct CopyContext<'a> {
    paths: &'a SessionPaths,
    variant: WbVariant,
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
    let Some(group) =
        session_link::find_group_for_identity(&store, context.variant, context.source_uid, cid)
    else {
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

/// 写入副本正文并做写后校验（复用同一次源快照，避免 TOCTOU）。
fn write_copy_body(
    snapshot: &ContentSnapshot,
    source_path: &Path,
    cid: &str,
    new_cid: &str,
) -> Result<PathBuf, String> {
    let dest = source_path.with_file_name(format!("{new_cid}.jsonl"));
    if dest.exists() {
        return Err("目标内容已存在同名文件，已停止复制".to_string());
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

    let Some(source_path) = find_project_jsonl(paths, cid) else {
        return Err("会话内容不存在，未复制".to_string());
    };
    let snapshot = match session_link::read_content_snapshot(&source_path, cid) {
        ContentState::Ready(snapshot) => snapshot,
        ContentState::Missing => return Err("会话内容不存在，未复制".to_string()),
        ContentState::Unavailable(reason) => {
            return Err(format!("会话内容无法验证（{reason}），未复制"));
        }
    };
    let source_owner = session_row_owner(paths, cid);
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
/// （恢复场景），直接继续数据库行与关联。
fn finish_copy_from_body(
    paths: &SessionPaths,
    variant: WbVariant,
    operation: &mut Operation,
    snapshot: &ContentSnapshot,
    body_source: Option<&Path>,
) -> Result<(), String> {
    if let Some(source_path) = body_source {
        write_copy_body(
            snapshot,
            source_path,
            &operation.source.session_id,
            &operation.target.session_id,
        )?;
        advance_operation(paths, operation, OpPhase::BodyWritten)?;
    }

    match insert_session_copy(
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
        DbCopyOutcome::NoSessionsTable => {
            return Err("会话数据缺少数据表，未复制".to_string())
        }
        DbCopyOutcome::NoDb => return Err("会话数据不存在，未复制".to_string()),
    }
    verify_session_row(paths, &operation.target.session_id, &operation.target.uid)?;
    advance_operation(paths, operation, OpPhase::DbWritten)?;

    match register_edge_sync_mapping(
        paths,
        variant,
        &operation.target.session_id,
        &operation.target.uid,
    ) {
        MappingOutcome::Registered => {}
        MappingOutcome::Unavailable(reason) => return Err(reason),
    }
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
fn member_content_state(paths: &SessionPaths, session_id: &str) -> ContentState {
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
fn live_preview_binding(
    group: &LinkGroup,
    source_member: &LinkMember,
    target_member: &LinkMember,
    source_content: &ContentState,
    target_content: &ContentState,
    baseline: &BaselineState,
    verdict: SyncVerdict,
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
                .map(|group| preview_group_item(paths, group, &source_uid, &target_uid))
                .collect();
            report["groups"] = json!(groups);
        }
    }
    Ok(report)
}

/// 单个关联组的预览项。
///
/// 记录数与差集只用于向用户解释；能否勾选只由判定结果决定（design §3.2）。
fn preview_group_item(
    paths: &SessionPaths,
    group: &LinkGroup,
    source_uid: &str,
    target_uid: &str,
) -> Value {
    let source_any = group.members.iter().find(|member| member.uid == source_uid);
    let target_any = group.members.iter().find(|member| member.uid == target_uid);
    // 组展示名取来源会话（同步保留目标的 sessionId、标题与自定义标题）。
    let (title, cwd) = source_any
        .and_then(|member| session_row_info(paths, &member.session_id))
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

    let baseline = session_link::load_pair_baseline(
        paths,
        group,
        &source_member.member_id,
        &target_member.member_id,
    );
    let source_content = member_content_state(paths, &source_member.session_id);
    let target_content = member_content_state(paths, &target_member.session_id);
    let rows_match =
        member_row_owned_by(paths, source_member) && member_row_owned_by(paths, target_member);
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
    let actionable = !modes.is_empty();

    let mut item = json!({
        "groupId": group.id,
        "title": title,
        "cwd": cwd,
        "verdict": decision.verdict.as_str(),
        "extraA": decision.extra_a,
        "extraB": decision.extra_b,
        "common": decision.common,
        "defaultChecked": decision.default_checked,
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
    if actionable {
        let binding = live_preview_binding(
            group,
            source_member,
            target_member,
            &source_content,
            &target_content,
            &baseline,
            decision.verdict,
        );
        match session_link::save_preview_token(paths, binding) {
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
    let variant = context.variant;
    let (source_uid, target_uid) = (context.source_uid, context.target_uid);
    // 凭据必须是我们服务端保存过的：伪造的 id 读不到，直接拒绝。
    let Some(token) = session_link::load_preview_token(paths, &selection.preview_token) else {
        return SyncItemOutcome::Rejected {
            message: "检查结果不存在或已失效，请重新检查后再操作".to_string(),
        };
    };
    let binding = &token.binding;
    if binding.variant != variant || binding.group_id != selection.group_id {
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
    let Some(group) = store
        .groups
        .iter()
        .find(|group| group.id == selection.group_id && group.variant == variant)
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
    if !member_row_owned_by(paths, source_member) || !member_row_owned_by(paths, target_member) {
        return skip("会话记录缺失或归属异常，检查结果已失效".to_string());
    }
    // 重新加载正文、基线与判定，再与凭据逐项核对；任一变化都跳过（含显式覆盖）。
    let source_content = member_content_state(paths, &source_member.session_id);
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
    );
    let mismatches = session_link::verify_preview(&token, &live);
    if !mismatches.is_empty() {
        return skip(format!("检查结果已失效：{}", mismatches.join("；")));
    }
    // 判定已按当前内容重算：mode 必须仍然成立，unknown 不得被覆盖绕过。
    if !decision.verdict.allows(selection.mode) {
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
    // 目标正文 = 来源正文，只把本副本 sessionId 换成目标 sessionId（保留目标 SID）。
    let incoming_text = source_snapshot
        .text
        .replace(&source_member.session_id, &target_member.session_id);
    let incoming = match session_link::normalize_jsonl(&incoming_text, &target_member.session_id) {
        Ok(normalized) => normalized,
        Err(reason) => {
            return SyncItemOutcome::Rejected {
                message: format!("目标内容无法按来源内容生成（{reason}），已拒绝"),
            }
        }
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
struct SyncContext<'a> {
    paths: &'a SessionPaths,
    variant: WbVariant,
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
    // 与复制同一条生命周期保护：会话写入必须发生在 App 停止写入之后。
    if is_app_running(variant) {
        return Err(SESSION_COPY_APP_RUNNING.to_string());
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

    // 档位操作锁与复制共用：预览后的校验与写入都不得与并发复制交错。
    let _ops_lock = session_link::try_acquire_variant_ops_lock(paths, variant)?;
    if is_app_running(variant) {
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
    new_baseline_ref: String,
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
    let sql = if columns.iter().any(|column| column == "custom_title") {
        "SELECT id, user_id, title, custom_title, updated_at, deleted_at \
         FROM sessions WHERE id = ?1"
    } else {
        "SELECT id, user_id, title, NULL, updated_at, deleted_at FROM sessions WHERE id = ?1"
    };
    conn.query_row(sql, [cid], |row| {
        Ok(SyncBackupRow {
            session_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            user_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            title: row.get(2)?,
            custom_title: row.get(3)?,
            updated_at: row.get(4)?,
            deleted_at: row.get(5)?,
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
    new_baseline_ref: &str,
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
            target_row: session_row_snapshot(paths, &plan.target_member.session_id)?,
            new_updated_at,
        },
        new_baseline_ref: new_baseline_ref.to_string(),
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
/// 只改 updated_at：sessionId、标题、custom_title 必须与覆盖前一致（R4 / 验收项）。
fn update_target_session_row(
    paths: &SessionPaths,
    target: &OperationMember,
    new_updated_at: i64,
    before: Option<&SyncBackupRow>,
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
    let affected = tx
        .execute(
            "UPDATE sessions SET updated_at = ?1 \
             WHERE id = ?2 AND user_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![new_updated_at, target.session_id, target.uid],
        )
        .map_err(|error| format!("目标会话记录更新失败：{error}"))?;
    if affected != 1 {
        return Err(
            "目标会话记录归属校验失败：会话不存在、不属于目标账号或已被删除，未按成功处理".to_string(),
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
                return Err("目标会话记录的归属或标题在保存期间发生变化，已回滚本次更新".to_string());
            }
            if after.updated_at != Some(new_updated_at) {
                return Err("目标会话记录更新时间未按本次保存生效，未按成功处理".to_string());
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
fn commit_sync_baseline(
    paths: &SessionPaths,
    variant: WbVariant,
    manifest: &SyncBackupManifest,
    normalized: &NormalizedContent,
) -> Result<(), String> {
    let source_member_id = manifest.source.member_id.as_str();
    let target_member_id = manifest.target.member_id.as_str();
    session_link::with_link_store_write(paths, |store| {
        let Some(group) = store
            .groups
            .iter_mut()
            .find(|group| group.id == manifest.group_id && group.variant == variant)
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
        session_link::save_baseline(paths, &manifest.new_baseline_ref, normalized)?;
        session_link::set_pair_base(
            group,
            source_member_id,
            target_member_id,
            &manifest.new_baseline_ref,
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
            if pair.baseline_ref != manifest.new_baseline_ref {
                return Err("同步记录与本次保存不一致，已停止恢复".to_string());
            }
            if session_link::load_baseline(paths, &manifest.new_baseline_ref).is_none() {
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

/// 阶段化写入：正文 → 数据库 → 组表 → completed；每步先落阶段再推进，可恢复。
///
/// 正常执行与恢复共用同一段代码：阶段只前进不回退，已越过的阶段不重放（只核验产物），
/// 因此恢复不会产生第二份写入。
fn run_sync_phases(
    paths: &SessionPaths,
    variant: WbVariant,
    operation: &mut Operation,
    backup_dir: &Path,
    manifest: &SyncBackupManifest,
    body: SyncBodyState,
) -> Result<(), String> {
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

    // 数据库：更新目标行 updated_at 是幂等操作（同一时间戳重复写等价），
    // 未命中归属/已删除时直接失败，不报成功。
    update_target_session_row(
        paths,
        &operation.target,
        manifest.db.new_updated_at,
        manifest.db.target_row.as_ref(),
    )?;
    advance_operation(paths, operation, OpPhase::DbWritten)?;

    if operation.phase < OpPhase::LinksCommitted {
        commit_sync_baseline(paths, variant, manifest, &normalized)?;
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
    let new_baseline_ref = uuid::Uuid::new_v4().to_string();
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
        &new_baseline_ref,
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
    if let Err(error) = run_sync_phases(
        paths,
        context.variant,
        &mut operation,
        &backup.dir,
        &backup.manifest,
        body,
    ) {
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
pub fn recover_pending_session_operations_at(
    paths: &SessionPaths,
    variant: WbVariant,
) -> RecoveryReport {
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
        match recover_operation(paths, variant, operation) {
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
    Ok(recover_pending_session_operations_at(&paths, variant))
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
) -> RecoverOutcome {
    if operation.kind == OPERATION_KIND_SYNC {
        return recover_sync_operation(paths, variant, operation);
    }
    recover_copy_operation(paths, variant, operation)
}

/// 恢复一次同步（design §5.6）：先校验现场，再按清单补完，绝不覆盖未知内容。
///
/// 顺序：备份必须完好 → 目标正文只能是「本次写入的」或「覆盖前的」→ 目标行归属与
/// 更新时间必须可安全识别 → 复用同一段阶段代码补完。任一项不满足即停止并上报
/// needsRecovery（阻断启动，宿主必须等待恢复一致后才能启动 App）。
fn recover_sync_operation(
    paths: &SessionPaths,
    variant: WbVariant,
    mut operation: Operation,
) -> RecoverOutcome {
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
        if matches!(body, SyncBodyState::Ours) {
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
    let before = manifest
        .db
        .target_row
        .as_ref()
        .and_then(|row| row.updated_at);
    let applied = row.updated_at == Some(manifest.db.new_updated_at);
    let untouched = before.is_some() && row.updated_at == before;
    if !applied && !untouched && before.is_some() {
        // 既不是本次写入的值、也不是覆盖前的值：被其它程序改动过，不覆盖。
        return needs(
            "目标会话记录的更新时间与本次保存及覆盖前值都不一致（可能被其它程序改动），已停止恢复"
                .to_string(),
            false,
        );
    }

    // 4) 复用同一段阶段代码补完（阶段只前进，已越过的阶段只核验不重放）。
    match run_sync_phases(paths, variant, &mut operation, &backup_dir, &manifest, body) {
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
) -> RecoverOutcome {
    let operation_id = operation.operation_id.clone();
    let needs = |reason: String, retryable: bool| RecoverOutcome::NeedsRecovery {
        id: operation_id.clone(),
        reason,
        retryable,
    };

    // 1) 目标正文：已写成则直接复用；未写成则用当前源内容补写；被改动则停止。
    let normalized = match check_target_body(paths, &operation) {
        BodyCheck::Verified(normalized) => normalized,
        BodyCheck::NeedsRecovery(reason) => return needs(reason, false),
        BodyCheck::Absent => {
            let Some(source_path) = find_project_jsonl(paths, &operation.source.session_id) else {
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

    // 3) 云端映射：已越过该阶段就不再重新登记（恢复不重放已完成的阶段）；
    // 但必须核验产物仍在，phase 写完、行却丢了时要报 needsRecovery，不能直接 Completed。
    if operation.phase < OpPhase::MappingWritten {
        match register_edge_sync_mapping(
            paths,
            variant,
            &operation.target.session_id,
            &operation.target.uid,
        ) {
            MappingOutcome::Registered => {}
            MappingOutcome::Unavailable(reason) => {
                fail_operation(paths, &mut operation, &reason);
                return needs(reason, true);
            }
        }
        if let Err(error) = advance_operation(paths, &mut operation, OpPhase::MappingWritten) {
            return needs(error, true);
        }
    } else if !mapping_row_matches(
        paths,
        variant,
        &operation.target.session_id,
        &operation.target.uid,
    ) {
        return needs("云端映射丢失或被改动，已停止恢复".to_string(), false);
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
        assert!(paths.preview_tokens_dir().ends_with("session-links/previews"));
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
        assert_eq!(at_root.session_links_file(), root.join("session_links.json"));
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
        assert_ne!(
            vscode.preview_tokens_dir(),
            workbuddy.preview_tokens_dir()
        );
        assert_ne!(vscode.operations_dir(), workbuddy.operations_dir());
        assert_ne!(
            vscode.link_store_lock_file(),
            workbuddy.link_store_lock_file()
        );
        // 默认命名空间是 WorkBuddy：`SessionPaths::for_variant` 之外的历史构造点
        // 不会因为新增字段而漂移到 VS Code 名字上。
        assert_eq!(LinkNamespace::default(), LinkNamespace::WorkBuddy);
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
    fn copy_writes_body_row_mapping_and_link_group() {
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

        // 云端映射沿用既有登记：convmsg:{target_uid}。
        let conn = Connection::open(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();
        let channel: String = conn
            .query_row(
                "SELECT msg_channel FROM edge_sync_mapping WHERE session_id = ?1",
                [&new_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(channel, "convmsg:uid-b");

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
        assert_eq!(env.body_files().len(), 4, "两条来源内容与两个复制后的内容都在");
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
                        state: MemberState::Active,
                        linked_at: 1,
                        last_synced_at: None,
                    },
                    LinkMember {
                        member_id: "m-b".to_string(),
                        account_id: Some("old-account-id".to_string()),
                        uid: "uid-b".to_string(),
                        session_id: "sess-b".to_string(),
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

    /// 映射登记失败 → 不报完整成功；修好后重试复用同一 UUID，不产生第二个副本。
    #[test]
    fn mapping_failure_keeps_pending_then_retry_reuses_same_uuid() {
        let env = ready_env("mapping");
        std::fs::remove_file(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();

        let failed = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(failed["copied"].as_array().unwrap().len(), 0);
        let error = failed["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("云端映射库"), "{error}");
        assert_eq!(failed["needsRecovery"], true);
        assert_eq!(
            env.body_files().len(),
            2,
            "内容与数据库行已写入，未按成功处理"
        );

        let pending = session_link::pending_operations(&env.paths(), WbVariant::Cn);
        assert_eq!(pending.len(), 1);
        let new_id = pending[0].target.session_id.clone();
        assert!(pending[0].phase < crate::modules::session_link::OpPhase::Completed);

        // 映射库恢复后重试：恢复流程补齐，随后报告 alreadyLinked，UUID 不变。
        env.create_edge_db(WbVariant::Cn);
        let retry = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(retry["copied"].as_array().unwrap().len(), 0);
        assert_eq!(retry["alreadyLinked"][0]["sessionId"], new_id);
        assert!(retry.get("errors").is_none());
        assert!(session_link::pending_operations(&env.paths(), WbVariant::Cn).is_empty());
        assert_eq!(env.body_files().len(), 2, "恢复不得产生第二个副本");
        assert_eq!(env.rows_for("uid-b"), vec![new_id.clone()]);

        let conn = Connection::open(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();
        let channel: String = conn
            .query_row(
                "SELECT msg_channel FROM edge_sync_mapping WHERE session_id = ?1",
                [&new_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(channel, "convmsg:uid-b");
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

    /// 拿档位锁后复查 App 是否运行：锁前未运行、拿锁后已被启动 → 拒绝且不写任何产物。
    #[test]
    fn copy_rechecks_app_running_after_acquiring_lock() {
        let env = ready_env("app-raced");
        let probes = std::cell::Cell::new(0usize);
        let lock_held_on_recheck = std::cell::Cell::new(false);
        let lock_path = env.paths.variant_ops_lock_file(WbVariant::Cn);
        let probe = |_: WbVariant| {
            let n = probes.get() + 1;
            probes.set(n);
            if n == 1 {
                false
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
        let err = copy_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &["sess-1".to_string()],
            probe,
        )
        .expect_err("拿锁后复查为运行中必须拒绝");
        assert_eq!(err, SESSION_COPY_APP_RUNNING);
        assert_eq!(probes.get(), 2, "锁前与锁后各检查一次");
        assert!(
            lock_held_on_recheck.get(),
            "复查必须发生在已经拿到档位锁之后、任何写入之前"
        );
        assert_eq!(env.body_files().len(), 1, "不得写入复制后的内容");
        assert_eq!(env.rows_for("uid-b").len(), 0, "不得写入数据库行");
        assert!(
            !env.paths.session_links_file().exists(),
            "不得初始化同步记录"
        );
        assert!(session_link::pending_operations(&env.paths(), WbVariant::Cn).is_empty());

        // 锁已释放：探针改回「未运行」后复制照常成功。
        let report = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
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
        std::fs::remove_file(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();
        let failed = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(failed["needsRecovery"], true);
        let new_id = session_link::pending_operations(&env.paths(), WbVariant::Cn)[0]
            .target
            .session_id
            .clone();

        // 中间产物被其它程序改动 → 停止恢复，不覆盖。
        let tampered = format!(
            "{}\n",
            json!({"type": "user", "sessionId": new_id, "text": "别人改的"})
        );
        std::fs::write(env.body_path(&new_id), &tampered).unwrap();

        let report = recover_pending_session_operations_at(&env.paths(), WbVariant::Cn);
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
        env.create_edge_db(WbVariant::Cn);
        let again = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(again["copied"].as_array().unwrap().len(), 0);
        assert_eq!(again["errors"].as_array().unwrap().len(), 1);
        assert!(again["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("上一次复制尚未完成"));
        assert_eq!(env.body_files().len(), 2, "不得产生第二个副本");
    }

    #[test]
    fn retry_after_partial_copy_does_not_duplicate() {
        let env = ready_env("partial-resume");
        std::fs::remove_file(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();
        copy(&env, "uid-b", &["sess-1"]);
        let body_count_after_failure = env.body_files().len();

        // 未修复映射库就再次请求：不得新建副本。
        let again = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(again["copied"].as_array().unwrap().len(), 0);
        assert_eq!(env.body_files().len(), body_count_after_failure);

        // 修好后恢复完成。
        env.create_edge_db(WbVariant::Cn);
        let retry = copy(&env, "uid-b", &["sess-1"]);
        assert_eq!(retry["alreadyLinked"].as_array().unwrap().len(), 1);
        assert_eq!(env.body_files().len(), body_count_after_failure);
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
        assert_eq!(env.mapping_rows(), 1, "不得重复登记云端映射");
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

    /// phase 已越过 MappingWritten，但映射行被删：不得跳过核验后标 Completed。
    #[test]
    fn recovery_stops_when_mapping_row_is_missing() {
        let env = ready_env("recover-mapping-gone");
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

        let conn = Connection::open(paths.edge_sync_db(WbVariant::Cn)).unwrap();
        conn.execute(
            "DELETE FROM edge_sync_mapping WHERE session_id = ?1",
            [&new_id],
        )
        .unwrap();
        drop(conn);

        let revision_before = env.store().revision;
        let report = recover_pending_session_operations_at(&paths, WbVariant::Cn);
        assert!(report.recovered.is_empty(), "{:?}", report.recovered);
        assert_eq!(report.needs_recovery.len(), 1);
        assert!(!report.needs_recovery[0].retryable);
        assert!(
            report.needs_recovery[0].reason.contains("云端映射"),
            "{}",
            report.needs_recovery[0].reason
        );
        assert_eq!(env.store().revision, revision_before, "不得重写同步记录");
        assert_eq!(env.mapping_rows(), 0, "不得悄悄补登记映射");
        assert_eq!(
            session_link::pending_operations(&paths, WbVariant::Cn).len(),
            1
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

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_switch_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
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

        let outcome =
            insert_session_copy(&env.paths(), "new-uuid-1", "src-1", "uid-a", "uid-b").unwrap();
        assert_eq!(outcome, DbCopyOutcome::Inserted);

        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        let (id, user_id, title, deleted_at, is_playground): (String, String, String, Option<i64>, i64) =
            conn.query_row(
                "SELECT id, user_id, title, deleted_at, is_playground FROM sessions WHERE id = 'new-uuid-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(id, "new-uuid-1");
        assert_eq!(user_id, "uid-b");
        assert_eq!(title, "旧标题"); // 普通列原样保留
        assert_eq!(deleted_at, None);
        assert_eq!(is_playground, 0);
    }

    /// 回归 #103：副本若落成复制时刻的时间戳，批量复制的历史会话会被顶到列表最上方，
    /// 把用户正在进行的对话挤下去，看起来像原有对话被覆盖或丢失。
    #[test]
    fn insert_session_copy_inherits_source_timestamps() {
        let env = ready_env("insert-timestamps");
        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, created_at, updated_at, deleted_at, is_playground)
             VALUES ('src-1', 'uid-a', '旧标题', '/ws', 1000, 2000, NULL, 0)",
            [],
        )
        .unwrap();
        // 源行时间戳为空（脏数据）：排序列不能落空，回退到复制时刻
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, created_at, updated_at, deleted_at, is_playground)
             VALUES ('src-null', 'uid-a', '空时间戳', '/ws', NULL, NULL, NULL, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            insert_session_copy(&env.paths(), "new-1", "src-1", "uid-a", "uid-b").unwrap(),
            DbCopyOutcome::Inserted
        );
        let before = now_ms();
        assert_eq!(
            insert_session_copy(&env.paths(), "new-2", "src-null", "uid-a", "uid-b").unwrap(),
            DbCopyOutcome::Inserted
        );
        let after = now_ms();

        let conn = Connection::open(env.paths.workbuddy_db()).unwrap();
        let inherited: (i64, i64) = conn
            .query_row(
                "SELECT created_at, updated_at FROM sessions WHERE id = 'new-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            inherited,
            (1000, 2000),
            "副本必须继承源会话的时间戳，而不是写成复制时刻"
        );

        let fallback: (i64, i64) = conn
            .query_row(
                "SELECT created_at, updated_at FROM sessions WHERE id = 'new-2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            fallback.0 >= before && fallback.0 <= after,
            "源时间戳为空时回退到复制时刻，实际 {}",
            fallback.0
        );
        assert!(
            fallback.1 >= before && fallback.1 <= after,
            "源时间戳为空时回退到复制时刻，实际 {}",
            fallback.1
        );
    }

    #[test]
    fn insert_session_copy_reports_missing_source_and_db() {
        let env = ready_env("insert-missing");
        assert_eq!(
            insert_session_copy(&env.paths(), "new-1", "missing", "uid-a", "uid-b").unwrap(),
            DbCopyOutcome::SourceRowMissing,
            "源行缺失必须显式上报，不能当成功（旧实现的假成功）"
        );

        std::fs::remove_file(env.paths.workbuddy_db()).unwrap();
        assert_eq!(
            insert_session_copy(&env.paths(), "new-1", "sess-1", "uid-a", "uid-b").unwrap(),
            DbCopyOutcome::NoDb
        );
    }

    #[test]
    fn register_edge_sync_mapping_reports_unavailable_reasons() {
        let env = ready_env("edge-outcome");
        // 缺表。
        let db = temp_db("edge-no-table");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        drop(conn);
        let paths = SessionPaths {
            store_root: env.root.join("s2"),
            data_root: temp_db("edge-root"),
            auth_file: env.root.join("auth2.info"),
            link_namespace: LinkNamespace::WorkBuddy,
        };
        std::fs::create_dir_all(&paths.data_root).unwrap();
        std::fs::copy(&db, paths.edge_sync_db(WbVariant::Cn)).unwrap();
        assert!(matches!(
            register_edge_sync_mapping(&paths, WbVariant::Cn, "new-1", "uid-b"),
            MappingOutcome::Unavailable(_)
        ));
        let _ = std::fs::remove_file(&db);

        // 正常登记。
        assert!(matches!(
            register_edge_sync_mapping(&env.paths(), WbVariant::Cn, "new-1", "uid-b"),
            MappingOutcome::Registered
        ));
        let conn = Connection::open(env.paths.edge_sync_db(WbVariant::Cn)).unwrap();
        let (sid, cid, channel): (String, String, String) = conn
            .query_row(
                "SELECT session_id, conversation_id, msg_channel FROM edge_sync_mapping",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(sid, "new-1");
        assert_eq!(cid, "new-1");
        assert_eq!(channel, "convmsg:uid-b");
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

    /// 仅目标变化 → ahead：不可勾选、不发凭据，目标正文不变。
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
        assert!(group.get("previewToken").is_none(), "不可勾选的组不发凭据");
        assert!(group["reason"].as_str().unwrap().contains("只有目标账号新增"));

        // 预览不写目标：正文与关联版本都不变。
        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
        );
        assert_eq!(env.store().revision, revision_before);
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
            group["reason"].as_str().unwrap().contains("对应的会话已失效"),
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
            skipped["message"].as_str().unwrap().contains("当前账号的内容已变化"),
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
                skipped["message"].as_str().unwrap().contains("目标账号的内容已变化"),
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

        // App 运行中 → 拒绝（与复制同一条生命周期保护），不写任何东西。
        let err = sync_sessions_for_switch_at(
            &env.paths(),
            WbVariant::Cn,
            &env.target("uid-b"),
            &one,
            |_| true,
        )
        .unwrap_err();
        assert_eq!(err, SESSION_COPY_APP_RUNNING);

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

        assert_eq!(
            std::fs::read_to_string(env.body_path(&target_id)).unwrap(),
            target_body_before
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
            pair_ref_of(&group_snapshot(&env, &group_id), "uid-a", "uid-b"),
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
            pair_ref_of(&group, "uid-a", "uid-b"),
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
            pair_ref_of(&group, "uid-a", "uid-b"),
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
