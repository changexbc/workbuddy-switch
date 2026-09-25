//! 账号切换：备份 → 关进程 → 恢复/复制会话（可选）→ 写认证 → 启动。
//!
//! 对照 server.py `switch_account`。切换过程中通过进度回调向前端推送实时进度，
//! 避免界面长时间无反馈被误认为卡死。core 不依赖 Tauri，进度回调由宿主适配
//! （桌面端转发为 `switch-progress` 事件，HTTP 端写入轮询/SSE）。
//!
//! 顺序要点（design §1）：互斥 → 关进程 → 恢复/复制/同步会话 → 写认证 → 启动。
//! 复制与同步顺序调用、共用同一把档位操作锁，且在「关闭 App + 恢复未完成写入」之后
//! 才执行；会话写入一律在 WorkBuddy 停止写入之后。普通会话操作失败不阻止认证切换，
//! 只有「无法安全恢复的中间产物」才暂停切换并明确报告恢复需求。
//!
//! 暂停启动的判定有两次，口径相同（design §5 / R5）：
//!   1. 写入之前——切换开头恢复未完成写入后仍有不可安全恢复的中间产物；
//!   2. 写入之后——本次复制/同步新留下的未完成操作（正文/数据库/组表任一阶段失败都会
//!      保留操作记录），不能带着不一致的会话内容写认证并启动 App。

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::modules::account;
use crate::modules::auth_file;
use crate::modules::process::{close_workbuddy, launch_workbuddy};
use crate::modules::session::{self, SessionPaths, SyncSelection};
use crate::modules::session_link::{
    self, Operation, RecoveryReport, LOCK_BUSY_MESSAGE_PREFIX, LOCK_UNAVAILABLE_MESSAGE_PREFIX,
};
use crate::modules::variant::WbVariant;

/// 切换进度回调（宿主注入，如 Tauri `app.emit` 或 HTTP 进度缓存）。
pub type ProgressFn = Box<dyn Fn(&str) + Send + Sync>;

/// `restart=false` 携带会话写入意图时的拒绝文案（复制/同步各自一条）。
pub const COPY_WITHOUT_RESTART_MESSAGE: &str =
    "本次切换未重启 WorkBuddy（restart=false），已拒绝会话复制请求；如需复制请勾选重启切换";
pub const SYNC_WITHOUT_RESTART_MESSAGE: &str =
    "本次切换未重启 WorkBuddy（restart=false），已拒绝会话同步请求；如需同步请勾选重启切换";

/// 恢复报告里是否存在阻碍启动的问题：未恢复一致的中间产物必须先处理。
///
/// 拿不到档位锁、或中间产物被改动/丢失都属此类（design §4 / §5）：此时继续写认证并
/// 启动 App 会让 Official App 在最坏状态下打开会话，因此暂停切换与启动。
/// `retryable=true` 沿用复制侧「可延后重试且不阻断启动」的约定；同步恢复失败必须
/// 返回 false，即使故障只是暂时的，也不能带着半完成的正文/数据库/基线启动。
pub fn recovery_blocks_startup(report: &RecoveryReport) -> bool {
    report.needs_recovery.iter().any(|issue| !issue.retryable)
}

/// 阻碍启动的原因汇总（用于错误文案）。
///
/// 必须带操作标识：阻断分支只返回 Err 字符串，前端 catch 不能只看到原因。
pub fn recovery_blocking_detail(report: &RecoveryReport) -> String {
    report
        .needs_recovery
        .iter()
        .filter(|issue| !issue.retryable)
        .map(|issue| format!("{}：{}", issue.operation_id, issue.reason))
        .collect::<Vec<String>>()
        .join("；")
}

/// 恢复报告的前端投影（与复制/同步报告同形，便于展示恢复信息）。
pub fn recovery_report_json(report: &RecoveryReport) -> Value {
    json!({
        "recovered": report.recovered.len(),
        "abandoned": report.abandoned.len(),
        "needsRecovery": report
            .needs_recovery
            .iter()
            .map(|issue| json!({
                "operationId": issue.operation_id,
                "reason": issue.reason,
                "retryable": issue.retryable,
            }))
            .collect::<Vec<Value>>(),
        // 待清理/待恢复的临时备份残留：与复制/同步报告同一结构。
        "temporaryFiles": report.temporary_files,
    })
}

