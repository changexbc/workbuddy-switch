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
use crate::modules::copy_map;

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

/// 查目标账号下是否已有「同标题+同目录」的未删除会话（判重兜底）。
/// 返回命中的会话 id。
fn find_same_session(conn: &Connection, uid: &str, title: &str, cwd: &str) -> Option<String> {
    if !table_exists(conn, "sessions") {
        return None;
    }
    let has_custom = column_exists(conn, "sessions", "custom_title");
    let sql = if has_custom {
        "SELECT id, title, custom_title FROM sessions \
         WHERE user_id = ?1 AND deleted_at IS NULL AND cwd = ?2"
    } else {
        "SELECT id, title, NULL FROM sessions \
         WHERE user_id = ?1 AND deleted_at IS NULL AND cwd = ?2"
    };
    let mut stmt = conn.prepare(sql).ok()?;
    let rows = stmt
        .query_map(rusqlite::params![uid, cwd], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .ok()?;
    for r in rows.flatten() {
        let (id, t, ct) = r;
        if session_display_title(t, ct) == title {
            return id;
        }
    }
    None
}

/// 读取源会话的展示标题与目录（供判重兜底使用）。
fn source_session_meta(conn: &Connection, cid: &str, uid: &str) -> Option<(String, String)> {
    if !table_exists(conn, "sessions") {
        return None;
    }
    let has_custom = column_exists(conn, "sessions", "custom_title");
    let sql = if has_custom {
        "SELECT title, custom_title, cwd FROM sessions WHERE id = ?1 AND user_id = ?2"
    } else {
        "SELECT title, NULL, cwd FROM sessions WHERE id = ?1 AND user_id = ?2"
    };
    conn.query_row(sql, rusqlite::params![cid, uid], |row| {
        Ok((
            session_display_title(
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ),
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        ))
    })
    .ok()
}

/// 目标会话当前是否存在且未删除。
fn session_alive(conn: &Connection, cid: &str, uid: &str) -> bool {
    if !table_exists(conn, "sessions") {
        return false;
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1 AND user_id = ?2 AND deleted_at IS NULL)",
        rusqlite::params![cid, uid],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        == 1
}

/// 复制前去重：命中返回已存在会话 id，未命中返回 None。
///
/// 顺序：复制关系表（精确）→ 同标题+同目录（兜底）。
fn find_existing_copy(conn: &Connection, cid: &str, source_uid: &str, target_uid: &str) -> Option<String> {
    if let Some(existing) = copy_map::find_copy(source_uid, cid, target_uid) {
        if session_alive(conn, &existing, target_uid) {
            return Some(existing);
        }
    }
    let (title, cwd) = source_session_meta(conn, cid, source_uid)?;
    find_same_session(conn, target_uid, &title, &cwd)
}

/// 把 source_uid 的一个会话复制为 target_uid 的新会话（路径 B：生成新 id）。
///
/// 全部按「新 id」复制一份给目标账号，源账号数据完全不动。
/// 新 id 必须用带连字符的 UUID 格式（`Uuid::new_v4().to_string()`），与官方一致；
/// 32 位无连字符形式会导致 WorkBuddy 无法识别新会话。
///
/// 去重：复制前先查复制关系表、再按「同标题+同目录」兜底；
/// 已复制过的会话直接跳过（返回 `skipped: true`），不再产生重复副本。
pub fn copy_session_to_user(
    cid: &str,
    source_uid: &str,
    target_uid: &str,
) -> Result<Value, String> {
    let db = workbuddy_db_path();
    if let Some(conn) = open_db(&db, true) {
        let cwd: Option<String> = conn
            .query_row(
                "SELECT cwd FROM sessions WHERE id = ?1 AND user_id = ?2",
                rusqlite::params![cid, source_uid],
                |r| r.get(0),
            )
            .ok();
        if cwd.as_deref().is_some_and(is_claw_workspace) {
            return Err("Claw 工作区绑定当前账号渠道，不支持复制".into());
        }
        // 去重检查：已有副本则跳过
        if let Some(existing) = find_existing_copy(&conn, cid, source_uid, target_uid) {
            return Ok(json!({
                "id": cid,
                "skipped": true,
                "reason": "目标账号已存在该会话的副本",
                "existingId": existing,
            }));
        }
    }

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

    // 4) 记录复制关系（下次复制同一条会话时据此判重）
    copy_map::record_copy(source_uid, cid, target_uid, &new_cid);

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
        "skipped": [],
    });
    let mut errors: Vec<Value> = Vec::new();
    for cid in session_ids {
        match copy_session_to_user(cid, &source_uid, &target_uid) {
            Ok(r) => {
                if r["skipped"].as_bool() == Some(true) {
                    report["skipped"].as_array_mut().unwrap().push(r);
                } else {
                    report["copied"].as_array_mut().unwrap().push(r);
                }
            }
            Err(e) => errors.push(json!({"id": cid, "error": e})),
        }
    }
    if !errors.is_empty() {
        report["errors"] = json!(errors);
    }
    Some(report)
}

