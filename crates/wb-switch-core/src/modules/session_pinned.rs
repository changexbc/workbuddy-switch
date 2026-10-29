//! 会话置顶状态的跨账号同步。
//!
//! WorkBuddy 的置顶状态不在 `workbuddy.db` 的 `sessions` 表里，而在每个账号独立的
//! `<data_root>/storage/user-<uid>-personal/global/conversations.json`：
//!
//! ```json
//! {"pinned": [{"id": "<sessionId>", "groupKey": "<工作区路径>"}]}
//! ```
//!
//! 跨账号复制会生成新的 sessionId（源 ↔ 目标的对应关系记在 `session_links.json` 的
//! 关联组里），目标账号的置顶列表不会自己跟着变，于是复制/同步之后置顶状态就丢了。
//! 本模块按关联映射把置顶条目「单向追加」到目标账号。
//!
//! 语义边界（都是有意的取舍）：
//! - 只改目标文件的 `pinned` 数组；顶层其它键、既有条目的顺序与字段原样保留。
//! - 单向追加，不做镜像删除：目标账号可能自己置顶了别的会话，静默删除等于毁用户数据。
//! - 幂等：映射后的 id 已在目标列表里就跳过；重跑不会写出重复条目。
//! - 任何失败都只体现为 [`PinnedSyncReport::skipped_reason`]，绝不阻断复制/同步主流程。

use crate::modules::config::utc_iso;
use crate::modules::session_backup;
use crate::modules::session_link::{member_variant, LinkGroup, LinkMember, LinkStore, MemberState};
use crate::modules::variant::WbVariant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 置顶状态文件相对数据根的位置：`storage/user-<uid>-personal/global/conversations.json`。
fn conversations_rel_path(uid: &str) -> PathBuf {
    PathBuf::from("storage")
        .join(format!("user-{uid}-personal"))
        .join("global")
        .join("conversations.json")
}

/// 指定账号的置顶状态文件（`conversations.json`）全路径。
pub fn conversations_file(data_root: &Path, uid: &str) -> PathBuf {
    data_root.join(conversations_rel_path(uid))
}

/// `pinned` 数组里的一条置顶记录。
///
/// 顶层实测只有 `id` 与 `groupKey` 两个键，但客户端是同一份 JSON 的写者，
/// 将来加字段不能让这里解析失败——未知字段一律忽略（不设 `deny_unknown_fields`）。
/// 需要「原样带回未知字段」的场合走内部原始 `Value` 路径（见 [`sync_pinned`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedEntry {
    /// 被置顶的会话 id（跨账号复制后是目标账号的新 id）。
    pub id: String,
    /// 置顶时所属工作区（客户端按它分组展示；原样搬运，不做改写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
}

impl PinnedEntry {
    /// 从原始 JSON 条目解析；缺 `id`（或为空）的条目无法映射，按不可用处理。
    fn from_value(value: &Value) -> Option<Self> {
        let entry: Self = serde_json::from_value(value.clone()).ok()?;
        (!entry.id.trim().is_empty()).then_some(entry)
    }
}

/// 源账号置顶状态的读取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinnedSource {
    /// 文件不存在：该账号从未产生过置顶记录。
    Missing,
    /// 文件可读，但没有任何置顶条目。
    NoPinned,
    /// 解析成功的置顶条目，顺序即客户端展示顺序。
    Ready(Vec<PinnedEntry>),
    /// 文件存在但不可用（读失败 / JSON 损坏 / 结构不符），携带原因。
    Corrupt(String),
}

/// 目标账号的置顶同步结果（报告字段，与前端 `SessionPinnedSyncInfo` 同形）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedSyncReport {
    /// 本次追加到目标账号的置顶条目数（已同步过则为 0）。
    pub added: usize,
    /// 未能同步的原因；成功或无事可做时不出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

impl PinnedSyncReport {
    /// 带原因的跳过结果（不写盘、不阻断主流程）。
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            added: 0,
            skipped_reason: Some(reason.into()),
        }
    }

    /// 报告片段（报告里的 `pinned` 字段）。
    ///
    /// 结构全部是 `usize` + `Option<String>`，序列化不可能失败；这里仍不引入 panic
    /// 路径，异常时退化成「零新增 + 原因」，让报告始终可读。
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|error| {
            json!({
                "added": self.added,
                "skippedReason": format!("置顶同步结果无法序列化（{error}）"),
            })
        })
    }
}

