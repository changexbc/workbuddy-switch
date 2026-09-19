//! 硬链接共享（autoLink，2026-09-16 定稿）：源账号「目标还没有」的存活会话零拷贝共享。
//!
//! 从 `session.rs` 拆出（2026-09-17）：与会话复制（同文件上游路径 B）解耦，
//! 压缩上游 #43 等改动对 `session.rs` 的合并冲突面。
//!
//! 两阶段设计（绕开单建 conv 的 10 条/窗口限流）：
//!   ① 本地逐条 stage（硬链接 + sessions 插行，失败即计 errors，不进批）；
//!   ② 1 个批量请求上云整批（`cloud_conv::migrate_conversations`），按逐条结果 register / 回滚。
//!
//! 判重 = inode 比对；保留名单 = 源 ∪ 目标逻辑会话按 cwd 分组取前 keep 条。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::modules::config::now_ms;
use crate::modules::session::{
    find_project_jsonl, insert_session_copy, is_claw_workspace, open_db,
    register_edge_sync_mapping, session_display_title, table_exists, workbuddy_db_path,
};
use crate::modules::variant::WbVariant;

// ---------------------------------------------------------------------------
// 硬链接共享（增量复制，2026-09-16 定稿）
// ---------------------------------------------------------------------------

/// 文件身份：同 inode 即同一份正文（硬链接共享的判重基石）。
///
/// Windows 返回 `(volume_serial_number, file_index)`（`GetFileInformationByHandle`；
/// std 的 `MetadataExt::file_index` 仍 unstable，故走 windows crate），
/// Unix 返回 `(dev, ino)`。读不到元数据返回 `None`（调用方按出错计）。
pub(crate) fn file_identity(p: &Path) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let f = std::fs::File::open(p).ok()?;
        let handle = HANDLE(f.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle, &mut info) }.ok()?;
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some((info.dwVolumeSerialNumber as u64, index))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(p).ok()?;
        Some((m.dev(), m.ino()))
    }
}

/// 共享会话的统一保留名单维度（2026-09-16 定稿，2026-09-17 补 `keep = 0` 语义）。
#[derive(Debug, Clone)]
pub struct LinkScope {
    /// `keep = 0` ⇒ **不限制数量**：目标还没有的会话全部共享（只共享、不清理时的语义）。
    /// `keep = N > 0` ⇒ 名单 = 源 ∪ 目标存活会话合并（同 inode 视为一条逻辑会话，
    /// 取 max(updated_at)），组内按活跃时间取前 N；名单外：源侧不动、目标侧由瘦身删除。
    pub keep: usize,
}

/// 从切号选项的 `slim_keep`（「清理旧对话」保留条数）构造共享范围。
/// 放本模块而非 `switch.rs`（2026-09-19）：上游发 PR 时本模块整体独占，
/// 不让 switch.rs 反向承载共享专属逻辑。
pub fn link_scope_from_keep(slim_keep: i64) -> LinkScope {
    LinkScope { keep: slim_keep.max(0) as usize }
}

impl Default for LinkScope {
    fn default() -> Self {
        // 与前端 keep 下拉（= slimKeep）一致
        Self { keep: 3 }
    }
}

/// 硬链接共享的**本地阶段**产物（阶段 1：硬链接 + sessions 插行，云端未动）。
pub(crate) struct StagedLink {
    /// 源会话 id。
    pub cid: String,
    /// 新会话 id（目标账号侧）。
    pub new_cid: String,
    /// 展示标题（建 conv 用）。
    pub title: String,
    /// 工作目录（建 conv 用）。
    pub cwd: String,
    /// 建 conv 用时间戳（源 created_at，缺省 now）。
    pub ts_ms: i64,
    /// 目标侧硬链接路径（回滚用）。
    pub dst_jsonl: PathBuf,
}

/// 批量提交的单条结果。
pub(crate) struct LinkOutcome {
    pub cid: String,
    pub new_cid: String,
    pub ok: bool,
    pub mapping_written: bool,
    pub error: Option<String>,
}

/// 回滚 staged 产物（删 sessions 行 + 删硬链接）——不留「本地有行云端无 conv」的假同步（坑 48）。
fn rollback_staged(db_path: &Path, s: &StagedLink) {
    if let Some(conn) = open_db(db_path, false) {
        let _ = conn.execute("DELETE FROM sessions WHERE id = ?1", [&s.new_cid]);
    }
    let _ = std::fs::remove_file(&s.dst_jsonl);
}

/// 把一条 staged 会话上云：先建 conv（单建兜底通道），成功才 register（坑 48 顺序铁律）。
/// 失败 ⇒ 回滚本地并返回错误结果。
fn fallback_single_commit(db_path: &Path, s: &StagedLink, token: &str, target_uid: &str) -> LinkOutcome {
    let mut out = LinkOutcome {
        cid: s.cid.clone(),
        new_cid: s.new_cid.clone(),
        ok: false,
        mapping_written: false,
        error: None,
    };
    match crate::modules::cloud_conv::create_conversation(token, &s.new_cid, &s.title, &s.cwd, s.ts_ms) {
        Ok(_) => {
            out.ok = true;
            out.mapping_written = register_edge_sync_mapping(WbVariant::Cn, &s.new_cid, target_uid);
        }
        Err(e) => {
            rollback_staged(db_path, s);
            out.error = Some(e);
        }
    }
    out
}

/// 生产批量提交（阶段 2）：**1 个 `migrations/legacy` 请求**打包整批（实测 1 请求绕开
/// 单建 conv 的 10 条/窗口限流，convId==sessionId）。单条未 imported / 批量整体失败时
/// 逐条单建兜底；兜底也失败 ⇒ 回滚该条本地产物，下次切号自动重试。
fn commit_staged_batch(db_path: &Path, staged: &[StagedLink], target_uid: &str) -> Vec<LinkOutcome> {
    let token = crate::modules::cloud_conv::token_of(target_uid).unwrap_or_default();
    if token.is_empty() {
        // 无法上云 ⇒ 全批回滚（半成品会被 inode 判重当 alreadyCopied，云端 conv 永缺）
        for s in staged {
            rollback_staged(db_path, s);
        }
        return staged
            .iter()
            .map(|s| LinkOutcome {
                cid: s.cid.clone(),
                new_cid: s.new_cid.clone(),
                ok: false,
                mapping_written: false,
                error: Some("目标账号 token 缺失".into()),
            })
            .collect();
    }
    let items: Vec<Value> = staged
        .iter()
        .map(|s| crate::modules::cloud_conv::migrate_item(&s.new_cid, &s.title, &s.cwd, s.ts_ms))
        .collect();
    let data = crate::modules::cloud_conv::migrate_conversations(&token, &items).ok();
    staged
        .iter()
        .map(|s| {
            let entry = data.as_ref().and_then(|d| d.get("results")).and_then(|r| r.get(&s.new_cid));
            let imported = entry
                .and_then(|e| e.get("status"))
                .and_then(|x| x.as_str())
                .map(|x| x == "imported")
                .unwrap_or(false);
            if imported {
                let mut out = LinkOutcome {
                    cid: s.cid.clone(),
                    new_cid: s.new_cid.clone(),
                    ok: true,
                    mapping_written: false,
                    error: None,
                };
                out.mapping_written = register_edge_sync_mapping(WbVariant::Cn, &s.new_cid, target_uid);
                return out;
            }
            // 非 imported（批量失败 / 服务端 failed|skipped）：先判云端有无，避免
            // 「回滚本地却留下云端 conv」或「本地留行而云端没建」两个极端。
            let exists = crate::modules::cloud_conv::conversation_exists(&token, &s.new_cid);
            match exists {
                Ok(true) => {
                    // 服务端其实建了 ⇒ register 即可
                    let mut out = LinkOutcome {
                        cid: s.cid.clone(),
                        new_cid: s.new_cid.clone(),
                        ok: true,
                        mapping_written: false,
                        error: None,
                    };
                    out.mapping_written = register_edge_sync_mapping(WbVariant::Cn, &s.new_cid, target_uid);
                    out
                }
                _ => fallback_single_commit(db_path, s, &token, target_uid),
            }
        })
        .collect()
}

