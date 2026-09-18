//! 会话清理（瘦身）——每账号每 cwd 保留 `updated_at` 最新 keep 条，其余软删。
//!
//! 云端删除是清理的默认组成部分：装配云端上下文——读映射表 + 取该账号 token；
//! 任何一步缺失都不致命，会在报告的 `slim.cloud.tokenReady` 里如实反映，
//! 流程退化为「只本地软删」。删除按 `cloud_conv` 的三条铁律走。
//! 末尾追加**对账阶段**（`cloud.reconcile`）+ **云端全貌**（`cloud.inventory`）。

use serde_json::{json, Value};
use std::path::Path;

use crate::modules::config::{backup_dir, home_dir, now_ms, utc_iso};
use crate::modules::session::{backup_workbuddy_db, open_db, table_exists, workbuddy_db_path};

/// 会话瘦身：每账号每 cwd 保留 updated_at 最新 keep 条，其余软删。
///
/// `exclude` = 本次切号刚复制过来的会话 id —— 它们既不被删、也不占用保留名额，
/// 否则「复制多条同项目会话 + 瘦身 keep=1」会让用户只看到 1 条（复制体互相挤掉）。
/// 云端删除上下文（`None` = 只做本地软删，保持旧行为）。
///
/// 归属判据 = `<configDir>/edge-sync-mapping-v4.db` 的 `msg_channel`；
/// 只有 `convmsg:<uid>`（本次瘦身的目标账号）才动云端 —— 其余（无映射 / 归别的账号）
/// 一律**只本地软删**，避免用错账号 token 触发 403 白跑、或误伤他人会话。
pub struct SlimCloudCtx {
    /// 本次瘦身的目标账号 uid。
    pub uid: String,
    /// 该账号的 `access_token`（取不到则整个云端环节跳过）。
    pub token: Option<String>,
    /// `sid → msg_channel` 全量映射。
    pub channels: std::collections::HashMap<String, String>,
}

impl SlimCloudCtx {
    /// 该 sid 是否「云端归属本次目标账号」⇒ 可以放心删云端。
    fn owns(&self, sid: &str) -> bool {
        self.channels
            .get(sid)
            .map(|ch| crate::modules::cloud_conv::channel_uid(ch) == self.uid)
            .unwrap_or(false)
    }
}

/// 会话瘦身的本地实现（不含云端）。旧签名保留，供既有调用与测试使用。
#[allow(dead_code)] // 仅测试与旧调用点用；生产路径统一走 _cloud 版
pub(crate) fn slim_sessions_in_db(
    db_path: &Path,
    uid: &str,
    keep: i64,
    dry_run: bool,
    exclude: &[String],
) -> Result<Value, String> {
    slim_sessions_in_db_cloud(&SlimArgs {
        db_path,
        uid,
        keep,
        dry_run,
        exclude,
        keep_sids: None,
        cloud: None,
    })
    .map(|(report, _sids)| report)
}

/// 会话瘦身（可带云端删除）。
///
/// 每条 victim 的处理顺序（**先云端、后本地**，保证失败可回退）：
///   `ch == convmsg:<uid>` 且有 token → 调云端删除
///        · 成功 / 404  → 本地软删，计 `cloudDeleted`
///        · 403         → 归属与映射不符（映射记错）⇒ **放弃云端**，本地照常软删，计 `cloudForbidden`
///        · 其它失败    → **本地不软删**（留到下次重试），计 `cloudFailed`
///   `ch` 缺失                                    → 本地软删，计 `cloudNoMapping`
///   `ch` 归别的账号                              → 本地软删，计 `cloudForeign`
/// dry_run 只统计「将调用云端几条」，不发起请求。
///
/// 返回 `(report, victims_sids)`：第二项 = 本次选中的 victim 会话 id，供同一次切号
/// 后续的**对账阶段**做 `skip`（避免对同一批 sid 再发一轮冗余删除请求）。
/// 一次瘦身的入参：位置参数收成一个，拆出的子函数只透传它。
pub(crate) struct SlimArgs<'a> {
    pub db_path: &'a Path,
    pub uid: &'a str,
    pub keep: i64,
    pub dry_run: bool,
    /// 本次切号刚复制过来的会话 id —— 不被删、也不占保留名额。
    pub exclude: &'a [String],
    /// 统一保留名单（`Some` = keepList 模式；`None` = 每项目保留 N 条）。
    pub keep_sids: Option<&'a [String]>,
    /// 云端上下文（`None` = 只做本地软删）。
    pub cloud: Option<&'a SlimCloudCtx>,
}