/// 读取指定账号的置顶状态（四态，调用方据此决定是否写盘）。
pub fn read_pinned(file: &Path) -> PinnedSource {
    match load_pins(file) {
        LoadedPins::Missing => PinnedSource::Missing,
        LoadedPins::Empty => PinnedSource::NoPinned,
        LoadedPins::Corrupt(reason) => PinnedSource::Corrupt(reason),
        LoadedPins::Ready(entries) => {
            PinnedSource::Ready(entries.iter().filter_map(PinnedEntry::from_value).collect())
        }
    }
}

/// 关联组内 source → target 的 sessionId 映射。
///
/// 遍历全部关联组，取组内「source 身份的 active 成员」与「target 身份的 active 成员」配对。
/// 身份是 (档位, uid) 而不只是 uid：跨档组的成员各自带档位（[`member_variant`]），
/// 同组内理论上可能出现同 uid 的两个档位副本，只按 uid 配对会张冠李戴。
///
/// 同一 source 会话出现在多个组时取第一组（与 [`crate::modules::session_link::find_group_for_identity`]
/// 的「首次命中」口径一致），避免一次同步分别写入两条目标条目。
pub fn link_mapping(
    store: &LinkStore,
    source_variant: WbVariant,
    source_uid: &str,
    target_variant: WbVariant,
    target_uid: &str,
) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for group in &store.groups {
        let Some(source_member) = active_member(group, source_variant, source_uid) else {
            continue;
        };
        let Some(target_member) = active_member(group, target_variant, target_uid) else {
            continue;
        };
        // 同一 id 不需要映射（也避免把源账号的自置顶搬成自己的重复项）。
        if source_member.session_id.trim().is_empty()
            || source_member.session_id == target_member.session_id
        {
            continue;
        }
        if !seen.insert(source_member.session_id.as_str()) {
            continue;
        }
        pairs.push((
            source_member.session_id.clone(),
            target_member.session_id.clone(),
        ));
    }
    pairs
}

/// 一次置顶同步的一侧：档位数据根 + 档位 + 账号 uid。
///
/// 源与目标各自解析自己的数据根，跨档（CN `.workbuddy` / AI `.workbuddy-ai`）不会串。
#[derive(Debug, Clone, Copy)]
pub struct PinnedSyncTarget<'a> {
    pub data_root: &'a Path,
    pub variant: WbVariant,
    pub uid: &'a str,
}

impl PinnedSyncTarget<'_> {
    /// 备份文件名前缀：档位 + uid（同一账号的多次同步可区分）。
    fn backup_label(&self) -> String {
        format!("{}-{}", self.variant.as_str(), self.uid)
    }
}

/// 按源/目标身份直接完成一次置顶同步（[`sync_pinned`] 的路径解析封装）。
pub fn sync_between(
    source: PinnedSyncTarget<'_>,
    target: PinnedSyncTarget<'_>,
    mapping: &[(String, String)],
    backup_dir: &Path,
) -> PinnedSyncReport {
    sync_pinned(
        &conversations_file(source.data_root, source.uid),
        &conversations_file(target.data_root, target.uid),
        mapping,
        backup_dir,
        &target.backup_label(),
    )
}