/// 把源账号的**一条**会话以硬链接方式共享给目标账号（零拷贝正文）。
///
/// 步骤与顺序（顺序错了会复刻「手机端看不到」的假同步，坑 48）：
///   1. `hard_link` 源 jsonl → 目标目录 `<new_cid>.jsonl`（同目录必同卷，Windows 普通权限即可）；
///   2. `sessions` 表插行（复用 `insert_session_copy`：新 id / 目标 uid / 时间戳=now，其余原样）；
///   3. **先在云端建 conv**（单建端点；失败 ⇒ 回滚 1+2，不留半成品——半成品会被 inode 判重
///      永久跳过，云端 conv 缺失且无人补救）;
///   4. 再 register 映射行（失败不回滚：无映射行时 App edge-sync 启动会自己 MIGRATE 兜底）。
///
/// 批量场景（切号 autoLink）请走 [`link_missing_sessions`]：本地逐条 stage 后
/// 用 1 个批量请求上云，不受单建限流影响。
///
/// 不做备份（批量入口统一备份一次）；不做判重（调用方负责）。
pub fn link_shared_session(
    cid: &str,
    source_uid: &str,
    target_uid: &str,
) -> Result<Value, String> {
    let db = workbuddy_db_path(WbVariant::Cn);
    let s = stage_shared_link(cid, source_uid, target_uid, &db)?;

    // 3) 先建云端 conv（单建端点；失败回滚，不留半成品）
    let token = crate::modules::cloud_conv::token_of(target_uid)
        .ok_or_else(|| "目标账号 token 缺失".to_string());
    let conv_result = token.and_then(|token| {
        crate::modules::cloud_conv::create_conversation(&token, &s.new_cid, &s.title, &s.cwd, s.ts_ms)
    });
    if let Err(e) = conv_result {
        rollback_staged(&db, &s);
        return Err(e);
    }

    // 4) 再 register 映射行（失败不算错：App edge-sync 会对无映射行的本地会话自行 MIGRATE）
    let mapping_written = register_edge_sync_mapping(WbVariant::Cn, &s.new_cid, target_uid);

    Ok(json!({
        "id": cid,
        "newId": s.new_cid,
        "linked": true,
        "mappingWritten": mapping_written,
    }))
}

/// [`link_shared_session`] 的本地阶段：源行信息 → Claw 检查 → 硬链接 → sessions 插行。
/// 任何一步失败都**不产生本地残留**（硬链接失败/插行失败各自清理）。
fn stage_shared_link(
    cid: &str,
    source_uid: &str,
    target_uid: &str,
    db: &Path,
) -> Result<StagedLink, String> {
    // 0) 源行信息（cwd 判 Claw、标题与时间戳供建 conv 用）
    let (cwd, title, created_at) = {
        let conn = open_db(db, true).ok_or("workbuddy.db 打不开")?;
        if !table_exists(&conn, "sessions") {
            return Err("sessions 表不存在".into());
        }
        let (cwd, custom, title, created): (String, Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT cwd, custom_title, title, created_at FROM sessions \
                 WHERE id = ?1 AND user_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![cid, source_uid],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    ))
                },
            )
            .map_err(|_| format!("源会话不存在或已删除: {cid}"))?;
        (cwd, session_display_title(title, custom), created)
    };
    if is_claw_workspace(&cwd) {
        return Err("Claw 工作区绑定当前账号渠道，不支持共享".into());
    }

    // 1) 硬链接正文（必须有正文；无正文的会话共享出去也是空壳，批量层已过滤）
    let src_jsonl = find_project_jsonl(WbVariant::Cn, cid).ok_or_else(|| format!("找不到会话正文: {cid}"))?;
    let new_cid = uuid::Uuid::new_v4().to_string();
    let dst_jsonl = src_jsonl.with_file_name(format!("{new_cid}.jsonl"));
    std::fs::hard_link(&src_jsonl, &dst_jsonl)
        .map_err(|e| format!("硬链接创建失败: {e}"))?;

    // 2) sessions 插行
    if let Err(e) = insert_session_copy(db, &new_cid, cid, source_uid, target_uid) {
        let _ = std::fs::remove_file(&dst_jsonl);
        return Err(format!("sessions 写入失败: {e}"));
    }

    let ts_ms = if created_at > 0 { created_at } else { now_ms() };
    Ok(StagedLink {
        cid: cid.to_string(),
        new_cid,
        title,
        cwd,
        ts_ms,
        dst_jsonl,
    })
}

/// 增量共享：把源账号「目标还没有」的存活会话零拷贝共享过去。
///
/// 判重 = **inode 比对**（硬链接方案下天然血缘，设计文档 §3.1 血缘表已划掉）：
/// 源会话正文的 inode 在目标项目目录已出现且对应文件属目标账号 ⇒ `alreadyCopied`。
/// 该判重同时天然覆盖「复制体不得再作为源」（来回切号不会膨胀）。
///
/// 两阶段（2026-09-16 改造，绕开单建 conv 的 10 条/窗口限流）：
///   ① 本地逐条 stage（硬链接+插行，失败即计 errors，不进批）；
///   ② **1 个批量请求**上云整批，按逐条结果 register/回滚。
///
/// 报告字段（坑 44：避开已被对账阶段占用的 `skipped` 字符串）：
/// `copied[]` / `alreadyCopied` / `beyondKeep` / `clawSkipped` / `noBody` / `errors[]` / `keepTargetSids`。
/// `dry_run=true` 只统计，不落盘不建 conv。
pub fn link_missing_sessions(
    source_uid: &str,
    target_uid: &str,
    scope: &LinkScope,
    dry_run: bool,
) -> Value {
    let db = workbuddy_db_path(WbVariant::Cn);
    let prep_one = |cid: &str| -> Result<StagedLink, String> {
        if dry_run {
            return Err(format!("__dry_run__:{cid}"));
        }
        stage_shared_link(cid, source_uid, target_uid, &db)
    };
    let commit_batch = |staged: &[StagedLink]| -> Vec<LinkOutcome> {
        commit_staged_batch(&db, staged, target_uid)
    };
    let mut report = link_missing_sessions_in(
        &db,
        &crate::modules::config::home_dir().join(".workbuddy").join("projects"),
        source_uid,
        target_uid,
        scope,
        dry_run,
        prep_one,
        commit_batch,
    );
    report["sourceUid"] = json!(source_uid);
    report["targetUid"] = json!(target_uid);
    report["dryRun"] = json!(dry_run);
    report
}

