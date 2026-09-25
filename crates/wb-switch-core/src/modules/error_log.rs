//! 错误日志落盘（`~/.wb-switch/error.log`，JSON Lines + 环形保留）。
//!
//! 前端崩溃 / 未捕获错误与后端错误共用这一份日志：每行一个 JSON 对象
//! （`ts` / `kind` / `message` / `detail` / `appVersion` / `os`），超过
//! [`ERROR_LOG_MAX_LINES`] 行时原子重写只留最后 200 行。
//!
//! 目录与签到日志同处 `~/.wb-switch`（[`crate::modules::config::store_dir`]）。
//! 写入失败一律静默降级：记日志本身绝不影响主流程。

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::modules::config::{atomic_write, now_ms, store_dir};

/// 环形保留上限：写入后超过该行数即重写，只保留最后 200 行。
pub const ERROR_LOG_MAX_LINES: usize = 200;
/// 单条 `detail` 的字节上限（4KB）：完整堆栈仍然可观，但不让日志被单条记录撑爆。
pub const ERROR_LOG_DETAIL_MAX_BYTES: usize = 4096;

/// 追加是「读已有行 → 追加 → 必要时重写」的读改写，必须串行，否则并发上报会互相覆盖。
static ERROR_LOG_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 错误日志文件路径（与签到日志同目录）。
pub fn error_log_path() -> PathBuf {
    store_dir().join("error.log")
}

/// `kind` 白名单归一化：只认三种来源，其它值按 `backend` 记（不落任意字符串）。
fn normalize_kind(kind: &str) -> &'static str {
    match kind {
        "frontend_crash" => "frontend_crash",
        "frontend_unhandled" => "frontend_unhandled",
        _ => "backend",
    }
}

/// 按字符边界截断 `detail`，避免把多字节字符切成乱码。
fn truncate_detail(detail: &str) -> &str {
    if detail.len() <= ERROR_LOG_DETAIL_MAX_BYTES {
        return detail;
    }
    let mut end = ERROR_LOG_DETAIL_MAX_BYTES;
    while end > 0 && !detail.is_char_boundary(end) {
        end -= 1;
    }
    &detail[..end]
}

/// 组装一条日志记录（字段固定为 6 个）。
fn build_entry(kind: &str, message: &str, detail: &str, at_ms: i64) -> Value {
    json!({
        "ts": at_ms,
        "kind": normalize_kind(kind),
        "message": message,
        "detail": truncate_detail(detail),
        "appVersion": crate::modules::update::APP_VERSION,
        "os": std::env::consts::OS,
    })
}

/// 记录一条错误日志；写入失败静默忽略（调用方不感知、不阻塞）。
pub fn record(kind: &str, message: &str, detail: &str) {
    let _ = record_at(&error_log_path(), kind, message, detail, now_ms());
}

