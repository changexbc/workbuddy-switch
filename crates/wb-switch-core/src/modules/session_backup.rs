//! 会话临时备份的生命周期：成功清理、未完成保护（design §2–§6）。
//!
//! 备份只用于操作安全与中断恢复，不做长期留档：操作可靠完成后立即回收本次
//! 专属临时目录；未完成（运行中/失败/中断/待恢复/状态不可验证）时保护材料。
//!
//! 三条硬约束：
//! - 删除授权来自「可靠落盘的业务完成状态」，不是 UI 成功文案，也不是缺日志；
//! - 删除范围只限本版自建、归属可验证的操作专属目录（可信根 + 校验后的 UUID 推导）；
//! - 清理失败不回滚业务、不报告操作失败，保留可重试依据，下次维护入口补清理。

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::modules::config::now_ms;
use crate::modules::session::SessionPaths;
use crate::modules::session_link::{
    self, op_scan_problem_prefix, OpPhase, Operation, TemporaryFileIssue,
};
use crate::modules::variant::WbVariant;
use crate::modules::i18n;

/// 维护记录格式版本；读到其它版本一律保护现场并上报，不猜测。
pub const LIFECYCLE_VERSION: u32 = 1;
/// 本版写出的操作日志携带的生命周期标记（protected + Completed 补转只认它）。
pub const OPERATION_LIFECYCLE_VERSION: u32 = 1;
/// 操作专属临时目录根：`backups/session-transactions/{variant}/{operationId}/`。
pub const TRANSACTIONS_DIR_NAME: &str = "session-transactions";
/// 维护记录目录：`session-links/backup-lifecycle/{variant}/{operationId}.json`。
pub const LIFECYCLE_DIR_NAME: &str = "backup-lifecycle";
/// 业务日志里标记「本次备份已清理」。
pub const CLEANUP_STATE_CLEANED: &str = "cleaned";
/// 业务日志里标记「已验证安全终止」（未写业务，允许回收）。
pub const CLEANUP_STATE_SAFE_TERMINATED: &str = "safeTerminated";

/// 当前平台能否持久化目录项更新（Windows 无法对目录句柄 fsync）。
pub const DIRECTORY_SYNC_SUPPORTED: bool = cfg!(unix);

/// 生命周期状态：三种状态之间的推进都由可靠落盘保证。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupState {
    /// 记录已可靠写入，正在准备备份；此阶段契约禁止任何业务写入。
    Allocating,
    /// 备份完备、即将或已经写业务；禁止回收，直到得到可验证终态。
    Protected,
    /// 已可靠确认业务完成或已验证安全终止；允许幂等回收。
    CleanupPending,
}

impl BackupState {
    pub fn as_str(self) -> &'static str {
        match self {
            BackupState::Allocating => "allocating",
            BackupState::Protected => "protected",
            BackupState::CleanupPending => "cleanupPending",
        }
    }
}

/// 操作级维护记录：独立于历史操作日志的数量裁剪，是补清理的持久依据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupLifecycle {
    pub version: u32,
    pub operation_id: String,
    pub variant: WbVariant,
    /// `copy` | `sync`。
    pub kind: String,
    pub state: BackupState,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub last_error: Option<String>,
    /// 目标会话 id（报告里帮助用户定位）。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 目标会话标题（能取到时优先展示）。
    #[serde(default)]
    pub title: Option<String>,
}

/// 维护记录扫描结果：读取/枚举/解析失败必须显式上报，不能当成「没有」。
#[derive(Debug, Default, Clone)]
pub struct LifecycleScan {
    pub records: Vec<BackupLifecycle>,
    pub problems: Vec<String>,
}

/// 单次清理的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// 目录与维护记录都已回收。
    Cleaned,
    /// 本轮未清理成功（例如权限错误/文件被占用），保留依据待下次重试。
    Pending { reason: String },
    /// 状态不可验证或归属异常：保留材料并上报，不做删除。
    Protected { reason: String },
}

impl CleanupOutcome {
    pub fn is_cleaned(&self) -> bool {
        matches!(self, CleanupOutcome::Cleaned)
    }
}

// ---------------------------------------------------------------------------
// 会话专用可靠落盘（design §4）
// ---------------------------------------------------------------------------

/// 持久化目录项更新（父目录 fsync）。
///
/// Unix 上对目录 `fsync` 才能保证「新建/改名/删除的目录项」在断电后仍可见；
/// Windows 无法打开目录句柄，属平台限制（[`DIRECTORY_SYNC_SUPPORTED`]），
/// 不静默假装成功——限制由报告与 spec 披露。
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// 持久化单个文件的已写内容。
///
/// Unix 上只读句柄即可 `fsync`；Windows 的 `FlushFileBuffers` 要求句柄具备写权限，
/// 而 `File::open` 只申请 `GENERIC_READ`，对只读句柄 `sync_all` 必定返回
/// `ERROR_ACCESS_DENIED(5)`。备份目录里的文件都是本进程刚写出的副本，故用可写句柄打开。
#[cfg(unix)]
pub fn sync_file(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

/// [`sync_file`] 的 Windows 实现：句柄必须可写（见上方说明）。
#[cfg(not(unix))]
pub fn sync_file(path: &Path) -> std::io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()
}

/// 目标文件所在目录的持久化（不存在时视为无需处理）。
fn sync_parent_of(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => sync_dir(parent),
        _ => Ok(()),
    }
}

/// 会话专用原子写：写完临时文件先 `sync_all`，原子替换后持久化父目录。
///
/// 普通 `flush`、回读摘要或 rename 成功都不能替代持久化证明；这里只用于会话
/// 业务与生命周期文件（认证/设置沿用 `config::atomic_write` 的既有行为）。
pub fn durable_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_parent_of(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// [`durable_write`] 的字符串版本。
pub fn durable_write_str(path: &Path, content: &str) -> std::io::Result<()> {
    durable_write(path, content.as_bytes())
}

/// 递归持久化一个目录内的文件与子目录（备份目录在转 protected 之前调用）。
pub fn sync_tree(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            sync_tree(&path)?;
        } else {
            sync_file(&path)?;
        }
    }
    sync_dir(dir)
}