/// 保留名单计算（2026-09-16 主人定稿）：
/// 源 ∪ 目标存活会话合并成**逻辑会话**（同 inode = 同一份正文 = 同一条，取 max(updated_at)；
/// 无正文的会话独立成条），按 cwd 项目分组，组内按活跃时间取前 `keep` 条。
///
/// 返回 `(入选源 sid 集合, 入选目标 sid 集合)`：
/// - 源侧：名单内才共享（`link_missing_sessions_in` 用）；
/// - 目标侧：名单外由瘦身删除（`switch.rs` 把它串给 slim 用）。
fn compute_keep_sids(
    projects_dir: &Path,
    sources: &[(String, String, i64)],          // (sid, cwd, updated_at)
    targets: &[(String, String, i64)],          // 同上（目标账号）
    keep: usize,
) -> (std::collections::HashSet<String>, Vec<String>) {
    use std::collections::{HashMap, HashSet};
    let keep = keep.max(1);

    // 全局 sid → inode（同目录扫描：源件与共享副本同名目录共存，各自指向同一 inode）
    let mut sid_inode: HashMap<String, (u64, u64)> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(projects_dir) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            for (ident, sid) in scan_dir_inodes(&e.path()) {
                sid_inode.insert(sid, ident);
            }
        }
    }

    // 逻辑会话合并：key = inode 或 "sid:<id>"（无正文）
    struct Logic {
        cwd: String,
        updated_at: i64,
        sids: Vec<String>,
    }
    let mut logic: HashMap<String, Logic> = HashMap::new();
    let mut merge = |sid: &str, cwd: &str, updated_at: i64, has_inode: bool, ident: (u64, u64)| {
        let key = if has_inode {
            format!("i:{}:{}", ident.0, ident.1)
        } else {
            format!("s:{sid}")
        };
        let e = logic.entry(key).or_insert_with(|| Logic {
            cwd: cwd.to_string(),
            updated_at,
            sids: Vec::new(),
        });
        if updated_at > e.updated_at {
            e.updated_at = updated_at;
        }
        if e.cwd.is_empty() {
            e.cwd = cwd.to_string();
        }
        e.sids.push(sid.to_string());
    };
    for (sid, cwd, updated_at) in sources {
        let ident = sid_inode.get(sid).copied();
        merge(sid, cwd, *updated_at, ident.is_some(), ident.unwrap_or((0, 0)));
    }
    for (sid, cwd, updated_at) in targets {
        let ident = sid_inode.get(sid).copied();
        merge(sid, cwd, *updated_at, ident.is_some(), ident.unwrap_or((0, 0)));
    }

    // 按项目分组，组内取活跃最新前 N
    let mut by_cwd: HashMap<&str, Vec<&Logic>> = HashMap::new();
    for l in logic.values() {
        by_cwd.entry(l.cwd.as_str()).or_default().push(l);
    }
    let mut keep_source: HashSet<String> = HashSet::new();
    let mut keep_target: Vec<String> = Vec::new();
    for list in by_cwd.values_mut() {
        list.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        for l in list.iter().take(keep) {
            for sid in &l.sids {
                if sources.iter().any(|(s, _, _)| s == sid) {
                    keep_source.insert(sid.clone());
                }
                if targets.iter().any(|(s, _, _)| s == sid) {
                    keep_target.push(sid.clone());
                }
            }
        }
    }
    (keep_source, keep_target)
}

/// 扫描阶段的纯计数（不含 staged，便于单独断言）。
#[derive(Default)]
struct ScanCounts {
    already: usize,
    beyond_keep: usize,
    claw: usize,
    no_body: usize,
}

/// [`collect_staged_links`] 的结果：待上云的批次 + dry-run 计划 + 计数 + 逐条错误。
struct ScanOutcome {
    staged: Vec<StagedLink>,
    planned: Vec<String>,
    errors: Vec<Value>,
    counts: ScanCounts,
}

/// 读取目标/源账号的存活会话：`(目标 (id, cwd, updated_at), 源 (id, cwd?, updated_at))`。
///
/// 源按 `updated_at DESC`（最新优先，供量控截断）。失败返回错误文案，
/// 由调用方写进 `report["error"]` —— 与拆分前逐条 early-return 的文案一致。
/// 会话行：`(id, cwd, updated_at)`。
type SessionRow = (String, String, i64);
/// 目标侧会话行：`(id, cwd（可能为空）, updated_at)`。
type TargetRow = (String, Option<String>, i64);

fn load_sessions_for_link(
    db_path: &Path,
    source_uid: &str,
    target_uid: &str,
) -> Result<(Vec<SessionRow>, Vec<TargetRow>), String> {
    let Some(conn) = open_db(db_path, true) else {
        return Err("workbuddy.db 打不开".to_string());
    };
    if !table_exists(&conn, "sessions") {
        return Err("sessions 表不存在".to_string());
    }
    let target_sessions: Vec<(String, String, i64)> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, COALESCE(cwd,''), COALESCE(updated_at,0) FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL",
        ) else {
            return Err("查询目标会话失败".to_string());
        };
        let Ok(rows) = stmt.query_map([target_uid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) else {
            return Err("查询目标会话失败".to_string());
        };
        rows.flatten().collect()
    };
    let sources: Vec<(String, Option<String>, i64)> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, cwd, updated_at FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC",
        ) else {
            return Err("查询源会话失败".to_string());
        };
        let Ok(rows) = stmt.query_map([source_uid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        }) else {
            return Err("查询源会话失败".to_string());
        };
        rows.flatten().collect()
    };
    Ok((target_sessions, sources))
}