/// 把源账号的置顶状态按映射单向追加到目标账号。
///
/// - 源 `Missing` / `NoPinned` → 无事可做（`added = 0`，不碰目标文件）；
/// - 源损坏、目标损坏、备份失败、写入失败 → 只填 `skipped_reason`，不写盘、不报错；
/// - 目标条目整体克隆源条目的 JSON（`groupKey` 等字段原样），只替换 `id`；
/// - 目标已有条目一直在前，新条目按源顺序追加到数组尾部；
/// - 无新增（幂等命中）时不写盘，因此重复调用不会改动文件。
pub fn sync_pinned(
    source_file: &Path,
    target_file: &Path,
    mapping: &[(String, String)],
    backup_dir: &Path,
    backup_label: &str,
) -> PinnedSyncReport {
    // 没有映射就没有可搬的置顶：不读目标、不写目标（绝大多数账号从未跨账号复制过）。
    if mapping.is_empty() {
        return PinnedSyncReport::default();
    }
    let source_entries = match load_pins(source_file) {
        LoadedPins::Ready(entries) => entries,
        LoadedPins::Missing | LoadedPins::Empty => return PinnedSyncReport::default(),
        LoadedPins::Corrupt(reason) => {
            return PinnedSyncReport::skipped(format!("源账号置顶记录无法解析（{reason}）"))
        }
    };

    // 目标整体解析为 Value：只改 pinned 数组，其余顶层键原样写回。
    let (mut document, original) = match read_document(target_file) {
        Ok(Some((document, raw))) => (document, Some(raw)),
        // 目标账号还没写过置顶文件：按最小合法结构新建。
        Ok(None) => (json!({ "pinned": [] }), None),
        Err(reason) => {
            return PinnedSyncReport::skipped(format!("目标账号置顶记录不可用（{reason}）"))
        }
    };
    let target_entries = match pinned_array(&document) {
        Ok(entries) => entries,
        Err(reason) => {
            return PinnedSyncReport::skipped(format!("目标账号置顶记录不可用（{reason}）"))
        }
    };
    let mut existing: HashSet<String> = target_entries
        .iter()
        .filter_map(pinned_id)
        .map(str::to_string)
        .collect();

    // 源 id → 目标 id（重复来源取首次出现，与 link_mapping 同口径）。
    let mut by_source: HashMap<&str, &str> = HashMap::new();
    for (source_id, target_id) in mapping {
        if source_id.trim().is_empty() || target_id.trim().is_empty() {
            continue;
        }
        by_source.entry(source_id.as_str()).or_insert(target_id);
    }

    let mut appended: Vec<Value> = Vec::new();
    for entry in &source_entries {
        let Some(source_id) = pinned_id(entry) else {
            continue;
        };
        let Some(target_id) = by_source.get(source_id) else {
            continue;
        };
        // 幂等去重：目标已有该条目（或本次已追加过同一目标 id）就跳过。
        if !existing.insert((*target_id).to_string()) {
            continue;
        }
        let mut cloned = entry.clone();
        match cloned.as_object_mut() {
            Some(object) => {
                object.insert("id".to_string(), Value::String((*target_id).to_string()));
                appended.push(cloned);
            }
            // 非对象条目没有可替换的 id（pinned_id 已过滤，理论上到不了这里）。
            None => continue,
        }
    }
    if appended.is_empty() {
        // 已同步过：不写盘，重复调用完全无副作用。
        return PinnedSyncReport::default();
    }

    let mut array = target_entries;
    array.extend(appended.iter().cloned());
    match document.as_object_mut() {
        Some(object) => {
            object.insert("pinned".to_string(), Value::Array(array));
        }
        None => return PinnedSyncReport::skipped("目标账号置顶记录不可用（顶层不是 JSON 对象）"),
    }

    // 写前备份目标原字节：置顶丢失的代价是用户重排一遍，能回滚就回滚。
    if let Some(raw) = original.as_deref() {
        if let Err(error) = write_backup(backup_dir, backup_label, raw) {
            return PinnedSyncReport::skipped(format!(
                "置顶状态备份失败（{error}），未写入目标账号"
            ));
        }
    }
    let serialized = match serde_json::to_string_pretty(&document) {
        Ok(text) => text,
        Err(error) => return PinnedSyncReport::skipped(format!("置顶状态序列化失败（{error}）")),
    };
    if let Some(parent) = target_file.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return PinnedSyncReport::skipped(format!("置顶状态目录创建失败（{error}）"));
        }
    }
    // 与关联表同一套持久化写（tmp + fsync + rename），不留半写文件。
    if let Err(error) = session_backup::durable_write_str(target_file, &serialized) {
        return PinnedSyncReport::skipped(format!("置顶状态写入失败（{error}）"));
    }
    PinnedSyncReport {
        added: appended.len(),
        skipped_reason: None,
    }
}