// ---------------------------------------------------------------------------
// 重复会话清理（同账号下「同标题+同目录」视为重复，保留最新一条）
// ---------------------------------------------------------------------------

/// 扫描某账号下的重复会话分组。
///
/// 返回每组 { key, keep, duplicates: [...] }：keep 为保留的最新会话，
/// duplicates 为建议删除的旧副本。无重复时返回空数组。
pub fn scan_duplicate_sessions(uid: &str) -> Value {
    let db = workbuddy_db_path();
    let mut groups: Vec<Value> = Vec::new();
    if !db.is_file() {
        return json!({"groups": groups});
    }
    let Some(conn) = open_db(&db, true) else {
        return json!({"groups": groups});
    };
    if !table_exists(&conn, "sessions") {
        return json!({"groups": groups});
    }
    let has_custom = column_exists(&conn, "sessions", "custom_title");
    let has_playground = column_exists(&conn, "sessions", "is_playground");
    let sql = match (has_custom, has_playground) {
        (true, true) => {
            "SELECT id, title, custom_title, cwd, updated_at, is_playground FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
        (true, false) => {
            "SELECT id, title, custom_title, cwd, updated_at, 0 FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
        (false, true) => {
            "SELECT id, title, NULL, cwd, updated_at, is_playground FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
        (false, false) => {
            "SELECT id, title, NULL, cwd, updated_at, 0 FROM sessions \
             WHERE user_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
        }
    };
    let Ok(mut stmt) = conn.prepare(sql) else {
        return json!({"groups": groups});
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

    // 按 (展示标题, cwd, is_playground) 分组；已按 updated_at DESC 排序，首条即保留项
    let mut seen: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    if let Ok(iter) = rows {
        for r in iter.flatten() {
            let (cid, title, custom_title, cwd, updated_at, is_playground) = r;
            let cid = cid.unwrap_or_default();
            let cwd = cwd.unwrap_or_default();
            if cid.is_empty() || is_claw_workspace(&cwd) {
                continue;
            }
            let display = session_display_title(title, custom_title);
            let key = format!("{}\u{1f}{}\u{1f}{}", display, cwd, is_playground.unwrap_or(0));
            seen.entry(key).or_default().push(json!({
                "id": cid,
                "title": display,
                "cwd": cwd,
                "updatedAt": updated_at.unwrap_or(0),
            }));
        }
    }
    for (_key, list) in seen {
        if list.len() < 2 {
            continue;
        }
        let keep = list[0].clone();
        let dups: Vec<Value> = list[1..].to_vec();
        groups.push(json!({
            "title": keep["title"],
            "cwd": keep["cwd"],
            "keep": keep,
            "duplicates": dups,
        }));
    }
    // 重复多的组排前面
    groups.sort_by(|a, b| {
        b["duplicates"]
            .as_array()
            .map(|d| d.len())
            .cmp(&a["duplicates"].as_array().map(|d| d.len()))
    });
    let dup_total: usize = groups
        .iter()
        .filter_map(|g| g["duplicates"].as_array().map(|d| d.len()))
        .sum();
    json!({"groups": groups, "duplicateCount": dup_total})
}

/// 硬删除指定会话：备份 db 与 jsonl 后，删除 sessions 行、edge_sync_mapping 行、jsonl 文件。
///
/// 仅允许删除属于 uid 的会话；跳过不存在的 id。返回删除报告与备份目录。
pub fn cleanup_duplicate_sessions(uid: &str, session_ids: &[String]) -> Result<Value, String> {
    if session_ids.is_empty() {
        return Err("没有需要删除的会话".into());
    }
    let db = workbuddy_db_path();
    if !db.is_file() {
        return Err("workbuddy.db 不存在".into());
    }

    // 1) 备份：db + 每个待删会话的 jsonl
    let backup_root = backup_dir().join("dedup-cleanup").join(utc_iso());
    let jsonl_backup = backup_root.join("jsonl");
    std::fs::create_dir_all(&jsonl_backup).map_err(|e| e.to_string())?;
    backup_workbuddy_db(&backup_root);
    for cid in session_ids {
        if let Some(p) = find_project_jsonl(cid) {
            let _ = std::fs::copy(&p, jsonl_backup.join(format!("{cid}.jsonl")));
        }
    }

    // 2) 删 db 行 + jsonl 文件
    let Some(conn) = open_db(&db, false) else {
        return Err("无法打开 workbuddy.db".into());
    };
    let mut deleted: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for cid in session_ids {
        let n = conn
            .execute(
                "DELETE FROM sessions WHERE id = ?1 AND user_id = ?2",
                rusqlite::params![cid, uid],
            )
            .unwrap_or(0);
        if n > 0 {
            if let Some(p) = find_project_jsonl(cid) {
                let _ = std::fs::remove_file(p);
            }
            delete_edge_sync_mapping(cid);
            deleted.push(cid.clone());
        } else {
            missing.push(cid.clone());
        }
    }

    // 3) 清理复制关系表中指向已删会话的映射
    copy_map::remove_copies_for_target(&deleted);

    Ok(json!({
        "deleted": deleted,
        "missing": missing,
        "backup": backup_root.to_string_lossy().to_string(),
    }))
}

/// 删除 edge_sync_mapping 中的映射行（避免已删会话残留云端映射）。
fn delete_edge_sync_mapping(cid: &str) {
    let db = edge_sync_db_path();
    if !db.is_file() {
        return;
    }
    if let Some(conn) = open_db(&db, false) {
        if table_exists(&conn, "edge_sync_mapping") {
            let _ = conn.execute(
                "DELETE FROM edge_sync_mapping WHERE session_id = ?1 OR conversation_id = ?1",
                rusqlite::params![cid],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn db_paths_point_to_home() {
        let db_path = workbuddy_db_path();
        assert_eq!(db_path.file_name().and_then(|s| s.to_str()), Some("workbuddy.db"));
        assert_eq!(
            db_path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()),
            Some(".workbuddy")
        );
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

    /// 建一个带 custom_title 的内存 sessions 表。
    fn mem_sessions() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                user_id TEXT,
                title TEXT,
                custom_title TEXT,
                cwd TEXT,
                updated_at INTEGER,
                deleted_at INTEGER
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn find_same_session_matches_display_title_and_cwd() {
        let conn = mem_sessions();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, custom_title, cwd, deleted_at)
             VALUES ('t-1', 'uid-b', '自动标题', '我的会话', '/ws', NULL)",
            [],
        )
        .unwrap();
        // 展示标题取 custom_title
        assert_eq!(
            find_same_session(&conn, "uid-b", "我的会话", "/ws"),
            Some("t-1".to_string())
        );
        // cwd 不同不命中
        assert_eq!(find_same_session(&conn, "uid-b", "我的会话", "/other"), None);
        // 标题不同不命中
        assert_eq!(find_same_session(&conn, "uid-b", "别的", "/ws"), None);
        // uid 不同不命中
        assert_eq!(find_same_session(&conn, "uid-c", "我的会话", "/ws"), None);
    }

    #[test]
    fn find_same_session_ignores_soft_deleted() {
        let conn = mem_sessions();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, custom_title, cwd, deleted_at)
             VALUES ('t-2', 'uid-b', '旧会话', NULL, '/ws', 12345)",
            [],
        )
        .unwrap();
        assert_eq!(find_same_session(&conn, "uid-b", "旧会话", "/ws"), None);
        assert!(!session_alive(&conn, "t-2", "uid-b"));
    }

    #[test]
    fn session_alive_true_only_for_live_row_of_uid() {
        let conn = mem_sessions();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, deleted_at)
             VALUES ('t-3', 'uid-b', '会话', '/ws', NULL)",
            [],
        )
        .unwrap();
        assert!(session_alive(&conn, "t-3", "uid-b"));
        assert!(!session_alive(&conn, "t-3", "uid-a")); // 别的账号
        assert!(!session_alive(&conn, "missing", "uid-b"));
    }

    #[test]
    fn find_existing_copy_falls_back_to_title_and_cwd() {
        let conn = mem_sessions();
        // 源会话（uid-a）与目标已存在的同标题同目录会话（uid-b）
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, deleted_at)
             VALUES ('s-1', 'uid-a', '重复会话', '/ws', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, deleted_at)
             VALUES ('t-9', 'uid-b', '重复会话', '/ws', NULL)",
            [],
        )
        .unwrap();
        // 复制关系表中没有记录（用不会出现在真实数据里的 uid），走兜底匹配
        assert_eq!(
            find_existing_copy(&conn, "s-1", "uid-a", "uid-b"),
            Some("t-9".to_string())
        );
        // 源会话不存在 → None
        assert_eq!(find_existing_copy(&conn, "missing", "uid-a", "uid-b"), None);
    }

    #[test]
    fn find_existing_copy_prefers_copy_map_record() {
        // 文件态 copy-map 测试串行执行，且使用不会与真实数据冲突的 uid/cid
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();

        let conn = mem_sessions();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, deleted_at)
             VALUES ('test-src-x1', 'test-uid-src-x1', '会话A', '/ws', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, user_id, title, cwd, deleted_at)
             VALUES ('test-tgt-x1', 'test-uid-tgt-x1', '不同标题也认', '/elsewhere', NULL)",
            [],
        )
        .unwrap();
        copy_map::record_copy("test-uid-src-x1", "test-src-x1", "test-uid-tgt-x1", "test-tgt-x1");
        assert_eq!(
            find_existing_copy(&conn, "test-src-x1", "test-uid-src-x1", "test-uid-tgt-x1"),
            Some("test-tgt-x1".to_string())
        );
        // 目标会话被删后，关系表命中但已失效 → 兜底也不命中 → None
        conn.execute(
            "UPDATE sessions SET deleted_at = 1 WHERE id = 'test-tgt-x1'",
            [],
        )
        .unwrap();
        assert_eq!(
            find_existing_copy(&conn, "test-src-x1", "test-uid-src-x1", "test-uid-tgt-x1"),
            None
        );
        copy_map::remove_copies_for_target(&["test-tgt-x1".to_string()]);
    }
}