/// 阶段 1：逐条判重 → 硬链接 → 插行 → 进批（不碰网络，失败只计 errors）。
///
/// `prep_one` = 单条本地准备（硬链接+插行）；`dry_run` 时只产出 `planned`，不 stage。
fn collect_staged_links<F1>(
    projects_dir: &Path,
    sources: &[(String, Option<String>, i64)],
    target_alive: &std::collections::HashSet<String>,
    keep_source_sids: Option<&std::collections::HashSet<String>>,
    dry_run: bool,
    mut prep_one: F1,
) -> ScanOutcome
where
    F1: FnMut(&str) -> Result<StagedLink, String>,
{
    let mut out = ScanOutcome {
        staged: Vec::new(),
        planned: Vec::new(),
        errors: Vec::new(),
        counts: ScanCounts::default(),
    };
    // 每个项目目录的 inode → sid 索引（懒构建一次，同目录反复判重不重扫）
    let mut dir_index: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<(u64, u64), String>,
    > = std::collections::HashMap::new();

    for (cid, cwd, _updated_at) in sources {
        let cwd = cwd.clone().unwrap_or_default();
        if is_claw_workspace(&cwd) {
            out.counts.claw += 1;
            continue;
        }
        let Some(src_jsonl) = find_project_jsonl_in(projects_dir, cid) else {
            out.counts.no_body += 1;
            continue;
        };
        // 硬链接与判重都以源文件**实际所在目录**为准（App 按 cwd→workspace 找正文，
        // 源文件所在目录就是权威位置；不按 cwd 反推目录名，避免两套编码规则漂移）
        let Some(dst_dir) = src_jsonl.parent().map(|p| p.to_path_buf()) else {
            out.counts.no_body += 1;
            continue;
        };
        let Some(ident) = file_identity(&src_jsonl) else {
            out.errors
                .push(json!({ "id": cid, "error": "读正文元数据失败" }));
            continue;
        };
        // 判重：该 inode 在目标目录出现，且对应文件属目标账号存活会话
        let index = dir_index.entry(dst_dir.clone()).or_insert_with(|| {
            scan_dir_inodes(&dst_dir)
                .into_iter()
                .filter(|(_, sid)| target_alive.contains(sid))
                .collect()
        });
        if index.contains_key(&ident) {
            out.counts.already += 1;
            continue;
        }
        // 保留名单：只在有限制时过滤；`None` = 不限制（目标没有的都搬）
        if let Some(keep) = keep_source_sids {
            if !keep.contains(cid) {
                out.counts.beyond_keep += 1;
                continue;
            }
        }
        // dry_run：只统计，不 stage 不上云
        if dry_run {
            out.planned.push(cid.clone());
            continue;
        }
        match prep_one(cid) {
            Ok(s) => {
                index.insert(ident, s.new_cid.clone());
                out.staged.push(s);
            }
            Err(e) => out.errors.push(json!({ "id": cid, "error": e })),
        }
    }
    out
}

/// 阶段 2：整批上云（1 个批量请求），按逐条结果进 copied/errors。
fn apply_commit_batch<F2>(report: &mut Value, staged: &[StagedLink], mut commit_batch: F2)
where
    F2: FnMut(&[StagedLink]) -> Vec<LinkOutcome>,
{
    if staged.is_empty() {
        return;
    }
    for o in commit_batch(staged) {
        if o.ok {
            report["copied"].as_array_mut().unwrap().push(json!({
                "id": o.cid, "newId": o.new_cid, "linked": true,
                "mappingWritten": o.mapping_written,
            }));
        } else {
            report["errors"].as_array_mut().unwrap().push(json!({
                "id": o.cid, "error": o.error.unwrap_or_default(),
            }));
        }
    }
}

/// [`link_missing_sessions`] 的可注入核心（生产给真实路径与真实动作，测试可替换）。
///
/// 只做编排：读库 → 算保留名单 → 阶段 1 本地 stage → 阶段 2 整批上云 → 汇总计数。
/// `prep_one` = 单条本地准备（硬链接+插行）；`commit_batch` = 整批上云（批量端点）。
///
/// ⚠️ 参数 8 个（超 clippy 默认阈值 7）是**有意保留**：前 6 个是路径与身份上下文，后 2 个
/// 是注入点。收成结构体字面量会让 8 个测试调用点更啰嗦、可读性反而下降 ⇒ 用 allow 标注。
#[allow(clippy::too_many_arguments)]
fn link_missing_sessions_in<F1, F2>(
    db_path: &Path,
    projects_dir: &Path,
    source_uid: &str,
    target_uid: &str,
    scope: &LinkScope,
    dry_run: bool,
    prep_one: F1,
    commit_batch: F2,
) -> Value
where
    F1: FnMut(&str) -> Result<StagedLink, String>,
    F2: FnMut(&[StagedLink]) -> Vec<LinkOutcome>,
{
    let mut report = json!({
        "copied": [],
        "alreadyCopied": 0,
        "beyondKeep": 0,
        "clawSkipped": 0,
        "noBody": 0,
        "errors": [],
    });

    let (target_sessions, sources) =
        match load_sessions_for_link(db_path, source_uid, target_uid) {
            Ok(v) => v,
            Err(e) => {
                report["error"] = json!(e);
                return report;
            }
        };
    let target_alive: std::collections::HashSet<String> =
        target_sessions.iter().map(|(id, _, _)| id.clone()).collect();

    // 保留名单：`keep = 0` ⇒ 不限制（只共享的场景，目标没有的全搬）；
    // `keep = N > 0` ⇒ 源 ∪ 目标合并、按项目分组取最新 N 条（inode 合并同会话）。
    let keep_source_sids = if scope.keep == 0 {
        report["keepTargetSids"] = json!([]);
        None
    } else {
        let src_for_keep: Vec<(String, String, i64)> = sources
            .iter()
            .map(|(id, cwd, ts)| (id.clone(), cwd.clone().unwrap_or_default(), *ts))
            .collect();
        let (keep_source, keep_target) =
            compute_keep_sids(projects_dir, &src_for_keep, &target_sessions, scope.keep);
        report["keepTargetSids"] = json!(keep_target);
        Some(keep_source)
    };

    let out = collect_staged_links(
        projects_dir,
        &sources,
        &target_alive,
        keep_source_sids.as_ref(),
        dry_run,
        prep_one,
    );
    for cid in &out.planned {
        report["copied"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "id": cid, "newId": "", "linked": true, "planned": true }));
    }
    for e in &out.errors {
        report["errors"].as_array_mut().unwrap().push(e.clone());
    }
    apply_commit_batch(&mut report, &out.staged, commit_batch);

    report["alreadyCopied"] = json!(out.counts.already);
    report["beyondKeep"] = json!(out.counts.beyond_keep);
    report["clawSkipped"] = json!(out.counts.claw);
    report["noBody"] = json!(out.counts.no_body);
    report
}