/// 读取已有行；文件缺失 / 读不出（非 UTF-8 等）按空处理——日志可以丢，不能因此报错。
fn read_lines(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// 追加一行；超出上限时原子重写保留最后 [`ERROR_LOG_MAX_LINES`] 行。调用方需持写锁。
fn append_line_locked(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lines = read_lines(path);
    lines.push(line.to_string());
    if lines.len() > ERROR_LOG_MAX_LINES {
        // 环形保留：临时文件 + rename 原子替换，避免重写中途留下半截文件。
        lines.drain(..lines.len() - ERROR_LOG_MAX_LINES);
        return atomic_write(path, &format!("{}\n", lines.join("\n")));
    }
    // 常规路径只追加一行。追加句柄不调用 sync_all / set_modified：Windows 上这两者
    // 需要写权限之外的一整套语义，日志不值得为此引入平台分叉（见跨平台契约 spec）。
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

/// 写入指定路径（单测注入用）：返回 IO 结果，由 [`record`] 决定是否忽略。
fn record_at(
    path: &Path,
    kind: &str,
    message: &str,
    detail: &str,
    at_ms: i64,
) -> std::io::Result<()> {
    let entry = build_entry(kind, message, detail, at_ms);
    let line = serde_json::to_string(&entry)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    // 锁中毒不应让「记日志」变成 panic 源：取回内部守卫继续写。
    let _guard = ERROR_LOG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    append_line_locked(path, &line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log_path(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "wb-switch-error-log-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("error.log"))
    }

    fn read_entries(path: &Path) -> Vec<Value> {
        let text = std::fs::read_to_string(path).unwrap();
        text.lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("每行都必须是合法 JSON"))
            .collect()
    }

    /// 环形截断：写 250 条只留最后 200 条，且保留的是「最新」的那批。
    #[test]
    fn error_log_ring_keeps_only_the_last_200_lines() {
        let (dir, path) = temp_log_path("ring");
        for index in 0..250 {
            record_at(
                &path,
                "frontend_unhandled",
                &format!("错误 {index}"),
                "",
                index as i64,
            )
            .unwrap();
        }

        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), ERROR_LOG_MAX_LINES);

        let entries = read_entries(&path);
        assert_eq!(entries.len(), ERROR_LOG_MAX_LINES);
        assert_eq!(entries[0]["message"], json!("错误 50"));
        assert_eq!(entries[0]["ts"], json!(50));
        assert_eq!(entries[199]["message"], json!("错误 249"));
        assert_eq!(entries[199]["ts"], json!(249));

        // 重写后继续追加仍在同一套环形语义下（不会因为一次重写而丢掉上限）。
        record_at(&path, "backend", "错误 250", "", 250).unwrap();
        let entries = read_entries(&path);
        assert_eq!(entries.len(), ERROR_LOG_MAX_LINES);
        assert_eq!(entries[0]["message"], json!("错误 51"));
        assert_eq!(entries[199]["message"], json!("错误 250"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 字段完整性 + kind 白名单 + detail 截断（含多字节字符边界）。
    #[test]
    fn error_log_entry_has_fixed_fields_and_truncates_detail() {
        let (dir, path) = temp_log_path("fields");

        record_at(
            &path,
            "frontend_crash",
            "渲染崩溃",
            "堆栈信息",
            1_700_000_000_000,
        )
        .unwrap();
        let entries = read_entries(&path);
        assert_eq!(entries.len(), 1);
        let entry = entries[0].as_object().unwrap();
        assert_eq!(
            entry.len(),
            6,
            "字段集固定为 ts/kind/message/detail/appVersion/os"
        );
        assert_eq!(entry["ts"], json!(1_700_000_000_000i64));
        assert_eq!(entry["kind"], json!("frontend_crash"));
        assert_eq!(entry["message"], json!("渲染崩溃"));
        assert_eq!(entry["detail"], json!("堆栈信息"));
        assert_eq!(
            entry["appVersion"],
            json!(crate::modules::update::APP_VERSION)
        );
        assert_eq!(entry["os"], json!(std::env::consts::OS));

        // 未知 kind 归一为 backend，不落任意字符串。
        record_at(&path, "something-else", "未知来源", "", 2).unwrap();
        let entries = read_entries(&path);
        assert_eq!(entries[1]["kind"], json!("backend"));

        // ASCII detail 截断到正好 4KB。
        let ascii = "x".repeat(ERROR_LOG_DETAIL_MAX_BYTES + 100);
        record_at(&path, "backend", "超长 detail", &ascii, 3).unwrap();
        let entries = read_entries(&path);
        assert_eq!(
            entries[2]["detail"].as_str().unwrap().len(),
            ERROR_LOG_DETAIL_MAX_BYTES
        );

        // 多字节 detail 截断按字符边界回退，不产生半个汉字。
        let wide = "错".repeat(3000); // 9000 字节
        record_at(&path, "backend", "多字节 detail", &wide, 4).unwrap();
        let entries = read_entries(&path);
        let detail = entries[3]["detail"].as_str().unwrap();
        assert_eq!(detail.len(), ERROR_LOG_DETAIL_MAX_BYTES - 1);
        assert!(detail.chars().all(|c| c == '错'));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 三平台通用的失败形态（父路径是普通文件）：返回 Err 而不是 panic。
    ///
    /// 不用「只读目录」构造这一条：Windows 上目录的只读属性不拦写文件，
    /// 只有 unix 能复现（另有 `#[cfg(unix)]` 用例单独覆盖）。
    #[test]
    fn error_log_write_failure_is_returned_not_panicked() {
        let (dir, _) = temp_log_path("blocked");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();

        let result = record_at(&blocker.join("error.log"), "backend", "父路径是文件", "", 1);
        assert!(result.is_err(), "父路径不可用时必须返回 Err");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 只读目录 / 只读文件：写入静默降级（返回 Err，公开入口 `record` 只做 `let _ =`，
    /// 不 panic、不中断主流程）。
    #[cfg(unix)]
    #[test]
    fn error_log_silently_degrades_when_the_target_is_read_only() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, path) = temp_log_path("readonly");

        // 只读目录（日志还没生成）：连文件都创建不出来。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(
            record_at(&path, "backend", "只读目录", "", 1).is_err(),
            "只读目录下必须失败而不是 panic"
        );
        assert!(!path.exists(), "失败不得留下半截文件");

        // 只读文件（目录可写）：追加被拒，已有内容原样保留。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        record_at(&path, "backend", "先写一条", "", 2).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(
            record_at(&path, "backend", "只读文件", "", 3).is_err(),
            "只读文件下必须失败而不是 panic"
        );
        let entries = read_entries(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["message"], json!("先写一条"));
        // 公开入口 [`record`] 对同一失败只做 `let _ =`：调用方不感知、主流程不中断。
        // （不在单测里直接调 `record`：它写真实 `~/.wb-switch`，单测不得触碰用户目录。）

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