/// 某档位当前未完成操作的 id 集合。
///
/// 用于区分「本次新产生的未完成写入」与「切换开头已尝试恢复的历史残留」：
/// 后者若是可重试类（例如映射库暂不可用），按既有口径不阻断切换。
fn pending_operation_ids(paths: &SessionPaths, variant: WbVariant) -> BTreeSet<String> {
    session_link::pending_operations(paths, variant)
        .into_iter()
        .map(|operation| operation.operation_id)
        .collect()
}

/// 本次会话写入新留下的未完成操作（按创建顺序，便于稳定输出）。
fn newly_unfinished_writes(before: &BTreeSet<String>, after: Vec<Operation>) -> Vec<Operation> {
    let mut created: Vec<Operation> = after
        .into_iter()
        .filter(|operation| !before.contains(&operation.operation_id))
        .collect();
    created.sort_by_key(|operation| operation.created_at);
    created
}

/// 未完成写入的说明文案：会话标识 + 操作标识 + 原因。
fn unfinished_writes_detail(writes: &[Operation]) -> String {
    writes
        .iter()
        .map(|operation| {
            let reason = operation
                .last_error
                .clone()
                .unwrap_or_else(|| "尚未完成".to_string());
            format!(
                "{}（操作 {}）：{reason}",
                operation.target.session_id, operation.operation_id
            )
        })
        .collect::<Vec<String>>()
        .join("；")
}

/// restart=false 时携带写入意图：显式拒绝对应的会话操作（不静默丢弃，design §4.1）。
fn reject_session_writes_without_restart(
    has_copy: bool,
    has_sync: bool,
) -> (Option<Value>, Option<Value>) {
    let copy = has_copy.then(|| json!({ "error": COPY_WITHOUT_RESTART_MESSAGE }));
    let sync = has_sync.then(|| {
        json!({
            "synced": [],
            "skipped": [],
            "errors": [{ "error": SYNC_WITHOUT_RESTART_MESSAGE }],
        })
    });
    (copy, sync)
}