// ---------------------------------------------------------------------------
// 内部：读取与备份
// ---------------------------------------------------------------------------

/// 置顶文件的内部读取结果（比 [`PinnedSource`] 多保留原始 JSON 条目，
/// 写回时要连未知字段一起带走）。
enum LoadedPins {
    Missing,
    Empty,
    Ready(Vec<Value>),
    Corrupt(String),
}

fn load_pins(file: &Path) -> LoadedPins {
    match read_document(file) {
        Ok(None) => LoadedPins::Missing,
        Err(reason) => LoadedPins::Corrupt(reason),
        Ok(Some((document, _))) => match pinned_array(&document) {
            Ok(entries) if entries.is_empty() => LoadedPins::Empty,
            Ok(entries) => LoadedPins::Ready(entries),
            Err(reason) => LoadedPins::Corrupt(reason),
        },
    }
}

/// 读取并解析一个置顶文件：不存在返回 `Ok(None)`，不可用返回原因。
fn read_document(file: &Path) -> Result<Option<(Value, Vec<u8>)>, String> {
    let raw = match std::fs::read(file) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取失败：{error}")),
    };
    let value: Value =
        serde_json::from_slice(&raw).map_err(|error| format!("JSON 解析失败：{error}"))?;
    if !value.is_object() {
        return Err("顶层不是 JSON 对象".to_string());
    }
    Ok(Some((value, raw)))
}

/// 取 `pinned` 数组的原始条目；键缺失视为空，非数组视为结构不符（不能覆盖写）。
fn pinned_array(document: &Value) -> Result<Vec<Value>, String> {
    match document.get("pinned") {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err("`pinned` 字段不是数组".to_string()),
    }
}

/// 条目的 `id`（缺失 / 非字符串 / 空串都返回 None）。
fn pinned_id(entry: &Value) -> Option<&str> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// 备份目标原字节；回读核验一致才算备份成功。
fn write_backup(backup_dir: &Path, label: &str, raw: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(backup_dir).map_err(|error| error.to_string())?;
    let path = backup_path(backup_dir, label);
    std::fs::write(&path, raw).map_err(|error| error.to_string())?;
    let readback = std::fs::read(&path).map_err(|error| error.to_string())?;
    if readback != raw {
        return Err("备份回读与原始字节不一致".to_string());
    }
    Ok(path)
}

/// 备份文件名：`<label>-<UTC 时间戳>.json`，同秒重名时追加序号（不覆盖旧备份）。
fn backup_path(backup_dir: &Path, label: &str) -> PathBuf {
    let stem = format!("{}-{}", safe_label(label), utc_iso());
    let mut candidate = backup_dir.join(format!("{stem}.json"));
    let mut index = 1;
    while candidate.exists() && index < 100 {
        candidate = backup_dir.join(format!("{stem}-{index}.json"));
        index += 1;
    }
    candidate
}