/// `find_project_jsonl` 的可注入版：先直接拼 `{projects}/{cid}.jsonl`，
/// 再遍历一级子目录（workspace 目录名编码了 cwd，见 projects_anchor）。
fn find_project_jsonl_in(projects_dir: &Path, cid: &str) -> Option<PathBuf> {
    if !projects_dir.is_dir() {
        return None;
    }
    let direct = projects_dir.join(format!("{cid}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(projects_dir).ok()?.flatten() {
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

/// 扫描目录下全部 `*.jsonl` 的 inode → sid。
fn scan_dir_inodes(dir: &Path) -> Vec<((u64, u64), String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        if p.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let Some(sid) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(ident) = file_identity(&p) {
            out.push((ident, sid.to_string()));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 共享会话身份改写（2026-09-18 实验定稿，主人拍板实施）
// ---------------------------------------------------------------------------
//
// 背景（reports/diag-stuck-session-2026-09-17.md）：硬链接共享后正文内嵌源 sid，
// CLI resume 认文件内容 ⇒ 记账/频控键挂源会话，源账号窗口刷爆则目标账号全员 429。
//
// 方案（切号时执行，App 已关）：把共享族正文内嵌的**全部族内 sid** 等长替换为
// 目标账号 sid。实验闭环两轮实测：
//   - 只改末轮无效（00:02 实测证伪「取末尾」规则）⇒ 必须全量替换；
//   - 全量等长替换 + `r+b` 原地写 ⇒ inode 不变 ⇒ **硬链接 nlink 保持**，兄弟文件实时同步；
//   - 改写后 `session-resume {sessionId == conversationId}`，身份跟随活跃账号。

/// 等长字节替换（二进制安全，不要求 UTF-8）。返回 `(新缓冲, 替换次数)`。
///
/// 单遍扫描，计数 = 实际替换次数（非重叠匹配）。
fn replace_all_bytes(data: &[u8], from: &[u8], to: &[u8]) -> (Vec<u8>, usize) {
    debug_assert_eq!(from.len(), to.len(), "调用方保证等长");
    let mut out = Vec::with_capacity(data.len());
    let mut n = 0usize;
    let mut i = 0;
    while i < data.len() {
        if from.len() > 0 && i + from.len() <= data.len() && &data[i..i + from.len()] == from {
            out.extend_from_slice(to);
            i += from.len();
            n += 1;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    (out, n)
}

/// 读目标账号存活会话行 `(id, cwd, updated_at)`。
fn load_target_sessions(
    db_path: &Path,
    target_uid: &str,
) -> Result<Vec<(String, String, i64)>, String> {
    let Some(conn) = open_db(db_path, true) else {
        return Err("workbuddy.db 打不开".to_string());
    };
    if !table_exists(&conn, "sessions") {
        return Err("sessions 表不存在".into());
    }
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, COALESCE(cwd,''), COALESCE(updated_at,0) FROM sessions \
         WHERE user_id = ?1 AND deleted_at IS NULL",
    ) else {
        return Err("查询目标会话失败".to_string());
    };
    let Ok(rows) = stmt.query_map([target_uid], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    }) else {
        return Err("查询目标会话失败".to_string());
    };
    Ok(rows.flatten().collect())
}

/// [`rewrite_shared_session_sids`] 的可注入核心（生产给真实路径，测试可替换）。
///
/// 算法：扫 projects 全目录建 sid→inode 索引 → 按 inode 分组（同 inode = 同一份正文
/// = 一个共享族）→ 族内目标账号 sid（多个取 updated_at 最新）为新身份 → 族内其余
/// sid 全文等长替换为新身份。替换数 > 0 时先备份再 `r+b` 原地写回（inode 不变）。
/// `dry_run = true` 只产出 planned 计划，不落盘。
fn rewrite_shared_sids_in(
    projects_dir: &Path,
    target_rows: &[(String, String, i64)],
    backup_dir: Option<&Path>,
    dry_run: bool,
) -> Value {
    let mut report = json!({
        "rewritten": [],
        "aligned": 0,
        "errors": [],
        "dryRun": dry_run,
    });
    let target_updated: std::collections::HashMap<&str, i64> = target_rows
        .iter()
        .map(|(id, _, ts)| (id.as_str(), *ts))
        .collect();
    let target_alive: std::collections::HashSet<&str> =
        target_rows.iter().map(|(id, _, _)| id.as_str()).collect();

    // inode → 族成员（sid, 文件路径）。扫根目录 + 各 workspace 子目录。
    let mut families: std::collections::HashMap<
        (u64, u64),
        Vec<(String, PathBuf)>,
    > = std::collections::HashMap::new();
    let scan_one = |dir: &Path, families: &mut std::collections::HashMap<(u64, u64), Vec<(String, PathBuf)>>| {
        for (ident, sid) in scan_dir_inodes(dir) {
            let path = dir.join(format!("{sid}.jsonl"));
            families.entry(ident).or_default().push((sid, path));
        }
    };
    scan_one(projects_dir, &mut families);
    if let Ok(entries) = std::fs::read_dir(projects_dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                scan_one(&e.path(), &mut families);
            }
        }
    }

    let mut processed: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for (ident, members) in families {
        // 同 inode 跨目录只处理一次（罕见，防御性去重）
        if !processed.insert(ident) {
            continue;
        }
        // 族内目标账号 sid：多个取 updated_at 最新的为权威身份；没有 ⇒ 该族与本账号无关
        let Some(target_sid) = members
            .iter()
            .filter(|(sid, _)| target_alive.contains(sid.as_str()))
            .map(|(sid, _)| sid.as_str())
            .max_by_key(|sid| target_updated.get(sid).copied().unwrap_or(0))
        else {
            continue;
        };
        let Some((_, file)) = members.iter().find(|(sid, _)| *sid == target_sid) else {
            continue;
        };
        if members.len() == 1 {
            // 单成员族 = 本账号自己的非共享会话，身份天然正确 ⇒ 无需读文件
            report["aligned"] = json!(report["aligned"].as_i64().unwrap_or(0) + 1);
            continue;
        }
        let Ok(data) = std::fs::read(file) else {
            report["errors"].as_array_mut().unwrap().push(json!({
                "file": file.display().to_string(), "error": "读正文失败",
            }));
            continue;
        };
        // 逐个族内旧 sid 等长替换
        let mut buf = data.clone();
        let mut replaced: Vec<Value> = Vec::new();
        for (sid, _) in &members {
            if sid.as_str() == target_sid {
                continue;
            }
            if sid.len() != target_sid.len() {
                report["errors"].as_array_mut().unwrap().push(json!({
                    "from": sid, "to": target_sid,
                    "error": "新旧 sid 长度不等，拒绝原地改写",
                }));
                continue;
            }
            let (next, n) = replace_all_bytes(&buf, sid.as_bytes(), target_sid.as_bytes());
            if n > 0 {
                buf = next;
                replaced.push(json!({ "from": sid, "count": n }));
            }
        }
        if replaced.is_empty() {
            // 已对齐（正文里只剩目标 sid）⇒ 幂等跳过
            report["aligned"] = json!(report["aligned"].as_i64().unwrap_or(0) + 1);
            continue;
        }
        if dry_run {
            report["rewritten"].as_array_mut().unwrap().push(json!({
                "targetSid": target_sid,
                "file": file.display().to_string(),
                "planned": true,
                "replaced": replaced,
            }));
            continue;
        }
        // 备份 + r+b 原地写回（write 不截断 ⇒ 尺寸不变 ⇒ inode 不变 ⇒ 硬链接保持）
        if let Some(bd) = backup_dir {
            let _ = std::fs::create_dir_all(bd);
            if let Some(name) = file.file_name() {
                let _ = std::fs::copy(file, bd.join(name));
            }
        }
        let write_result = (|| -> std::io::Result<()> {
            // 写前复查尺寸：读后写前若被并发追加，write_all 会留下错位尾部 ⇒ 拒写
            let cur = std::fs::metadata(file)?.len();
            if cur != buf.len() as u64 {
                return Err(std::io::Error::other(format!(
                    "正文尺寸已变化（磁盘 {cur} ≠ 预期 {}），疑并发修改，跳过本次改写",
                    buf.len()
                )));
            }
            std::fs::OpenOptions::new()
                .write(true)
                .open(file)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(&buf).and_then(|_| f.flush())
                })
        })();
        match write_result {
            Ok(_) => {
                report["rewritten"].as_array_mut().unwrap().push(json!({
                    "targetSid": target_sid,
                    "file": file.display().to_string(),
                    "replaced": replaced,
                }));
            }
            Err(e) => {
                report["errors"].as_array_mut().unwrap().push(json!({
                    "file": file.display().to_string(),
                    "error": format!("原地写回失败: {e}"),
                }));
            }
        }
    }
    report
}

/// 切号时把共享族正文内嵌 sid 原地改写为目标账号 sid（记账/频控键随活跃账号走）。
///
/// 前置：App 已关（switch 链路在 `close_workbuddy` 之后调用）。幂等：已对齐的族
/// 自动跳过。失败只计报告，不阻断切号主流程。
pub fn rewrite_shared_session_sids(target_uid: &str, dry_run: bool) -> Value {
    let db = workbuddy_db_path(WbVariant::Cn);
    let mut report = match load_target_sessions(&db, target_uid) {
        Ok(rows) => {
            let backup_dir = (!dry_run).then(|| {
                crate::modules::config::backup_dir()
                    .join("session_sid_rewrite")
                    .join(crate::modules::config::utc_iso())
            });
            rewrite_shared_sids_in(
                &crate::modules::config::home_dir()
                    .join(".workbuddy")
                    .join("projects"),
                &rows,
                backup_dir.as_deref(),
                dry_run,
            )
        }
        Err(e) => json!({
            "rewritten": [], "aligned": 0,
            "errors": [{ "error": e }], "dryRun": dry_run,
        }),
    };
    report["targetUid"] = json!(target_uid);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::Path;

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_switch_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// 建一个本次用例独占的临时目录（Windows 下并发跑用例不会撞名）。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wb_switch_{name}_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn temp_projects() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wb_switch_proj_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp projects");
        dir
    }

    /// 建一个 sessions 表（最小列集），插入源/目标会话行。
    fn setup_sessions_db(path: &Path, rows: &[(&str, &str, i64)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                user_id TEXT,
                cwd TEXT,
                created_at INTEGER,
                updated_at INTEGER,
                deleted_at INTEGER
            );",
        )
        .unwrap();
        for (id, uid, updated) in rows {
            conn.execute(
                "INSERT INTO sessions (id, user_id, cwd, created_at, updated_at, deleted_at)
                 VALUES (?1, ?2, 'D:\\ws\\proj', 1, ?3, NULL)",
                rusqlite::params![id, uid, updated],
            )
            .unwrap();
        }
    }

    fn scope(keep: usize) -> LinkScope {
        LinkScope { keep }
    }

    /// 源会话有正文、目标为空 ⇒ copied 1 条（走两阶段注入：stage 1 条 + 批量提交 1 次）。
    #[test]
    fn link_missing_copies_when_target_empty() {
        let db = temp_db("link-empty");
        setup_sessions_db(&db, &[("src-1", "uid-a", 9_999)]);
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-1.jsonl"), b"body").unwrap();

        let mut commit_calls = 0usize;
        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(50),
            false,
            |cid| {
                Ok(StagedLink {
                    cid: cid.to_string(),
                    new_cid: format!("{cid}-new"),
                    title: "t".into(),
                    cwd: "D:\\ws\\proj".into(),
                    ts_ms: 1,
                    dst_jsonl: PathBuf::new(),
                })
            },
            |staged| {
                commit_calls += 1;
                assert_eq!(staged.len(), 1);
                staged
                    .iter()
                    .map(|s| LinkOutcome {
                        cid: s.cid.clone(),
                        new_cid: s.new_cid.clone(),
                        ok: true,
                        mapping_written: true,
                        error: None,
                    })
                    .collect()
            },
        );
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert_eq!(report["copied"][0]["newId"], "src-1-new");
        assert_eq!(report["alreadyCopied"], 0);
        assert_eq!(commit_calls, 1, "整批只提交 1 次");
    }

    /// 目标目录已存在同 inode 的文件且属目标账号 ⇒ alreadyCopied，不再进入任何阶段。
    #[test]
    fn link_missing_skips_same_inode_of_target() {
        let db = temp_db("link-inode");
        setup_sessions_db(&db, &[("src-1", "uid-a", 9_999), ("dst-1", "uid-b", 9_998)]);
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-1.jsonl"), b"shared body").unwrap();
        std::fs::hard_link(ws.join("src-1.jsonl"), ws.join("dst-1.jsonl"))
            .expect("hard_link 应成功（同目录同卷）");

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(50),
            false,
            |_| Err("不该被调用".into()),
            |_| Vec::new(),
        );
        assert_eq!(report["alreadyCopied"], 1, "同 inode + 属目标账号 ⇒ 判已共享");
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
        assert!(report["errors"].as_array().unwrap().is_empty());
    }

    /// 源自己也在目标目录里（inode 命中但属源账号）⇒ 不算已共享（目标 uid 过滤生效）。
    #[test]
    fn link_missing_ignores_inode_of_source_side() {
        let db = temp_db("link-self");
        // 目标账号名下没有任何会话
        setup_sessions_db(&db, &[("src-1", "uid-a", 9_999)]);
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-1.jsonl"), b"body").unwrap();

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(50),
            false,
            |cid| {
                Ok(StagedLink {
                    cid: cid.to_string(),
                    new_cid: "n1".into(),
                    title: "t".into(),
                    cwd: String::new(),
                    ts_ms: 1,
                    dst_jsonl: PathBuf::new(),
                })
            },
            |staged| {
                staged
                    .iter()
                    .map(|s| LinkOutcome {
                        cid: s.cid.clone(),
                        new_cid: s.new_cid.clone(),
                        ok: true,
                        mapping_written: false,
                        error: None,
                    })
                    .collect()
            },
        );
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
    }

    /// Claw 工作区跳过计数；无正文跳过计数。
    #[test]
    fn link_missing_counts_claw_and_no_body() {
        let db = temp_db("link-skip");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT, cwd TEXT, created_at INTEGER, updated_at INTEGER, deleted_at INTEGER);
             INSERT INTO sessions VALUES ('src-claw', 'uid-a', 'D:\\ws\\Claw', 1, 9999, NULL);
             INSERT INTO sessions VALUES ('src-nobody', 'uid-a', 'D:\\ws\\proj', 1, 9998, NULL);",
        )
        .unwrap();
        let projects = temp_projects();

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(50),
            false,
            |_| Err("不该被调用".into()),
            |_| Vec::new(),
        );
        assert_eq!(report["clawSkipped"], 1);
        assert_eq!(report["noBody"], 1);
        assert_eq!(report["copied"].as_array().unwrap().len(), 0);
    }

    /// 保留名单：keep=1 时同项目只有最新 1 条进名单，旧的进 beyondKeep（源侧不动）。
    #[test]
    fn link_missing_keep_list_gates_sharing() {
        let now = crate::modules::config::now_ms();
        let db = temp_db("link-keep");
        setup_sessions_db(
            &db,
            &[
                ("src-new", "uid-a", now),
                ("src-old", "uid-a", 100), // 同项目更旧 ⇒ 名单外
            ],
        );
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-new.jsonl"), b"n").unwrap();
        std::fs::write(ws.join("src-old.jsonl"), b"o").unwrap();

        let mk_prep = |cid: &str| {
            Ok(StagedLink {
                cid: cid.to_string(),
                new_cid: format!("{cid}-new"),
                title: "t".into(),
                cwd: String::new(),
                ts_ms: 1,
                dst_jsonl: PathBuf::new(),
            })
        };
        let mk_commit = |staged: &[StagedLink]| {
            staged
                .iter()
                .map(|s| LinkOutcome {
                    cid: s.cid.clone(),
                    new_cid: s.new_cid.clone(),
                    ok: true,
                    mapping_written: false,
                    error: None,
                })
                .collect()
        };

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(1),
            false,
            mk_prep,
            mk_commit,
        );
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert_eq!(report["copied"][0]["id"], "src-new", "名单内 = 项目里最新活跃的");
        assert_eq!(report["beyondKeep"], 1, "名单外的源会话不动，只计数");
        assert!(report.get("deferred").is_none(), "deferred 已废除");
    }

    /// keep = 0 ⇒ **不限制数量**：目标还没有的会话全部共享（只共享、不清理的场景）。
    #[test]
    fn link_missing_keep_zero_shares_everything() {
        let now = crate::modules::config::now_ms();
        let db = temp_db("link-keep0");
        setup_sessions_db(
            &db,
            &[
                ("src-new", "uid-a", now),
                ("src-old", "uid-a", 100), // 同项目更旧：keep=0 时也照样共享
            ],
        );
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-new.jsonl"), b"n").unwrap();
        std::fs::write(ws.join("src-old.jsonl"), b"o").unwrap();

        let mk_prep = |cid: &str| {
            Ok(StagedLink {
                cid: cid.to_string(),
                new_cid: format!("{cid}-new"),
                title: "t".into(),
                cwd: String::new(),
                ts_ms: 1,
                dst_jsonl: PathBuf::new(),
            })
        };
        let mk_commit = |staged: &[StagedLink]| {
            staged
                .iter()
                .map(|s| LinkOutcome {
                    cid: s.cid.clone(),
                    new_cid: s.new_cid.clone(),
                    ok: true,
                    mapping_written: false,
                    error: None,
                })
                .collect::<Vec<_>>()
        };

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(0),
            false,
            mk_prep,
            mk_commit,
        );
        assert_eq!(report["copied"].as_array().unwrap().len(), 2, "keep=0 不限制条数");
        assert_eq!(report["beyondKeep"], 0, "不限制时没有『名单外』");
        assert_eq!(
            report["keepTargetSids"].as_array().unwrap().len(),
            0,
            "不限制时不给瘦身名单"
        );
    }


    /// dry_run：只统计不落盘——prep/commit 都不被调用，copied 带 planned 标记。
    #[test]
    fn link_missing_dry_run_skips_all_actions() {
        let db = temp_db("link-dry");
        setup_sessions_db(&db, &[("src-1", "uid-a", 9_999)]);
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-1.jsonl"), b"body").unwrap();

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(50),
            true,
            |_| panic!("dry_run 不应 stage"),
            |_| panic!("dry_run 不应提交"),
        );
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert_eq!(report["copied"][0]["planned"], true);
    }

    /// 批量提交部分失败：成功的进 copied，失败的进 errors。
    #[test]
    fn link_missing_partial_commit_failure_splits_report() {
        let db = temp_db("link-partial");
        setup_sessions_db(&db, &[("src-1", "uid-a", 9_999), ("src-2", "uid-a", 9_998)]);
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("src-1.jsonl"), b"1").unwrap();
        std::fs::write(ws.join("src-2.jsonl"), b"2").unwrap();

        let report = link_missing_sessions_in(
            &db,
            &projects,
            "uid-a",
            "uid-b",
            &scope(50),
            false,
            |cid| {
                Ok(StagedLink {
                    cid: cid.to_string(),
                    new_cid: format!("{cid}-new"),
                    title: "t".into(),
                    cwd: String::new(),
                    ts_ms: 1,
                    dst_jsonl: PathBuf::new(),
                })
            },
            |staged| {
                staged
                    .iter()
                    .enumerate()
                    .map(|(i, s)| LinkOutcome {
                        cid: s.cid.clone(),
                        new_cid: s.new_cid.clone(),
                        ok: i == 0, // 第二条模拟失败
                        mapping_written: i == 0,
                        error: if i == 0 { None } else { Some("批量条目失败".into()) },
                    })
                    .collect()
            },
        );
        assert_eq!(report["copied"].as_array().unwrap().len(), 1);
        assert_eq!(report["errors"].as_array().unwrap().len(), 1);
        assert_eq!(report["errors"][0]["id"], "src-2");
    }

    /// file_identity：同一文件两次读取一致；硬链接两路径身份相同；不同文件不同。
    #[test]
    fn file_identity_distinguishes_hardlinks_and_files() {
        let dir = temp_dir("identity");
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let c = dir.join("c.jsonl");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&c, b"other").unwrap();
        std::fs::hard_link(&a, &b).unwrap();

        let ia = file_identity(&a).expect("identity a");
        let ib = file_identity(&b).expect("identity b");
        let ic = file_identity(&c).expect("identity c");
        assert_eq!(ia, ib, "硬链接两路径身份相同");
        assert_ne!(ia, ic, "不同文件身份不同");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const SID_A: &str = "11111111-1111-4111-8111-111111111111";
    const SID_B: &str = "22222222-2222-4222-8222-222222222222";
    const SID_C: &str = "33333333-3333-4333-8333-333333333333";

    /// 全量改写：族内源 sid → 目标 sid，硬链接 inode 保持（核心卖点）。
    #[test]
    fn rewrite_flips_embedded_sid_and_keeps_hardlink() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_A}.jsonl"));
        let dst = ws.join(format!("{SID_B}.jsonl"));
        std::fs::write(&src, format!("x {SID_A} y {SID_A} z")).unwrap();
        std::fs::hard_link(&src, &dst).expect("hard_link");

        let report = rewrite_shared_sids_in(
            &projects,
            &[(SID_B.to_string(), "D:\\ws\\proj".into(), 100)],
            None,
            false,
        );
        assert_eq!(report["rewritten"].as_array().unwrap().len(), 1);
        assert_eq!(report["rewritten"][0]["targetSid"], SID_B);
        let body = std::fs::read_to_string(&src).unwrap();
        assert_eq!(body, format!("x {SID_B} y {SID_B} z"), "源 sid 全文翻转");
        assert_eq!(
            file_identity(&src), file_identity(&dst),
            "r+b 原地写 ⇒ inode 不变 ⇒ 硬链接保持"
        );
    }

    /// 幂等：已对齐的族第二轮 aligned 计数，内容不变。
    #[test]
    fn rewrite_is_idempotent() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_A}.jsonl"));
        let dst = ws.join(format!("{SID_B}.jsonl"));
        std::fs::write(&src, format!("only {SID_B} here")).unwrap();
        std::fs::hard_link(&src, &dst).unwrap();

        let report = rewrite_shared_sids_in(
            &projects,
            &[(SID_B.to_string(), "D:\\ws\\proj".into(), 100)],
            None,
            false,
        );
        assert_eq!(report["aligned"].as_i64(), Some(1));
        assert!(report["rewritten"].as_array().unwrap().is_empty());
        let body = std::fs::read_to_string(&src).unwrap();
        assert_eq!(body, format!("only {SID_B} here"), "内容不被改动");
    }

    /// 族内没有目标账号 sid ⇒ 跳过不动。
    #[test]
    fn rewrite_skips_family_without_target() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_A}.jsonl"));
        std::fs::write(&src, format!("body {SID_A}")).unwrap();

        let report = rewrite_shared_sids_in(
            &projects,
            &[(SID_B.to_string(), "D:\\ws\\proj".into(), 100)], // B 与族无关
            None,
            false,
        );
        assert!(report["rewritten"].as_array().unwrap().is_empty());
        assert_eq!(report["aligned"].as_i64(), Some(0));
        let body = std::fs::read_to_string(&src).unwrap();
        assert_eq!(body, format!("body {SID_A}"), "无关族不被触碰");
    }

    /// 新旧 sid 长度不等 ⇒ 计 error 且不写盘（防御性）。
    #[test]
    fn rewrite_length_mismatch_reports_error() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_A}.jsonl"));
        let dst = ws.join("short-target.jsonl");
        std::fs::write(&src, format!("body {SID_A}")).unwrap();
        std::fs::hard_link(&src, &dst).unwrap();

        let report = rewrite_shared_sids_in(
            &projects,
            &[("short-target".to_string(), "D:\\ws\\proj".into(), 100)],
            None,
            false,
        );
        assert_eq!(report["errors"].as_array().unwrap().len(), 1);
        let body = std::fs::read_to_string(&src).unwrap();
        assert_eq!(body, format!("body {SID_A}"), "出错不写盘");
    }

    /// 备份：替换数 > 0 时先落备份文件。
    #[test]
    fn rewrite_backs_up_before_write() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_A}.jsonl"));
        let dst = ws.join(format!("{SID_C}.jsonl"));
        std::fs::write(&src, format!("body {SID_A}")).unwrap();
        std::fs::hard_link(&src, &dst).unwrap();

        let backup_dir = projects.join("backups");
        let report = rewrite_shared_sids_in(
            &projects,
            &[(SID_C.to_string(), "D:\\ws\\proj".into(), 100)],
            Some(&backup_dir),
            false,
        );
        assert_eq!(report["rewritten"].as_array().unwrap().len(), 1, "改写应发生");
        // 备份名取代表文件名（目标 sid 文件）
        let bak = backup_dir.join(format!("{SID_C}.jsonl"));
        assert!(bak.is_file(), "备份文件存在");
        let bak_body = std::fs::read_to_string(&bak).unwrap();
        assert_eq!(bak_body, format!("body {SID_A}"), "备份是改写前原文");
    }

    /// 单成员族（本账号自己的非共享会话）⇒ 快路径 aligned，不触碰文件。
    #[test]
    fn rewrite_counts_single_member_as_aligned() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_B}.jsonl"));
        std::fs::write(&src, format!("own body {SID_B}")).unwrap();

        let report = rewrite_shared_sids_in(
            &projects,
            &[(SID_B.to_string(), "D:\\ws\\proj".into(), 100)],
            None,
            false,
        );
        assert_eq!(report["aligned"].as_i64(), Some(1));
        assert!(report["rewritten"].as_array().unwrap().is_empty());
        let body = std::fs::read_to_string(&src).unwrap();
        assert_eq!(body, format!("own body {SID_B}"));
    }

    /// dry_run：只出 planned，不落盘。
    #[test]
    fn rewrite_dry_run_plans_without_writing() {
        let projects = temp_projects();
        let ws = projects.join("d-ws-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let src = ws.join(format!("{SID_A}.jsonl"));
        let dst = ws.join(format!("{SID_B}.jsonl"));
        std::fs::write(&src, format!("body {SID_A}")).unwrap();
        std::fs::hard_link(&src, &dst).unwrap();

        let report = rewrite_shared_sids_in(
            &projects,
            &[(SID_B.to_string(), "D:\\ws\\proj".into(), 100)],
            None,
            true,
        );
        assert_eq!(report["rewritten"].as_array().unwrap().len(), 1);
        assert_eq!(report["rewritten"][0]["planned"], true);
        let body = std::fs::read_to_string(&src).unwrap();
        assert_eq!(body, format!("body {SID_A}"), "dry_run 不写盘");
    }

    /// replace_all_bytes：计数与替换一致；无命中零拷贝返回；等长前提。
    #[test]
    fn replace_all_bytes_counts_and_replaces() {
        let (out, n) = replace_all_bytes(b"aXbXc", b"X", b"Y");
        assert_eq!((out.as_slice(), n), (b"aYbYc".as_slice(), 2));
        let (out, n) = replace_all_bytes(b"abc", b"Z", b"Y");
        assert_eq!((out.as_slice(), n), (b"abc".as_slice(), 0));
        let (out, n) = replace_all_bytes(b"aaaa", b"aa", b"bb");
        assert_eq!((out.as_slice(), n), (b"bbbb".as_slice(), 2));
    }
}