/// 一次瘦身的云端逐态计数（原先 9 个裸变量散在循环里）。
///
/// `deleted` = 合计（200 + 404）；`removed` = 真被这次请求删掉（200）；
/// `already_gone` = 云端本来就没有（404）。**只有 200 才能证明删成功** ——
/// 两个数字混在一起时，报告无法自证「到底真删了没」（实测教训）。
#[derive(Default)]
struct CloudStats {
    /// dry_run 下「将调用云端」条数。
    planned: usize,
    deleted: usize,
    removed: usize,
    already_gone: usize,
    /// 云端说这条不归本次账号（映射记错）⇒ 只本地软删。
    forbidden: usize,
    /// 云端删除失败 ⇒ **本地保留未删**，下次切号再试。
    failed: usize,
    /// 本机没有该会话的云端映射 ⇒ 只本地软删。
    no_mapping: usize,
    /// 归属对得上但没取到凭证 ⇒ 云端整轮跳过，只本地软删。
    no_token: usize,
    /// 映射显示云端归别的账号 ⇒ 只本地软删（不碰别人的对话）。
    foreign: usize,
    /// 因云端失败而保留未删的 sid（带原因），落报告的 `samples`。
    kept: Vec<String>,
}

impl CloudStats {
    /// 记一条云端删除结果，返回**是否允许本地软删**（铁律 3：云端没删掉 ⇒ 本地保留）。
    fn record(&mut self, sid: &str, outcome: &crate::modules::cloud_conv::CloudDelete) -> bool {
        use crate::modules::cloud_conv::CloudDelete;
        match outcome {
            CloudDelete::Deleted => {
                self.deleted += 1;
                self.removed += 1;
                true
            }
            CloudDelete::AlreadyGone => {
                self.deleted += 1;
                self.already_gone += 1;
                true
            }
            CloudDelete::Forbidden => {
                self.forbidden += 1;
                true
            }
            CloudDelete::Failed(msg) => {
                self.failed += 1;
                self.kept.push(format!("{sid}（{msg}）"));
                false
            }
        }
    }

    /// 落进报告（字段名与顺序保持原样 —— 前端与脚本按名取值）。
    fn to_json(&self, token_ready: bool) -> Value {
        json!({
            "enabled": true,
            "tokenReady": token_ready,
            "planned": self.planned,
            "deleted": self.deleted,
            "removed": self.removed,
            "alreadyGone": self.already_gone,
            "forbidden": self.forbidden,
            "failed": self.failed,
            "keptLocal": self.kept.len(),
            "samples": self.kept.iter().take(5).cloned().collect::<Vec<_>>(),
            "noMapping": self.no_mapping,
            "noToken": self.no_token,
            "foreign": self.foreign,
        })
    }
}