/// 切换账号。
///
/// `copy_session_ids` 非空时按路径 B 复制勾选会话（新 id，云端可同步）；
/// `sync_selections` 非空时把来源账号的新增内容同步到目标账号的关联会话（保留目标
/// sessionId、标题与自定义标题）。两者共用同一把档位操作锁，顺序执行、不重复写入。
pub fn switch_account(
    progress_fn: Option<&ProgressFn>,
    account_id: &str,
    restart: bool,
    share_sessions: bool,
    copy_session_ids: &[String],
    sync_selections: &[SyncSelection],
) -> Result<Value, String> {
    let progress = |message: &str| {
        eprintln!("[switch] progress: {message}");
        if let Some(p) = progress_fn {
            p(message);
        }
    };

    progress("开始切换账号…");
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    // 档位以账号自身为准：签名里的参数无法表达「用 A 档位操作 B 档位账号」。
    let variant = account::variant_of(&acc);
    let backup = auth_file::backup_auth_file(variant);

    let mut copy_report: Option<Value> = None;
    let mut session_report: Option<Value> = None;
    let mut sync_report: Option<Value> = None;
    let mut recovery_report: Option<Value> = None;
    if restart {
        progress("正在关闭 WorkBuddy…");
        close_workbuddy(variant, 20)?;
        // 关进程后先恢复未完成的会话写入：恢复成功或复制侧可安全延后重试的失败
        // 不阻断切换；同步仍未完成、拿不到锁或中间产物异常则暂停启动（design §4 / §5）。
        let recovery = match session::recover_pending_session_operations(variant) {
            Ok(report) => report,
            Err(error) => {
                return Err(format!(
                    "无法恢复未完成的会话写入（{error}），已暂停切换与启动 WorkBuddy；请稍后重试"
                ));
            }
        };
        let blocking = recovery_blocks_startup(&recovery);
        if !recovery.is_empty() {
            recovery_report = Some(recovery_report_json(&recovery));
        }
        if blocking {
            let detail = recovery_blocking_detail(&recovery);
            return Err(format!(
                "检测到无法安全恢复的会话写入（{detail}），已暂停切换与启动 WorkBuddy；请先处理该会话后再试"
            ));
        }
        // 本次是否请求了会话写入；同时记下写入前已存在的未完成记录，供写入后比对。
        let ops_paths = SessionPaths::for_variant(variant);
        let attempts_session_writes = !copy_session_ids.is_empty() || !sync_selections.is_empty();
        let pending_before = if attempts_session_writes {
            pending_operation_ids(&ops_paths, variant)
        } else {
            BTreeSet::new()
        };
        if !copy_session_ids.is_empty() {
            progress("正在复制会话到目标账号…");
            // 复制失败本身只记进报告、不阻断切换；但若本次留下了未完成的写入，
            // 会在复制与同步都跑完后统一暂停切换与启动（见下方 pending 差集检查）。
            copy_report = Some(
                match session::copy_sessions_for_switch(&acc, copy_session_ids) {
                    Ok(report) => report,
                    // 档位锁被占用或无法建立互斥：此时可能另有会话写入正在进行，
                    // 不能当成普通复制失败后继续写认证并启动 App。锁失败文案前缀
                    // 由 session_link 提供，不在这里嗅探整句错误文案。
                    Err(error)
                        if error.starts_with(LOCK_BUSY_MESSAGE_PREFIX)
                            || error.starts_with(LOCK_UNAVAILABLE_MESSAGE_PREFIX) =>
                    {
                        return Err(format!(
                            "无法独占会话操作（{error}），已暂停切换与启动 WorkBuddy；请稍后重试"
                        ));
                    }
                    Err(error) => json!({"error": error}),
                },
            );
        }
        if !sync_selections.is_empty() {
            // 同步与复制顺序执行（同一把档位锁），不重复写入。
            progress("正在同步会话到目标账号…");
            sync_report = Some(
                match session::sync_sessions_for_switch(&acc, sync_selections) {
                    Ok(report) => report,
                    Err(error)
                        if error.starts_with(LOCK_BUSY_MESSAGE_PREFIX)
                            || error.starts_with(LOCK_UNAVAILABLE_MESSAGE_PREFIX) =>
                    {
                        return Err(format!(
                            "无法独占会话操作（{error}），已暂停切换与启动 WorkBuddy；请稍后重试"
                        ));
                    }
                    // 同步失败不阻断切换：契约与成功路径同形，错误挂在 errors 里。
                    Err(error) => json!({
                        "synced": [],
                        "skipped": [],
                        "errors": [{ "error": error }],
                    }),
                },
            );
        }
        if share_sessions {
            // 旧的「全体转移」兼容路径（默认关闭），Rust 版暂未实现
            session_report = Some(json!({"error": "share_sessions 兼容路径暂未在 Rust 版实现"}));
        }
        // 本次复制/同步新留下的未完成操作：不带着不一致的会话内容写认证并启动 App
        // （design §5 / R5）。未完成写入也不在这里被报告成成功——错误已在各自报告里，
        // 这里只决定「暂停」。未开始写入的失败（预览过期、源会话被删等）不产生操作记录，
        // 因此仍然只是跳过该项、继续切号。
        if attempts_session_writes {
            let created = newly_unfinished_writes(
                &pending_before,
                session_link::pending_operations(&ops_paths, variant),
            );
            if !created.is_empty() {
                return Err(format!(
                    "本次会话写入未完成（{}），已暂停切换与启动 WorkBuddy；请重试，下次切号会先完成恢复",
                    unfinished_writes_detail(&created)
                ));
            }
        }
    } else {
        // restart=false 表示本次不做任何会话写入；携带写入意图时显式拒绝该会话操作，
        // 不能静默丢弃（design §4.1）。
        let (copy, sync) = reject_session_writes_without_restart(
            !copy_session_ids.is_empty(),
            !sync_selections.is_empty(),
        );
        copy_report = copy;
        sync_report = sync;
    }
    progress("正在写入认证文件…");
    auth_file::write_account_to_auth_file(&acc, variant)?;
    if restart {
        progress("正在启动 WorkBuddy…");
        launch_workbuddy(variant, Some(&progress))?;
    }
    progress("切换完成");

    let mut result = json!({
        "ok": true,
        "account": account::account_display_name(&acc),
        "variant": variant.as_str(),
        "backup": backup.map(|p| p.to_string_lossy().to_string()),
    });
    if let Some(c) = copy_report {
        result["sessionCopy"] = c;
    }
    if let Some(s) = session_report {
        result["sessionShare"] = s;
    }
    if let Some(s) = sync_report {
        result["sessionSync"] = s;
    }
    if let Some(r) = recovery_report {
        result["sessionRecovery"] = r;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    //! 编排层可注入纯函数的单测：切换全流程（关进程/写认证/启动）需真机验收，
    //! 这里覆盖「不可安全恢复必须暂停启动」「本次新留下的未完成写入必须暂停启动」
    //! 与「restart=false 拒绝写入意图」三条判定。

    use super::*;
    use crate::modules::session_link::{
        save_operation, OpPhase, OperationMember, RecoveryIssue, TemporaryFileIssue,
        OPERATION_VERSION,
    };

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wb_switch_orchestrate_{}_{name}",
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
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 测试用操作记录：只关心 id、阶段与失败原因。
    fn operation(id: &str, phase: OpPhase, error: Option<&str>, created_at: i64) -> Operation {
        Operation {
            version: OPERATION_VERSION,
            operation_id: id.to_string(),
            kind: "sync".to_string(),
            variant: WbVariant::Cn,
            group_id: "g-1".to_string(),
            source: OperationMember {
                account_id: None,
                uid: "uid-a".to_string(),
                session_id: "sess-1".to_string(),
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
            lifecycle_version: None,
            cleanup_state: None,
            last_error: error.map(str::to_string),
            created_at,
            updated_at: created_at,
        }
    }

    fn report_with(retryable: bool) -> RecoveryReport {
        RecoveryReport {
            recovered: Vec::new(),
            abandoned: Vec::new(),
            temporary_files: Vec::new(),
            needs_recovery: vec![RecoveryIssue {
                operation_id: "op-1".to_string(),
                reason: "目标内容与操作记录不一致，已停止恢复".to_string(),
                retryable,
            }],
        }
    }

    /// 中间产物被改动/丢失（不可重试）→ 暂停启动；可重试的失败不阻断。
    #[test]
    fn recovery_blocks_startup_only_for_unrecoverable_issues() {
        assert!(recovery_blocks_startup(&report_with(false)));
        assert!(!recovery_blocks_startup(&report_with(true)));
        assert!(!recovery_blocks_startup(&RecoveryReport::default()));
        let blocking = recovery_blocking_detail(&report_with(false));
        assert!(blocking.contains("op-1"), "{blocking}");
        assert!(blocking.contains("已停止恢复"), "{blocking}");
        assert!(recovery_blocking_detail(&report_with(true)).is_empty());
    }

    /// 恢复报告投影：三类结果与不可重试标记都要能被前端看到。
    #[test]
    fn recovery_report_projection_carries_all_categories() {
        let mut report = report_with(false);
        report.recovered.push("op-2".to_string());
        report.abandoned.push("op-3".to_string());
        let value = recovery_report_json(&report);
        assert_eq!(value["recovered"], 1);
        assert_eq!(value["abandoned"], 1);
        assert_eq!(value["needsRecovery"][0]["operationId"], "op-1");
        assert_eq!(value["needsRecovery"][0]["retryable"], false);
        assert_eq!(value["temporaryFiles"], json!([]));
    }

    /// 只有临时备份残留时也不能当成「恢复什么都没做」：宿主必须把 sessionRecovery 带给前端。
    #[test]
    fn recovery_is_not_empty_when_only_temporary_files_remain() {
        let mut report = RecoveryReport::default();
        report
            .temporary_files
            .push(TemporaryFileIssue::cleanup_pending(
                "op-cleanup".to_string(),
                Some("sess-1".to_string()),
                Some("标题".to_string()),
                "临时目录删除失败：权限不足".to_string(),
            ));
        assert!(!report.is_empty());
        assert!(report.is_clean(), "待清理不得变成启动阻断");
        let value = recovery_report_json(&report);
        assert_eq!(value["temporaryFiles"][0]["operationId"], "op-cleanup");
        assert_eq!(value["temporaryFiles"][0]["state"], "cleanupPending");
        assert_eq!(value["temporaryFiles"][0]["sessionId"], "sess-1");
        assert_eq!(value["temporaryFiles"][0]["title"], "标题");
    }

    /// 本次新留下的未完成写入必须被识别出来：历史残留不算在本次头上，已完成的也不算。
    #[test]
    fn newly_unfinished_writes_only_counts_this_switch() {
        let dir = TempDir::new("pending-writes");
        let paths = dir.paths();
        // 历史残留（切换开头已尝试恢复，可重试类不阻断）。
        save_operation(
            &paths,
            &operation("op-old", OpPhase::BodyWritten, Some("映射库暂不可用"), 1),
        )
        .unwrap();
        let before = pending_operation_ids(&paths, WbVariant::Cn);
        assert_eq!(before.len(), 1);

        // 本次复制/同步新产生一条未完成操作。
        save_operation(
            &paths,
            &operation(
                "op-new",
                OpPhase::DbWritten,
                Some("目标会话记录更新失败"),
                2,
            ),
        )
        .unwrap();
        let created = newly_unfinished_writes(
            &before,
            session_link::pending_operations(&paths, WbVariant::Cn),
        );
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].operation_id, "op-new");
        let detail = unfinished_writes_detail(&created);
        assert!(detail.contains("目标会话记录更新失败"), "{detail}");

        // 已完成的写入不算未完成；没有失败原因时退化为操作 id。
        save_operation(&paths, &operation("op-done", OpPhase::Completed, None, 3)).unwrap();
        let created = newly_unfinished_writes(
            &BTreeSet::new(),
            session_link::pending_operations(&paths, WbVariant::Cn),
        );
        assert_eq!(
            created
                .iter()
                .map(|operation| operation.operation_id.as_str())
                .collect::<Vec<_>>(),
            vec!["op-old", "op-new"],
            "按创建时间排序，且不含已完成操作"
        );
        assert!(
            unfinished_writes_detail(&[operation("op-bare", OpPhase::Prepared, None, 4)])
                .contains("op-bare")
        );

        // 别的档位的未完成写入不算在本档位头上。
        let mut other = operation("op-ai", OpPhase::Prepared, Some("待恢复"), 5);
        other.variant = WbVariant::Ai;
        save_operation(&paths, &other).unwrap();
        assert!(!pending_operation_ids(&paths, WbVariant::Cn).contains("op-ai"));
    }

    /// restart=false 携带同步意图 → 与复制同样显式拒绝，且契约与成功路径同形。
    #[test]
    fn session_writes_without_restart_are_rejected_explicitly() {
        let (copy, sync) = reject_session_writes_without_restart(false, true);
        assert!(copy.is_none());
        let sync = sync.expect("携带同步意图必须给出拒绝报告");
        assert_eq!(sync["errors"][0]["error"], SYNC_WITHOUT_RESTART_MESSAGE);
        assert!(sync["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("restart=false"));
        assert!(sync["synced"].as_array().unwrap().is_empty());
        assert!(sync["skipped"].as_array().unwrap().is_empty());

        // 两种意图同时存在：各自拒绝，互不吞掉。
        let (copy, sync) = reject_session_writes_without_restart(true, true);
        assert_eq!(copy.unwrap()["error"], COPY_WITHOUT_RESTART_MESSAGE);
        assert_eq!(
            sync.unwrap()["errors"][0]["error"],
            SYNC_WITHOUT_RESTART_MESSAGE
        );

        // 都没有写入意图：不产生任何报告（保持原有语义）。
        let (copy, sync) = reject_session_writes_without_restart(false, false);
        assert!(copy.is_none() && sync.is_none());
    }
}
