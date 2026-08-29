//! 会话列表与按需复制（路径 B：生成新 id，云端可正常同步）。
//!
//! 对照 server.py `current_user_uid` / `list_sessions_for_user` /
//! `_find_project_jsonl` / `copy_session_to_user` / `_register_edge_sync_mapping` /
//! `copy_sessions_for_switch` / `backup_workbuddy_db` / `workbuddy_db_path`。
//!
//! WorkBuddy 5.x 数据三件套（缺一不可）：
//!   1) 正文：`~/.workbuddy/projects/{workspace}/{cid}.jsonl`（JSONL 含 sessionId 字段）
//!   2) 元数据：`~/.workbuddy/workbuddy.db` sessions 表（id = conversation id = UUID）
//!   3) 云端映射：`~/.workbuddy/edge-sync-mapping-v2.db` edge_sync_mapping
//!      （session_id=conversation_id，msg_channel=convmsg:{uid} 决定云端归属）

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::modules::auth_file;
use crate::modules::config::{backup_dir, home_dir, now_ms, now_secs, utc_iso};

/// 打开数据库并设置 busy_timeout（对照 Python `sqlite3.connect(timeout=5)`）。
fn open_db(path: &Path, read_only: bool) -> Option<Connection> {
    let conn = if read_only {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?
    } else {
        Connection::open(path).ok()?
    };
    let _ = conn.busy_timeout(Duration::from_secs(5));
    Some(conn)
}

pub fn workbuddy_db_path() -> PathBuf {
    home_dir().join(".workbuddy").join("workbuddy.db")
}

fn edge_sync_db_path() -> PathBuf {
    home_dir()
        .join(".workbuddy")
        .join("edge-sync-mapping-v2.db")
}

/// 当前认证账号的 uid（认证文件 account.uid）。
pub fn current_user_uid() -> Option<String> {
    let auth = auth_file::read_auth_file()?;
    auth.get("account")
        .and_then(|a| a.get("uid"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn table_exists(conn: &Connection, name: &str) -> bool {
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

/// 归一化工作目录：去掉首尾空白与尾部路径分隔符，用于跨副本的 cwd 比对。
fn normalize_cwd(cwd: &str) -> String {
    cwd.trim().trim_end_matches(['/', '\\']).to_string()
}

/// 会话正文去标识：把「本会话自身 id」替换为中性占位符，
/// 使复制产生的不同 id 副本在比对时视为同一份内容。
/// 复制时会把源 cid 替换为新 cid，故正文里出现的是会话自身 id。
const NEUTRAL_ID: &str = "\u{0}WB_SWITCH_NEUTRAL_ID\u{0}";

fn normalize_jsonl(text: &str, cid: &str) -> String {
    text.replace(cid, NEUTRAL_ID)
}

/// 一次性索引 `~/.workbuddy/projects` 下所有 `{cid}.jsonl`，返回 cid -> 路径。
/// 查找范围与 `find_project_jsonl` 一致（直接文件 + 各 workspace 子目录）。
fn index_project_jsonls() -> std::collections::HashMap<String, PathBuf> {
    let mut map = std::collections::HashMap::new();
    let projects = home_dir().join(".workbuddy").join("projects");
    if !projects.is_dir() {
        return map;
    }
    let mut visit = |p: PathBuf| {
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            map.entry(stem.to_string()).or_insert(p);
        }
    };
    if let Ok(entries) = std::fs::read_dir(&projects) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                visit(p);
            } else if p.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&p) {
                    for f in sub.flatten() {
                        let fp = f.path();
                        if fp.is_file() {
                            visit(fp);
                        }
                    }
                }
            }
        }
    }
    map
}

/// 删除云端映射中指定会话的归属记录（会话被去重软删后清理孤儿映射）。
fn delete_edge_sync_mappings(db_path: &Path, cids: &[String]) {
    if !db_path.is_file() {
        return;
    }
    let Some(conn) = open_db(db_path, false) else {
        return;
    };
    if !table_exists(&conn, "edge_sync_mapping") {
        return;
    }
    for cid in cids {
        let _ = conn.execute(
            "DELETE FROM edge_sync_mapping WHERE session_id = ?1 OR conversation_id = ?1",
            rusqlite::params![cid],
        );
    }
}

/// 目标账号是否已存在与源会话「等价」的会话。
///
/// 等价判定：cwd 一致，且正文（抹平各自 id 后）一致。仅当双方都有正文时
/// 做内容比对；若双方都无正文（空会话），退化为「同 cwd + 同展示标题」判定，
/// 仅在目标也无正文时才跳过，避免误跳正常会话。
///
/// 读取失败（如 WorkBuddy 占用锁）时返回 `false`，即「不跳过」，保持与原行为一致。
fn target_has_equivalent_session(
    db: &Path,
    _source_cid: &str,
    _source_uid: &str,
    target_uid: &str,
    source_cwd: &str,
    source_title: &str,
    source_jsonl_norm: &str,
    source_has_jsonl: bool,
) -> bool {
    let Some(conn) = open_db(db, true) else {
        return false;
    };
    if !table_exists(&conn, "sessions") {
        return false;
    }
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, cwd, title, custom_title FROM sessions \
         WHERE user_id = ?1 AND deleted_at IS NULL",
    ) else {
        return false;
    };
    let Ok(rows) = stmt.query_map([target_uid], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    }) else {
        return false;
    };

    let index = index_project_jsonls();
    for r in rows.flatten() {
        let (cid, cwd, title, custom_title) = r;
        let cid = cid.unwrap_or_default();
        let cwd = cwd.unwrap_or_default();
        if is_claw_workspace(&cwd) {
            continue;
        }
        if normalize_cwd(&cwd) != normalize_cwd(source_cwd) {
            continue;
        }
        let target_has_jsonl = index.contains_key(&cid);
        if source_has_jsonl && target_has_jsonl {
            if let Some(p) = index.get(&cid) {
                if let Ok(text) = std::fs::read_to_string(p) {
                    if normalize_jsonl(&text, &cid) == source_jsonl_norm {
                        return true;
                    }
                }
            }
        } else if !source_has_jsonl && !target_has_jsonl {
            if session_display_title(title, custom_title) == source_title {
                return true;
            }
        }
    }
    false
}

