//! 带走定时任务（L3 归属层，DB）：把 `automations` 与 `automation_delivery_outbox`
//! 的 `owner_user_id` 对齐到目标账号。
//!
//! 独立成模块（2026-09-19 主人定）：上游发 PR 单独走一个，不与「同步设置与文件」
//! （`align.rs` 的 L4/L5）捆绑。与 align 的关系：
//! - `align_data`（真实执行/预览）在 `align_automations` 开关下调
//!   [`align_automations_owner_in_db`]；
//! - 独立入口 [`align_automations_owner`] 由 `/api/automations/align` 使用
//!   （不切号也能对齐，需先完全退出 WorkBuddy）。

use serde_json::{json, Value};
use std::path::Path;

use crate::modules::config::{backup_dir, now_ms, utc_iso};
use crate::modules::session::{backup_workbuddy_db, open_db, table_exists, workbuddy_db_path};
use crate::modules::variant::WbVariant;

/// 把未删除自动化的 owner 对齐到目标账号（含备份）。db 不存在返回 None。
pub fn align_automations_owner(target_uid: &str) -> Option<Value> {
    let db = workbuddy_db_path(WbVariant::Cn);
    if !db.is_file() {
        return None;
    }
    let backup = backup_workbuddy_db(WbVariant::Cn, &backup_dir().join("automations").join(utc_iso()))
        .map(|p| p.to_string_lossy().to_string());
    let (automations, outbox) =
        align_automations_owner_in_db(&db, target_uid, false).unwrap_or((0, 0));
    Some(json!({
        "targetUid": target_uid,
        "automationsUpdated": automations,
        "outboxUpdated": outbox,
        "backup": backup,
    }))
}

/// 低层：对齐 automations + outbox 的 owner，返回 (automations 行数, outbox 行数)。
pub(crate) fn align_automations_owner_in_db(
    db_path: &Path,
    target_uid: &str,
    dry_run: bool,
) -> Result<(usize, usize), String> {
    if !db_path.is_file() {
        return Ok((0, 0));
    }
    let Some(conn) = open_db(db_path, false) else {
        return Ok((0, 0));
    };

    let mut automations_updated = 0usize;
    if table_exists(&conn, "automations") {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM automations \
                 WHERE deleted_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if n > 0 && !dry_run {
            conn.execute(
                "UPDATE automations SET owner_user_id = ?1, updated_at = ?2 \
                 WHERE deleted_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        }
        automations_updated = n as usize;
    }

    let mut outbox_updated = 0usize;
    if table_exists(&conn, "automation_delivery_outbox") {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM automation_delivery_outbox \
                 WHERE finished_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if n > 0 && !dry_run {
            conn.execute(
                "UPDATE automation_delivery_outbox SET owner_user_id = ?1, updated_at = ?2 \
                 WHERE finished_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        }
        outbox_updated = n as usize;
    }

    Ok((automations_updated, outbox_updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_switch_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn setup(db: &Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "CREATE TABLE automations (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                owner_user_id TEXT,
                status TEXT NOT NULL DEFAULT 'ACTIVE',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                deleted_at INTEGER
            );
            CREATE TABLE automation_delivery_outbox (
                id TEXT PRIMARY KEY,
                automation_id TEXT NOT NULL,
                owner_user_id TEXT,
                status TEXT NOT NULL,
                finished_at INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            INSERT INTO automations (id, name, owner_user_id, created_at, updated_at, deleted_at)
            VALUES
               ('a-1', '旧账号的自动化', 'uid-a', 1, 1, NULL),
               ('a-2', '已删除的自动化', 'uid-a', 1, 1, 100),
               ('a-3', '已是目标账号',    'uid-b', 1, 1, NULL),
               ('a-4', '无归属 legacy',   NULL,    1, 1, NULL);
            INSERT INTO automation_delivery_outbox
               (id, automation_id, owner_user_id, status, finished_at, created_at, updated_at)
            VALUES
               ('o-1', 'a-1', 'uid-a', 'pending', NULL, 1, 1),
               ('o-2', 'a-1', 'uid-a', 'finished', 999, 1, 1);",
        )
        .unwrap();
    }

    #[test]
    fn align_automations_moves_live_rows_to_target() {
        let db = temp_db("align_auto");
        setup(&db);

        let (n, o) = align_automations_owner_in_db(&db, "uid-b", false).unwrap();
        assert_eq!(n, 2, "a-1(owner 不同)/a-4(NULL) 两行；a-2 已软删、a-3 已是目标，均不动");
        assert_eq!(o, 1, "只有未投递完成的 o-1 会被对齐");

        let conn = Connection::open(&db).unwrap();
        let get = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT owner_user_id FROM automations WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .ok()
        };
        assert_eq!(get("a-1").as_deref(), Some("uid-b"));
        assert_eq!(get("a-2").as_deref(), Some("uid-a"), "软删行保留原归属");
        assert_eq!(get("a-3").as_deref(), Some("uid-b"));
        assert_eq!(get("a-4").as_deref(), Some("uid-b"), "legacy 无归属行一并接管");

        let outbox_owner: String = conn
            .query_row(
                "SELECT owner_user_id FROM automation_delivery_outbox WHERE id = 'o-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outbox_owner, "uid-a", "已完成的投递行不动");
    }

    #[test]
    fn align_automations_dry_run_does_not_write() {
        let db = temp_db("align_auto_dry");
        setup(&db);
        let (n, _o) = align_automations_owner_in_db(&db, "uid-b", true).unwrap();
        assert_eq!(n, 2, "dry-run 也要统计计划行数");
        let owner: String = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT owner_user_id FROM automations WHERE id='a-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner, "uid-a", "dry-run 不落盘");
    }

    #[test]
    fn align_missing_db_is_noop() {
        let db = temp_db("missing");
        assert_eq!(
            align_automations_owner_in_db(&db, "uid-b", false).unwrap(),
            (0, 0)
        );
    }
}