/// 选出 victims（待删集合）。
///
/// 两种模式：
/// - keepList（统一保留名单，由调用方传入）：目标存活 & 名单外 ⇒ 删；
/// - perProjectKeep（旧逻辑，无名单时兼容）：每 cwd 分组，updated_at 倒序第 keep 条之后全删。
///
/// 排除集（本次手动复制体）两种模式下都生效：不被删、也不占保留名额。
fn select_victims(
    conn: &rusqlite::Connection,
    uid: &str,
    keep: i64,
    excl: &[&String],
    keep_sids: Option<&[String]>,
) -> Result<Vec<(String, String)>, String> {
    let excl_sql = |alias: &str| -> String {
        if excl.is_empty() {
            return String::new();
        }
        let marks = (0..excl.len())
            .map(|i| format!("?{}", i + 3)) // ?1=uid, ?2=keep
            .collect::<Vec<_>>()
            .join(",");
        format!(" AND {alias}id NOT IN ({marks})")
    };
    let mut params: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(uid.to_string()),
        rusqlite::types::Value::Integer(keep),
    ];
    for e in excl {
        params.push(rusqlite::types::Value::Text((*e).clone()));
    }
    let mut keep_marks = String::new();
    if let Some(ks) = keep_sids {
        let list: Vec<&String> = ks.iter().filter(|s| !s.is_empty()).collect();
        if !list.is_empty() {
            let base = 3 + excl.len();
            keep_marks = (0..list.len())
                .map(|i| format!("?{}", base + i))
                .collect::<Vec<_>>()
                .join(",");
            for s in &list {
                params.push(rusqlite::types::Value::Text((*s).clone()));
            }
        }
    }
    let sql = match (keep_sids, keep_marks.is_empty()) {
        (Some(_), false) => format!(
            "SELECT id, COALESCE(cwd,'') FROM sessions s \
             WHERE deleted_at IS NULL AND user_id = ?1{} AND id NOT IN ({keep_marks})",
            excl_sql("s."),
        ),
        _ => format!(
            "SELECT id, cwd FROM sessions s \
             WHERE deleted_at IS NULL AND user_id = ?1{} AND id NOT IN (\
               SELECT id FROM sessions \
               WHERE deleted_at IS NULL AND user_id = ?1 AND cwd = s.cwd{} \
               ORDER BY updated_at DESC LIMIT ?2\
             )",
            excl_sql("s."),
            excl_sql(""),
        ),
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

pub(crate) fn slim_sessions_in_db_cloud(args: &SlimArgs) -> Result<(Value, Vec<String>), String> {
    let keep = args.keep.max(1);
    // 只看数（dry_run）用**只读**连接：预览不该产生 WAL / 抢写锁，真删才需要读写。
    let Some(conn) = open_db(args.db_path, args.dry_run) else {
        return Err("无法打开 workbuddy.db".into());
    };
    if !table_exists(&conn, "sessions") {
        return Ok((json!({ "skipped": "no sessions table" }), Vec::new()));
    }

    let excl: Vec<&String> = args.exclude.iter().filter(|s| !s.is_empty()).collect();
    let victims = select_victims(&conn, args.uid, keep, &excl, args.keep_sids)?;

    let (deleted, stats) = apply_victims(&conn, args, &victims)?;

    let mut groups: std::collections::BTreeMap<&str, i64> = Default::default();
    for (_id, cwd) in &victims {
        *groups.entry(cwd.as_str()).or_insert(0) += 1;
    }
    let mut report = json!({
        "uid": args.uid,
        "keep": keep,
        "keepList": args.keep_sids.is_some(),
        "excluded": excl.len(),
        "planned": victims.len(),
        "deleted": deleted,
        "groups": groups.iter().map(|(c, n)| json!({ "cwd": c, "count": n })).collect::<Vec<_>>(),
        "dryRun": args.dry_run,
    });
    if args.cloud.is_some() {
        report["cloud"] =
            stats.to_json(args.cloud.map(|c| c.token.is_some()).unwrap_or(false));
    }
    let victim_sids: Vec<String> = victims.iter().map(|(id, _cwd)| id.clone()).collect();
    Ok((report, victim_sids))
}

/// 逐条处理 victims：云端删除（勾了才做）+ 本机软删，返回 `(本机软删行数, 云端逐态计数)`。
fn apply_victims(
    conn: &rusqlite::Connection,
    args: &SlimArgs,
    victims: &[(String, String)],
) -> Result<(usize, CloudStats), String> {
    // 云端开启但没有 token ⇒ 退化为「只本地」，避免误报为 foreign
    let token_ok = args.cloud.map(|c| c.token.is_some()).unwrap_or(false);
    let mut stats = CloudStats::default();
    let mut deleted = 0usize;

    for (id, _cwd) in victims {
        let mut allow_local = true;
        if let Some(c) = args.cloud {
            if c.owns(id) {
                if !token_ok {
                    // 归属对得上但没凭证 ⇒ 云端这轮跳过，本地照常软删
                    stats.no_token += 1;
                } else if args.dry_run {
                    stats.planned += 1;
                } else {
                    let token = c.token.as_deref().unwrap_or("");
                    let outcome = crate::modules::cloud_conv::delete_conversation(token, id);
                    allow_local = stats.record(id, &outcome);
                    append_cloud_delete_audit(args.uid, id, &outcome);
                }
            } else if c.channels.contains_key(id.as_str()) {
                stats.foreign += 1;
            } else {
                stats.no_mapping += 1;
            }
        }

        if !args.dry_run && allow_local {
            let n = conn
                .execute(
                    "UPDATE sessions SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                    rusqlite::params![id, now_ms()],
                )
                .map_err(|e| e.to_string())?;
            deleted += n;
        }
    }
    Ok((deleted, stats))
}

/// 逐条云端删除审计 → `~/.wb-switch/cloud_delete_log.jsonl`（一行一条 JSON）。
///
/// 为什么单独留这个文件：统计报告里 `deleted` 是 200 与 404 的**合计**，
/// 事后无法回答「这次到底真删了几条」。App 侧日志也不记录我们直连的请求
/// （我们走 `POST /console/as/conversations/{sid}/delete`，不经 App 的
/// `syncDeleteConversation`）⇒ 逐条落盘是**唯一**可事后核验的证据源。
///
/// 只记录真实执行（非 dry_run），写失败不影响业务。
fn append_cloud_delete_audit(uid: &str, sid: &str, outcome: &crate::modules::cloud_conv::CloudDelete) {
    use std::io::Write;
    let path = home_dir().join(".wb-switch").join("cloud_delete_log.jsonl");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 简易轮转：超过 2 MiB 直接换名覆盖，避免无限增长（审计只需近期）。
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 2 * 1024 * 1024 {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.old"));
    }
    let (result, detail) = match outcome {
        crate::modules::cloud_conv::CloudDelete::Deleted => ("removed", "http 200"),
        crate::modules::cloud_conv::CloudDelete::AlreadyGone => ("alreadyGone", "http 404"),
        crate::modules::cloud_conv::CloudDelete::Forbidden => ("forbidden", "http 403"),
        crate::modules::cloud_conv::CloudDelete::Failed(m) => ("failed", m.as_str()),
    };
    let line = json!({
        "at": now_ms(),
        "ts": utc_iso(),
        "uid": uid,
        "sid": sid,
        "result": result,
        "detail": detail,
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

/// 备份目录时间戳：毫秒级。
///
/// 不能用 `utc_iso()`（只到秒）——一次切号里项目侧栏同步与会话瘦身会连续各备份一次，
/// 秒级目录名重名导致后一次覆盖前一次，**pre-同步的快照丢失**，回滚点被后移。
fn backup_stamp() -> String {
    format!("{}-{}Z", utc_iso().trim_end_matches('Z'), now_ms())
}

/// 真实路径包装：会话瘦身（含 db 备份）。`exclude` = 不参与瘦身的会话 id（本次复制体）。
///
/// `keep_sids` = **统一保留名单**（源∪目标合并、每项目最新 N 条等由调用方计算后
/// 整体传入）。传入时瘦身判定 =
/// 「目标存活 & 名单外 ⇒ 删」，`keep` 参数被忽略；`None` 时退回按每 cwd 保留 `keep` 条的旧逻辑。
///
/// 云端删除是瘦身的默认组成部分（不再单独设开关）：装配云端上下文——读映射表 +
/// 取该账号 token；任何一步缺失都不致命，会在报告的 `slim.cloud.tokenReady` 里
/// 如实反映，流程退化为「只本地瘦身」。删除按 `cloud_conv` 的三条铁律走。
///
/// 末尾追加**对账阶段**（`cloud.reconcile`）：映射行全集 × 本机 sessions，
/// 清「本机已软删但云端还在」的残留；映射行有、本机无行的 Unknown 只计数不删
/// （可能是其他设备的活会话）。映射库只读，绝不写删。
pub fn slim_sessions(
    uid: &str,
    keep: i64,
    dry_run: bool,
    exclude: &[String],
    keep_sids: Option<&[String]>,
) -> Result<Value, String> {
    let mut db_backup: Option<String> = None;
    if !dry_run {
        let root = backup_dir().join("projects_anchor").join(backup_stamp());
        db_backup = backup_workbuddy_db(crate::modules::variant::WbVariant::Cn, &root).map(|p| p.to_string_lossy().to_string());
    }
    let ctx = Some(SlimCloudCtx {
        uid: uid.to_string(),
        token: crate::modules::cloud_conv::token_of(uid),
        channels: crate::modules::cloud_conv::mapping_channels(),
    });
    let db = workbuddy_db_path(crate::modules::variant::WbVariant::Cn);
    let (mut report, victim_sids) = slim_sessions_in_db_cloud(&SlimArgs {
        db_path: &db,
        uid,
        keep,
        dry_run,
        exclude,
        keep_sids,
        cloud: ctx.as_ref(),
    })?;
    if let Some(cloud) = report.get_mut("cloud") {
        // 本次瘦身刚处理过的 sid 交给对账做 skip：否则它们刚被软删就落入「本机已软删」
        // 集合，对账会对同一批再发一轮删除请求（幂等但冗余）。
        let skip: std::collections::HashSet<String> = victim_sids.into_iter().collect();
        cloud["reconcile"] = reconcile_cloud_stage(uid, dry_run, &skip);
        cloud["inventory"] = inventory_cloud_stage(uid);
    }
    // `null` = 这次没拿到回滚点（拷贝失败不再被静默吞掉，见 session::backup_workbuddy_db）。
    report["backupDb"] = json!(db_backup);
    Ok(report)
}

/// 对账阶段：映射行全集 × 本机 sessions → 清「本机已软删但云端还在」。
/// 任何一步失败都不致命（返回 error 对象，不影响瘦身主流程）。
///
/// `skip` 见 `cloud_reconcile::sweep_stale_mappings`。
fn reconcile_cloud_stage(
    uid: &str,
    dry_run: bool,
    skip: &std::collections::HashSet<String>,
) -> Value {
    let rows = crate::modules::cloud_conv::mapping_rows();
    if rows.is_empty() {
        return json!({ "skipped": "no mapping db" });
    }
    let (alive, deleted) = match read_local_alive_deleted(&workbuddy_db_path(crate::modules::variant::WbVariant::Cn), uid) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e }),
    };
    let token = crate::modules::cloud_conv::token_of(uid);
    crate::modules::cloud_reconcile::sweep_stale_mappings(
        &rows,
        &alive,
        &deleted,
        token.as_deref(),
        dry_run,
        skip,
        |tok, cid| {
            let outcome = crate::modules::cloud_conv::delete_conversation(tok, cid);
            // 对账阶段也必须逐条落审计：
            // `cloud_delete_log.jsonl` 是唯一事后证据源，漏这段 ⇒ 对账删了什么无从查证。
            append_cloud_delete_audit(uid, cid, &outcome);
            outcome
        },
    )
}

/// 全账巡检阶段（**只读，永不删**）：云端全账 × 本机 sessions → 分类计数。
///
/// 吃 `GET /v2/as/conversations/?type=all`（跨设备全账）——
/// 补上 `reconcile_cloud_stage` 的盲区：映射行那本账里**没有**的云端会话
/// （他机创建 / 云端自动化）它根本看不见。
///
/// 只报数，不做任何删除：`foreign`（本机无痕迹）多半是别的设备的活会话，
/// 删了就伤到别人。dry_run 与否都照跑（无副作用）。
fn inventory_cloud_stage(uid: &str) -> Value {
    let (alive, deleted) = match read_local_alive_deleted(&workbuddy_db_path(crate::modules::variant::WbVariant::Cn), uid) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e }),
    };
    let token = crate::modules::cloud_conv::token_of(uid);
    crate::modules::cloud_reconcile::inventory(
        token.as_deref(),
        &alive,
        &deleted,
        crate::modules::cloud_conv::list_conversation_ids,
    )
}