/// 清理某账号下「同一会话被复制多次」产生的重复行（路径 B 副本）。
///
/// 判重键 = (归一化 cwd, 归一化 jsonl 正文)；仅正文一致才视为重复，
/// 不依赖 (cwd + title) 这种过宽键，避免误删同工作区下「无标题 / 会议纪要」
/// 等正常同名会话。每组保留 `updated_at` 最新一条，其余软删除
/// （`deleted_at` 置值），并清理其孤儿 jsonl 与云端映射，避免残留。
///
/// 注意：直接调用会写入 `~/.workbuddy/workbuddy.db`，请在 WorkBuddy 关闭后执行。
pub fn dedup_sessions_for_user(uid: &str) -> Value {
    let db = workbuddy_db_path();
    if !db.is_file() {
        return json!({ "ok": false, "reason": "workbuddy.db 不存在", "removed": 0 });
    }
    let Some(conn) = open_db(&db, false) else {
        return json!({ "ok": false, "reason": "无法打开 workbuddy.db", "removed": 0 });
    };
    if !table_exists(&conn, "sessions") {
        return json!({ "ok": false, "reason": "sessions 表不存在", "removed": 0 });
    }

    let Ok(mut stmt) = conn.prepare(
        "SELECT id, cwd, updated_at FROM sessions WHERE user_id = ?1 AND deleted_at IS NULL",
    ) else {
        return json!({ "ok": false, "reason": "查询会话失败", "removed": 0 });
    };
    let Ok(rows) = stmt.query_map([uid], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    }) else {
        return json!({ "ok": false, "reason": "查询会话失败", "removed": 0 });
    };

    let index = index_project_jsonls();
    // key = (归一化 cwd, 归一化正文) -> 成员列表 (cid, updated_at, jsonl 路径)
    let mut groups: std::collections::HashMap<
        (String, String),
        Vec<(String, i64, Option<PathBuf>)>,
    > = std::collections::HashMap::new();
    for r in rows.flatten() {
        let (cid, cwd, updated_at) = r;
        let cid = cid.unwrap_or_default();
        let cwd = cwd.unwrap_or_default();
        if is_claw_workspace(&cwd) {
            continue;
        }
        let norm_cwd = normalize_cwd(&cwd);
        let jsonl_path = index.get(&cid).cloned();
        // 无正文会话使用唯一键（含 cid），不参与内容去重，避免误删。
        let norm_jsonl = match &jsonl_path {
            Some(p) => std::fs::read_to_string(p)
                .ok()
                .map(|t| normalize_jsonl(&t, &cid))
                .unwrap_or_else(|| format!("__nojsonl__{cid}")),
            None => format!("__nojsonl__{cid}"),
        };
        groups
            .entry((norm_cwd, norm_jsonl))
            .or_default()
            .push((cid, updated_at.unwrap_or(0), jsonl_path));
    }

    let mut removed = 0usize;
    let mut removed_ids: Vec<String> = Vec::new();
    for ((_, _), mut members) in groups {
        if members.len() <= 1 {
            continue;
        }
        // 保留 updated_at 最大的一条（并列时取遇到的第一条）
        members.sort_by(|a, b| b.1.cmp(&a.1));
        let _keep = members.remove(0);
        for (cid, _ua, jsonl_path) in members {
            let _ = conn.execute(
                "UPDATE sessions SET deleted_at = ?1 WHERE id = ?2 AND user_id = ?3",
                rusqlite::params![now_ms(), &cid, uid],
            );
            if let Some(p) = jsonl_path {
                let _ = std::fs::remove_file(&p);
            }
            removed += 1;
            removed_ids.push(cid);
        }
    }

    if !removed_ids.is_empty() {
        delete_edge_sync_mappings(&edge_sync_db_path(), &removed_ids);
    }

    json!({
        "ok": true,
        "removed": removed,
        "removedIds": removed_ids,
    })
}

/// 列出某账号未删除的会话（workbuddy.db sessions 表，db 为准）。
///
/// `title` 为 WorkBuddy 侧栏同款展示名；`isPlayground` 对应侧栏「任务」，
/// 其余按 `cwd` 最后一段归入「空间」。
pub fn list_sessions_for_user(uid: &str) -> Value {
    let db = workbuddy_db_path();
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
                "hasHistory": find_project_jsonl(&cid).is_some(),
                "isPlayground": is_playground.unwrap_or(0) != 0,
            }));
        }
    }
    json!(sessions)
}

/// 在 `~/.workbuddy/projects/{workspace}/{cid}.jsonl` 定位会话正文。
fn find_project_jsonl(cid: &str) -> Option<PathBuf> {
    let projects = home_dir().join(".workbuddy").join("projects");
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

/// 备份 workbuddy.db（含 -wal/-shm），返回主库备份路径。对照 `backup_workbuddy_db`。
fn backup_workbuddy_db(backup_root: &Path) -> Option<PathBuf> {
    let db = workbuddy_db_path();
    if !db.is_file() {
        return None;
    }
    std::fs::create_dir_all(backup_root).ok()?;
    for suffix in ["", "-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{}", db.to_string_lossy(), suffix));
        if src.is_file() {
            let _ = std::fs::copy(&src, backup_root.join(format!("workbuddy.db{suffix}")));
        }
    }
    Some(backup_root.join("workbuddy.db"))
}