/// 文件名安全的标签：只保留字母数字与 `-`/`_`，其余替换为 `-`。
fn safe_label(raw: &str) -> String {
    let label: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = label.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "pinned".to_string()
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------------
// 内部：关联组成员定位
// ---------------------------------------------------------------------------

/// 组内某身份（档位 + uid）的 active 成员。
fn active_member<'a>(
    group: &'a LinkGroup,
    variant: WbVariant,
    uid: &str,
) -> Option<&'a LinkMember> {
    let uid = uid.trim();
    group.members.iter().find(|member| {
        member.state == MemberState::Active
            && member.uid.trim() == uid
            && member_variant(group, member) == variant
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wb_switch_pinned_test_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const UID_A: &str = "uid-a";
    const UID_B: &str = "uid-b";

    /// 造一份真实形态的置顶文件（目录层级与客户端一致）。
    fn write_conversations(data_root: &Path, uid: &str, document: Value) {
        let file = conversations_file(data_root, uid);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    }

    fn read_json(file: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
    }

    fn pair(source: &str, target: &str) -> Vec<(String, String)> {
        vec![(source.to_string(), target.to_string())]
    }

    fn member(uid: &str, session_id: &str, variant: WbVariant, state: MemberState) -> LinkMember {
        LinkMember {
            member_id: format!("m-{uid}-{session_id}"),
            account_id: None,
            uid: uid.to_string(),
            session_id: session_id.to_string(),
            variant: Some(variant),
            state,
            linked_at: 1,
            last_synced_at: None,
        }
    }

    fn group(variant: WbVariant, members: Vec<LinkMember>) -> LinkGroup {
        LinkGroup {
            id: format!("g-{}", members.len()),
            variant,
            created_at: 1,
            members,
            pair_bases: Vec::new(),
        }
    }

    #[test]
    fn conversations_file_lives_in_account_scoped_storage() {
        let root = Path::new("/data-root");
        let file = conversations_file(root, UID_A);
        assert_eq!(
            file,
            root.join("storage")
                .join("user-uid-a-personal")
                .join("global")
                .join("conversations.json")
        );
        // 同档不同账号、同账号不同档位都必须是不同文件。
        assert_ne!(
            conversations_file(root, UID_A),
            conversations_file(root, UID_B)
        );
        assert_ne!(
            conversations_file(Path::new("/cn"), UID_A),
            conversations_file(Path::new("/ai"), UID_A)
        );
    }

    #[test]
    fn pinned_entry_uses_camel_case_and_ignores_unknown_fields() {
        let entry: PinnedEntry = serde_json::from_value(json!({
            "id": "s-1",
            "groupKey": "/ws/a",
            "pinnedAt": 123,
        }))
        .unwrap();
        assert_eq!(entry.id, "s-1");
        assert_eq!(entry.group_key.as_deref(), Some("/ws/a"));
        // 只带 id 也能解析（groupKey 缺失）。
        let bare: PinnedEntry = serde_json::from_value(json!({"id": "s-2"})).unwrap();
        assert_eq!(bare.group_key, None);
        // 序列化键名就是客户端认的 camelCase。
        let text = serde_json::to_string(&entry).unwrap();
        assert!(text.contains("\"groupKey\""), "{text}");
        assert!(!text.contains("group_key"), "{text}");
    }

    #[test]
    fn read_pinned_covers_missing_unpinned_ready_and_corrupt() {
        let dir = TempDir::new("read-states");
        let file = conversations_file(dir.path(), UID_A);

        assert_eq!(read_pinned(&file), PinnedSource::Missing);

        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // 空对象 / 空数组 / 顶层没有 pinned 都算「没有置顶」。
        for document in [json!({}), json!({"pinned": []}), json!({"other": 1})] {
            std::fs::write(&file, document.to_string()).unwrap();
            assert_eq!(read_pinned(&file), PinnedSource::NoPinned, "{document}");
        }

        std::fs::write(
            &file,
            json!({"pinned": [{"id": "s-1", "groupKey": "/ws/a"}, {"id": "s-2"}]}).to_string(),
        )
        .unwrap();
        assert_eq!(
            read_pinned(&file),
            PinnedSource::Ready(vec![
                PinnedEntry {
                    id: "s-1".to_string(),
                    group_key: Some("/ws/a".to_string()),
                },
                PinnedEntry {
                    id: "s-2".to_string(),
                    group_key: None,
                },
            ])
        );

        // 损坏的三种形态都必须被识别为不可用（而不是当成空列表照写）。
        for (raw, expect) in [
            ("{", "JSON 解析失败"),
            ("[]", "顶层不是 JSON 对象"),
            (r#"{"pinned": "oops"}"#, "不是数组"),
        ] {
            std::fs::write(&file, raw).unwrap();
            match read_pinned(&file) {
                PinnedSource::Corrupt(reason) => assert!(
                    reason.contains(expect),
                    "原始内容 {raw} 的原因应含 {expect}，实际 {reason}"
                ),
                other => panic!("原始内容 {raw} 应为 Corrupt，实际 {other:?}"),
            }
        }
    }

    #[test]
    fn link_mapping_pairs_active_members_of_both_identities() {
        let store = LinkStore {
            version: crate::modules::session_link::LINK_STORE_VERSION,
            revision: 1,
            groups: vec![
                // 正常组：双方 active。
                group(
                    WbVariant::Cn,
                    vec![
                        member(UID_A, "s-1", WbVariant::Cn, MemberState::Active),
                        member(UID_B, "t-1", WbVariant::Cn, MemberState::Active),
                    ],
                ),
                // 目标成员已失效：不配对。
                group(
                    WbVariant::Cn,
                    vec![
                        member(UID_A, "s-2", WbVariant::Cn, MemberState::Active),
                        member(UID_B, "t-2", WbVariant::Cn, MemberState::Superseded),
                    ],
                ),
                // 跨档组：源 CN、目标 AI，各自档位必须分别命中。
                group(
                    WbVariant::Cn,
                    vec![
                        member(UID_A, "s-3", WbVariant::Cn, MemberState::Active),
                        member(UID_B, "t-3", WbVariant::Ai, MemberState::Active),
                    ],
                ),
                // 只有一个账号的组：不配对。
                group(
                    WbVariant::Cn,
                    vec![member(UID_A, "s-4", WbVariant::Cn, MemberState::Active)],
                ),
            ],
        };

        // 同档（CN → CN）：只取同档 member 对；跨档组里 CN 源与 AI 目标不匹配。
        assert_eq!(
            link_mapping(&store, WbVariant::Cn, UID_A, WbVariant::Cn, UID_B),
            vec![("s-1".to_string(), "t-1".to_string())]
        );
        // 跨档（CN → AI）：只认成员档位各自相符的那一组。
        assert_eq!(
            link_mapping(&store, WbVariant::Cn, UID_A, WbVariant::Ai, UID_B),
            vec![("s-3".to_string(), "t-3".to_string())]
        );
        // 方向反了（B → A）时映射方向跟着反，不会误用 source 方向的 id。
        assert_eq!(
            link_mapping(&store, WbVariant::Cn, UID_B, WbVariant::Cn, UID_A),
            vec![("t-1".to_string(), "s-1".to_string())]
        );
        assert!(link_mapping(&store, WbVariant::Cn, "uid-x", WbVariant::Cn, UID_B).is_empty());
    }

    #[test]
    fn sync_pinned_appends_mapped_entries_and_keeps_unknown_keys() {
        let dir = TempDir::new("sync-append");
        let root = dir.path().join("data");
        write_conversations(
            &root,
            UID_A,
            json!({"pinned": [
                {"id": "s-1", "groupKey": "/ws/a", "pinnedAt": 11},
                {"id": "s-unlinked", "groupKey": "/ws/a"},
                {"id": "s-2", "groupKey": "/ws/b"},
            ]}),
        );
        // 目标已有自己的置顶（顺序必须保持在前）。
        write_conversations(
            &root,
            UID_B,
            json!({"pinned": [{"id": "own", "groupKey": "/ws/z"}], "pinnedGroups": ["x"]}),
        );

        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &conversations_file(&root, UID_B),
            &[
                ("s-1".to_string(), "t-1".to_string()),
                ("s-2".to_string(), "t-2".to_string()),
            ],
            &root.join("backups"),
            "cn-uid-b",
        );
        assert_eq!(report.added, 2);
        assert_eq!(report.skipped_reason, None);

        let document = read_json(&conversations_file(&root, UID_B));
        // 顶层未知键原样保留。
        assert_eq!(document["pinnedGroups"], json!(["x"]));
        let pinned = document["pinned"].as_array().unwrap();
        assert_eq!(pinned.len(), 3);
        // 目标已有条目在前，新条目按源顺序追加。
        assert_eq!(pinned[0]["id"], "own");
        assert_eq!(pinned[1]["id"], "t-1");
        assert_eq!(pinned[2]["id"], "t-2");
        // groupKey 与其它未知字段整体搬过来（只换 id）。
        assert_eq!(pinned[1]["groupKey"], "/ws/a");
        assert_eq!(pinned[1]["pinnedAt"], 11);
        assert_eq!(pinned[2]["groupKey"], "/ws/b");
        // 源文件不动。
        assert_eq!(
            read_json(&conversations_file(&root, UID_A))["pinned"][0]["id"],
            "s-1"
        );
    }

    #[test]
    fn sync_pinned_is_idempotent_and_backs_up_only_when_writing() {
        let dir = TempDir::new("sync-idempotent");
        let root = dir.path().join("data");
        write_conversations(
            &root,
            UID_A,
            json!({"pinned": [{"id": "s-1", "groupKey": "/ws/a"}]}),
        );
        write_conversations(&root, UID_B, json!({"pinned": []}));
        let backup_dir = root.join("backups").join("pinned");
        let mapping = pair("s-1", "t-1");

        let first = sync_pinned(
            &conversations_file(&root, UID_A),
            &conversations_file(&root, UID_B),
            &mapping,
            &backup_dir,
            "cn-uid-b",
        );
        assert_eq!(first.added, 1);
        let after_first = std::fs::read(conversations_file(&root, UID_B)).unwrap();
        let backups: Vec<_> = std::fs::read_dir(&backup_dir).unwrap().flatten().collect();
        assert_eq!(backups.len(), 1, "首次写入必须留一份原字节备份");

        let second = sync_pinned(
            &conversations_file(&root, UID_A),
            &conversations_file(&root, UID_B),
            &mapping,
            &backup_dir,
            "cn-uid-b",
        );
        assert_eq!(second.added, 0);
        assert_eq!(second.skipped_reason, None);
        // 幂等：文件字节不变，也不再多出备份（没有写就没有新备份）。
        assert_eq!(
            std::fs::read(conversations_file(&root, UID_B)).unwrap(),
            after_first
        );
        assert_eq!(std::fs::read_dir(&backup_dir).unwrap().count(), 1);
        // 目标里只有一条 t-1（不会因为重复同步涨成两条）。
        let document = read_json(&conversations_file(&root, UID_B));
        assert_eq!(document["pinned"].as_array().unwrap().len(), 1);

        // 备份内容就是覆盖前的原字节。
        let backup_file = backups[0].path();
        assert_eq!(
            read_json(&backup_file),
            json!({"pinned": []}),
            "备份必须是逐字节的原文件内容"
        );
        assert!(backup_file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cn-uid-b-"));
    }

    #[test]
    fn sync_pinned_is_noop_when_source_has_nothing_to_carry() {
        let dir = TempDir::new("sync-noop");
        let root = dir.path().join("data");
        let mapping = pair("s-1", "t-1");
        let backup_dir = root.join("backups");

        // 源文件缺失：不读目标、不写目标。
        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &conversations_file(&root, UID_B),
            &mapping,
            &backup_dir,
            "cn-uid-b",
        );
        assert_eq!(report, PinnedSyncReport::default());
        assert!(!conversations_file(&root, UID_B).exists());

        // 源没有置顶条目：同样不写目标。
        write_conversations(&root, UID_A, json!({"pinned": []}));
        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &conversations_file(&root, UID_B),
            &mapping,
            &backup_dir,
            "cn-uid-b",
        );
        assert_eq!(report, PinnedSyncReport::default());
        assert!(!conversations_file(&root, UID_B).exists());

        // 映射为空：即使源有置顶也不碰目标。
        write_conversations(
            &root,
            UID_A,
            json!({"pinned": [{"id": "s-1", "groupKey": "/ws/a"}]}),
        );
        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &conversations_file(&root, UID_B),
            &[],
            &backup_dir,
            "cn-uid-b",
        );
        assert_eq!(report, PinnedSyncReport::default());
        assert!(!conversations_file(&root, UID_B).exists());
    }

    #[test]
    fn sync_pinned_reports_reason_and_never_touches_corrupt_target() {
        let dir = TempDir::new("sync-corrupt");
        let root = dir.path().join("data");
        write_conversations(
            &root,
            UID_A,
            json!({"pinned": [{"id": "s-1", "groupKey": "/ws/a"}]}),
        );
        let target_file = conversations_file(&root, UID_B);
        std::fs::create_dir_all(target_file.parent().unwrap()).unwrap();
        std::fs::write(&target_file, "{ 坏掉的 JSON").unwrap();

        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &target_file,
            &pair("s-1", "t-1"),
            &root.join("backups"),
            "cn-uid-b",
        );
        assert_eq!(report.added, 0);
        let reason = report.skipped_reason.as_deref().expect("损坏必须给出原因");
        assert!(reason.contains("目标账号置顶记录不可用"), "{reason}");
        // 目标原字节一字不改。
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "{ 坏掉的 JSON"
        );
        // 报告片段可读：`added` 恒在，原因只在该出现的时候出现。
        let value = report.to_json();
        assert_eq!(value["added"], 0);
        assert!(value["skippedReason"].as_str().is_some(), "{value}");
        assert!(PinnedSyncReport::default()
            .to_json()
            .get("skippedReason")
            .is_none());

        // 源损坏同样只报原因、不写盘。
        std::fs::write(&target_file, json!({"pinned": []}).to_string()).unwrap();
        std::fs::write(conversations_file(&root, UID_A), "not json").unwrap();
        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &target_file,
            &pair("s-1", "t-1"),
            &root.join("backups"),
            "cn-uid-b",
        );
        assert_eq!(report.added, 0);
        assert!(report
            .skipped_reason
            .unwrap()
            .contains("源账号置顶记录无法解析"));
        assert_eq!(
            read_json(&target_file),
            json!({"pinned": []}),
            "源损坏时目标不得被改写"
        );
    }

    #[test]
    fn sync_pinned_creates_target_file_when_missing() {
        let dir = TempDir::new("sync-create");
        let root = dir.path().join("data");
        write_conversations(
            &root,
            UID_A,
            json!({"pinned": [{"id": "s-1", "groupKey": "/ws/a"}]}),
        );
        let target_file = conversations_file(&root, UID_B);
        assert!(!target_file.exists());

        let report = sync_pinned(
            &conversations_file(&root, UID_A),
            &target_file,
            &pair("s-1", "t-1"),
            &root.join("backups"),
            "cn-uid-b",
        );
        assert_eq!(report.added, 1);
        assert_eq!(
            read_json(&target_file),
            json!({"pinned": [{"id": "t-1", "groupKey": "/ws/a"}]})
        );
        // 本来没有文件就没有可备份的原字节：不造空备份。
        assert!(!root.join("backups").exists());
    }

    #[test]
    fn sync_between_keeps_variant_data_roots_apart() {
        let dir = TempDir::new("sync-between");
        let cn_root = dir.path().join("cn");
        let ai_root = dir.path().join("ai");
        write_conversations(
            &cn_root,
            UID_A,
            json!({"pinned": [{"id": "s-1", "groupKey": "/ws/a"}]}),
        );
        // 目标档已有自己的空置顶文件：用于核对「备份写在目标档标签下」。
        write_conversations(&ai_root, UID_B, json!({"pinned": []}));

        let report = sync_between(
            PinnedSyncTarget {
                data_root: &cn_root,
                variant: WbVariant::Cn,
                uid: UID_A,
            },
            PinnedSyncTarget {
                data_root: &ai_root,
                variant: WbVariant::Ai,
                uid: UID_B,
            },
            &pair("s-1", "t-1"),
            &dir.path().join("backups/pinned"),
        );
        assert_eq!(report.added, 1);
        // 只写到目标档的数据根；源档与「目标账号在源档的文件」都不存在。
        assert_eq!(
            read_json(&conversations_file(&ai_root, UID_B))["pinned"][0]["id"],
            "t-1"
        );
        assert!(!conversations_file(&ai_root, UID_A).exists());
        assert!(!conversations_file(&cn_root, UID_B).exists());
        // 备份标签带目标档位。
        let backup = std::fs::read_dir(dir.path().join("backups/pinned"))
            .unwrap()
            .flatten()
            .next()
            .expect("必须有备份");
        assert!(backup
            .file_name()
            .to_string_lossy()
            .starts_with("ai-uid-b-"));
    }

    #[test]
    fn safe_label_keeps_names_path_free() {
        assert_eq!(safe_label("cn-uid-b"), "cn-uid-b");
        let escaped = safe_label("ai/../../etc");
        assert!(
            !escaped.contains('/') && !escaped.contains('.') && !escaped.contains('\\'),
            "{escaped}"
        );
        assert!(
            escaped.starts_with("ai") && escaped.ends_with("etc"),
            "{escaped}"
        );
        assert_eq!(safe_label("   "), "pinned");
    }
}
