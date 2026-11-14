//! 应用内通知（toast）存档：最近 [`NOTIFICATION_LIMIT`] 条，落盘在工具存储根。
//!
//! 用途：提示是一次性的（sonner toast 存活几秒），事后无法回看。存档让用户与排障者
//! 能核对「应用当时到底提示了什么」——例如切号成功后是否出现过可还原路径、清理失败
//! 的待清理提示等。存档是尽力而为：写入失败不影响提示本身，读取失败不阻塞界面。
//!
//! 边界：只存本机（`store_dir()/notifications.json`），不做云同步；内容可能包含账号
//! 昵称与本地路径，属本机明文数据，UI 里如实说明。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::i18n;

/// 保留的最近通知条数。
pub const NOTIFICATION_LIMIT: usize = 100;
/// 存档格式版本；读到其它版本视为不可用（保留原文件，不覆盖）。
pub const NOTIFICATION_VERSION: u32 = 1;
/// 标题/描述的长度上限（按字符截断，避免一条异常提示把存档撑爆）。
const TITLE_LIMIT: usize = 200;
const DESCRIPTION_LIMIT: usize = 2000;
/// 同一提示在该窗口内重复出现时只记一条（去抖；不改变 toast 本身的显示）。
const DEDUPE_WINDOW_MS: i64 = 2_000;
const STORE_FILE_NAME: &str = "notifications.json";

/// 一条通知：级别、标题、可选描述与发生时间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEntry {
    /// `success` | `error` | `warning` | `info`。
    pub level: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationStore {
    version: u32,
    #[serde(default)]
    items: Vec<NotificationEntry>,
}

impl Default for NotificationStore {
    fn default() -> Self {
        Self {
            version: NOTIFICATION_VERSION,
            items: Vec::new(),
        }
    }
}

fn store_file_at(root: &Path) -> PathBuf {
    root.join(STORE_FILE_NAME)
}

/// 读取存档；缺失返回空，损坏/版本不符返回 Err（不覆盖原文件）。
fn load_at(root: &Path) -> Result<NotificationStore, String> {
    let file = store_file_at(root);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NotificationStore::default())
        }
        Err(error) => {
            return Err(i18n::pick(
                format!("Could not read the notification archive: {error}"),
                format!("알림 저장소를 읽을 수 없습니다: {error}"),
            ))
        }
    };
    let store: NotificationStore = serde_json::from_str(&text).map_err(|error| {
        i18n::pick(
            format!("The notification archive is damaged: {error}"),
            format!("알림 저장소 내용이 손상되었습니다: {error}"),
        )
    })?;
    if store.version != NOTIFICATION_VERSION {
        return Err(i18n::pick(
            format!(
                "Notification archive version {} is not supported (supported: {}); the original file was kept",
                store.version, NOTIFICATION_VERSION
            ),
            format!(
                "지원하지 않는 알림 저장소 버전입니다: {} (지원 버전: {}). 원본 파일은 보관했습니다",
                store.version, NOTIFICATION_VERSION
            ),
        ));
    }
    Ok(store)
}

/// 按字符截断（超长时补省略号），避免单条提示过大。
fn trim_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut trimmed: String = text.chars().take(limit).collect();
    trimmed.push('…');
    trimmed
}

/// 记录一条通知（可注入存储根，供单测使用）。
///
/// 同一条提示在 [`DEDUPE_WINDOW_MS`] 内重复出现时只保留一条。返回 Err 表示没写进去
/// （存档损坏、权限失败等）——调用方应忽略并保证提示照常显示。
pub fn record_at(
    root: &Path,
    level: &str,
    title: &str,
    description: Option<&str>,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(());
    }
    let mut store = load_at(root)?;
    let entry = NotificationEntry {
        level: level.to_string(),
        title: trim_text(title, TITLE_LIMIT),
        description: description
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| trim_text(text, DESCRIPTION_LIMIT)),
        at: now_ms(),
    };
    let duplicate = store.items.last().is_some_and(|last| {
        last.level == entry.level
            && last.title == entry.title
            && last.description == entry.description
            && entry.at.saturating_sub(last.at) < DEDUPE_WINDOW_MS
    });
    if duplicate {
        return Ok(());
    }
    store.items.push(entry);
    if store.items.len() > NOTIFICATION_LIMIT {
        let drop_count = store.items.len() - NOTIFICATION_LIMIT;
        store.items.drain(0..drop_count);
    }
    let content = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())?;
    atomic_write(&store_file_at(root), &content).map_err(|error| {
        i18n::pick(
            format!("Could not write the notification archive: {error}"),
            format!("알림 저장소를 쓰지 못했습니다: {error}"),
        )
    })
}