/// 把 source_uid 的一个会话复制为 target_uid 的新会话（路径 B：生成新 id）。
///
/// 全部按「新 id」复制一份给目标账号，源账号数据完全不动。
/// 新 id 必须用带连字符的 UUID 格式（`Uuid::new_v4().to_string()`），与官方一致；
/// 32 位无连字符形式会导致 WorkBuddy 无法识别新会话。
pub fn copy_session_to_user(
    cid: &str,
    source_uid: &str,
    target_uid: &str,
) -> Result<Value, String> {
    let db = workbuddy_db_path();

    // 收集源会话元信息（cwd / 展示标题 / 正文），用于 claw 校验与复制前去重。
    // 源会话不存在时直接返回错误，避免后续注册出孤儿云端映射。
    let mut source_cwd = String::new();
    let mut source_title = String::new();
    let mut source_jsonl_norm = String::new();
    let mut source_has_jsonl = false;
    let mut source_found = false;
    if let Some(conn) = open_db(&db, true) {
        if let Ok((cwd, title, custom_title)) = conn.query_row(
            "SELECT cwd, title, custom_title FROM sessions WHERE id = ?1 AND user_id = ?2",
            rusqlite::params![cid, source_uid],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        ) {
            source_found = true;
            let cwd = cwd.unwrap_or_default();
            if is_claw_workspace(&cwd) {
                return Err("Claw 工作区绑定当前账号渠道，不支持复制".into());
            }
            if let Some(p) = find_project_jsonl(cid) {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    source_jsonl_norm = normalize_jsonl(&text, cid);
                    source_has_jsonl = true;
                }
            }
            source_cwd = cwd;
            source_title = session_display_title(title, custom_title);
        }
    }
    if !source_found {
        return Err("源会话不存在，无法复制".into());
    }

    // B.1 复制前去重：目标账号已存在等价会话则跳过新增，从源头杜绝重复行累积。
    if target_has_equivalent_session(
        &db,
        cid,
        source_uid,
        target_uid,
        &source_cwd,
        &source_title,
        &source_jsonl_norm,
        source_has_jsonl,
    ) {
        return Ok(json!({
            "id": cid,
            "newId": "",
            "skipped": true,
            "reason": "目标账号已存在等价会话，跳过复制以避免重复",
            "jsonlCopied": false,
            "mappingWritten": false,
            "backup": "",
        }));
    }

    // 生成新 id 并复制（路径 B：新 id，源账号数据不动）
    let new_cid = uuid::Uuid::new_v4().to_string();

    // 1) 复制正文 jsonl：{projects}/{ws}/{cid}.jsonl → {projects}/{ws}/{new_cid}.jsonl
    let mut jsonl_copied = false;
    if let Some(src_jsonl) = find_project_jsonl(cid) {
        let dst_jsonl = src_jsonl.with_file_name(format!("{new_cid}.jsonl"));
        if let Ok(text) = std::fs::read_to_string(&src_jsonl) {
            let text = text.replace(cid, &new_cid); // 替换 sessionId 等旧 id 引用
            if std::fs::write(&dst_jsonl, text).is_ok() {
                jsonl_copied = true;
            }
        }
    }

    // 2) 备份 db（复制前），再 INSERT 新 sessions 行
    let backup_root = backup_dir().join("sessions").join(utc_iso());
    backup_workbuddy_db(&backup_root);
    insert_session_copy(&db, &new_cid, cid, source_uid, target_uid)?;

    // 3) 注册云端映射：新会话归属目标账号（msg_channel=convmsg:{target_uid}）
    let mapping_written = register_edge_sync_mapping(&new_cid, target_uid);

    Ok(json!({
        "id": cid,
        "newId": new_cid,
        "skipped": false,
        "jsonlCopied": jsonl_copied,
        "mappingWritten": mapping_written,
        "backup": backup_root.to_string_lossy().to_string(),
    }))
}