/// 读**某个账号名下** sessions 的存活/软删 id 集合（对账用，只读）。
///
/// ⚠️ 必须按 `user_id` 过滤：本机 sessions 表里躺着所有账号的会话，
/// 全表取出来跟「单个账号的云端清单」比对 ⇒ 别人家的会话全被算成「该账号没上云」
/// （实测 localOnlyAlive 虚报 165 条，真实为 0）、「清遗留」也混进别账号的软删
/// （199 vs 目标账号自己 129）。删不删得掉另说，数字先得是真的。
fn read_local_alive_deleted(
    db: &std::path::Path,
    uid: &str,
) -> Result<(std::collections::HashSet<String>, std::collections::HashSet<String>), String> {
    use std::collections::HashSet;
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("打开 workbuddy.db 失败: {e}"))?;
    let mut alive = HashSet::new();
    let mut deleted = HashSet::new();
    let mut stmt = conn
        .prepare("SELECT id, deleted_at FROM sessions WHERE user_id = ?1")
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    let rows = stmt
        .query_map([uid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    for row in rows {
        let (id, deleted_at) = row.map_err(|e| format!("读取 sessions 行失败: {e}"))?;
        if deleted_at.is_none() {
            alive.insert(id);
        } else {
            deleted.insert(id);
        }
    }
    Ok((alive, deleted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::PathBuf;

    /// 备份戳必须带毫秒：一次切号内连续两次备份若重名，后一次会覆盖前一次的快照。
    #[test]
    fn backup_stamp_is_unique_per_millisecond() {
        let a = backup_stamp();
        assert!(a.ends_with('Z'), "保持与 utc_iso 一致的 Z 后缀: {a}");
        let sec_len = "2026-09-13T20-04-43Z".len();
        assert!(a.len() > sec_len, "毫秒后缀不能丢: {a}");
        // 连续两次至少不因「格式不含毫秒」而重名
        let millis = a.trim_end_matches('Z').rsplit('-').next().unwrap_or("");
        assert!(
            millis.chars().all(|c| c.is_ascii_digit()) && millis.len() >= 12,
            "毫秒段应为 now_ms 的数值: {a}"
        );
    }

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_anchor_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn setup(db: &Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                user_id TEXT NOT NULL,
                title TEXT,
                status TEXT DEFAULT 'Pending',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                last_activity_at INTEGER,
                deleted_at INTEGER,
                is_playground INTEGER DEFAULT 0,
                source_mode TEXT,
                mode TEXT
            );",
        )
        .unwrap();
        for (id, cwd, uid, upd) in [
            ("s1", "D:\\p1", "uid-a", 1000),
            ("s2", "D:\\p1", "uid-a", 2000), // p1 两条，瘦身应删 s1（旧）
            ("s3", "D:\\p2", "uid-a", 1000),
            ("s4", "D:\\p3", "uid-b", 1000), // b 独有 → 删多目标
            ("s5", "D:\\p2", "uid-b", 1000), // b 在 p2 也有 → 不补
        ] {
            conn.execute(
                "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 't', 1, ?4)",
                rusqlite::params![id, cwd, uid, upd],
            )
            .unwrap();
        }
    }

    #[test]
    fn slim_keeps_latest_per_cwd() {
        let db = temp_db("slim");
        setup(&db);
        let rep = slim_sessions_in_db(&db, "uid-a", 1, false, &[]).unwrap();
        assert_eq!(rep["deleted"], 1, "p1 两条留最新，删 1");
        let conn = Connection::open(&db).unwrap();
        let alive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE user_id='uid-a' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive, 2, "p1 留 s2、p2 留 s3");
        let kept: String = conn
            .query_row("SELECT id FROM sessions WHERE user_id='uid-a' AND cwd='D:\\p1' AND deleted_at IS NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, "s2", "保留 updated_at 最新的");
    }

    /// 对账取本机存活/软删必须**按账号过滤**：本机 sessions 里躺着所有账号的会话，
    /// 全表取出来跟「单个账号的云端清单」比对，会把别人家的会话全算成「该账号没上云」
    /// （实测：不按 uid 过滤会把全账号话计入，严重虚高）。
    #[test]
    fn read_local_alive_deleted_is_scoped_to_one_account() {
        let db = temp_db("alive_deleted_scope");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, cwd TEXT, user_id TEXT, title TEXT,
                    created_at INTEGER, updated_at INTEGER, deleted_at INTEGER);",
            )
            .unwrap();
            let rows: [(&str, &str, Option<i64>); 6] = [
                ("me-alive-1", "uid-me", None),
                ("me-alive-2", "uid-me", None),
                ("me-dead-1", "uid-me", Some(1)),
                ("other-alive-1", "uid-other", None),
                ("other-alive-2", "uid-other", None),
                ("other-dead-1", "uid-other", Some(1)),
            ];
            for (id, uid, del) in rows {
                conn.execute(
                    "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at, deleted_at)
                     VALUES (?1, 'D:\\p', ?2, 't', 1, 1, ?3)",
                    rusqlite::params![id, uid, del],
                )
                .unwrap();
            }
        }
        let (alive, deleted) = read_local_alive_deleted(&db, "uid-me").unwrap();
        assert_eq!(alive.len(), 2, "只数目标账号的存活会话");
        assert_eq!(deleted.len(), 1, "只数目标账号的软删会话");
        assert!(alive.contains("me-alive-1") && !alive.contains("other-alive-1"));
        assert!(deleted.contains("me-dead-1") && !deleted.contains("other-dead-1"));
    }

    /// 云端连带删除的**归属分流**：无映射 / 归本账号（无 token）/ 归别的账号 三类必须分得清，
    /// 且本用例**全程不联网**（token 传 `None` ⇒ 一律不发请求）。
    #[test]
    fn slim_cloud_routes_by_ownership_without_network() {
        let db = temp_db("slim_cloud");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, cwd TEXT, user_id TEXT, title TEXT,
                    created_at INTEGER, updated_at INTEGER, deleted_at INTEGER);",
            )
            .unwrap();
            // 每个 cwd 两条：旧的会被淘汰（keep=1 留最新）
            for (cwd, pfx) in [("D:\\p9", "none"), ("D:\\p8", "own"), ("D:\\p7", "foreign")] {
                for (suf, upd) in [("a", 100), ("b", 200)] {
                    conn.execute(
                        "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                         VALUES (?1, ?2, 'uid-a', 't', 1, ?3)",
                        rusqlite::params![format!("{pfx}_{suf}"), cwd, upd],
                    )
                    .unwrap();
                }
            }
        }
        // 只给 own_* 与 foreign_* 配映射；none_* 故意不配
        let mut channels = std::collections::HashMap::new();
        channels.insert("own_a".to_string(), "convmsg:uid-a".to_string());
        channels.insert("foreign_a".to_string(), "convmsg:uid-b".to_string());
        let ctx = SlimCloudCtx {
            uid: "uid-a".into(),
            token: None,
            channels,
        };

        // ① dry-run：只统计「将调云端」条数，不落盘、不发请求
        let (rep, sids) = slim_sessions_in_db_cloud(&SlimArgs {
            db_path: &db,
            uid: "uid-a",
            keep: 1,
            dry_run: true,
            exclude: &[],
            keep_sids: None,
            cloud: Some(&ctx),
        })
        .unwrap();
        assert_eq!(rep["planned"], 3, "三个 cwd 各淘汰 1 条");
        assert_eq!(rep["deleted"], 0, "dry-run 不落盘");
        assert_eq!(rep["cloud"]["enabled"], true);
        assert_eq!(rep["cloud"]["tokenReady"], false);
        assert_eq!(sids.len(), 3, "第二项返回本次 victim sid，供对账阶段 skip");

        // ② 真执行但无 token ⇒ 三类都只本地软删，一条云端请求都不发
        let rep2 = slim_sessions_in_db_cloud(&SlimArgs {
            db_path: &db,
            uid: "uid-a",
            keep: 1,
            dry_run: false,
            exclude: &[],
            keep_sids: None,
            cloud: Some(&ctx),
        })
        .unwrap()
        .0;
        assert_eq!(rep2["deleted"], 3, "三类都应完成本地软删");
        assert_eq!(rep2["cloud"]["deleted"], 0);
        assert_eq!(
            rep2["cloud"]["removed"], 0,
            "真删数（200）与 alreadyGone（404）必须分开报，否则事后无法自证删成功"
        );
        assert_eq!(rep2["cloud"]["alreadyGone"], 0);
        assert_eq!(rep2["cloud"]["failed"], 0, "无 token 不算失败，本地不被卡住");
        assert_eq!(rep2["cloud"]["noToken"], 1, "own_a：归属对但没凭证");
        assert_eq!(rep2["cloud"]["foreign"], 1, "foreign_a：云端归别的账号，不碰");
        assert_eq!(rep2["cloud"]["noMapping"], 1, "none_a：本机没有映射");

        // ③ 不勾云端 ⇒ 报告里根本没有 cloud 段（旧行为不变）
        let rep3 = slim_sessions_in_db_cloud(&SlimArgs {
            db_path: &db,
            uid: "uid-b",
            keep: 1,
            dry_run: true,
            exclude: &[],
            keep_sids: None,
            cloud: None,
        })
        .unwrap()
        .0;
        assert!(rep3.get("cloud").is_none(), "未开启时不应出现 cloud 字段");
    }

    /// 回归：「复制多条同项目会话 + 瘦身 keep=1」——复制体必须全部存活，且不挤掉原有保留名额。
    #[test]
    fn slim_protects_copied_sessions() {
        let db = temp_db("slim_protect");
        setup(&db);
        {
            let conn = Connection::open(&db).unwrap();
            // 模拟本次切号复制到 uid-a 的两条同项目会话（时间戳最新）
            for (id, upd) in [("c1", 3000), ("c2", 4000)] {
                conn.execute(
                    "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                     VALUES (?1, 'D:\\p1', 'uid-a', 'copied', 1, ?2)",
                    rusqlite::params![id, upd],
                )
                .unwrap();
            }
        }
        let alive = |db: &Path| -> Vec<String> {
            let conn = Connection::open(db).unwrap();
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE cwd='D:\\p1' AND deleted_at IS NULL ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect()
        };

        // 无保护：keep=1 只留最新的 c2，复制体互相挤掉（用户只看到 1 条）
        let rep = slim_sessions_in_db(&db, "uid-a", 1, false, &[]).unwrap();
        let kept = alive(&db);
        assert_eq!(kept, vec!["c2".to_string()], "无保护时只剩最新一条: {kept:?}");
        assert_eq!(rep["deleted"], 3);

        // 有保护：c1/c2 都留，且 p1 原有的最新一条 s2 也留（复制体不占名额）
        let db2 = temp_db("slim_protect2");
        setup(&db2);
        {
            let conn = Connection::open(&db2).unwrap();
            for (id, upd) in [("c1", 3000), ("c2", 4000)] {
                conn.execute(
                    "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                     VALUES (?1, 'D:\\p1', 'uid-a', 'copied', 1, ?2)",
                    rusqlite::params![id, upd],
                )
                .unwrap();
            }
        }
        let rep2 = slim_sessions_in_db(
            &db2,
            "uid-a",
            1,
            false,
            &["c1".to_string(), "c2".to_string()],
        )
        .unwrap();
        let kept2 = alive(&db2);
        assert_eq!(
            kept2,
            vec!["c1".to_string(), "c2".to_string(), "s2".to_string()],
            "复制体受保护 + 原有最新一条: {kept2:?}"
        );
        assert_eq!(rep2["deleted"], 1, "只删旧的 s1");
        assert_eq!(rep2["excluded"], 2);
    }

}