/// 在写连接上确认 `synchronous` 至少为 FULL（2）；不足则显式设置并核验。
///
/// 只影响当前连接，不改 WorkBuddy 的 `journal_mode`；WAL 模式下的提交持久性
/// 由 SQLite 自己按 `synchronous` 保证，不能只 fsync 主库。
pub fn ensure_full_synchronous(conn: &rusqlite::Connection) -> Result<(), String> {
    let level: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(|error| i18n::pick(format!("Could not read the database sync level: {error}"), format!("데이터베이스 동기화 수준을 읽지 못했습니다: {error}")))?;
    if level < 2 {
        conn.execute_batch("PRAGMA synchronous=FULL")
            .map_err(|error| i18n::pick(format!("Could not set the database sync level: {error}"), format!("데이터베이스 동기화 수준을 설정하지 못했습니다: {error}")))?;
        let after: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .map_err(|error| i18n::pick(format!("Could not verify the database sync level: {error}"), format!("데이터베이스 동기화 수준을 확인하지 못했습니다: {error}")))?;
        if after < 2 {
            return Err(i18n::pick(format!("The database cannot guarantee durability (synchronous={after}, FULL required); writing stopped"
            ), format!("데이터베이스가 내구성을 보장할 수 없습니다(synchronous={after}, FULL 필요). 쓰기를 중단했습니다"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 路径（可信根 + 校验后的 UUID 推导）
// ---------------------------------------------------------------------------

/// 操作专属临时目录根：`backups/session-transactions/{variant}`。
pub fn transactions_root(paths: &SessionPaths, variant: WbVariant) -> PathBuf {
    paths
        .backup_root()
        .join(TRANSACTIONS_DIR_NAME)
        .join(variant.as_str())
}

/// 一次操作的专属临时目录；`operation_id` 必须是合法 UUID。
pub fn transaction_dir(
    paths: &SessionPaths,
    variant: WbVariant,
    operation_id: &str,
) -> Result<PathBuf, String> {
    let id = validated_operation_id(operation_id)?;
    Ok(transactions_root(paths, variant).join(id))
}

/// 维护记录目录：`session-links/backup-lifecycle/{variant}`。
pub fn lifecycle_root(paths: &SessionPaths, variant: WbVariant) -> PathBuf {
    paths
        .session_links_dir()
        .join(LIFECYCLE_DIR_NAME)
        .join(variant.as_str())
}

fn lifecycle_file(
    paths: &SessionPaths,
    variant: WbVariant,
    operation_id: &str,
) -> Result<PathBuf, String> {
    let id = validated_operation_id(operation_id)?;
    Ok(lifecycle_root(paths, variant).join(format!("{id}.json")))
}

/// operationId 必须是规范 UUID：路径由它推导，不能接受任意字符串（路径穿越）。
fn validated_operation_id(operation_id: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(operation_id)
        .map(|id| id.to_string())
        .map_err(|_| i18n::pick(format!("The operation id is not a valid UUID ({operation_id}); deletion stopped"), format!("작업 식별자가 올바른 UUID가 아닙니다({operation_id}). 삭제를 중단했습니다")))
}

/// 拒绝根目录之下的符号链接跳转（含祖先路径）；不跟随、不删除链接目标。
fn ensure_no_symlink_components(root: &Path, target: &Path) -> Result<(), String> {
    let Ok(relative) = target.strip_prefix(root) else {
        return Err(i18n::pick(format!("The target path is outside the trusted root ({}); deletion stopped",
            target.display()
        ), format!("대상 경로가 신뢰할 수 있는 루트 밖에 있습니다({}). 삭제를 중단했습니다",
            target.display()
        )));
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(i18n::pick(format!("The path contains a symbolic link ({}); deletion stopped",
                    current.display()
                ), format!("경로에 심볼릭 링크가 있습니다({}). 삭제를 중단했습니다",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(i18n::pick(format!("The path state cannot be confirmed ({}: {error}); deletion stopped",
                    current.display()
                ), format!("경로 상태를 확인할 수 없습니다({}: {error}). 삭제를 중단했습니다",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 维护记录的读写与扫描
// ---------------------------------------------------------------------------

/// 可靠写入维护记录（不裁剪、不合并，一条操作一份记录）。
pub fn save_lifecycle(paths: &SessionPaths, record: &BackupLifecycle) -> Result<(), String> {
    let file = lifecycle_file(paths, record.variant, &record.operation_id)?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| i18n::pick(format!("Could not create the maintenance record directory: {error}"), format!("유지보수 기록 디렉터리를 만들지 못했습니다: {error}")))?;
    }
    let content = serde_json::to_string_pretty(record).map_err(|error| error.to_string())?;
    durable_write_str(&file, &content).map_err(|error| i18n::pick(format!("Could not write the maintenance record: {error}"), format!("유지보수 기록을 쓰지 못했습니다: {error}")))?;
    Ok(())
}

/// 读取单条维护记录；缺失返回 None，损坏/未知版本返回 Err（调用方不得当作空）。
pub fn load_lifecycle(
    paths: &SessionPaths,
    variant: WbVariant,
    operation_id: &str,
) -> Result<Option<BackupLifecycle>, String> {
    let file = lifecycle_file(paths, variant, operation_id)?;
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(i18n::pick(format!("The maintenance record cannot be read: {error}"), format!("유지보수 기록을 읽을 수 없습니다: {error}"))),
    };
    let record: BackupLifecycle =
        serde_json::from_str(&text).map_err(|error| i18n::pick(format!("The maintenance record is damaged: {error}"), format!("유지보수 기록이 손상되었습니다: {error}")))?;
    if record.version != LIFECYCLE_VERSION {
        return Err(i18n::pick(format!("Maintenance record version {} is not supported (supported: {})",
            record.version, LIFECYCLE_VERSION
        ), format!("지원하지 않는 유지보수 기록 버전입니다: {} (지원 버전: {})",
            record.version, LIFECYCLE_VERSION
        )));
    }
    Ok(Some(record))
}

/// 扫描全部维护记录；目录不可读、条目枚举失败、解析失败一律进 problems。
pub fn scan_lifecycle(paths: &SessionPaths) -> LifecycleScan {
    let mut scan = LifecycleScan::default();
    for variant in WbVariant::ALL {
        let dir = lifecycle_root(paths, variant);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                scan.problems
                    .push(i18n::pick(format!("The maintenance record directory cannot be read ({}): {error}", dir.display()), format!("유지보수 기록 디렉터리를 읽을 수 없습니다({}): {error}", dir.display())));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    scan.problems.push(i18n::pick(format!("Failed to enumerate the maintenance record directory ({}): {error}",
                        dir.display()
                    ), format!("유지보수 기록 디렉터리 조회에 실패했습니다({}): {error}",
                        dir.display()
                    )));
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    scan.problems
                        .push(i18n::pick(format!("The maintenance record cannot be read ({name}): {error}"), format!("유지보수 기록을 읽을 수 없습니다({name}): {error}")));
                    continue;
                }
            };
            match serde_json::from_str::<BackupLifecycle>(&text) {
                Ok(record) if record.version == LIFECYCLE_VERSION => {
                    if let Err(reason) = validate_lifecycle_identity(&path, variant, &record) {
                        scan.problems.push(reason);
                    } else {
                        scan.records.push(record);
                    }
                }
                Ok(record) => scan.problems.push(i18n::pick(format!("Maintenance record version is not supported ({name}: {})",
                    record.version
                ), format!("지원하지 않는 유지보수 기록 버전입니다({name}: {})",
                    record.version
                ))),
                Err(error) => scan
                    .problems
                    .push(i18n::pick(format!("Failed to parse the maintenance record ({name}): {error}"), format!("유지보수 기록 해석에 실패했습니다({name}): {error}"))),
            }
        }
    }
    scan.records.sort_by_key(|record| record.created_at);
    scan
}

/// 仍被维护记录引用的操作日志：日志裁剪不得丢弃它们（否则补清理失去依据）。
///
/// 扫描不完整（损坏/未知版本/归属不一致）时返回 `Err`：调用方不得把部分结果
/// 当成「未引用」而去裁剪——设计要求状态不明就不裁剪。
pub fn referenced_operation_ids(
    paths: &SessionPaths,
) -> Result<std::collections::HashSet<String>, Vec<String>> {
    let scan = scan_lifecycle(paths);
    if !scan.problems.is_empty() {
        return Err(scan.problems);
    }
    Ok(scan
        .records
        .into_iter()
        .map(|record| record.operation_id)
        .collect())
}

/// 维护记录必须落在本档位目录、kind 受支持、文件名等于校验后的 UUID。
fn validate_lifecycle_identity(
    path: &Path,
    dir_variant: WbVariant,
    record: &BackupLifecycle,
) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if record.variant != dir_variant {
        return Err(i18n::pick(format!("The maintenance record edition does not match the directory ({name})"), format!("유지보수 기록의 에디션이 디렉터리와 일치하지 않습니다({name})")));
    }
    if record.kind != "copy" && record.kind != "sync" {
        return Err(i18n::pick(format!("Unsupported maintenance record kind ({name}: {})", record.kind), format!("지원하지 않는 유지보수 기록 kind입니다({name}: {})", record.kind)));
    }
    let id = validated_operation_id(&record.operation_id).map_err(|_| {
        i18n::pick(format!("The maintenance record operation id is not a valid UUID ({name}: {})",
            record.operation_id
        ), format!("유지보수 기록의 작업 식별자가 올바른 UUID가 아닙니다({name}: {})",
            record.operation_id
        ))
    })?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if stem != id {
        return Err(i18n::pick(format!("The maintenance record file name does not match the operation id ({name})"), format!("유지보수 기록 파일 이름이 작업 식별자와 일치하지 않습니다({name})")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 生命周期推进
// ---------------------------------------------------------------------------

/// 开始一次操作：先可靠写 allocating 记录，再创建专属目录。
///
/// 返回的记录由调用方持有，后续阶段推进都基于它；目录创建失败时记录保留，
/// 由下次维护入口按「allocating 且无业务日志」回收（本版契约保证业务前 protected）。
pub fn begin_operation(
    paths: &SessionPaths,
    variant: WbVariant,
    kind: &str,
    session_id: Option<String>,
    title: Option<String>,
) -> Result<BackupLifecycle, String> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let record = BackupLifecycle {
        version: LIFECYCLE_VERSION,
        operation_id: operation_id.clone(),
        variant,
        kind: kind.to_string(),
        state: BackupState::Allocating,
        created_at: now_ms(),
        updated_at: now_ms(),
        last_error: None,
        session_id,
        title,
    };
    save_lifecycle(paths, &record)?;
    let dir = transaction_dir(paths, variant, &operation_id)?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|error| i18n::pick(format!("Could not create the temporary directory: {error}"), format!("임시 디렉터리를 만들지 못했습니다: {error}")))?;
        sync_dir(parent).map_err(|error| i18n::pick(format!("Could not persist the temporary directory: {error}"), format!("임시 디렉터리를 영구 저장하지 못했습니다: {error}")))?;
    }
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        Err(error) => {
            return Err(i18n::pick(format!("Could not create the operation directory ({error}); no business content was written"
            ), format!("작업 전용 디렉터리를 만들지 못했습니다({error}). 비즈니스 내용은 기록하지 않았습니다"
            )));
        }
    }
    sync_dir(&dir).map_err(|error| i18n::pick(format!("Could not persist the operation directory: {error}"), format!("작업 전용 디렉터리를 영구 저장하지 못했습니다: {error}")))?;
    Ok(record)
}

/// 记录状态推进：可靠落盘后才更新内存副本（失败时内存不变，不产生虚假状态）。
pub fn advance_record(
    paths: &SessionPaths,
    record: &mut BackupLifecycle,
    state: BackupState,
    last_error: Option<&str>,
) -> Result<(), String> {
    let mut candidate = record.clone();
    candidate.state = state;
    candidate.updated_at = now_ms();
    candidate.last_error = last_error.map(str::to_string);
    save_lifecycle(paths, &candidate)?;
    *record = candidate;
    Ok(())
}

/// 备份完备、即将或已经写业务：转 protected。
pub fn mark_protected(paths: &SessionPaths, record: &mut BackupLifecycle) -> Result<(), String> {
    let dir = transaction_dir(paths, record.variant, &record.operation_id)?;
    sync_tree(&dir).map_err(|error| i18n::pick(format!("Could not persist the backup directory: {error}"), format!("백업 디렉터리를 영구 저장하지 못했습니다: {error}")))?;
    advance_record(paths, record, BackupState::Protected, None)
}

/// 已可靠确认完成（或已验证安全终止）：允许回收；清理前若此步失败，业务成功不受影响。
pub fn mark_cleanup_pending(
    paths: &SessionPaths,
    record: &mut BackupLifecycle,
    reason: Option<&str>,
) -> Result<(), String> {
    advance_record(paths, record, BackupState::CleanupPending, reason)
}

// ---------------------------------------------------------------------------
// 回收
// ---------------------------------------------------------------------------

/// 回收一次操作的临时目录与维护记录（幂等；调用方必须已持有档位操作锁）。
///
/// 顺序：目录 → 业务日志（backup=null / cleaned）→ 维护记录；任一步失败都保留
/// 可重试依据。目录已经不存在时视为已删，不要求备份清单仍然完整。
pub fn cleanup_operation(
    paths: &SessionPaths,
    variant: WbVariant,
    record: &BackupLifecycle,
) -> CleanupOutcome {
    let dir = match transaction_dir(paths, variant, &record.operation_id) {
        Ok(dir) => dir,
        Err(reason) => return CleanupOutcome::Protected { reason },
    };
    let root = transactions_root(paths, variant);
    // 可信根是工具存储根：从 store_root 往下的每一级（含 backups/ 祖先）都不能是符号链接。
    if let Err(reason) = ensure_no_symlink_components(&paths.store_root, &dir) {
        return CleanupOutcome::Protected { reason };
    }
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                return CleanupOutcome::Pending {
                    reason: i18n::pick(format!("Could not delete the temporary directory: {error}"), format!("임시 디렉터리를 삭제하지 못했습니다: {error}")),
                };
            }
            if let Err(error) = sync_dir(&root) {
                return CleanupOutcome::Pending {
                    reason: i18n::pick(format!("Could not persist the directory entry: {error}"), format!("디렉터리 항목을 영구 저장하지 못했습니다: {error}")),
                };
            }
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            return CleanupOutcome::Protected {
                reason: i18n::tr("The temporary directory was replaced by a symbolic link; the current state was preserved", "임시 디렉터리가 심볼릭 링크로 바뀌어 현재 상태를 보존했습니다").to_string(),
            };
        }
        Ok(_) => {
            return CleanupOutcome::Protected {
                reason: i18n::tr("A non-directory file with the same name exists at the temporary directory location; the current state was preserved", "임시 디렉터리 위치에 같은 이름의 디렉터리가 아닌 파일이 있어 현재 상태를 보존했습니다").to_string(),
            };
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return CleanupOutcome::Pending {
                reason: i18n::pick(format!("The temporary directory state cannot be confirmed: {error}"), format!("임시 디렉터리 상태를 확인할 수 없습니다: {error}")),
            };
        }
    }

    if let Err(reason) = mark_operation_cleaned(paths, variant, record) {
        return CleanupOutcome::Pending { reason };
    }
    let file = match lifecycle_file(paths, variant, &record.operation_id) {
        Ok(file) => file,
        Err(reason) => return CleanupOutcome::Protected { reason },
    };
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return CleanupOutcome::Pending {
                reason: i18n::pick(format!("Could not delete the maintenance record: {error}"), format!("유지보수 기록을 삭제하지 못했습니다: {error}")),
            };
        }
    }
    if let Err(error) = sync_parent_of(&file) {
        return CleanupOutcome::Pending {
            reason: i18n::pick(format!("Could not persist the maintenance record directory entry: {error}"), format!("유지보수 기록 디렉터리 항목을 영구 저장하지 못했습니다: {error}")),
        };
    }
    CleanupOutcome::Cleaned
}

/// 业务日志标注备份已清理（`backup=null` + `cleanupState=cleaned`）。
///
/// 日志缺失不算失败（例如准备阶段失败的残留）；日志仍在且仍未完成时拒绝清理，
/// 避免把未完成业务的日志改成「已清理」。
fn mark_operation_cleaned(
    paths: &SessionPaths,
    variant: WbVariant,
    record: &BackupLifecycle,
) -> Result<(), String> {
    let scan = session_link::scan_operations(paths);
    if !scan.complete {
        return Err(i18n::tr("The operation log scan is incomplete; the business log is not updated this round", "작업 기록 검사가 불완전하여 이번에는 비즈니스 로그를 갱신하지 않습니다").to_string());
    }
    let Some(mut operation) = scan.operations.into_iter().find(|operation| {
        operation.operation_id == record.operation_id && operation.variant == variant
    }) else {
        return Ok(());
    };
    if operation.phase.is_unfinished() {
        return Err(i18n::tr("The business work is not finished; refusing to mark the operation log as cleaned", "작업이 아직 완료되지 않아 작업 로그를 정리 완료로 표시하지 않습니다").to_string());
    }
    operation.backup = None;
    operation.cleanup_state = Some(CLEANUP_STATE_CLEANED.to_string());
    operation.updated_at = now_ms();
    session_link::save_operation(paths, &operation)
}

// ---------------------------------------------------------------------------
// 维护入口：补清理与保护上报
// ---------------------------------------------------------------------------

/// 单条维护记录的处理判定。
enum Decision {
    Clean,
    Protect(String),
}

/// 按操作日志阶段判断能否回收（只有可靠终态才授权删除）。
fn decide_by_operation(operation: Option<&Operation>) -> Decision {
    let Some(operation) = operation else {
        return Decision::Protect(i18n::tr("No operation log, so completion cannot be proven; the material was preserved", "작업 로그가 없어 완료를 증명할 수 없습니다. 자료를 보존했습니다").to_string());
    };
    if operation.lifecycle_version != Some(OPERATION_LIFECYCLE_VERSION) {
        return Decision::Protect(
            i18n::tr("The operation log lacks this version's lifecycle marker, so it cannot authorize deletion; the material was preserved", "작업 로그에 이 버전의 수명 주기 표시가 없어 삭제 근거로 쓸 수 없습니다. 자료를 보존했습니다").to_string(),
        );
    }
    if operation.phase == OpPhase::Completed {
        return Decision::Clean;
    }
    if operation.phase == OpPhase::Abandoned {
        if operation.cleanup_state.as_deref() == Some(CLEANUP_STATE_SAFE_TERMINATED) {
            return Decision::Clean;
        }
        return Decision::Protect(
            i18n::tr("The operation was abandoned but the \"verified safe termination\" marker is missing; the material was preserved for manual review", "작업이 중단되었지만 \"안전 종료 확인\" 표시가 없어 자료를 보존하고 수동 확인이 필요합니다").to_string(),
        );
    }
    Decision::Protect(
        operation
            .last_error
            .clone()
            .unwrap_or_else(|| i18n::tr("The business work is not finished yet; clean up after recovery", "작업이 아직 완료되지 않았습니다. 복구 후 정리하세요").to_string()),
    )
}

/// 维护一个档位的临时备份：回收可安全回收的残留，保护其余并返回上报项。
///
/// 调用方必须已持有档位操作锁。扫描不完整（目录不可读/枚举失败/记录损坏）时
/// 本轮不做任何删除——无法限定影响范围时宁可保留。
pub fn maintain(paths: &SessionPaths, variant: WbVariant) -> Vec<TemporaryFileIssue> {
    let mut issues: Vec<TemporaryFileIssue> = Vec::new();
    let operation_scan = session_link::scan_operations(paths);
    let lifecycle_scan = scan_lifecycle(paths);

    for problem in &operation_scan.problems {
        issues.push(TemporaryFileIssue::needs_recovery(
            String::new(),
            None,
            None,
            format!("{}{problem}", op_scan_problem_prefix()),
        ));
    }
    for problem in &lifecycle_scan.problems {
        issues.push(TemporaryFileIssue::needs_recovery(
            String::new(),
            None,
            None,
            problem.clone(),
        ));
    }
    // 任何一处读不到/解析不了（操作日志或维护记录）都无法限定影响范围：本轮不删除。
    let deletions_allowed = operation_scan.complete
        && operation_scan.problems.is_empty()
        && lifecycle_scan.problems.is_empty();

    for mut record in lifecycle_scan
        .records
        .into_iter()
        .filter(|record| record.variant == variant)
    {
        let operation = operation_scan.operations.iter().find(|operation| {
            operation.operation_id == record.operation_id && operation.variant == variant
        });
        let decision = match record.state {
            BackupState::CleanupPending => Decision::Clean,
            BackupState::Allocating => match operation {
                // 本版契约：protected 之前禁止业务写入；扫描完整且无操作日志 ⇒ 尚未写业务。
                None if deletions_allowed => Decision::Clean,
                None => Decision::Protect(
                    i18n::tr("Cannot confirm whether business data was written (incomplete scan); the preparation leftovers were preserved", "비즈니스 데이터 기록 여부를 확인할 수 없습니다(검사 불완전). 준비 잔여물을 보존했습니다").to_string(),
                ),
                Some(operation) => decide_by_operation(Some(operation)),
            },
            BackupState::Protected => decide_by_operation(operation),
        };
        match decision {
            Decision::Clean => {
                if !deletions_allowed {
                    issues.push(TemporaryFileIssue::needs_recovery(
                        record.operation_id.clone(),
                        record.session_id.clone(),
                        record.title.clone(),
                        i18n::tr("This scan was incomplete; cleanup was skipped", "이번 검사가 불완전하여 정리를 건너뛰었습니다").to_string(),
                    ));
                    continue;
                }
                // 补转 cleanupPending 失败也继续尝试清理：目录归属由记录推导，
                // 不需要状态推进成功；失败时记录仍在，下次继续。
                let _ = mark_cleanup_pending(paths, &mut record, None);
                match cleanup_operation(paths, variant, &record) {
                    CleanupOutcome::Cleaned => {}
                    CleanupOutcome::Pending { reason } => {
                        issues.push(TemporaryFileIssue::cleanup_pending(
                            record.operation_id.clone(),
                            record.session_id.clone(),
                            record.title.clone(),
                            reason,
                        ));
                    }
                    CleanupOutcome::Protected { reason } => {
                        issues.push(TemporaryFileIssue::needs_recovery(
                            record.operation_id.clone(),
                            record.session_id.clone(),
                            record.title.clone(),
                            reason,
                        ));
                    }
                }
            }
            Decision::Protect(reason) => {
                issues.push(TemporaryFileIssue::needs_recovery(
                    record.operation_id.clone(),
                    record.session_id.clone(),
                    record.title.clone(),
                    reason,
                ));
            }
        }
    }
    issues
}

/// 清理失败后的即时落盘：把失败原因写回维护记录，作为下次补清理的依据。
pub fn record_cleanup_failure(
    paths: &SessionPaths,
    record: &BackupLifecycle,
    reason: &str,
) -> Result<(), String> {
    let mut candidate = record.clone();
    candidate.state = BackupState::CleanupPending;
    candidate.updated_at = now_ms();
    candidate.last_error = Some(reason.to_string());
    save_lifecycle(paths, &candidate)
}

/// 回收一个操作并处理失败：失败原因写回维护记录，业务结果不受影响。
pub fn cleanup_after_success(
    paths: &SessionPaths,
    variant: WbVariant,
    record: &BackupLifecycle,
) -> CleanupOutcome {
    let outcome = cleanup_operation(paths, variant, record);
    if let CleanupOutcome::Pending { reason } = &outcome {
        let _ = record_cleanup_failure(paths, record, reason);
    }
    outcome
}

/// 受控失败分支：尚未写业务（contract：protected 之前禁止业务写入）时回收自己的残留。
///
/// 先把「已验证安全终止」可靠写回维护记录再回收；写不进去就保留 allocating 记录，
/// 由下次维护入口按「无操作日志」判定回收。
pub fn reclaim_unwritten(
    paths: &SessionPaths,
    variant: WbVariant,
    record: &mut BackupLifecycle,
    reason: &str,
) -> CleanupOutcome {
    let mut candidate = record.clone();
    candidate.state = BackupState::CleanupPending;
    candidate.updated_at = now_ms();
    candidate.last_error = Some(reason.to_string());
    match save_lifecycle(paths, &candidate) {
        Ok(()) => {
            *record = candidate;
            cleanup_operation(paths, variant, record)
        }
        Err(error) => CleanupOutcome::Pending {
            reason: i18n::pick(format!("Could not write the safe-termination marker ({error}); the preparation leftovers were preserved for the next maintenance run"), format!("안전 종료 표시를 쓰지 못했습니다({error}). 준비 잔여물을 보존하고 다음 유지보수에서 처리합니다")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::session_link::{OpPhase, Operation, OperationMember, OPERATION_VERSION};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wb_switch_backup_test_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn paths(&self) -> SessionPaths {
            SessionPaths {
                store_root: self.0.join("store"),
                data_root: self.0.join("data"),
                link_namespace: crate::modules::session::LinkNamespace::WorkBuddy,
                auth_file: self.0.join("auth.info"),
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // 失败注入用例可能把目录改成只读，先恢复权限再清理。
            #[cfg(unix)]
            restore_permissions(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn restore_permissions(root: &Path) {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(meta) = std::fs::symlink_metadata(&path) {
                    if meta.is_dir() && !meta.file_type().is_symlink() {
                        let mut permissions = meta.permissions();
                        permissions.set_mode(0o755);
                        let _ = std::fs::set_permissions(&path, permissions);
                        restore_permissions(&path);
                    }
                }
            }
        }
    }

    fn operation(id: &str, phase: OpPhase, lifecycle: Option<u32>) -> Operation {
        Operation {
            version: OPERATION_VERSION,
            operation_id: id.to_string(),
            kind: "copy".to_string(),
            variant: WbVariant::Cn,
            source_variant: None,
            group_id: "g-1".to_string(),
            source: OperationMember {
                account_id: None,
                uid: "uid-a".to_string(),
                session_id: "sess-a".to_string(),
            },
            target: OperationMember {
                account_id: None,
                uid: "uid-b".to_string(),
                session_id: "sess-b".to_string(),
            },
            expected_content_digest: "digest".to_string(),
            expected_record_count: 1,
            phase,
            backup: None,
            lifecycle_version: lifecycle,
            cleanup_state: None,
            last_error: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn durable_write_persists_and_replaces_atomically() {
        let dir = TempDir::new("durable");
        let target = dir.0.join("nested").join("file.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        durable_write_str(&target, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first");
        durable_write_str(&target, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        // 没有残留的临时文件（rename 语义）。
        let leftovers = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn begin_operation_rejects_duplicate_directory() {
        let dir = TempDir::new("duplicate");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let created = transaction_dir(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        assert!(created.is_dir());
        // 复用同一目录必须失败：不允许两个操作共用目录。
        assert!(std::fs::create_dir(&created).is_err());
    }

    #[test]
    fn allocating_without_operation_log_is_reclaimed() {
        let dir = TempDir::new("allocating");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        std::fs::write(
            transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
                .unwrap()
                .join("workbuddy.db"),
            b"partial",
        )
        .unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(
            !transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
                .unwrap()
                .exists()
        );
        assert!(
            scan_lifecycle(&paths).records.is_empty(),
            "回收后维护记录必须删除"
        );
    }

    #[test]
    fn protected_without_operation_log_is_preserved_and_reported() {
        let dir = TempDir::new("protected");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        mark_protected(&paths, &mut record).unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].state, "needsRecovery");
        assert!(
            transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
                .unwrap()
                .is_dir(),
            "缺日志不得作为删除依据"
        );
    }

    /// 回归（issue #76）：备份目录里有真实文件时，「转 protected」必须成功。
    ///
    /// Windows 的 `FlushFileBuffers` 要求句柄具备写权限，而 `File::open` 只申请
    /// `GENERIC_READ`：只读句柄上 `sync_all` 必定返回 `ERROR_ACCESS_DENIED(5)`，导致
    /// Windows 上复制会话 100% 报「备份目录持久化失败：拒绝访问。(os error 5)」。
    /// 目录为空时不会触发（空目录不进入文件分支），所以必须显式写入真实文件。
    #[test]
    fn sync_tree_flushes_files_before_protected() {
        let dir = TempDir::new("flush");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let target = transaction_dir(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        std::fs::write(target.join("workbuddy.db"), b"snapshot").unwrap();
        std::fs::create_dir_all(target.join("nested")).unwrap();
        std::fs::write(target.join("nested").join("workbuddy.db-wal"), b"wal").unwrap();

        sync_tree(&target).expect("备份目录必须能持久化其中的文件");
        mark_protected(&paths, &mut record).expect("备份目录持久化必须成功");
        assert_eq!(record.state, BackupState::Protected);
    }

    #[test]
    fn protected_with_completed_log_is_swept_but_unfinished_is_not() {
        let dir = TempDir::new("sweep");
        let paths = dir.paths();

        let mut done = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        std::fs::write(
            transaction_dir(&paths, WbVariant::Cn, &done.operation_id)
                .unwrap()
                .join("workbuddy.db"),
            b"snapshot",
        )
        .unwrap();
        mark_protected(&paths, &mut done).unwrap();
        let mut log = operation(&done.operation_id, OpPhase::Completed, Some(1));
        log.backup = Some("backups/session-transactions/cn/x".to_string());
        session_link::save_operation(&paths, &log).unwrap();

        let mut stuck = begin_operation(&paths, WbVariant::Cn, "sync", None, None).unwrap();
        mark_protected(&paths, &mut stuck).unwrap();
        session_link::save_operation(
            &paths,
            &operation(&stuck.operation_id, OpPhase::BodyWritten, Some(1)),
        )
        .unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].operation_id, stuck.operation_id);
        assert!(
            !transaction_dir(&paths, WbVariant::Cn, &done.operation_id)
                .unwrap()
                .exists(),
            "完成的业务必须被补清理"
        );
        assert!(transaction_dir(&paths, WbVariant::Cn, &stuck.operation_id)
            .unwrap()
            .is_dir());
        // 已清理的日志：backup=null 且 cleanupState=cleaned。
        let stored = session_link::scan_operations(&paths)
            .operations
            .into_iter()
            .find(|operation| operation.operation_id == done.operation_id)
            .unwrap();
        assert!(stored.backup.is_none());
        assert_eq!(stored.cleanup_state.as_deref(), Some(CLEANUP_STATE_CLEANED));
    }

    #[test]
    fn cleared_log_without_lifecycle_marker_is_preserved() {
        let dir = TempDir::new("legacy-mark");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        mark_protected(&paths, &mut record).unwrap();
        // 旧格式日志（无生命周期标记）不能被当作删除授权。
        session_link::save_operation(
            &paths,
            &operation(&record.operation_id, OpPhase::Completed, None),
        )
        .unwrap();
        let issues = maintain(&paths, WbVariant::Cn);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].reason.contains("生命周期标记"), "{issues:?}");
        assert!(transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
            .unwrap()
            .is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_transaction_dir_is_preserved() {
        let dir = TempDir::new("symlink");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let target = transaction_dir(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        std::fs::remove_dir(&target).unwrap();
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, &target).unwrap();

        let outcome = cleanup_operation(&paths, WbVariant::Cn, &record);
        match outcome {
            CleanupOutcome::Protected { reason } => assert!(reason.contains("符号链接")),
            other => panic!("必须拒绝符号链接：{other:?}"),
        }
        assert!(outside.join("keep.txt").exists(), "链接目标不得被删除");
    }

    #[cfg(unix)]
    #[test]
    fn permission_failure_keeps_record_for_retry() {
        let dir = TempDir::new("perm");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let root = transactions_root(&paths, WbVariant::Cn);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = cleanup_operation(&paths, WbVariant::Cn, &record);
        match outcome {
            CleanupOutcome::Pending { reason } => assert!(reason.contains("删除失败"), "{reason}"),
            other => panic!("只读目录必须报告待重试：{other:?}"),
        }
        assert!(referenced_operation_ids(&paths)
            .expect("维护记录扫描必须完整")
            .contains(&record.operation_id));

        // 解除故障后：幂等补清理成功，不再有上报项。
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(scan_lifecycle(&paths).records.is_empty());
    }

    #[test]
    fn partial_delete_is_idempotent() {
        let dir = TempDir::new("partial");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let target = transaction_dir(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        std::fs::create_dir_all(target.join("bodies")).unwrap();
        std::fs::write(target.join("bodies").join("original.jsonl"), b"x").unwrap();
        // 模拟「删除到一半」：手工清掉目录内容，等价于上次中断。
        std::fs::remove_dir_all(&target).unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(scan_lifecycle(&paths).records.is_empty());
    }

    #[test]
    fn damaged_lifecycle_record_blocks_deletion() {
        let dir = TempDir::new("damaged");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let file = lifecycle_file(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        std::fs::write(&file, b"{not json").unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.iter().any(|issue| issue.reason.contains("解析失败")));
        assert!(
            transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
                .unwrap()
                .is_dir(),
            "受损记录存在时不得删除任何目录"
        );
    }

    #[test]
    fn cleanup_refuses_invalid_operation_id() {
        let dir = TempDir::new("bad-id");
        let paths = dir.paths();
        let record = BackupLifecycle {
            version: LIFECYCLE_VERSION,
            operation_id: "../escape".to_string(),
            variant: WbVariant::Cn,
            kind: "copy".to_string(),
            state: BackupState::CleanupPending,
            created_at: 1,
            updated_at: 1,
            last_error: None,
            session_id: None,
            title: None,
        };
        match cleanup_operation(&paths, WbVariant::Cn, &record) {
            CleanupOutcome::Protected { reason } => assert!(reason.contains("UUID")),
            other => panic!("非法标识必须拒绝：{other:?}"),
        }
    }

    #[test]
    fn cn_and_ai_records_are_isolated() {
        let dir = TempDir::new("variants");
        let paths = dir.paths();
        let cn = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let ai = begin_operation(&paths, WbVariant::Ai, "copy", None, None).unwrap();
        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(!transaction_dir(&paths, WbVariant::Cn, &cn.operation_id)
            .unwrap()
            .exists());
        assert!(
            transaction_dir(&paths, WbVariant::Ai, &ai.operation_id)
                .unwrap()
                .is_dir(),
            "另一档位的准备残留不得被本档位维护删除"
        );
    }

    #[test]
    fn ensure_full_synchronous_upgrades_below_full() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA synchronous=NORMAL").unwrap();
        let before: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert!(before < 2, "前置条件：NORMAL 必须低于 FULL");
        ensure_full_synchronous(&conn).unwrap();
        let after: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert!(after >= 2, "必须提升到 FULL 或更高：{after}");
    }

    #[test]
    fn mismatched_filename_blocks_deletion() {
        let dir = TempDir::new("name-mismatch");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let original = lifecycle_file(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        let spoofed_id = uuid::Uuid::new_v4().to_string();
        let spoofed = lifecycle_root(&paths, WbVariant::Cn).join(format!("{spoofed_id}.json"));
        std::fs::rename(&original, &spoofed).unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(
            issues
                .iter()
                .any(|issue| issue.reason.contains("文件名与操作标识不一致")),
            "{issues:?}"
        );
        assert!(
            transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
                .unwrap()
                .is_dir(),
            "归属不一致时不得删除目录"
        );
    }

    #[test]
    fn mismatched_variant_blocks_deletion() {
        let dir = TempDir::new("variant-mismatch");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        record.variant = WbVariant::Ai;
        let file = lifecycle_file(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        std::fs::write(&file, serde_json::to_string(&record).unwrap()).unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(
            issues
                .iter()
                .any(|issue| issue.reason.contains("档位与目录不一致")),
            "{issues:?}"
        );
        assert!(transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
            .unwrap()
            .is_dir());
    }

    #[test]
    fn unknown_kind_blocks_deletion() {
        let dir = TempDir::new("kind");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        record.kind = "evil".to_string();
        save_lifecycle(&paths, &record).unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(
            issues
                .iter()
                .any(|issue| issue.reason.contains("kind 不受支持")),
            "{issues:?}"
        );
        assert!(transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
            .unwrap()
            .is_dir());
    }

    #[test]
    fn abandoned_without_safe_terminated_marker_is_preserved() {
        let dir = TempDir::new("abandoned-protect");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        mark_protected(&paths, &mut record).unwrap();
        session_link::save_operation(
            &paths,
            &operation(&record.operation_id, OpPhase::Abandoned, Some(1)),
        )
        .unwrap();
        let issues = maintain(&paths, WbVariant::Cn);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].reason.contains("已验证安全终止"), "{issues:?}");
        assert!(transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
            .unwrap()
            .is_dir());
    }

    #[test]
    fn abandoned_with_safe_terminated_marker_is_reclaimed() {
        let dir = TempDir::new("abandoned-clean");
        let paths = dir.paths();
        let mut record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        mark_protected(&paths, &mut record).unwrap();
        let mut log = operation(&record.operation_id, OpPhase::Abandoned, Some(1));
        log.cleanup_state = Some(CLEANUP_STATE_SAFE_TERMINATED.to_string());
        session_link::save_operation(&paths, &log).unwrap();
        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(
            !transaction_dir(&paths, WbVariant::Cn, &record.operation_id)
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn legacy_timestamp_backup_dirs_are_not_swept() {
        let dir = TempDir::new("legacy-dirs");
        let paths = dir.paths();
        let old_sessions = paths
            .backup_root()
            .join("sessions")
            .join("2026-01-01T00-00-00Z");
        let old_sync = paths
            .backup_root()
            .join("session-sync")
            .join(WbVariant::Cn.as_str())
            .join("op-old");
        std::fs::create_dir_all(&old_sessions).unwrap();
        std::fs::write(old_sessions.join("workbuddy.db"), b"legacy").unwrap();
        std::fs::create_dir_all(&old_sync).unwrap();
        std::fs::write(old_sync.join("manifest.json"), b"{}").unwrap();

        let issues = maintain(&paths, WbVariant::Cn);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(old_sessions.join("workbuddy.db").exists());
        assert!(old_sync.join("manifest.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_under_store_root_is_rejected() {
        let dir = TempDir::new("ancestor-link");
        let paths = dir.paths();
        let record = begin_operation(&paths, WbVariant::Cn, "copy", None, None).unwrap();
        let target = transaction_dir(&paths, WbVariant::Cn, &record.operation_id).unwrap();
        std::fs::write(target.join("keep.txt"), b"keep").unwrap();

        let backups = paths.backup_root();
        let outside = dir.0.join("outside-backups");
        std::fs::rename(&backups, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &backups).unwrap();

        let outcome = cleanup_operation(&paths, WbVariant::Cn, &record);
        match outcome {
            CleanupOutcome::Protected { reason } => {
                assert!(reason.contains("符号链接"), "{reason}")
            }
            other => panic!("祖先符号链接必须拒绝：{other:?}"),
        }
        assert!(
            outside
                .join(TRANSACTIONS_DIR_NAME)
                .join(WbVariant::Cn.as_str())
                .join(&record.operation_id)
                .join("keep.txt")
                .exists(),
            "链接目标内的文件不得被删除"
        );
    }
}