/// 在 workbuddy.db 中把源会话行复制为新 id（动态列，覆盖 id/user_id/时间戳）。
///
/// db 不存在或 sessions 表不存在时静默成功（对应 Python 版跳过）。源行不存在则无操作。
fn insert_session_copy(
    db_path: &Path,
    new_cid: &str,
    cid: &str,
    source_uid: &str,
    target_uid: &str,
) -> Result<(), String> {
    if !db_path.is_file() {
        return Ok(());
    }
    let Some(conn) = open_db(db_path, false) else {
        return Ok(());
    };
    if !table_exists(&conn, "sessions") {
        return Ok(());
    }
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
    if let Ok(Some(row)) = rows.next() {
        let mut vals: Vec<rusqlite::types::Value> = Vec::with_capacity(cols.len());
        for (i, col) in cols.iter().enumerate() {
            let v = row
                .get::<_, rusqlite::types::Value>(i)
                .unwrap_or(rusqlite::types::Value::Null);
            if col == "cwd" {
                if let rusqlite::types::Value::Text(ref path) = v {
                    if is_claw_workspace(path) {
                        return Err("Claw 工作区绑定当前账号渠道，不支持复制".into());
                    }
                }
            }
            match col.as_str() {
                "id" => vals.push(rusqlite::types::Value::Text(new_cid.to_string())),
                "user_id" => vals.push(rusqlite::types::Value::Text(target_uid.to_string())),
                "created_at" | "updated_at" => vals.push(rusqlite::types::Value::Integer(now_ms())),
                "deleted_at" => vals.push(rusqlite::types::Value::Null),
                _ => vals.push(v),
            }
        }
        drop(rows);
        drop(src_stmt);

        let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let colnames = cols.join(", ");
        let sql = format!("INSERT OR REPLACE INTO sessions ({colnames}) VALUES ({placeholders})");
        let params: Vec<&rusqlite::types::Value> = vals.iter().collect();
        conn.execute(&sql, rusqlite::params_from_iter(params))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 把新会话注册进 edge_sync_mapping（云端归属关键）。失败不致命，返回 False。
fn register_edge_sync_mapping(new_cid: &str, target_uid: &str) -> bool {
    insert_edge_sync_mapping(&edge_sync_db_path(), new_cid, target_uid)
}

fn insert_edge_sync_mapping(db_path: &Path, new_cid: &str, target_uid: &str) -> bool {
    if !db_path.is_file() {
        return false;
    }
    let Some(conn) = open_db(db_path, false) else {
        return false;
    };
    if !table_exists(&conn, "edge_sync_mapping") {
        return false;
    }
    let created_at = now_secs();
    let r = conn.execute(
        "INSERT OR REPLACE INTO edge_sync_mapping \
         (session_id, conversation_id, msg_channel, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            new_cid,
            new_cid,
            format!("convmsg:{target_uid}"),
            created_at
        ],
    );
    match r {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// 切换前把勾选的会话复制到目标账号（路径 B）。返回复制报告。
pub fn copy_sessions_for_switch(target_acc: &Value, session_ids: &[String]) -> Option<Value> {
    let target_uid = target_acc
        .get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if target_uid.is_empty() {
        return None;
    }
    let source_uid = current_user_uid()?;
    if source_uid == target_uid {
        return None;
    }

    let mut report = json!({
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "copied": [],
    });
    let mut errors: Vec<Value> = Vec::new();
    for cid in session_ids {
        match copy_session_to_user(cid, &source_uid, &target_uid) {
            Ok(r) => report["copied"].as_array_mut().unwrap().push(r),
            Err(e) => errors.push(json!({"id": cid, "error": e})),
        }
    }
    if !errors.is_empty() {
        report["errors"] = json!(errors);
    }
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn db_paths_point_to_home() {
        assert!(workbuddy_db_path()
            .to_string_lossy()
            .ends_with(".workbuddy/workbuddy.db"));
        assert!(edge_sync_db_path()
            .to_string_lossy()
            .ends_with("edge-sync-mapping-v2.db"));
    }

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_switch_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn insert_session_copy_duplicates_row_with_target_uid() {
        let db = temp_db("sessions");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                title TEXT,
                cwd TEXT,
                created_at INTEGER,
                updated_at INTEGER,
                deleted_at INTEGER,
                payload BLOB
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, created_at, updated_at, deleted_at, payload)
             VALUES ('src-1', 'uid-a', '旧标题', '/ws', 1000, 2000, NULL, x'DEADBEEF')",
            [],
        )
        .unwrap();

        insert_session_copy(&db, "new-uuid-1", "src-1", "uid-a", "uid-b").unwrap();

        let (id, user_id, title, deleted_at): (String, String, String, Option<i64>) = conn
            .query_row(
                "SELECT id, user_id, title, deleted_at FROM sessions WHERE id = 'new-uuid-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(id, "new-uuid-1");
        assert_eq!(user_id, "uid-b");
        assert_eq!(title, "旧标题"); // 普通列原样保留
        assert_eq!(deleted_at, None); // deleted_at 置空

        // 源行保持不变
        let src_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'src-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(src_count, 1);
    }

    #[test]
    fn insert_session_copy_missing_source_is_noop() {
        let db = temp_db("noop");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT, title TEXT, created_at INTEGER, updated_at INTEGER, deleted_at INTEGER);",
        )
        .unwrap();
        insert_session_copy(&db, "new-1", "missing", "uid-a", "uid-b").unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_session_copy_missing_db_is_ok() {
        let db = temp_db("missing");
        // 不创建文件
        assert!(insert_session_copy(&db, "new-1", "src-1", "a", "b").is_ok());
    }

    #[test]
    fn insert_edge_sync_mapping_registers_channel() {
        let db = temp_db("edge");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE edge_sync_mapping (
                session_id TEXT,
                conversation_id TEXT,
                msg_channel TEXT,
                created_at INTEGER
            );",
        )
        .unwrap();
        assert!(insert_edge_sync_mapping(&db, "new-1", "uid-b"));
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
    fn insert_edge_sync_mapping_missing_table_false() {
        let db = temp_db("edge-no-table");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        assert!(!insert_edge_sync_mapping(&db, "new-1", "uid-b"));
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
    fn normalize_cwd_trims_trailing_separators() {
        assert_eq!(normalize_cwd("/ws/"), "/ws");
        assert_eq!(normalize_cwd("C:\\ws\\"), "C:\\ws");
        assert_eq!(normalize_cwd("  /ws  "), "/ws");
        // 仅去尾部分隔符，前导分隔符保留（同一机器上复制会话的 cwd 完全一致）
        assert_eq!(normalize_cwd("/ws"), "/ws");
    }

    #[test]
    fn normalize_jsonl_neutralizes_session_id_for_comparison() {
        let src = "{\"sessionId\":\"abc-123\",\"text\":\"hello\"}";
        let tgt = "{\"sessionId\":\"def-456\",\"text\":\"hello\"}";
        // 抹平各自 id 后两份副本应相等
        assert_eq!(
            normalize_jsonl(src, "abc-123"),
            normalize_jsonl(tgt, "def-456")
        );
        // 中性占位符不应再包含原始 id
        assert!(!normalize_jsonl(src, "abc-123").contains("abc-123"));
    }
}

/// 端到端测试：模拟真实用户全生命周期操作，核心断言「任意 copy/dedup/list 操作
/// 都不会导致会话（状态/数据）意外丢失」。
///
/// 通过 `WORKBUDDY_HOME` 环境变量将 `home_dir()` 重定向到临时目录，实现密封（hermetic）
/// 测试，绝不触碰真实 `~/.workbuddy`。所有用例经全局互斥锁串行执行，避免环境变量竞争。
#[cfg(test)]
mod e2e_sessions {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static LOCK: Mutex<()> = Mutex::new(());
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_home() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("wb_e2e_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 建库 + 设置 WORKBUDDY_HOME（必须在持有 LOCK 时调用）。
    fn setup() -> PathBuf {
        let home = temp_home();
        std::env::set_var("WORKBUDDY_HOME", &home);
        let wb = home.join(".workbuddy");
        std::fs::create_dir_all(wb.join("projects")).unwrap();
        let conn = rusqlite::Connection::open(wb.join("workbuddy.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, user_id TEXT NOT NULL, title TEXT,
                custom_title TEXT, cwd TEXT, created_at INTEGER,
                updated_at INTEGER, deleted_at INTEGER, is_playground INTEGER DEFAULT 0
            );",
        )
        .unwrap();
        let econn = rusqlite::Connection::open(wb.join("edge-sync-mapping-v2.db")).unwrap();
        econn
            .execute_batch(
                "CREATE TABLE edge_sync_mapping (
                    session_id TEXT, conversation_id TEXT, msg_channel TEXT, created_at INTEGER
                );",
            )
            .unwrap();
        home
    }

    fn projects(home: &Path) -> PathBuf {
        home.join(".workbuddy").join("projects")
    }

    fn seed_session(
        home: &Path, uid: &str, cid: &str, title: &str, cwd: &str,
        body: Option<&str>, custom_title: Option<&str>, is_pg: i64, ua: i64,
    ) {
        let db = home.join(".workbuddy").join("workbuddy.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO sessions (id,user_id,title,custom_title,cwd,created_at,updated_at,deleted_at,is_playground) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,?8)",
            rusqlite::params![cid, uid, title, custom_title, cwd, ua, ua, is_pg],
        )
        .unwrap();
        if let Some(body) = body {
            let cwd_t = cwd.trim_end_matches(['/', '\\']);
            let ws = if cwd_t.is_empty() {
                "playground".to_string()
            } else {
                Path::new(cwd_t)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "ws".to_string())
            };
            let d = projects(home).join(&ws);
            std::fs::create_dir_all(&d).unwrap();
            let content = format!(
                "{{\"sessionId\":\"{cid}\",\"messages\":[{{\"role\":\"user\",\"content\":\"{body}\"}}]}}"
            );
            std::fs::write(d.join(format!("{cid}.jsonl")), content).unwrap();
        }
    }

    /// 灌入一份遗留重复行（同 cwd、同正文、不同 id、较早时间戳）。
    fn seed_duplicate(home: &Path, uid: &str, cid: &str, cwd: &str, body: &str, ua: i64) {
        let db = home.join(".workbuddy").join("workbuddy.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO sessions (id,user_id,title,custom_title,cwd,created_at,updated_at,deleted_at,is_playground) \
             VALUES (?1,?2,?3,NULL,?4,?5,?5,NULL,0)",
            rusqlite::params![cid, uid, "需求评审", cwd, ua],
        )
        .unwrap();
        let cwd_t = cwd.trim_end_matches(['/', '\\']);
        let ws = if cwd_t.is_empty() {
            "playground".to_string()
        } else {
            Path::new(cwd_t)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "ws".to_string())
        };
        let d = projects(home).join(&ws);
        std::fs::create_dir_all(&d).unwrap();
        let content =
            format!("{{\"sessionId\":\"{cid}\",\"messages\":[{{\"role\":\"user\",\"content\":\"{body}\"}}]}}");
        std::fs::write(d.join(format!("{cid}.jsonl")), content).unwrap();
        // 注册云端映射，便于验证去重清理
        let edb = home.join(".workbuddy").join("edge-sync-mapping-v2.db");
        let econn = rusqlite::Connection::open(&edb).unwrap();
        econn
            .execute(
                "INSERT OR REPLACE INTO edge_sync_mapping (session_id,conversation_id,msg_channel,created_at) \
                 VALUES (?1,?1,?2,0)",
                rusqlite::params![cid, format!("convmsg:{uid}")],
            )
            .unwrap();
    }

    // -- 与 session.rs 一致的纯函数（测试内复刻，用于断言） --
    fn n_cwd(cwd: &str) -> String {
        cwd.trim().trim_end_matches(['/', '\\']).to_string()
    }
    const NEUTRAL: &str = "\u{0}WB_SWITCH_NEUTRAL_ID\u{0}";
    fn n_jsonl(text: &str, cid: &str) -> String {
        text.replace(cid, NEUTRAL)
    }
    fn is_claw(cwd: &str) -> bool {
        let s = cwd.trim().trim_end_matches(['/', '\\']);
        let parts: Vec<&str> = s.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        let last = parts.last().copied().unwrap_or("");
        last.eq_ignore_ascii_case("claw")
    }
    fn find_jsonl(home: &Path, cid: &str) -> Option<PathBuf> {
        let base = projects(home);
        let direct = base.join(format!("{cid}.jsonl"));
        if direct.is_file() {
            return Some(direct);
        }
        if let Ok(entries) = std::fs::read_dir(&base) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let cand = p.join(format!("{cid}.jsonl"));
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }
        None
    }
    /// 带正文会话的 (归一化cwd, 归一化正文) 集合 —— 应跨操作保持不变。
    fn content_fps(home: &Path, uids: &[&str]) -> HashSet<(String, String)> {
        let db = home.join(".workbuddy").join("workbuddy.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut set = HashSet::new();
        for uid in uids {
            let u = *uid;
            let mut stmt = conn
                .prepare("SELECT id, cwd FROM sessions WHERE user_id=?1 AND deleted_at IS NULL")
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params![u], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap();
            for row in rows {
                let (cid, cwd) = row.unwrap();
                if is_claw(&cwd) {
                    continue;
                }
                if let Some(p) = find_jsonl(home, &cid) {
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        set.insert((n_cwd(&cwd), n_jsonl(&text, &cid)));
                    }
                }
            }
        }
        set
    }
    fn active_count(home: &Path, uids: &[&str]) -> usize {
        let db = home.join(".workbuddy").join("workbuddy.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut n = 0;
        for uid in uids {
            let u = *uid;
            let mut stmt = conn
                .prepare("SELECT id, cwd FROM sessions WHERE user_id=?1 AND deleted_at IS NULL")
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params![u], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap();
            for row in rows {
                let (_cid, cwd) = row.unwrap();
                if !is_claw(&cwd) {
                    n += 1;
                }
            }
        }
        n
    }
    fn edge_leftover(home: &Path, cids: &[&str]) -> i64 {
        let edb = home.join(".workbuddy").join("edge-sync-mapping-v2.db");
        let conn = rusqlite::Connection::open(&edb).unwrap();
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM edge_sync_mapping WHERE session_id = ?1")
            .unwrap();
        let mut total = 0i64;
        for c in cids {
            total += stmt.query_row(rusqlite::params![c], |r| r.get::<_, i64>(0)).unwrap();
        }
        total
    }

    // ============================ 场景 ============================

    fn scn_e1(home: &Path) {
        let uid = "uid-A";
        seed_session(home, uid, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 1000);
        seed_session(home, uid, "a2", "会议纪要", "/ws/项目乙", Some("乙的会议内容"), None, 0, 1000);
        seed_session(home, uid, "a3", "快速问答", "", Some("问答内容"), None, 1, 1000);
        seed_session(home, uid, "a4", "空任务", "", None, None, 1, 1000); // 无正文
        seed_session(home, uid, "acl", "claw绑定", "/ws/Claw", Some("claw内容"), None, 0, 1000);

        let listed = list_sessions_for_user(uid);
        assert_eq!(listed.as_array().map(|a| a.len()).unwrap_or(0), 4, "列举应排除 claw，返回 4 条");

        let cfp = content_fps(home, &[uid]);
        assert_eq!(cfp.len(), 3, "内容指纹应含 3 份正文");

        for (cid, body) in [("a1", "甲的需求内容"), ("a2", "乙的会议内容"), ("a3", "问答内容")] {
            let p = find_jsonl(home, cid);
            let ok = p.map(|p| std::fs::read_to_string(&p).unwrap_or_default().contains(body)).unwrap_or(false);
            assert!(ok, "恢复 {cid} 的 jsonl 内容应完整");
        }
        assert!(find_jsonl(home, "a4").is_none(), "空任务 a4 不应有 jsonl");
        assert!(find_jsonl(home, "acl").is_some(), "claw 会话 acl 始终存活");
    }

    fn scn_e2(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        seed_session(home, ua, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 1000);
        let before_a = active_count(home, &[ua]);
        let res = copy_session_to_user("a1", ua, ub).unwrap();
        assert!(!res["skipped"].as_bool().unwrap() && res["newId"].as_str().unwrap().len() > 0);
        assert_eq!(active_count(home, &[ua]), before_a, "源账号行数不变");
        assert_eq!(active_count(home, &[ub]), 1, "目标新增 1 条");
        let new_id = res["newId"].as_str().unwrap().to_string();
        let p = find_jsonl(home, &new_id);
        assert!(p.is_some() && p.as_ref().unwrap().is_file(), "目标新会话 jsonl 已生成");
        let src_p = find_jsonl(home, "a1").unwrap();
        let eq = n_jsonl(&std::fs::read_to_string(&p.unwrap()).unwrap(), &new_id)
            == n_jsonl(&std::fs::read_to_string(&src_p).unwrap(), "a1");
        assert!(eq, "复制正文与源等价(去标识后)");
        assert_eq!(edge_leftover(home, &[&new_id]), 0, "新会话已注册云端映射");
    }

    fn scn_e3(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        seed_session(home, ua, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 1000);
        copy_session_to_user("a1", ua, ub).unwrap();
        let before_b = active_count(home, &[ub]);
        let res = copy_session_to_user("a1", ua, ub).unwrap();
        assert!(res["skipped"].as_bool().unwrap(), "重复复制应跳过");
        assert_eq!(res["newId"].as_str().unwrap(), "", "重复复制 newId 应为空串");
        assert_eq!(active_count(home, &[ub]), before_b, "目标行数不再增加");
    }

    fn scn_e4(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        seed_session(home, ua, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 1000);
        seed_session(home, ua, "a2", "会议纪要", "/ws/项目乙", Some("乙的会议内容"), None, 0, 1000);
        let cfp0 = content_fps(home, &[ua, ub]);
        assert_eq!(active_count(home, &[ua]), 2, "初始 A 有 2 行");

        let r1 = copy_session_to_user("a1", ua, ub).unwrap();
        let r2 = copy_session_to_user("a2", ua, ub).unwrap();
        assert!(r1["newId"].as_str().unwrap().len() > 0 && r2["newId"].as_str().unwrap().len() > 0);
        assert_eq!(active_count(home, &[ub]), 2, "复制后 B 有 2 行");

        // B->A 回复制：A 已存在等价会话，应被跳过（防雪球）
        let a1p = r1["newId"].as_str().unwrap().to_string();
        let a2p = r2["newId"].as_str().unwrap().to_string();
        let r3 = copy_session_to_user(&a1p, ub, ua).unwrap();
        let r4 = copy_session_to_user(&a2p, ub, ua).unwrap();
        assert!(r3["skipped"].as_bool().unwrap() && r4["skipped"].as_bool().unwrap(), "B->A 回复制应被跳过(防雪球)");
        assert_eq!(active_count(home, &[ua]), 2, "跳过回复制后 A 仍仅 2 行");

        // A->B 再次复制：B 已有等价会话，亦应跳过
        let r5 = copy_session_to_user("a1", ua, ub).unwrap();
        let r6 = copy_session_to_user("a2", ua, ub).unwrap();
        assert!(r5["skipped"].as_bool().unwrap() && r6["skipped"].as_bool().unwrap(), "A->B 二次复制应被跳过");

        let cfp1 = content_fps(home, &[ua, ub]);
        assert_eq!(cfp1, cfp0, "雪球后内容指纹集合不变(零丢失)");
        assert_eq!(active_count(home, &[ua]), 2, "A 无重复行");
        assert_eq!(active_count(home, &[ub]), 2, "B 无重复行");
    }

    fn scn_e5(home: &Path) {
        let uid = "uid-A";
        seed_session(home, uid, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 2000);
        seed_session(home, uid, "a2", "会议纪要", "/ws/项目乙", Some("乙的会议内容"), None, 0, 2000);
        seed_duplicate(home, uid, "a1_dup", "/ws/项目甲/", "甲的需求内容", 1000);
        seed_duplicate(home, uid, "a2_dup", "/ws/项目乙", "乙的会议内容", 1000);
        assert_eq!(active_count(home, &[uid]), 4, "去重前 A 有 4 行(2 原件+2 遗留重复)");
        let cfp0 = content_fps(home, &[uid]);
        assert_eq!(cfp0.len(), 2, "去重前内容指纹为 2 份");

        let res = dedup_sessions_for_user(uid);
        assert_eq!(res["removed"].as_u64().unwrap(), 2, "去重应移除 2 份重复");
        assert_eq!(active_count(home, &[uid]), 2, "去重后回到 2 行");
        let cfp1 = content_fps(home, &[uid]);
        assert_eq!(cfp1, cfp0, "去重后内容指纹不变(零丢失)");

        for cid in ["a1", "a2"] {
            let p = find_jsonl(home, cid);
            assert!(p.is_some() && p.unwrap().is_file(), "原件 {cid} 存活且 jsonl 完整");
        }
        for cid in ["a1_dup", "a2_dup"] {
            assert!(find_jsonl(home, cid).is_none(), "重复 {cid} 的 jsonl 已清理");
        }
        assert_eq!(edge_leftover(home, &["a1_dup", "a2_dup"]), 0, "被删重复云端映射已清理");
    }

    fn scn_e6(home: &Path) {
        let uid = "uid-A";
        seed_session(home, uid, "x1", "无标题", "/ws/项目甲/", Some("内容一：关于方案A"), None, 0, 1000);
        seed_session(home, uid, "x2", "无标题", "/ws/项目甲/", Some("内容二：关于方案B"), None, 0, 1000);
        assert_eq!(active_count(home, &[uid]), 2, "去重前 2 条同名会话");
        let res = dedup_sessions_for_user(uid);
        assert_eq!(res["removed"].as_u64().unwrap(), 0, "同名不同内容者不应误删");
        assert_eq!(active_count(home, &[uid]), 2, "两者均应存活");
        assert_eq!(content_fps(home, &[uid]).len(), 2, "内容指纹仍为 2 份");
    }

    fn scn_e7(home: &Path) {
        let uid = "uid-A";
        seed_session(home, uid, "e1", "空任务", "", None, None, 1, 1000);
        seed_session(home, uid, "e2", "空任务", "", None, None, 1, 1000);
        seed_session(home, uid, "e3", "空任务", "", None, None, 1, 1000);
        assert_eq!(active_count(home, &[uid]), 3, "去重前 3 个空会话");
        let res = dedup_sessions_for_user(uid);
        assert_eq!(res["removed"].as_u64().unwrap(), 0, "空会话各自唯一键，不应合并");
        assert_eq!(active_count(home, &[uid]), 3, "三者均应存活");
    }

    fn scn_e8(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        seed_session(home, ua, "acl", "claw绑定", "/ws/Claw", Some("claw内容"), None, 0, 1000);
        let before_b = active_count(home, &[ub]);
        let res = copy_session_to_user("acl", ua, ub);
        assert!(res.is_err(), "复制 claw 应返回错误");
        assert!(find_jsonl(home, "acl").is_some(), "源 claw 会话仍存活");
        assert_eq!(active_count(home, &[ub]), before_b, "目标未受影响");
    }

    fn scn_e9(home: &Path) {
        let res = dedup_sessions_for_user("uid-EMPTY");
        assert!(res["ok"].as_bool().unwrap() && res["removed"].as_u64().unwrap() == 0, "空账号去重应 ok 且 removed==0");
        // 缺失 db：临时 home 无 .workbuddy
        let missing = temp_home();
        std::env::set_var("WORKBUDDY_HOME", &missing);
        let res2 = dedup_sessions_for_user("uid-X");
        assert!(!res2["ok"].as_bool().unwrap() && res2["removed"].as_u64().unwrap() == 0, "缺失 db 应优雅返回 ok:false");
        // 还原到本测试 home（后续断言不再依赖，但保持整洁）
        std::env::set_var("WORKBUDDY_HOME", home);
    }

    fn scn_e10(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        seed_session(home, ua, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 1000);
        let before_a = active_count(home, &[ua]);
        let before_b = active_count(home, &[ub]);
        let _res = copy_session_to_user("不存在的cid", ua, ub);
        assert_eq!(active_count(home, &[ub]), before_b, "缺失源复制：B 行数不变");
        assert_eq!(active_count(home, &[ua]), before_a, "缺失源复制：A 行数不变");
        assert!(find_jsonl(home, "不存在的cid").is_none(), "缺失源复制：未产生孤儿 jsonl");
    }

    fn scn_e11(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        let cwd = "/工作区/项目甲/"; // 尾部带分隔符
        seed_session(home, ua, "z1", "中文标题", cwd, Some("中文会话内容"), None, 0, 1000);
        let res = copy_session_to_user("z1", ua, ub).unwrap();
        assert!(res["newId"].as_str().unwrap().len() > 0, "中文路径复制应成功");
        let new_id = res["newId"].as_str().unwrap().to_string();
        let p = find_jsonl(home, &new_id);
        let expected_dir = projects(home).join("项目甲");
        assert_eq!(p.map(|p| p.parent().map(|x| x.to_path_buf())), Some(Some(expected_dir)),
                   "新 jsonl 应落于正确子目录(尾部分隔符已归一)");
        let res2 = dedup_sessions_for_user(ub);
        assert_eq!(res2["removed"].as_u64().unwrap(), 0, "中文路径去重 removed==0");
    }

    fn scn_e12(home: &Path) {
        let (ua, ub) = ("uid-A", "uid-B");
        seed_session(home, ua, "a1", "需求评审", "/ws/项目甲/", Some("甲的需求内容"), None, 0, 1000);
        seed_session(home, ua, "a2", "会议纪要", "/ws/项目乙", Some("乙的会议内容"), None, 0, 1000);
        seed_session(home, ua, "a3", "问答", "", Some("问答内容"), None, 1, 1000);
        seed_session(home, ua, "ae", "空任务", "", None, None, 1, 1000); // 空会话，应始终存活

        let cfp0 = content_fps(home, &[ua, ub]);
        let uids: &[&str] = &[ua, ub];
        let mut expected = active_count(home, uids);
        let assert_invariant = |home: &Path, expected: usize, tag: &str| {
            let cfp = content_fps(home, &[ua, ub]);
            assert_eq!(cfp, cfp0, "[{tag}] 内容指纹集合不变(零丢失)");
            let njs = active_count(home, &[ua, ub]);
            assert_eq!(njs, expected, "[{tag}] 活跃行数应等于预期(无意外丢失)");
        };
        assert_invariant(home, expected, "init");

        // 确定性交替序列：copy / dedup / list 交错
        copy_session_to_user("a1", ua, ub).unwrap();
        expected += 1;
        copy_session_to_user("a2", ua, ub).unwrap();
        expected += 1;
        assert_invariant(home, expected, "after-copy-ab");

        // B->A 回复制应被跳过（防雪球），不新增
        let b_rows = list_sessions_for_user(ub);
        let b_ids: Vec<String> = b_rows
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect();
        for bid in &b_ids {
            let r = copy_session_to_user(bid, ub, ua).unwrap();
            assert!(r["skipped"].as_bool().unwrap(), "B->A 回复制应跳过");
        }
        assert_invariant(home, expected, "after-backcopy");

        dedup_sessions_for_user(ua);
        dedup_sessions_for_user(ub);
        assert_invariant(home, expected, "after-dedup");

        // 复制空会话 ae 到 B（应新增一份空会话，不增加指纹）
        let r = copy_session_to_user("ae", ua, ub).unwrap();
        assert!(!r["skipped"].as_bool().unwrap(), "空会话复制应成功新增");
        expected += 1;
        assert_invariant(home, expected, "after-copy-empty");

        dedup_sessions_for_user(ub);
        assert_invariant(home, expected, "after-dedup2");

        // 仅列举（只读）
        let _ = list_sessions_for_user(ua);
        let _ = list_sessions_for_user(ub);
        assert_invariant(home, expected, "final");

        assert_eq!(content_fps(home, &[ua, ub]), cfp0, "最终内容指纹 == 初始内容指纹");
    }

    // ============================ 测试入口（串行） ============================
    #[test]
    fn e2e_e1_creation_persistence_recovery() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e1(&home);
    }
    #[test]
    fn e2e_e2_copy_first() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e2(&home);
    }
    #[test]
    fn e2e_e3_copy_duplicate_skip() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e3(&home);
    }
    #[test]
    fn e2e_e4_snowball_no_accumulation() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e4(&home);
    }
    #[test]
    fn e2e_e5_dedup_keeps_unique() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e5(&home);
    }
    #[test]
    fn e2e_e6_same_name_diff_content() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e6(&home);
    }
    #[test]
    fn e2e_e7_empty_sessions_not_merged() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e7(&home);
    }
    #[test]
    fn e2e_e8_claw_not_copyable() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e8(&home);
    }
    #[test]
    fn e2e_e9_empty_and_missing_db() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e9(&home);
    }
    #[test]
    fn e2e_e10_copy_missing_source() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e10(&home);
    }
    #[test]
    fn e2e_e11_chinese_paths() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e11(&home);
    }
    #[test]
    fn e2e_e12_interleaved_invariant() {
        let _g = LOCK.lock().unwrap();
        let home = setup();
        scn_e12(&home);
    }
}