/// 读取最近的通知（新的在前）。
pub fn list_at(root: &Path) -> Result<Vec<NotificationEntry>, String> {
    let mut items = load_at(root)?.items;
    items.reverse();
    Ok(items)
}

/// 清空存档（只保留版本字段）。
pub fn clear_at(root: &Path) -> Result<(), String> {
    let content =
        serde_json::to_string_pretty(&NotificationStore::default()).map_err(|e| e.to_string())?;
    atomic_write(&store_file_at(root), &content).map_err(|error| {
        i18n::pick(
            format!("Could not write the notification archive: {error}"),
            format!("알림 저장소를 쓰지 못했습니다: {error}"),
        )
    })
}

/// 生产入口：记录到工具存储根。
pub fn record(level: &str, title: &str, description: Option<&str>) -> Result<(), String> {
    record_at(&store_dir(), level, title, description)
}

/// 生产入口：读取通知存档（新的在前）。
pub fn list() -> Result<Vec<NotificationEntry>, String> {
    list_at(&store_dir())
}

/// 生产入口：清空通知存档。
pub fn clear() -> Result<(), String> {
    clear_at(&store_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wb_switch_notify_test_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn trim_text_truncates_on_char_boundary() {
        assert_eq!(trim_text("短文本", 10), "短文本");
        let trimmed = trim_text(&"汉".repeat(20), 5);
        assert_eq!(trimmed.chars().count(), 6, "5 个字符 + 省略号");
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn record_appends_newest_first_and_ignores_empty_titles() {
        let dir = TempDir::new("append");
        record_at(&dir.0, "success", "第一条", None).unwrap();
        record_at(&dir.0, "error", "第二条", Some("原因")).unwrap();
        record_at(&dir.0, "info", "   ", None).unwrap();

        let items = list_at(&dir.0).unwrap();
        assert_eq!(items.len(), 2, "空标题不记录");
        assert_eq!(items[0].title, "第二条");
        assert_eq!(items[0].description.as_deref(), Some("原因"));
        assert_eq!(items[1].title, "第一条");
        assert!(items[0].at >= items[1].at);
    }

    #[test]
    fn identical_notice_within_window_is_deduped() {
        let dir = TempDir::new("dedupe");
        record_at(&dir.0, "warning", "轮换推迟", Some("渠道限流")).unwrap();
        record_at(&dir.0, "warning", "轮换推迟", Some("渠道限流")).unwrap();
        assert_eq!(list_at(&dir.0).unwrap().len(), 1);

        // 内容不同或窗口外照常记录。
        record_at(&dir.0, "warning", "轮换推迟", Some("另一种原因")).unwrap();
        assert_eq!(list_at(&dir.0).unwrap().len(), 2);
    }

    #[test]
    fn ring_keeps_only_the_latest_entries() {
        let dir = TempDir::new("ring");
        for index in 0..NOTIFICATION_LIMIT + 5 {
            record_at(&dir.0, "info", &format!("提示 {index}"), None).unwrap();
        }
        let items = list_at(&dir.0).unwrap();
        assert_eq!(items.len(), NOTIFICATION_LIMIT);
        assert_eq!(items[0].title, format!("提示 {}", NOTIFICATION_LIMIT + 4));
        assert_eq!(items.last().unwrap().title, "提示 5");
    }

    #[test]
    fn long_text_is_trimmed() {
        let dir = TempDir::new("trim");
        let long = "汉".repeat(TITLE_LIMIT + 50);
        record_at(
            &dir.0,
            "info",
            &long,
            Some(&"描".repeat(DESCRIPTION_LIMIT + 10)),
        )
        .unwrap();
        let items = list_at(&dir.0).unwrap();
        assert_eq!(items[0].title.chars().count(), TITLE_LIMIT + 1);
        assert_eq!(
            items[0].description.as_ref().unwrap().chars().count(),
            DESCRIPTION_LIMIT + 1
        );
    }

    #[test]
    fn damaged_store_is_reported_and_preserved() {
        let dir = TempDir::new("damaged");
        let file = store_file_at(&dir.0);
        std::fs::write(&file, b"{not json").unwrap();
        let error = record_at(&dir.0, "info", "提示", None).unwrap_err();
        assert!(error.contains("damaged"), "{error}");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"{not json",
            "不得覆盖原文件"
        );
        assert!(list_at(&dir.0).is_err());
    }

    #[test]
    fn clear_empties_the_archive() {
        let dir = TempDir::new("clear");
        record_at(&dir.0, "success", "提示", None).unwrap();
        clear_at(&dir.0).unwrap();
        assert!(list_at(&dir.0).unwrap().is_empty());
    }
}
