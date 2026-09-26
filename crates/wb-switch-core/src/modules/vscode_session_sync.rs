//! VS Code CodeBuddy 插件「关联会话」：复制后登记、切换预览与增量同步（design §5 / §6）。
//!
//! 与 WorkBuddy 侧**分层同构但不共用编排**：内核（关联表 / 配对基线 / `decide_sync` /
//! 预览凭据 / 存储锁）复用 [`crate::modules::session_link`]，存储按
//! [`SessionPaths::for_vscode_ext`] 的命名空间与 WorkBuddy 完全隔离；内容身份与写入形态
//! 则是插件专属的（一条消息一个文件、按记录顺序摘要、整体重建）。
//!
//! 触发时机（design §7）：只在切换 wrapper 内、编辑器**已关闭**之后执行；顺序固定为
//! 「复制 → 登记关联 → 执行同步 → 写凭证并重开编辑器」。**没有**操作日志、中断自动恢复
//! 与新增跨进程锁（D2 / D7）：中断后靠「重跑收敛」，覆盖模式靠整目录备份收场。
//!
//! 写入强度：会话文件与索引走 [`session_backup::durable_write_str`]（tmp + fsync + rename +
//! 父目录持久化，Windows 用可写句柄），备份与回滚沿用复制路径的既有原语。

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::modules::account;
use crate::modules::config::{now_ms, utc_iso};
use crate::modules::session::{SessionPaths, SyncSelection, REASON_PREVIEW_STALE};
use crate::modules::session_backup;
use crate::modules::session_link::{
    self, BaselineState, ContentSnapshot, ContentState, LinkGroup, LinkMember, LinkStore,
    MemberState, PreviewBinding, PreviewMemberBinding, StoreState, SyncMode, SyncVerdict,
    NORMALIZATION_VERSION,
};
use crate::modules::variant::WbVariant;
use crate::modules::vscode_ext;
use crate::modules::vscode_session;
use crate::modules::vscode_session::{SessionStoreSpec, VSCODE_STORE};
use crate::modules::vscode_session_link;

/// 派生 id 冲突时的 salt 重试上限（design §6.1）。
const SALT_LIMIT: u32 = 8;
/// 整目录备份的后缀：`<utc_iso>/<workspaceHash>/<targetConvId>-overwrite/`。
const OVERWRITE_SUFFIX: &str = "-overwrite";

// ---------------------------------------------------------------------------
// 会话定位（按 md5(工作区) 分桶，会话 id 不含工作区信息 → 反查一次索引）
// ---------------------------------------------------------------------------

/// 会话在客户端数据里的定位信息：所属工作区 hash 与展示标题。
#[derive(Debug, Clone)]
struct ConversationLocator {
    workspace_hash: String,
    title: String,
}

/// 扫某账号的工作区索引，得到 `会话 id → 定位信息`。
///
/// 客户端把会话按 `md5(工作区)` 分桶，会话 id 本身无法还原工作区；这里按工作区目录名
/// 排序处理，同名会话（不该出现）取字典序最小者，保证结果与目录遍历顺序无关。
fn conversation_index(
    spec: SessionStoreSpec,
    root: &Path,
    uid: &str,
) -> BTreeMap<String, ConversationLocator> {
    let mut out: BTreeMap<String, ConversationLocator> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(vscode_session::history_root_in(spec, root, uid)) else {
        return out;
    };
    let workspaces: BTreeSet<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    for ws_dir in workspaces {
        let Some(index) = vscode_session::read_json(&ws_dir.join("index.json")) else {
            continue;
        };
        let Some(conversations) = index.get("conversations").and_then(Value::as_array) else {
            continue;
        };
        let workspace_hash = ws_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        for conversation in conversations {
            let Some(id) = conversation
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let title = conversation
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("(无标题)")
                .to_string();
            out.entry(id.to_string())
                .or_insert_with(|| ConversationLocator {
                    workspace_hash: workspace_hash.clone(),
                    title,
                });
        }
    }
    out
}

/// 某账号某会话的目录：`history/<workspaceHash>/<conversationId>`。
fn conversation_dir(
    spec: SessionStoreSpec,
    root: &Path,
    uid: &str,
    workspace_hash: &str,
    conversation_id: &str,
) -> PathBuf {
    vscode_session::history_root_in(spec, root, uid)
        .join(workspace_hash)
        .join(conversation_id)
}

/// 一次预览 / 同步内不变的输入：数据仓、账号、数据根与两侧会话定位索引。
struct SyncContext<'a> {
    spec: SessionStoreSpec,
    paths: &'a SessionPaths,
    root: &'a Path,
    source_uid: &'a str,
    target_uid: &'a str,
    source_index: &'a BTreeMap<String, ConversationLocator>,
    target_index: &'a BTreeMap<String, ConversationLocator>,
    /// `Some` 时只处理该档位的关联组。IDE 两侧共用一份表，必须带上；
    /// VS Code 插件跨档位复制，保持 `None`（不过滤）。
    variant_filter: Option<WbVariant>,
}

impl SyncContext<'_> {
    /// 账号对应的定位索引（只处理来源与目标两个账号）。
    fn index_of(&self, uid: &str) -> &BTreeMap<String, ConversationLocator> {
        if uid == self.source_uid {
            self.source_index
        } else {
            self.target_index
        }
    }

    /// 某账号某会话的目录（索引里找不到该会话时为 `None`）。
    fn dir_of(&self, uid: &str, conversation_id: &str) -> Option<PathBuf> {
        self.index_of(uid).get(conversation_id).map(|locator| {
            conversation_dir(
                self.spec,
                self.root,
                uid,
                &locator.workspace_hash,
                conversation_id,
            )
        })
    }

    /// 会话内容状态；索引里找不到该会话（被删除/改归属）时按 `Missing`（判定侧得到 unknown）。
    fn content_of(&self, uid: &str, conversation_id: &str) -> ContentState {
        match self.dir_of(uid, conversation_id) {
            Some(dir) => vscode_session_link::read_session_content(&dir, conversation_id),
            None => ContentState::Missing,
        }
    }

    /// 某账号某会话的定位信息（工作区 hash + 标题）。
    fn locator_of(&self, uid: &str, conversation_id: &str) -> Option<&ConversationLocator> {
        self.index_of(uid).get(conversation_id)
    }

    /// 会话标题（取工作区索引里的名字；插件没有会话级 cwd，留空由标题承担区分）。
    fn title_of(&self, uid: &str, conversation_id: &str) -> String {
        self.index_of(uid)
            .get(conversation_id)
            .map(|locator| locator.title.clone())
            .unwrap_or_else(|| "(无标题)".to_string())
    }
}

/// 账号库里按 uid 找账号 id（成员 `accountId` 仅作展示，身份判定以 uid 为准）。
fn account_id_for_uid(paths: &SessionPaths, uid: &str) -> Option<String> {
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    accounts
        .iter()
        .find(|account| account.get("uid").and_then(Value::as_str) == Some(uid))
        .and_then(|account| account.get("id").and_then(Value::as_str))
        .map(String::from)
}

/// 取报告里的账号 uid（空串按缺失处理）。
fn uid_of(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|uid| !uid.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// R1：复制成功后登记关联（design §5）
// ---------------------------------------------------------------------------

/// 为本次复制成功的会话登记「源 ↔ 副本」关联，返回逐条失败原因。
///
/// **登记失败不回滚复制**：复制已完成、文件已落盘，删除副本只会让用户两头落空；
/// 失败原因由调用方并入报告 `linkErrors[]`，前端提示「已复制但未建立关联」。
pub fn register_copied_sessions(
    root: &Path,
    paths: &SessionPaths,
    variant: WbVariant,
    report: &Value,
) -> Vec<Value> {
    register_copied_sessions_in(VSCODE_STORE, root, paths, variant, report)
}

/// [`register_copied_sessions`] 的数据仓参数化版本（VS Code 插件用：不按 variant 拆组）。
pub fn register_copied_sessions_in(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    variant: WbVariant,
    report: &Value,
) -> Vec<Value> {
    // 插件侧同一扩展数据仓可跨档位复制，登记时不按 variant 拆组。
    register_copied_sessions_inner(spec, root, paths, variant, report, false)
}

/// IDE 两侧共用一份关联表时的登记：只复用同 `variant` 的组，不并入另一档已有的组。
pub(crate) fn register_copied_sessions_isolated(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    variant: WbVariant,
    report: &Value,
) -> Vec<Value> {
    register_copied_sessions_inner(spec, root, paths, variant, report, true)
}

fn register_copied_sessions_inner(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    variant: WbVariant,
    report: &Value,
    isolate_variant: bool,
) -> Vec<Value> {
    let source_uid = uid_of(report, "sourceUid");
    let target_uid = uid_of(report, "targetUid");
    let (Some(source_uid), Some(target_uid)) = (source_uid, target_uid) else {
        return vec![json!({ "error": "复制报告缺少账号信息，未能建立会话关联" })];
    };
    let source_index = conversation_index(spec, root, &source_uid);
    let target_index = conversation_index(spec, root, &target_uid);
    let context = SyncContext {
        spec,
        paths,
        root,
        source_uid: &source_uid,
        target_uid: &target_uid,
        source_index: &source_index,
        target_index: &target_index,
        variant_filter: None,
    };
    let mut errors: Vec<Value> = Vec::new();
    let copied = report
        .get("copied")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for item in copied {
        let workspace_hash = item.get("workspaceHash").and_then(Value::as_str);
        let source_id = item.get("oldId").and_then(Value::as_str);
        let target_id = item.get("newId").and_then(Value::as_str);
        let (Some(workspace_hash), Some(source_id), Some(target_id)) =
            (workspace_hash, source_id, target_id)
        else {
            errors.push(json!({ "error": "复制报告条目缺少会话信息，未能建立会话关联" }));
            continue;
        };
        let outcome = register_one(&context, variant, isolate_variant, source_id, target_id);
        if let Err(reason) = outcome {
            errors.push(json!({
                "workspaceHash": workspace_hash,
                "conversationId": source_id,
                "error": reason,
            }));
        }
    }
    errors
}

/// 登记单个副本：比对双方归一化摘要，写组 / 成员 / 配对基线（全部在存储锁内）。
fn register_one(
    context: &SyncContext<'_>,
    variant: WbVariant,
    isolate_variant: bool,
    source_id: &str,
    target_id: &str,
) -> Result<(), String> {
    let paths = context.paths;
    let source_uid = context.source_uid;
    let target_uid = context.target_uid;
    let source_content = ready_content(context, source_uid, source_id, "来源")?;
    let target_content = ready_content(context, target_uid, target_id, "副本")?;
    // 基线只能建立在「双方内容确实一致」之上：不一致时不登记，
    // 让该会话以「未关联」呈现，而不是留下一条错误的共同基线。
    if source_content.normalized.line_digests != target_content.normalized.line_digests {
        return Err("已复制但副本内容与来源不一致，未建立关联".to_string());
    }
    let normalized = source_content.normalized.clone();
    let source_account_id = account_id_for_uid(paths, source_uid);
    let target_account_id = account_id_for_uid(paths, target_uid);
    let (source_uid, target_uid) = (source_uid.to_string(), target_uid.to_string());
    let (source_id, target_id) = (source_id.to_string(), target_id.to_string());
    session_link::with_link_store_write(paths, move |store| {
        // 同一逻辑会话复用同一组：按「源身份」找组，找不到才新建。
        // 插件路径不按 variant 过滤（同一扩展数据仓可跨档位复制）。
        // IDE 路径 `isolate_variant`：只复用同档位的组，避免国内版 / 国际版并成一组。
        let index = match store.groups.iter().position(|group| {
            let same_variant = !isolate_variant || group.variant == variant;
            same_variant
                && group
                    .members
                    .iter()
                    .any(|member| member.uid == source_uid && member.session_id == source_id)
        }) {
            Some(index) => index,
            None => {
                store.groups.push(LinkGroup {
                    id: uuid::Uuid::new_v4().to_string(),
                    variant,
                    created_at: now_ms(),
                    members: Vec::new(),
                    pair_bases: Vec::new(),
                });
                store.groups.len() - 1
            }
        };
        let group = &mut store.groups[index];
        let source_member_id = match session_link::find_member(group, &source_uid, &source_id)
            .map(|m| m.member_id.clone())
        {
            Some(member_id) => {
                session_link::set_member_state(group, &member_id, MemberState::Active);
                member_id
            }
            None => {
                let member_id = uuid::Uuid::new_v4().to_string();
                session_link::add_active_member(
                    group,
                    LinkMember {
                        member_id: member_id.clone(),
                        account_id: source_account_id,
                        uid: source_uid.clone(),
                        session_id: source_id.clone(),
                        state: MemberState::Active,
                        linked_at: now_ms(),
                        last_synced_at: None,
                    },
                );
                member_id
            }
        };
        let target_member_id = match session_link::find_member(group, &target_uid, &target_id)
            .map(|m| m.member_id.clone())
        {
            Some(member_id) => {
                session_link::set_member_state(group, &member_id, MemberState::Active);
                member_id
            }
            None => {
                let member_id = uuid::Uuid::new_v4().to_string();
                // 同账号上一次的 active 成员在这里被显式 supersede：保留记录、不再有效。
                session_link::add_active_member(
                    group,
                    LinkMember {
                        member_id: member_id.clone(),
                        account_id: target_account_id,
                        uid: target_uid.clone(),
                        session_id: target_id.clone(),
                        state: MemberState::Active,
                        linked_at: now_ms(),
                        last_synced_at: None,
                    },
                );
                member_id
            }
        };
        // 复制完成时的双方内容即共同基线（design §5：复制时即打基线）。
        let baseline_ref = uuid::Uuid::new_v4().to_string();
        session_link::save_baseline(paths, &baseline_ref, &normalized)?;
        session_link::set_pair_base(
            group,
            &source_member_id,
            &target_member_id,
            &baseline_ref,
            NORMALIZATION_VERSION,
        );
        Ok(())
    })
}

/// 登记时读一侧内容：定位失败或内容不可验证都按可读原因拒绝登记。
fn ready_content(
    context: &SyncContext<'_>,
    uid: &str,
    conversation_id: &str,
    what: &str,
) -> Result<ContentSnapshot, String> {
    match context.content_of(uid, conversation_id) {
        ContentState::Ready(snapshot) => Ok(snapshot),
        ContentState::Missing => Err(format!("已复制但找不到{what}内容，未建立关联")),
        ContentState::Unavailable(reason) => Err(format!("内容无法确认（{reason}），未建立关联")),
    }
}

// ---------------------------------------------------------------------------
// R2：切换预览（design §4 / §6）
// ---------------------------------------------------------------------------

/// 预览「当前插件账号 → 目标账号」可同步的关联会话（只读，不写任何会话正文）。
pub fn links_preview(target_acc: &Value) -> Result<Value, String> {
    let root = vscode_session::ext_data_root()
        .ok_or_else(|| "未找到 VS Code CodeBuddy 插件数据目录，无法同步会话".to_string())?;
    let source_uid = vscode_ext::active_ext_uid()
        .filter(|uid| !uid.trim().is_empty())
        .ok_or_else(|| {
            "未检测到 VS Code CodeBuddy 插件当前登录账号，无法同步会话。请先在 VS Code 中登录该插件后重试。"
                .to_string()
        })?;
    links_preview_at(
        &root,
        &SessionPaths::for_vscode_ext(),
        &source_uid,
        target_acc,
    )
}

/// [`links_preview`] 的可测实现：显式传入数据根、存储路径与来源 uid（VS Code 插件数据仓）。
pub fn links_preview_at(
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
) -> Result<Value, String> {
    links_preview_in(VSCODE_STORE, root, paths, source_uid, target_acc)
}

/// [`links_preview`] 的数据仓参数化版本（VS Code 插件：不按 variant 过滤）。
pub fn links_preview_in(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
) -> Result<Value, String> {
    links_preview_in_filtered(spec, root, paths, source_uid, target_acc, None)
}

/// IDE 共用关联表时的预览：只列出 `variant` 与当前档位一致的组。
pub(crate) fn links_preview_in_for_variant(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
    variant: WbVariant,
) -> Result<Value, String> {
    links_preview_in_filtered(spec, root, paths, source_uid, target_acc, Some(variant))
}

fn links_preview_in_filtered(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
    variant_filter: Option<WbVariant>,
) -> Result<Value, String> {
    let target_uid = target_uid_of(target_acc)?;
    if source_uid == target_uid {
        return Err("当前账号与目标账号相同，无需同步会话".to_string());
    }
    // 两侧数据仓都没有档位能力探测：同一数据仓可跨档位复制。
    let mut report = json!({
        "supported": true,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "groups": [],
    });
    match session_link::load_store(paths) {
        StoreState::Missing => report["storeStatus"] = json!("missing"),
        StoreState::Unavailable(reason) => {
            report["storeStatus"] = json!("unavailable");
            report["storeError"] = json!(reason);
        }
        StoreState::Ready(store) => {
            report["storeStatus"] = json!("ready");
            let source_index = conversation_index(spec, root, source_uid);
            let target_index = conversation_index(spec, root, &target_uid);
            let context = SyncContext {
                spec,
                paths,
                root,
                source_uid,
                target_uid: &target_uid,
                source_index: &source_index,
                target_index: &target_index,
                variant_filter,
            };
            let groups: Vec<Value> = store
                .groups
                .iter()
                .filter(|group| {
                    passes_variant_filter(variant_filter, group.variant)
                        && has_member_for(group, source_uid)
                        && has_member_for(group, &target_uid)
                })
                .map(|group| preview_group_item(&context, group))
                .collect();
            report["groups"] = json!(groups);
        }
    }
    Ok(report)
}

/// 目标账号 uid（缺 uid 时按可读原因拒绝）。
fn target_uid_of(target_acc: &Value) -> Result<String, String> {
    account::get_str(target_acc, "uid")
        .map(|uid| uid.trim().to_string())
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| "目标账号缺少 uid，无法同步会话".to_string())
}

/// `None` 表示不过滤（插件路径）；`Some` 时组档位必须一致（IDE 共用命名空间）。
fn passes_variant_filter(filter: Option<WbVariant>, variant: WbVariant) -> bool {
    match filter {
        Some(expected) => variant == expected,
        None => true,
    }
}

/// 组内是否存在该账号的成员（任意状态）：双方都有成员才谈得上「共同参与」。
fn has_member_for(group: &LinkGroup, uid: &str) -> bool {
    group.members.iter().any(|member| member.uid == uid)
}

/// 成员摘要（只含身份与状态，不含正文）。
fn member_summary(member: Option<&LinkMember>) -> Value {
    match member {
        Some(member) => json!({
            "memberId": member.member_id,
            "uid": member.uid,
            "accountId": member.account_id,
            "sessionId": member.session_id,
            "state": member.state.as_str(),
        }),
        None => Value::Null,
    }
}

/// 成员在预览时刻的内容绑定（不可验证时摘要留空，判定必然为 unknown）。
fn member_binding(member: &LinkMember, content: &ContentState) -> PreviewMemberBinding {
    let (raw_digest, normalized_digest, record_count) = match content {
        ContentState::Ready(snapshot) => (
            snapshot.full_digest.clone(),
            snapshot.normalized.total_digest.clone(),
            snapshot.normalized.record_count,
        ),
        _ => (String::new(), String::new(), 0),
    };
    PreviewMemberBinding {
        member_id: member.member_id.clone(),
        account_id: member.account_id.clone(),
        uid: member.uid.clone(),
        session_id: member.session_id.clone(),
        raw_digest,
        normalized_digest,
        record_count,
    }
}

/// 由实时状态组装预览绑定（预览与执行前核对共用同一份装配逻辑）。
fn live_preview_binding(
    group: &LinkGroup,
    source_member: &LinkMember,
    target_member: &LinkMember,
    source_content: &ContentState,
    target_content: &ContentState,
    baseline: &BaselineState,
    verdict: SyncVerdict,
) -> PreviewBinding {
    PreviewBinding {
        variant: group.variant,
        group_id: group.id.clone(),
        group_fingerprint: session_link::group_fingerprint(group),
        source: member_binding(source_member, source_content),
        target: member_binding(target_member, target_content),
        baseline_ref: session_link::find_pair_base(
            group,
            &source_member.member_id,
            &target_member.member_id,
        )
        .map(|pair| pair.baseline_ref.clone()),
        baseline_total_digest: baseline.ready().map(|record| record.total_digest.clone()),
        baseline_record_count: baseline.ready().map(|record| record.record_count),
        verdict,
    }
}

/// 内容里的记录数（不可验证时为 0，仅用于展示）。
fn record_count_of(content: &ContentState) -> usize {
    match content {
        ContentState::Ready(snapshot) => snapshot.normalized.record_count,
        _ => 0,
    }
}

/// 单个关联组的预览项（形状对齐 WorkBuddy 侧 `preview_group_item`）。
///
/// 记录数与差集只用于向用户解释；能否勾选只由判定结果决定（design §3.2）。
fn preview_group_item(context: &SyncContext<'_>, group: &LinkGroup) -> Value {
    let source_any = group.members.iter().find(|m| m.uid == context.source_uid);
    let target_any = group.members.iter().find(|m| m.uid == context.target_uid);
    let title = source_any
        .map(|member| context.title_of(context.source_uid, &member.session_id))
        .unwrap_or_else(|| "(无标题)".to_string());
    let source_summary = member_summary(source_any);
    let target_summary = member_summary(target_any);

    let (Some(source_member), Some(target_member)) = (
        session_link::active_member_for(group, context.source_uid),
        session_link::active_member_for(group, context.target_uid),
    ) else {
        return json!({
            "groupId": group.id,
            "title": title,
            "cwd": "",
            "verdict": SyncVerdict::Unknown.as_str(),
            "extraA": 0,
            "extraB": 0,
            "common": 0,
            "defaultChecked": false,
            "availableModes": [],
            "reason": "对应的会话已失效或已被替换，需手动处理",
            "recordCount": {"source": 0, "target": 0, "baseline": null},
            "source": source_summary,
            "target": target_summary,
        });
    };

    let baseline = session_link::load_pair_baseline(
        context.paths,
        group,
        &source_member.member_id,
        &target_member.member_id,
    );
    let source_content = context.content_of(context.source_uid, &source_member.session_id);
    let target_content = context.content_of(context.target_uid, &target_member.session_id);
    let decision = session_link::decide_sync(&source_content, &target_content, &baseline);
    let reason = decision.reason.clone();
    let modes: Vec<&str> = decision
        .verdict
        .available_modes()
        .iter()
        .map(|mode| mode.as_str())
        .collect();
    let actionable = !modes.is_empty();

    let mut item = json!({
        "groupId": group.id,
        "title": title,
        "cwd": "",
        "verdict": decision.verdict.as_str(),
        "extraA": decision.extra_a,
        "extraB": decision.extra_b,
        "common": decision.common,
        "defaultChecked": decision.default_checked,
        "availableModes": modes,
        "reason": reason.clone(),
        "recordCount": {
            "source": record_count_of(&source_content),
            "target": record_count_of(&target_content),
            "baseline": baseline.ready().map(|record| record.record_count),
        },
        "source": source_summary,
        "target": target_summary,
    });
    if actionable {
        let binding = live_preview_binding(
            group,
            source_member,
            target_member,
            &source_content,
            &target_content,
            &baseline,
            decision.verdict,
        );
        match session_link::save_preview_token(context.paths, binding) {
            Ok(preview_id) => item["previewToken"] = json!(preview_id),
            Err(error) => {
                // 绑定存不下来就不能让用户勾选：不给出可执行动作，并说明原因。
                item["availableModes"] = json!([]);
                item["defaultChecked"] = json!(false);
                item["reason"] = json!(format!("{reason}（检查结果无法保存：{error}）"));
            }
        }
    }
    item
}

// ---------------------------------------------------------------------------
// R3 / R4：执行同步（design §6）
// ---------------------------------------------------------------------------

/// 空报告（与 WorkBuddy 侧同形）。
fn empty_report() -> Value {
    json!({ "synced": [], "skipped": [], "errors": [] })
}

/// 执行勾选的同步项；返回 `{synced, skipped, errors}` 报告。
///
/// **前提**：调用方已确认编辑器完全退出（写入会被运行中的插件覆盖）。本函数自行复查一次，
/// 并在执行前重新加载身份 / 成员 / 基线 / 正文后用预览凭据逐项核对：
/// 任一变化都跳过该项（原因码 [`REASON_PREVIEW_STALE`]），绝不静默写入。
pub fn sync_selected(target_acc: &Value, selections: &[SyncSelection]) -> Result<Value, String> {
    if selections.is_empty() {
        return Ok(empty_report());
    }
    if vscode_ext::is_vscode_running() {
        return Err(
            "检测到 VS Code 正在运行，请先完全退出后再同步会话，否则写入会被 VS Code 覆盖。"
                .to_string(),
        );
    }
    let root = vscode_session::ext_data_root()
        .ok_or_else(|| "未找到 VS Code CodeBuddy 插件数据目录，无法同步会话".to_string())?;
    let source_uid = vscode_ext::active_ext_uid()
        .filter(|uid| !uid.trim().is_empty())
        .ok_or_else(|| {
            "未检测到 VS Code CodeBuddy 插件当前登录账号，无法同步会话。请先在 VS Code 中登录该插件后重试。"
                .to_string()
        })?;
    sync_selected_at(
        &root,
        &SessionPaths::for_vscode_ext(),
        &source_uid,
        target_acc,
        selections,
    )
}

/// [`sync_selected`] 的可测实现：显式传入数据根、存储路径与来源 uid（VS Code 插件数据仓）。
pub fn sync_selected_at(
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
    selections: &[SyncSelection],
) -> Result<Value, String> {
    sync_selected_in(
        VSCODE_STORE,
        root,
        paths,
        source_uid,
        target_acc,
        selections,
    )
}

/// [`sync_selected`] 的数据仓参数化版本（VS Code 插件：不按 variant 过滤）。
pub fn sync_selected_in(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
    selections: &[SyncSelection],
) -> Result<Value, String> {
    sync_selected_in_filtered(spec, root, paths, source_uid, target_acc, selections, None)
}

/// IDE 共用关联表时的同步：拒绝另一档位的组（即使成员 uid 碰巧相同）。
pub(crate) fn sync_selected_in_for_variant(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
    selections: &[SyncSelection],
    variant: WbVariant,
) -> Result<Value, String> {
    sync_selected_in_filtered(
        spec,
        root,
        paths,
        source_uid,
        target_acc,
        selections,
        Some(variant),
    )
}

fn sync_selected_in_filtered(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    source_uid: &str,
    target_acc: &Value,
    selections: &[SyncSelection],
    variant_filter: Option<WbVariant>,
) -> Result<Value, String> {
    if selections.is_empty() {
        return Ok(empty_report());
    }
    let target_uid = target_uid_of(target_acc)?;
    if source_uid == target_uid {
        return Err("当前账号与目标账号相同，无需同步会话".to_string());
    }
    let store = match session_link::load_store(paths) {
        StoreState::Ready(store) => store,
        StoreState::Missing => {
            return Err("同步记录不存在或不可用，无法同步会话".to_string());
        }
        StoreState::Unavailable(reason) => return Err(reason),
    };
    let source_index = conversation_index(spec, root, source_uid);
    let target_index = conversation_index(spec, root, &target_uid);
    let context = SyncContext {
        spec,
        paths,
        root,
        source_uid,
        target_uid: &target_uid,
        source_index: &source_index,
        target_index: &target_index,
        variant_filter,
    };

    let mut synced: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    for selection in selections {
        let group_id = selection.group_id.clone();
        match plan_sync_item(&context, &store, selection) {
            SyncItemOutcome::Validated(plan) => match execute_sync_item(paths, &plan) {
                Ok(item) => synced.push(item),
                Err(error) => errors.push(json!({ "groupId": group_id, "error": error })),
            },
            SyncItemOutcome::Skipped { message, verdict } => skipped.push(json!({
                "groupId": group_id,
                "status": "skipped",
                "reasonCode": REASON_PREVIEW_STALE,
                "message": message,
                "verdict": verdict.map(SyncVerdict::as_str),
            })),
            SyncItemOutcome::Rejected { message } => {
                errors.push(json!({ "groupId": group_id, "error": message }))
            }
        }
    }
    Ok(json!({ "synced": synced, "skipped": skipped, "errors": errors }))
}

/// 一次写入所需的已核对信息（校验与写入之间不再回读来源，避免 TOCTOU）。
pub(crate) struct SyncItemPlan {
    group_id: String,
    mode: SyncMode,
    verdict: SyncVerdict,
    source_member: LinkMember,
    target_member: LinkMember,
    source_dir: PathBuf,
    target_dir: PathBuf,
    /// 目标会话所属工作区（备份目录布局用）。
    workspace_hash: String,
    source_conversation_id: String,
    target_conversation_id: String,
    target_uid: String,
    /// 备份根目录名（`<工具存储根>/backups/<backup_kind>/<utc_iso>/`）。
    backup_kind: &'static str,
    source_content: ContentSnapshot,
    target_content: ContentSnapshot,
}

/// 单条同步选择的校验结果。
enum SyncItemOutcome {
    Validated(Box<SyncItemPlan>),
    /// 版本变化 / 前置条件不满足：跳过该项，不沿用用户旧选择。
    Skipped {
        message: String,
        verdict: Option<SyncVerdict>,
    },
    /// 入参或凭据非法、模式越权：拒绝，不静默执行。
    Rejected {
        message: String,
    },
}

/// 重新校验单条选择：凭据、身份、成员、基线、正文逐项核对（design §7.3）。
fn plan_sync_item(
    context: &SyncContext<'_>,
    store: &LinkStore,
    selection: &SyncSelection,
) -> SyncItemOutcome {
    // 凭据必须是我们服务端保存过的：伪造的 id 读不到，直接拒绝。
    let Some(token) = session_link::load_preview_token(context.paths, &selection.preview_token)
    else {
        return SyncItemOutcome::Rejected {
            message: "检查结果不存在或已失效，请重新检查后再操作".to_string(),
        };
    };
    let binding = &token.binding;
    if binding.group_id != selection.group_id
        || !passes_variant_filter(context.variant_filter, binding.variant)
    {
        return SyncItemOutcome::Rejected {
            message: "检查结果与所选会话不匹配，已拒绝".to_string(),
        };
    }
    let skip = |message: String| SyncItemOutcome::Skipped {
        message,
        verdict: Some(binding.verdict),
    };
    if binding.source.uid != context.source_uid || binding.target.uid != context.target_uid {
        return skip("账号已变化，检查结果已失效".to_string());
    }
    let Some(group) = store.groups.iter().find(|group| {
        group.id == selection.group_id
            && passes_variant_filter(context.variant_filter, group.variant)
    }) else {
        return skip("会话的关联关系已不存在，检查结果已失效".to_string());
    };
    let (Some(source_member), Some(target_member)) = (
        session_link::active_member_for(group, context.source_uid),
        session_link::active_member_for(group, context.target_uid),
    ) else {
        return skip("对应的会话已失效，检查结果已失效".to_string());
    };
    let (Some(source_dir), Some(target_dir)) = (
        context.dir_of(context.source_uid, &source_member.session_id),
        context.dir_of(context.target_uid, &target_member.session_id),
    ) else {
        return skip("找不到会话所在的工作区，检查结果已失效".to_string());
    };
    // 重新加载正文、基线与判定，再与凭据逐项核对；任一变化都跳过（含显式覆盖）。
    let source_content =
        vscode_session_link::read_session_content(&source_dir, &source_member.session_id);
    let target_content =
        vscode_session_link::read_session_content(&target_dir, &target_member.session_id);
    let baseline = session_link::load_pair_baseline(
        context.paths,
        group,
        &source_member.member_id,
        &target_member.member_id,
    );
    let decision = session_link::decide_sync(&source_content, &target_content, &baseline);
    let live = live_preview_binding(
        group,
        source_member,
        target_member,
        &source_content,
        &target_content,
        &baseline,
        decision.verdict,
    );
    let mismatches = session_link::verify_preview(&token, &live);
    if !mismatches.is_empty() {
        return skip(format!("检查结果已失效：{}", mismatches.join("；")));
    }
    if !decision.verdict.allows(selection.mode) {
        return SyncItemOutcome::Rejected {
            message: format!(
                "该组判定为 {}，不允许以 {} 模式同步",
                decision.verdict.as_str(),
                selection.mode.as_str()
            ),
        };
    }
    // 判定非 unknown 必然双方正文可验证（decide_sync 的前置条件），这里仍然显式兜底。
    let (ContentState::Ready(source_snapshot), ContentState::Ready(target_snapshot)) =
        (&source_content, &target_content)
    else {
        return skip("双方内容不可验证，检查结果已失效".to_string());
    };
    SyncItemOutcome::Validated(Box::new(SyncItemPlan {
        group_id: selection.group_id.clone(),
        mode: selection.mode,
        verdict: decision.verdict,
        source_member: source_member.clone(),
        target_member: target_member.clone(),
        source_dir,
        target_dir,
        workspace_hash: context
            .locator_of(context.target_uid, &target_member.session_id)
            .map(|locator| locator.workspace_hash.clone())
            .unwrap_or_default(),
        source_conversation_id: source_member.session_id.clone(),
        target_conversation_id: target_member.session_id.clone(),
        target_uid: context.target_uid.to_string(),
        backup_kind: context.spec.backup_kind,
        source_content: source_snapshot.clone(),
        target_content: target_snapshot.clone(),
    }))
}

/// 按模式执行写入。
fn execute_sync_item(paths: &SessionPaths, plan: &SyncItemPlan) -> Result<Value, String> {
    match plan.mode {
        SyncMode::FastForward => fast_forward(paths, plan),
        SyncMode::Overwrite => overwrite(paths, plan),
    }
}

/// 一条待写入的副本消息文件。
struct PlannedFile {
    /// 派生后的消息 id（写进索引的条目 id）。
    id: String,
    path: PathBuf,
    content: String,
    /// 目标已有同名同内容文件：本次不写、回滚也不删（重跑幂等）。
    already_applied: bool,
}

// --- 快进（design §6.1） ---------------------------------------------------

/// 备份根目录：`<工具存储根>/backups/<backup_kind>/<utc_iso>/`（与复制路径同布局）。
///
/// 从 [`SessionPaths::backup_root`] 派生（而不是直接取 [`backup_dir`]），
/// 单测才能注入临时存储根而不污染真实 `~/.wb-switch`。
fn backup_root(paths: &SessionPaths, backup_kind: &str) -> PathBuf {
    paths.backup_root().join(backup_kind).join(utc_iso())
}

/// 备份目标会话索引，返回备份目录。
fn begin_index_backup(paths: &SessionPaths, plan: &SyncItemPlan) -> Result<PathBuf, String> {
    let dir = backup_root(paths, plan.backup_kind)
        .join(&plan.workspace_hash)
        .join(&plan.target_conversation_id);
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建索引备份目录失败：{error}"))?;
    for name in ["index.json", ".index_bak.json"] {
        let from = plan.target_dir.join(name);
        if from.is_file() {
            std::fs::copy(&from, dir.join(name))
                .map_err(|error| format!("备份目标索引失败（{name}）：{error}"))?;
        }
    }
    Ok(dir)
}

/// 回滚快进：索引按备份恢复、删除本次新建的消息文件。
///
/// 失败必须**上报**而不是丢弃：索引没恢复、新增文件没删掉，调用方都不能对用户声称
/// 「已同步内容已回滚」。返回的失败说明自带备份路径，用户可据此手工恢复。
/// 成败口径与原来一致（备份存在则 `copy` 回、不存在则删目标），只是不再吞错：
/// `NotFound` 视为成功（幂等删除，文件本就不在），只有真实失败才计入。
///
/// 本次追加的附件**不回滚**：它们是纯增量，且同名覆盖前目标侧的原始副本仍在源账号。
pub(crate) fn rollback_index(
    plan: &SyncItemPlan,
    backup_dir: &Path,
    created: &[PathBuf],
) -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();
    for name in ["index.json", ".index_bak.json"] {
        let backup = backup_dir.join(name);
        let target = plan.target_dir.join(name);
        if backup.is_file() {
            if let Err(error) = std::fs::copy(&backup, &target) {
                failures.push(format!("索引 {name} 未恢复：{error}"));
            }
        } else if let Err(error) = remove_file_if_exists(&target) {
            failures.push(format!("索引 {name} 未删除：{error}"));
        }
    }
    // 新增消息可能上百条：逐条列名会把用户可见文案撑爆，这里只报失败条数。
    let undeleted = created
        .iter()
        .filter(|path| remove_file_if_exists(path).is_err())
        .count();
    if undeleted > 0 {
        failures.push(format!("本次新增的 {undeleted} 个消息文件未删除"));
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "副本回滚未完成（{}），请从备份目录 {} 手工恢复",
        failures.join("；"),
        backup_dir.display()
    ))
}

/// 幂等删除文件：`NotFound` 视为成功（文件本就不在，目标状态已达成）。
fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// 写目标会话索引（`index.json` 与 `.index_bak.json` 同内容，与复制路径一致）。
fn write_session_index(target_dir: &Path, index: &Value) -> Result<(), String> {
    let text = index.to_string();
    session_backup::durable_write_str(&target_dir.join("index.json"), &text)
        .map_err(|error| format!("写入副本索引失败：{error}"))?;
    session_backup::durable_write_str(&target_dir.join(".index_bak.json"), &text)
        .map_err(|error| format!("写入副本索引备份失败：{error}"))?;
    Ok(())
}

/// 快进：把来源基线之后的新增记录追加到副本（design §6.1）。
fn fast_forward(paths: &SessionPaths, plan: &SyncItemPlan) -> Result<Value, String> {
    let source_index = vscode_session_link::read_session_index(&plan.source_dir)
        .ok_or_else(|| "源会话索引缺失或损坏，已停止同步".to_string())?;
    let target_index = vscode_session_link::read_session_index(&plan.target_dir)
        .ok_or_else(|| "目标会话索引缺失或损坏，已停止同步".to_string())?;
    let source_message_ids = vscode_session_link::message_ids_of(&source_index);
    let target_message_ids = vscode_session_link::message_ids_of(&target_index);
    let baseline_count = plan.target_content.normalized.record_count;
    if target_message_ids.len() != baseline_count {
        return Err("目标内容与索引不一致，已停止同步".to_string());
    }
    if source_message_ids.len() < baseline_count
        || plan.source_content.normalized.line_digests.len() != source_message_ids.len()
    {
        return Err("来源内容与索引不一致，已停止同步".to_string());
    }
    let source_messages = source_index
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let source_requests = source_index
        .get("requests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // 翻译表：前缀按位置对齐（判定保证「目标内容是来源内容的严格有序前缀」），
    // 新增消息 / 请求用确定性派生 id（design §6.1：重跑覆盖同一批文件）。
    let mut translation =
        vscode_session_link::positional_translation(&source_message_ids, &target_message_ids);
    let target_request_ids = request_ids_of(&target_index);
    let source_request_ids = request_ids_of(&source_index);
    let request_prefix = target_request_ids.len().min(source_request_ids.len());
    translation.extend(vscode_session_link::positional_translation(
        &source_request_ids[..request_prefix],
        &target_request_ids[..request_prefix],
    ));
    let request_digests =
        vscode_session_link::request_digests(&source_index, &plan.source_conversation_id);

    // 新增请求的确定性 id 必须先落进翻译表：消息文件里的 `extra.requestId` 要一起翻译，
    // 否则副本里会残留源请求 id，归一化摘要也就对不上源（判定地基被破坏）。
    let mut used_request_ids: BTreeSet<String> = target_request_ids.iter().cloned().collect();
    for (index, source_id) in source_request_ids.iter().enumerate().skip(request_prefix) {
        let digest = request_digests.get(index).cloned().unwrap_or_default();
        let mut derived = None;
        for salt in 0..SALT_LIMIT {
            let candidate = vscode_session_link::derive_request_id(
                &plan.target_uid,
                &plan.target_conversation_id,
                index,
                &digest,
                salt,
            );
            if !used_request_ids.contains(&candidate) {
                derived = Some(candidate);
                break;
            }
        }
        let derived = derived.ok_or_else(|| "目标账号已存在同名请求，已停止同步".to_string())?;
        used_request_ids.insert(derived.clone());
        translation.insert(source_id.clone(), derived);
    }

    let mut files: Vec<PlannedFile> = Vec::new();
    for (index, source_id) in source_message_ids.iter().enumerate().skip(baseline_count) {
        let digest = &plan.source_content.normalized.line_digests[index];
        let source_file = plan
            .source_dir
            .join("messages")
            .join(format!("{source_id}.json"));
        let text = std::fs::read_to_string(&source_file)
            .map_err(|error| format!("读取来源消息失败（{source_id}）：{error}"))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|_| format!("来源消息 {source_id} 无法解析，已停止同步"))?;
        let planned = plan_message_file(plan, index, digest, source_id, &value, &mut translation)?;
        translation.insert(source_id.clone(), planned.id.clone());
        files.push(planned);
    }

    // 目标索引 = 旧索引 + 翻译后的新增消息 / 请求。
    let mut out_index = target_index.clone();
    if let Some(object) = out_index.as_object_mut() {
        if let Some(array) = object
            .entry("messages".to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
        {
            for entry in source_messages.iter().skip(baseline_count) {
                array.push(vscode_session_link::translate_message_entry(
                    entry,
                    &translation,
                ));
            }
        }
        if let Some(array) = object
            .entry("requests".to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
        {
            for entry in source_requests.iter().skip(request_prefix) {
                array.push(vscode_session_link::translate_request_entry(
                    entry,
                    &translation,
                ));
            }
        }
    }

    let backup_dir = begin_index_backup(paths, plan)?;
    let mut created: Vec<PathBuf> = Vec::new();
    let assets = match write_fast_forward(plan, &out_index, &files, &mut created) {
        Ok(assets) => assets,
        Err(error) => {
            return Err(rollback_message(
                rollback_index(plan, &backup_dir, &created),
                &error,
            ));
        }
    };
    // 基线提交失败 → 恢复索引 + 删除本次新增文件（design §6.1 步 6）。
    if let Err(error) = commit_baseline(paths, plan) {
        return Err(rollback_message(
            rollback_index(plan, &backup_dir, &created),
            &error,
        ));
    }

    let source_count = plan.source_content.normalized.record_count;
    let target_before = plan.target_content.normalized.record_count;
    Ok(json!({
        "groupId": plan.group_id,
        "status": "synced",
        "verdict": plan.verdict.as_str(),
        "mode": plan.mode.as_str(),
        "recordCount": {"source": source_count, "targetBefore": target_before, "target": source_count},
        "assets": {"copied": assets.0, "overwritten": assets.1},
        "backup": backup_dir.to_string_lossy(),
        "message": format!(
            "已把当前账号新增的 {} 条内容同步到副本（追加，不改动目标已有内容）",
            source_count.saturating_sub(target_before)
        ),
    }))
}

/// 写入计划：消息文件 → 会话索引 → 附件。任一步失败由调用方回滚。
fn write_fast_forward(
    plan: &SyncItemPlan,
    index: &Value,
    files: &[PlannedFile],
    created: &mut Vec<PathBuf>,
) -> Result<(usize, usize), String> {
    for file in files {
        if file.already_applied {
            continue;
        }
        session_backup::durable_write_str(&file.path, &file.content)
            .map_err(|error| format!("写入副本消息失败：{error}"))?;
        created.push(file.path.clone());
    }
    write_session_index(&plan.target_dir, index)?;
    merge_children(&plan.source_dir, &plan.target_dir)
}

/// 规划一条新增消息文件：确定性 id + 冲突 salt 重试（design §6.1）。
fn plan_message_file(
    plan: &SyncItemPlan,
    index: usize,
    digest: &str,
    source_id: &str,
    value: &Value,
    translation: &mut BTreeMap<String, String>,
) -> Result<PlannedFile, String> {
    for salt in 0..SALT_LIMIT {
        let id = vscode_session_link::derive_message_id(
            &plan.target_uid,
            &plan.target_conversation_id,
            index,
            digest,
            salt,
        );
        translation.insert(source_id.to_string(), id.clone());
        let content = vscode_session_link::translate_message(value, translation).to_string();
        let path = plan.target_dir.join("messages").join(format!("{id}.json"));
        match std::fs::read_to_string(&path) {
            // 目标已有同名文件且内容不同：换 salt 重试，不覆盖别人的内容。
            Ok(existing) if existing != content => continue,
            // 已应用过同一份内容：本次不写（重跑幂等）。
            Ok(_) => {
                return Ok(PlannedFile {
                    id,
                    path,
                    content,
                    already_applied: true,
                })
            }
            Err(_) => {
                return Ok(PlannedFile {
                    id,
                    path,
                    content,
                    already_applied: false,
                })
            }
        }
    }
    Err("目标账号已存在同名消息且内容不同，已停止同步".to_string())
}

/// 索引里的请求 id 序列。
fn request_ids_of(index: &Value) -> Vec<String> {
    index
        .get("requests")
        .and_then(Value::as_array)
        .map(|requests| {
            requests
                .iter()
                .filter_map(|request| {
                    request
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 提交配对基线：写来源当前记录摘要 + 刷新两侧成员的同步时间（定向更新，不动其它配对）。
fn commit_baseline(paths: &SessionPaths, plan: &SyncItemPlan) -> Result<(), String> {
    let group_id = plan.group_id.clone();
    let source_member_id = plan.source_member.member_id.clone();
    let target_member_id = plan.target_member.member_id.clone();
    let normalized = plan.source_content.normalized.clone();
    let baseline_ref = uuid::Uuid::new_v4().to_string();
    session_link::with_link_store_write(paths, move |store| {
        let Some(group) = store.groups.iter_mut().find(|group| group.id == group_id) else {
            return Err("会话的关联关系已不存在，未提交同步结果".to_string());
        };
        if !group
            .members
            .iter()
            .any(|member| member.member_id == source_member_id)
        {
            return Err("当前账号的会话已不存在，未提交同步结果".to_string());
        }
        let now = now_ms();
        for member_id in [&source_member_id, &target_member_id] {
            if let Some(member) = group
                .members
                .iter_mut()
                .find(|member| member.member_id == *member_id)
            {
                member.last_synced_at = Some(now);
            }
        }
        session_link::save_baseline(paths, &baseline_ref, &normalized)?;
        session_link::set_pair_base(
            group,
            &source_member_id,
            &target_member_id,
            &baseline_ref,
            NORMALIZATION_VERSION,
        );
        Ok(())
    })
}

// --- 覆盖（design §6.2） ---------------------------------------------------

/// 覆盖：整目录备份 → 按确定性翻译表全量重建（保留目标会话 id）→ 失败整目录恢复 → 提交基线。
fn overwrite(paths: &SessionPaths, plan: &SyncItemPlan) -> Result<Value, String> {
    let source_index = vscode_session_link::read_session_index(&plan.source_dir)
        .ok_or_else(|| "源会话索引缺失或损坏，已停止同步".to_string())?;
    let source_message_ids = vscode_session_link::message_ids_of(&source_index);
    let digests = &plan.source_content.normalized.line_digests;
    if digests.len() != source_message_ids.len() {
        return Err("来源内容与索引不一致，已停止同步".to_string());
    }

    // 1) 整目录备份（先备份再动任何东西）。
    let backup_dir = backup_root(paths, plan.backup_kind)
        .join(&plan.workspace_hash)
        .join(format!("{}{OVERWRITE_SUFFIX}", plan.target_conversation_id));
    vscode_session::remove_dir_all_if_exists(&backup_dir);
    vscode_session::copy_dir_recursive(&plan.target_dir, &backup_dir)
        .map_err(|error| format!("备份目标会话目录失败：{error}"))?;

    // 2) 翻译表：全量重建，消息 / 请求 id 用确定性派生（保留目标会话 id → 工作区索引无需改动）。
    let mut message_ids: BTreeMap<String, String> = BTreeMap::new();
    for (index, source_id) in source_message_ids.iter().enumerate() {
        message_ids.insert(
            source_id.clone(),
            vscode_session_link::derive_message_id(
                &plan.target_uid,
                &plan.target_conversation_id,
                index,
                &digests[index],
                0,
            ),
        );
    }
    let source_request_ids = request_ids_of(&source_index);
    let request_digests =
        vscode_session_link::request_digests(&source_index, &plan.source_conversation_id);
    let mut request_ids: BTreeMap<String, String> = BTreeMap::new();
    for (index, source_id) in source_request_ids.iter().enumerate() {
        request_ids.insert(
            source_id.clone(),
            vscode_session_link::derive_request_id(
                &plan.target_uid,
                &plan.target_conversation_id,
                index,
                &request_digests.get(index).cloned().unwrap_or_default(),
                0,
            ),
        );
    }
    let remap = vscode_session::remap_plan_from_maps(message_ids, request_ids);

    // 3) 先写临时目录，再原子替换（保留目标会话 id，工作区索引与 `current` 语义天然不变）。
    let tmp_dir = plan.target_dir.with_file_name(format!(
        ".tmp-{}{OVERWRITE_SUFFIX}",
        plan.target_conversation_id
    ));
    vscode_session::remove_dir_all_if_exists(&tmp_dir);
    if let Err(error) =
        vscode_session::write_conversation(&plan.source_dir, &tmp_dir, &source_index, &remap)
    {
        vscode_session::remove_dir_all_if_exists(&tmp_dir);
        return Err(format!("重建副本会话目录失败：{error}"));
    }
    vscode_session::remove_dir_all_if_exists(&plan.target_dir);
    if let Err(error) = std::fs::rename(&tmp_dir, &plan.target_dir) {
        vscode_session::remove_dir_all_if_exists(&tmp_dir);
        return Err(rollback_message(
            restore_dir(&backup_dir, &plan.target_dir),
            &format!("提交副本会话目录失败：{error}"),
        ));
    }

    // 4) 复算核验：重建后的记录摘要必须与来源一致，否则整目录回滚。
    let rebuilt =
        vscode_session_link::read_session_content(&plan.target_dir, &plan.target_conversation_id);
    let verified = matches!(
        &rebuilt,
        ContentState::Ready(snapshot)
            if snapshot.normalized.line_digests == plan.source_content.normalized.line_digests
    );
    if !verified {
        return Err(rollback_message(
            restore_dir(&backup_dir, &plan.target_dir),
            "重建后的内容与来源不一致，已整目录回滚",
        ));
    }

    // 5) 提交基线；失败 → 整目录从备份恢复。
    if let Err(error) = commit_baseline(paths, plan) {
        return Err(rollback_message(
            restore_dir(&backup_dir, &plan.target_dir),
            &error,
        ));
    }

    let source_count = plan.source_content.normalized.record_count;
    Ok(json!({
        "groupId": plan.group_id,
        "status": "synced",
        "verdict": plan.verdict.as_str(),
        "mode": plan.mode.as_str(),
        "recordCount": {
            "source": source_count,
            "targetBefore": plan.target_content.normalized.record_count,
            "target": source_count,
        },
        "assets": {"copied": 0, "overwritten": 0},
        "backup": backup_dir.to_string_lossy(),
        "message": format!("已用当前账号的 {source_count} 条内容覆盖副本（覆盖前已整目录备份）"),
    }))
}

/// 从备份目录整目录恢复目标会话目录。
///
/// 失败必须**上报**而不是丢弃：目标目录已先被清空，`copy_dir_recursive` 又会在读源之前
/// 先建出目标目录，恢复失败意味着副本只剩一个空目录（或半成品），调用方绝不能对用户
/// 声称「已回滚」。失败文案自带备份路径，用户可据此手工恢复。
pub(crate) fn restore_dir(backup_dir: &Path, target_dir: &Path) -> Result<(), String> {
    vscode_session::remove_dir_all_if_exists(target_dir);
    vscode_session::copy_dir_recursive(backup_dir, target_dir).map_err(|error| {
        format!(
            "副本目录未恢复（{error}），请从备份目录 {} 手工恢复",
            backup_dir.display()
        )
    })
}

/// 回滚后的用户可见文案：回滚成功时与原来的文案逐字一致；回滚失败时追加
/// [`restore_dir`] / [`rollback_index`] 的说明（未回滚 + 备份路径），不掩盖失败。
fn rollback_message(restored: Result<(), String>, message: &str) -> String {
    match restored {
        Ok(()) => message.to_string(),
        Err(error) => format!("{message}；且{error}"),
    }
}

// --- 附件合并 --------------------------------------------------------------

/// 把源会话里目标缺失的附件 / 子目录补进目标，返回（新增数, 覆盖数）。
///
/// 只处理非索引顶层条目：`index.json` / `.index_bak.json` / `messages/` 由写入流程单独处理。
/// 同名不同内容以源为准覆盖（源是权威），覆盖数在报告里标注。
fn merge_children(source_dir: &Path, target_dir: &Path) -> Result<(usize, usize), String> {
    let mut copied = 0usize;
    let mut overwritten = 0usize;
    let entries = std::fs::read_dir(source_dir).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "index.json" || name == ".index_bak.json" || name == "messages" {
            continue;
        }
        let from = entry.path();
        let to = target_dir.join(&name);
        if from.is_dir() {
            let (nested_copied, nested_overwritten) = merge_dir(&from, &to)?;
            copied += nested_copied;
            overwritten += nested_overwritten;
        } else if from.is_file() {
            match copy_if_needed(&from, &to)? {
                Some(true) => copied += 1,
                Some(false) => overwritten += 1,
                None => {}
            }
        }
    }
    Ok((copied, overwritten))
}

/// [`merge_children`] 的递归部分。
fn merge_dir(source_dir: &Path, target_dir: &Path) -> Result<(usize, usize), String> {
    let mut copied = 0usize;
    let mut overwritten = 0usize;
    let entries = std::fs::read_dir(source_dir).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = target_dir.join(entry.file_name());
        if from.is_dir() {
            let (nested_copied, nested_overwritten) = merge_dir(&from, &to)?;
            copied += nested_copied;
            overwritten += nested_overwritten;
        } else if from.is_file() {
            match copy_if_needed(&from, &to)? {
                Some(true) => copied += 1,
                Some(false) => overwritten += 1,
                None => {}
            }
        }
    }
    Ok((copied, overwritten))
}

/// 目标缺失 → 复制（`Some(true)`）；同名不同内容 → 以源为准覆盖（`Some(false)`）；相同 → `None`。
fn copy_if_needed(from: &Path, to: &Path) -> Result<Option<bool>, String> {
    let same = match (std::fs::read(to), std::fs::read(from)) {
        (Ok(existing), Ok(source)) => existing == source,
        _ => false,
    };
    if same {
        return Ok(None);
    }
    let existed = to.exists();
    if let Some(parent) = to.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    std::fs::copy(from, to).map_err(|error| format!("复制附件失败：{error}"))?;
    Ok(Some(!existed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::vscode_session::{copy_sessions_in, CopyItem};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const WS: &str = "0123456789abcdef0123456789abcdef";
    const SRC_UID: &str = "uid-src-0001";
    const DST_UID: &str = "uid-dst-0002";
    const CONV_SRC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MSG_1: &str = "11111111111111111111111111111111";
    const MSG_2: &str = "22222222222222222222222222222222";
    const MSG_3: &str = "33333333333333333333333333333333";
    const MSG_4: &str = "44444444444444444444444444444444";
    const REQ_1: &str = "55555555555555555555555555555555";
    const REQ_2: &str = "66666666666666666666666666666666";
    const MSG_5: &str = "77777777777777777777777777777777";
    const MSG_6: &str = "88888888888888888888888888888888";
    const REQ_3: &str = "99999999999999999999999999999999";

    struct Fixture {
        base: PathBuf,
        /// 扩展数据根（`<base>/Data`）。
        root: PathBuf,
        /// 复制备份根（注入给 `copy_sessions_in`）。
        backup: PathBuf,
        /// 工具存储根（注入给 `SessionPaths`）。
        store: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "wb_switch_vscode_sync_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            let root = base.join("Data");
            let backup = base.join("copy-backup");
            let store = base.join("store");
            for dir in [&root, &backup, &store] {
                std::fs::create_dir_all(dir).unwrap();
            }
            Self {
                base,
                root,
                backup,
                store,
            }
        }

        fn paths(&self) -> SessionPaths {
            SessionPaths::for_vscode_ext_at(self.store.clone())
        }

        fn src_conv_dir(&self) -> PathBuf {
            vscode_session::history_root(&self.root, SRC_UID)
                .join(WS)
                .join(CONV_SRC)
        }

        fn dst_conv_dir(&self, conv_id: &str) -> PathBuf {
            vscode_session::history_root(&self.root, DST_UID)
                .join(WS)
                .join(conv_id)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn target_acc() -> Value {
        json!({ "id": "acc-dst", "uid": DST_UID, "nickname": "目标账号" })
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// 写一条消息文件：`extra` 为字符串化 JSON（实测形态）。
    fn write_message(conv_dir: &Path, id: &str, role: &str, request_id: &str, text: &str) {
        write(
            &conv_dir.join(format!("messages/{id}.json")),
            &json!({
                "role": role,
                "message": json!({"role": role, "content": text}).to_string(),
                "id": id,
                "extra": json!({"requestId": request_id, "responseId": id, "modelId": "deepseek-v4"})
                    .to_string(),
                "createdAt": "2026-09-16T05:05:29.751Z",
            })
            .to_string(),
        );
    }

    /// 造源账号的会话：工作区索引 + 2 条记录（user + assistant）+ 一个附件。
    fn seed_source(fixture: &Fixture) {
        let ws_dir = vscode_session::history_root(&fixture.root, SRC_UID).join(WS);
        write(
            &ws_dir.join("index.json"),
            &json!({
                "conversations": [{
                    "id": CONV_SRC, "type": "craft", "name": "测试会话",
                    "createdAt": "2026-09-15T07:47:01.550Z",
                    "lastMessageAt": "2026-09-16T05:22:25.406Z", "chatMode": "craft",
                }],
                "current": CONV_SRC,
            })
            .to_string(),
        );
        let conv_dir = ws_dir.join(CONV_SRC);
        write(
            &conv_dir.join("index.json"),
            &json!({
                "messages": [
                    {"id": MSG_1, "type": "text", "role": "user", "isComplete": true},
                    // 助手消息的 isComplete 恒为 false（实测常态，不得据此判断写完）
                    {"id": MSG_2, "type": "text", "role": "assistant", "isComplete": false},
                ],
                "requests": [{
                    "id": REQ_1, "type": "craft", "messages": [MSG_1, MSG_2],
                    "state": "complete", "startedAt": 1789532362052_i64,
                }],
            })
            .to_string(),
        );
        write_message(&conv_dir, MSG_1, "user", REQ_1, "你好");
        write_message(&conv_dir, MSG_2, "assistant", REQ_1, "在的");
        std::fs::create_dir_all(conv_dir.join("assets")).unwrap();
        std::fs::write(conv_dir.join("assets/图片.1.jpeg"), b"\x01\x02\x03binary").unwrap();
    }

    /// 源账号追加一轮对话（user + assistant + 一条请求）与一个新附件。
    fn append_round(
        fixture: &Fixture,
        user_id: &str,
        assistant_id: &str,
        request_id: &str,
        text: &str,
    ) {
        let conv_dir = fixture.src_conv_dir();
        let mut index = vscode_session::read_json(&conv_dir.join("index.json")).unwrap();
        index["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": user_id, "type": "text", "role": "user", "isComplete": true}));
        index["messages"].as_array_mut().unwrap().push(
            json!({"id": assistant_id, "type": "text", "role": "assistant", "isComplete": false}),
        );
        index["requests"].as_array_mut().unwrap().push(json!({
            "id": request_id, "type": "craft", "messages": [user_id, assistant_id],
            "state": "complete", "startedAt": 1789532363000_i64,
        }));
        write(&conv_dir.join("index.json"), &index.to_string());
        write_message(&conv_dir, user_id, "user", request_id, text);
        write_message(&conv_dir, assistant_id, "assistant", request_id, "收到");
        std::fs::write(conv_dir.join("assets/新附件.txt"), b"new asset").unwrap();
    }

    /// 复制一次并登记关联，返回副本会话 id（登记失败即断言失败）。
    fn copy_and_register(fixture: &Fixture) -> String {
        let items = vec![CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_SRC.to_string(),
        }];
        let report =
            copy_sessions_in(&fixture.root, &fixture.backup, SRC_UID, DST_UID, &items).unwrap();
        let errors =
            register_copied_sessions(&fixture.root, &fixture.paths(), WbVariant::Cn, &report);
        assert!(errors.is_empty(), "登记失败：{errors:?}");
        report["copied"][0]["newId"].as_str().unwrap().to_string()
    }

    fn records(conv_dir: &Path) -> Vec<String> {
        let index = vscode_session::read_json(&conv_dir.join("index.json")).unwrap();
        vscode_session_link::message_ids_of(&index)
    }

    fn digests(conv_dir: &Path, conv_id: &str) -> Vec<String> {
        match vscode_session_link::read_session_content(conv_dir, conv_id) {
            ContentState::Ready(snapshot) => snapshot.normalized.line_digests,
            other => panic!("内容不可验证：{other:?}"),
        }
    }

    /// 目录快照：相对路径 → 文件字节（比较「逐字节一致」与增删）。
    fn dir_snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(root, &path, out);
                } else if path.is_file() {
                    let key = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .to_string();
                    out.insert(key, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(dir, dir, &mut out);
        out
    }

    fn selection(group_id: &str, token: &str, mode: SyncMode) -> SyncSelection {
        SyncSelection {
            group_id: group_id.to_string(),
            preview_token: token.to_string(),
            mode,
        }
    }

    /// `messages/` 下的文件名集合（含未被索引引用的「孤儿」文件）。
    fn message_files(conv_dir: &Path) -> BTreeSet<String> {
        std::fs::read_dir(conv_dir.join("messages"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect()
    }

    /// 整树拷贝目录（造现场 / 还原现场用；`from` 不存在时把 `to` 清空）。
    fn copy_tree(from: &Path, to: &Path) {
        vscode_session::remove_dir_all_if_exists(to);
        if from.is_dir() {
            vscode_session::copy_dir_recursive(from, to).unwrap();
        }
    }

    /// 反查目标会话的整目录备份：`<备份根>/<utc_iso>/<workspaceHash>/<会话 id>-overwrite`。
    ///
    /// 按目录名反查而不是重新拼 `utc_iso()`，避免断言与备份时刻跨秒时取到别的目录。
    fn find_overwrite_backup(fixture: &Fixture, conv_id: &str) -> PathBuf {
        let root = fixture.paths().backup_root().join(VSCODE_STORE.backup_kind);
        let mut pending = vec![root];
        let mut found: Vec<PathBuf> = Vec::new();
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name == format!("{conv_id}{OVERWRITE_SUFFIX}") {
                    found.push(path);
                } else {
                    pending.push(path);
                }
            }
        }
        assert_eq!(found.len(), 1, "应恰好有一份整目录备份：{found:?}");
        found.pop().unwrap()
    }

    /// 把文件 mtime 设成指定时刻（Windows 上 `File::set_modified` 需要 `FILE_WRITE_ATTRIBUTES`，
    /// 只读句柄会得到 `os error 5`，故用可写句柄打开——同 `token_stats::pin_mtime`）。
    fn set_mtime(path: &Path, time: SystemTime) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    /// 造「进程恰好在写完新增消息文件、更新索引之前被 kill」的现场（design §6.1 步骤 2→3 之间）。
    ///
    /// 先正常同步一次（产出派生 id 的消息文件），再把会话索引与关联存储回滚到操作前、
    /// 保留已落盘的消息文件；返回这批「已落盘但未被索引引用」的文件名。
    fn interrupted_index_write_scene(fixture: &Fixture, target_conv: &str) -> BTreeSet<String> {
        let target_dir = fixture.dst_conv_dir(target_conv);
        let index_path = target_dir.join("index.json");
        let bak_path = target_dir.join(".index_bak.json");
        let before_index = std::fs::read(&index_path).unwrap();
        let before_bak = std::fs::read(&bak_path).ok();
        let store_backup = fixture.base.join("store-before-sync");
        copy_tree(&fixture.store, &store_backup);

        // 第一次同步正常完成：目标里留下两条派生 id 的消息文件，索引与基线都已更新。
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward");
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();
        let first = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(first["synced"].as_array().unwrap().len(), 1, "{first}");
        assert_eq!(records(&target_dir).len(), 4, "造数：第一次同步成功");
        let written = message_files(&target_dir);
        assert_eq!(written.len(), 4, "造数：两条新增消息文件已落盘");

        // 回滚索引与关联存储（消息文件保留）→ 「上次中途失败」的现场。
        std::fs::write(&index_path, &before_index).unwrap();
        match &before_bak {
            Some(bytes) => std::fs::write(&bak_path, bytes).unwrap(),
            None => {
                let _ = std::fs::remove_file(&bak_path);
            }
        }
        copy_tree(&store_backup, &fixture.store);

        let tracked: BTreeSet<String> = records(&target_dir)
            .iter()
            .map(|id| format!("{id}.json"))
            .collect();
        assert_eq!(tracked.len(), 2, "造数：索引回到操作前");
        let orphans: BTreeSet<String> = written.difference(&tracked).cloned().collect();
        assert_eq!(orphans.len(), 2, "造数：两条新增文件已落盘但未被索引引用");
        orphans
    }

    /// AC1：复制成功后出现 1 个组、2 个 active 成员、1 条配对基线；重复复制不产生重复组。
    #[test]
    fn copy_registers_one_group_and_does_not_duplicate() {
        let fixture = Fixture::new("register");
        seed_source(&fixture);
        let first_copy = copy_and_register(&fixture);

        // 命名空间隔离：只写 VS Code 专属文件，WorkBuddy 的关联表不出现（AC6 的存储面）。
        assert!(fixture.store.join("vscode_session_links.json").is_file());
        assert!(!fixture.store.join("session_links.json").exists());
        assert!(fixture
            .store
            .join("vscode-session-links/baselines")
            .is_dir());
        assert!(!fixture.store.join("session-links").exists());

        let store = match session_link::load_store(&fixture.paths()) {
            StoreState::Ready(store) => store,
            other => panic!("关联表不可用：{other:?}"),
        };
        assert_eq!(store.groups.len(), 1);
        let group = &store.groups[0];
        assert_eq!(group.members.len(), 2);
        assert_eq!(group.pair_bases.len(), 1);
        assert!(group
            .members
            .iter()
            .all(|member| member.state == MemberState::Active));
        assert!(group.members.iter().any(|member| member.uid == SRC_UID));
        assert!(group.members.iter().any(|member| member.uid == DST_UID));

        // 再复制同一会话：仍然只有一个组（同账号旧的 active 成员转 superseded）。
        let second_copy = copy_and_register(&fixture);
        assert_ne!(first_copy, second_copy);
        let store = match session_link::load_store(&fixture.paths()) {
            StoreState::Ready(store) => store,
            other => panic!("关联表不可用：{other:?}"),
        };
        assert_eq!(store.groups.len(), 1, "重复复制不得产生第二个组");
        assert_eq!(store.groups[0].members.len(), 3);
        assert_eq!(
            store.groups[0]
                .members
                .iter()
                .filter(|member| member.state == MemberState::Active)
                .count(),
            2
        );
    }

    /// 无关联表 → storeStatus 为 missing（前端据此提示「先复制一次」）。
    #[test]
    fn preview_reports_missing_store() {
        let fixture = Fixture::new("missing-store");
        seed_source(&fixture);
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        assert_eq!(preview["storeStatus"], "missing");
        assert_eq!(preview["supported"], true);
        assert_eq!(preview["groups"].as_array().unwrap().len(), 0);
    }

    /// AC3：可快进 → 同步后副本记录数与源一致；重复执行不产生重复消息（幂等）。
    #[test]
    fn fast_forward_sync_is_idempotent() {
        let fixture = Fixture::new("fast-forward");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        assert_eq!(preview["storeStatus"], "ready");
        let groups = preview["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["verdict"], "fastForward");
        assert_eq!(groups[0]["defaultChecked"], true);
        assert_eq!(groups[0]["availableModes"], json!(["fastForward"]));
        assert_eq!(groups[0]["title"], "测试会话");
        assert_eq!(groups[0]["recordCount"]["source"], 4);
        assert_eq!(groups[0]["recordCount"]["target"], 2);
        assert_eq!(groups[0]["recordCount"]["baseline"], 2);
        let group_id = groups[0]["groupId"].as_str().unwrap().to_string();
        let token = groups[0]["previewToken"].as_str().unwrap().to_string();

        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["synced"][0]["recordCount"]["target"], 4);
        assert!(!report["synced"][0]["backup"].as_str().unwrap().is_empty());

        let target_dir = fixture.dst_conv_dir(&target_conv);
        let target_records = records(&target_dir);
        assert_eq!(target_records.len(), 4, "副本记录数必须与源一致");
        let unique: BTreeSet<&String> = target_records.iter().collect();
        assert_eq!(unique.len(), 4, "不得出现重复消息 id");
        // 派生 id 必须走真实写入路径（不是只在单测里算一遍）：新增两条的 id
        // 等于 `derive_message_id(目标 uid, 目标会话 id, 序号, 源摘要, salt=0)`。
        let source_digests = digests(&fixture.src_conv_dir(), CONV_SRC);
        assert_eq!(
            target_records[2],
            vscode_session_link::derive_message_id(DST_UID, &target_conv, 2, &source_digests[2], 0)
        );
        assert_eq!(
            target_records[3],
            vscode_session_link::derive_message_id(DST_UID, &target_conv, 3, &source_digests[3], 0)
        );
        // 最强断言：归一化后逐条摘要与源完全一致。
        assert_eq!(digests(&target_dir, &target_conv), source_digests);
        // 附件补入（源侧新增的附件出现在副本里）。
        assert!(target_dir.join("assets/新附件.txt").is_file());

        // 幂等：同一凭据再执行一次 → 跳过（previewStale，零新增），不产生重复消息。
        // 写入层 already_applied 正向路径见 `fast_forward_converges_after_interrupted_index_write`。
        let again = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(again["synced"].as_array().unwrap().len(), 0, "{again}");
        assert_eq!(again["skipped"].as_array().unwrap().len(), 1, "{again}");
        assert_eq!(again["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(records(&target_dir).len(), 4);

        // 之后重新预览：判定为内容一致且不可勾选、不发凭据。
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "identical");
        assert_eq!(group["availableModes"], json!([]));
        assert!(group.get("previewToken").is_none());
    }

    /// AC5：副本本地改动 → ahead；双方都改动 → diverge；覆盖后判定一致，且覆盖前有整目录备份。
    #[test]
    fn overwrite_converges_after_diverge_with_backup() {
        let fixture = Fixture::new("overwrite");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        let target_dir = fixture.dst_conv_dir(&target_conv);

        // 副本侧本地改动（改写最后一条助手消息）→ 只有目标变化。
        let assistant = target_dir.join(format!("messages/{}.json", records(&target_dir)[1]));
        let text = std::fs::read_to_string(&assistant)
            .unwrap()
            .replace("在的", "在的（副本本地改动）");
        std::fs::write(&assistant, text).unwrap();
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        assert_eq!(preview["groups"][0]["verdict"], "ahead");
        assert_eq!(preview["groups"][0]["availableModes"], json!([]));

        // 源侧也新增一轮 → 双方都有更新 → 分叉，需显式覆盖。
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "diverge");
        assert_eq!(group["defaultChecked"], false, "覆盖必须手动勾选");
        assert_eq!(group["availableModes"], json!(["overwrite"]));
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        let before = dir_snapshot(&target_dir);
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::Overwrite)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");

        // 覆盖前存在整目录备份，且内容与覆盖前逐字节一致。
        let backup = PathBuf::from(report["synced"][0]["backup"].as_str().unwrap());
        assert!(backup.is_dir(), "覆盖前必须有整目录备份");
        assert_eq!(dir_snapshot(&backup), before);

        // 覆盖后内容与源一致（保留目标会话 id）→ 判定变为内容一致。
        assert_eq!(records(&target_dir).len(), 4);
        assert_eq!(
            digests(&target_dir, &target_conv),
            digests(&fixture.src_conv_dir(), CONV_SRC)
        );
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        assert_eq!(preview["groups"][0]["verdict"], "identical");

        // 目标工作区索引未被改动（会话 id 保留，仍指向原会话）。
        let ws_index = vscode_session::read_json(
            &vscode_session::history_root(&fixture.root, DST_UID)
                .join(WS)
                .join("index.json"),
        )
        .unwrap();
        assert_eq!(ws_index["current"], target_conv);
    }

    /// AC5：覆盖重建失败 → 目标目录内容与操作前逐字节一致。
    #[test]
    fn overwrite_failure_keeps_target_intact() {
        let fixture = Fixture::new("overwrite-failure");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        let target_dir = fixture.dst_conv_dir(&target_conv);
        let assistant = target_dir.join(format!("messages/{}.json", records(&target_dir)[1]));
        let text = std::fs::read_to_string(&assistant)
            .unwrap()
            .replace("在的", "在的（副本本地改动）");
        std::fs::write(&assistant, text).unwrap();
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        // 让重建的临时目录位置被一个文件占住 → 重建必然失败。
        let tmp = target_dir.with_file_name(format!(".tmp-{target_conv}{OVERWRITE_SUFFIX}"));
        write(&tmp, "blocked");

        let before = dir_snapshot(&target_dir);
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::Overwrite)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 0, "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("重建副本会话目录失败"), "{error}");
        assert_eq!(
            dir_snapshot(&target_dir),
            before,
            "失败后目录必须与操作前一致"
        );
    }

    /// design §6.1 步 6：基线提交失败 → 恢复索引并删除本次新增的消息文件。
    #[test]
    fn baseline_commit_failure_rolls_back_index_and_new_files() {
        let fixture = Fixture::new("rollback-baseline");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        // 让关联存储锁无法建立（路径被目录占住）→ 提交基线必定失败。
        let lock = fixture.store.join("locks/vscode-session-links.lock");
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir_all(&lock).unwrap();

        let target_dir = fixture.dst_conv_dir(&target_conv);
        let before_index = std::fs::read(target_dir.join("index.json")).unwrap();
        let before_messages = dir_snapshot(&target_dir.join("messages"));
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 0, "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("同步记录"), "{error}");
        assert!(
            !error.contains("；且"),
            "回滚成功时用户可见文案必须与原始错误逐字一致：{error}"
        );
        assert_eq!(
            std::fs::read(target_dir.join("index.json")).unwrap(),
            before_index,
            "回滚后索引必须回到操作前状态"
        );
        assert_eq!(
            dir_snapshot(&target_dir.join("messages")),
            before_messages,
            "回滚后新增消息文件必须被删除"
        );
        // 附件是纯增量：回滚不涉及（design §6.1 步 6 只恢复索引 + 删本次新增文件）。
    }

    /// 写入中途失败 → 已写入的新增文件被删除、索引回滚（覆盖 design §6.1 步 6 的另一半）。
    #[test]
    fn write_failure_rolls_back_written_files() {
        let fixture = Fixture::new("rollback-write");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        // 第二个新增记录的落点被目录占住：第一个记录会先写入成功，第二个失败 → 必须回滚。
        let source_digests = digests(&fixture.src_conv_dir(), CONV_SRC);
        let blocked =
            vscode_session_link::derive_message_id(DST_UID, &target_conv, 3, &source_digests[3], 0);
        let target_dir = fixture.dst_conv_dir(&target_conv);
        std::fs::create_dir_all(target_dir.join(format!("messages/{blocked}.json"))).unwrap();

        let before = dir_snapshot(&target_dir);
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 0, "{report}");
        assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("写入副本消息失败"), "{error}");
        assert!(
            !error.contains("；且"),
            "回滚成功时用户可见文案必须与原始错误逐字一致：{error}"
        );
        assert_eq!(
            dir_snapshot(&target_dir),
            before,
            "已写入的新增消息文件必须被删除"
        );
    }

    /// design §6.2 步 5：重建与复算核验都成功、只有「提交基线」失败 → 目标整目录从备份恢复。
    ///
    /// 与 `overwrite_failure_keeps_target_intact`（重建就被挡住）互补：这里必须走
    /// `commit_baseline` 失败后的 `restore_dir` 分支，而不是重建失败分支。
    #[test]
    fn overwrite_baseline_commit_failure_restores_whole_dir() {
        let fixture = Fixture::new("overwrite-rollback-baseline");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        let target_dir = fixture.dst_conv_dir(&target_conv);

        // 副本侧本地改动 + 源侧新增一轮 → 分叉，需要显式覆盖。
        let assistant = target_dir.join(format!("messages/{}.json", records(&target_dir)[1]));
        let text = std::fs::read_to_string(&assistant)
            .unwrap()
            .replace("在的", "在的（副本本地改动）");
        std::fs::write(&assistant, text).unwrap();
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");
        // 会话级索引备份文件（实测形态）：重建不会产出它，整目录恢复必须把它带回来。
        let target_index = std::fs::read_to_string(target_dir.join("index.json")).unwrap();
        write(&target_dir.join(".index_bak.json"), &target_index);

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "diverge");
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        // 让关联存储锁无法建立（路径被目录占住）→ 只有「提交基线」这一步会失败。
        let lock = fixture.store.join("locks/vscode-session-links.lock");
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir_all(&lock).unwrap();

        // 断言范围取目标账号的整个数据根：内容逐字节一致 + 不留 `.tmp-*-overwrite` 残骸。
        let target_root = vscode_session::history_root(&fixture.root, DST_UID);
        let before_root = dir_snapshot(&target_root);
        let before_conv = dir_snapshot(&target_dir);
        let before_messages = dir_snapshot(&target_dir.join("messages"));
        assert_eq!(before_messages.len(), 2, "造数：操作前副本有 2 条消息文件");
        assert!(
            before_conv
                .keys()
                .any(|key| key.ends_with(".index_bak.json")),
            "造数：操作前副本已有索引备份文件"
        );

        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::Overwrite)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 0, "{report}");
        assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report}");
        let error = report["errors"][0]["error"].as_str().unwrap();
        assert!(
            error.contains("同步记录"),
            "失败必须来自提交基线（而非重建）：{error}"
        );

        assert_eq!(
            dir_snapshot(&target_root),
            before_root,
            "基线提交失败后目标目录必须整目录恢复（含 messages/ 与索引备份文件）"
        );
        assert_eq!(
            dir_snapshot(&target_dir.join("messages")),
            before_messages,
            "messages/ 必须与操作前逐字节一致"
        );
        assert_eq!(records(&target_dir).len(), 2, "索引必须回到操作前");
        let tmp = target_dir.with_file_name(format!(".tmp-{target_conv}{OVERWRITE_SUFFIX}"));
        assert!(!tmp.exists(), "重建用的临时目录不得残留：{tmp:?}");

        // 覆盖前的整目录备份必须保留（恢复的唯一依据），内容 = 操作前的目标会话目录。
        let backup = find_overwrite_backup(&fixture, &target_conv);
        assert_eq!(
            dir_snapshot(&backup),
            before_conv,
            "备份内容必须等于操作前的目标会话目录"
        );
    }

    /// design §6.1 相邻窗口：「文件已写、索引未改」被中断后重跑必须收敛。
    ///
    /// 造数：先正常同步一次（产出派生 id 的消息文件），再把会话索引与关联存储回滚到操作前，
    /// 但保留已写入的消息文件——等价于进程恰好在「写完新增文件、更新索引」之间被 kill。
    /// 重跑时 [`plan_message_file`] 必须走 `already_applied` 分支复用同名同内容文件，
    /// 而不是换 salt 再写一份（那会留下索引引用不到的孤儿文件）。
    #[test]
    fn fast_forward_converges_after_interrupted_index_write() {
        let fixture = Fixture::new("already-applied");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");
        let target_dir = fixture.dst_conv_dir(&target_conv);
        let orphans = interrupted_index_write_scene(&fixture, &target_conv);

        // 重跑（凭据必须重新取得）：目标内容仍等于旧基线 → 判定还是可快进。
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward");
        assert_eq!(
            group["recordCount"],
            json!({"source": 4, "target": 2, "baseline": 2})
        );
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");

        // 收敛：索引补齐这批记录、无重复、复用已落盘文件、无孤儿文件、摘要与源逐条一致。
        let after = records(&target_dir);
        assert_eq!(after.len(), 4, "索引必须补齐这批记录");
        assert_eq!(
            after.iter().collect::<BTreeSet<_>>().len(),
            4,
            "不得出现重复 id"
        );
        let source_digests = digests(&fixture.src_conv_dir(), CONV_SRC);
        assert_eq!(
            after[2],
            vscode_session_link::derive_message_id(DST_UID, &target_conv, 2, &source_digests[2], 0)
        );
        assert_eq!(
            after[3],
            vscode_session_link::derive_message_id(DST_UID, &target_conv, 3, &source_digests[3], 0)
        );
        let final_files = message_files(&target_dir);
        assert!(
            orphans.iter().all(|name| final_files.contains(name)),
            "已落盘的新增文件必须被复用（不得换 salt 重写）：{orphans:?}"
        );
        assert_eq!(
            final_files,
            after
                .iter()
                .map(|id| format!("{id}.json"))
                .collect::<BTreeSet<String>>(),
            "落盘文件必须与索引引用的消息一一对应（不产生多余的 id / 孤儿文件）"
        );
        assert_eq!(digests(&target_dir, &target_conv), source_digests);

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        assert_eq!(preview["groups"][0]["verdict"], "identical");
    }

    /// AC4：预览之后源侧再追加一条 → 旧凭据失效，该项按 `previewStale` 跳过且目标零写入。
    ///
    /// 与 `fast_forward_sync_is_idempotent`（同步成功后再用旧凭据）互补：这里的变化发生在
    /// **源侧**、且发生在预览与执行之间（用户在确认切换前又聊了一轮）。
    #[test]
    fn sync_skips_when_source_appended_after_preview() {
        let fixture = Fixture::new("stale-after-append");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward");
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        // 预览之后源侧再追加一轮 → 源内容与预览时不一致（目标侧没有任何变化）。
        append_round(&fixture, MSG_5, MSG_6, REQ_3, "预览之后又来一条");

        let target_dir = fixture.dst_conv_dir(&target_conv);
        let before = dir_snapshot(&target_dir);
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 0, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
        assert_eq!(report["skipped"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(report["skipped"][0]["reasonCode"], REASON_PREVIEW_STALE);
        assert_eq!(report["skipped"][0]["verdict"], "fastForward");
        assert_eq!(records(&target_dir).len(), 2, "目标记录数不得变化");
        assert_eq!(
            dir_snapshot(&target_dir),
            before,
            "目标目录必须完全未被写入"
        );

        // 失效只是「需要重新检查」而不是死局：重新预览仍是可快进，同步后与源逐条一致。
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward");
        assert_eq!(group["recordCount"]["source"], 6);
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(records(&target_dir).len(), 6);
        assert_eq!(
            digests(&target_dir, &target_conv),
            digests(&fixture.src_conv_dir(), CONV_SRC)
        );
    }

    /// AC4 的另一半：预览之后**副本侧**再被改 → 旧凭据失效，两种模式都被跳过且目标零写入。
    ///
    /// 与 `sync_skips_when_source_appended_after_preview`（变化在源侧）互补，对齐 WorkBuddy 侧
    /// `sync_skips_when_target_changed_after_preview_even_with_overwrite`：显式覆盖也不能绕过
    /// 版本校验，更不得据此回写目标。
    #[test]
    fn sync_skips_when_target_changed_after_preview_even_with_overwrite() {
        let fixture = Fixture::new("stale-target");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        let target_dir = fixture.dst_conv_dir(&target_conv);

        // 副本本地改动 + 源侧新增一轮 → 分叉：预览会发放覆盖凭据（该项可勾选）。
        let assistant = target_dir.join(format!("messages/{}.json", records(&target_dir)[1]));
        let text = std::fs::read_to_string(&assistant)
            .unwrap()
            .replace("在的", "在的（副本本地改动）");
        std::fs::write(&assistant, text).unwrap();
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "diverge");
        assert_eq!(group["availableModes"], json!(["overwrite"]));
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();

        // 预览之后副本侧又被改（用户在确认切换前又动过副本）→ 与凭据里的目标内容不一致。
        let user_message = target_dir.join(format!("messages/{}.json", records(&target_dir)[0]));
        let text = std::fs::read_to_string(&user_message)
            .unwrap()
            .replace("你好", "你好（预览之后又改）");
        std::fs::write(&user_message, text).unwrap();

        let before = dir_snapshot(&target_dir);
        for mode in [SyncMode::FastForward, SyncMode::Overwrite] {
            let report = sync_selected_at(
                &fixture.root,
                &fixture.paths(),
                SRC_UID,
                &target_acc(),
                &[selection(&group_id, &token, mode)],
            )
            .unwrap();
            assert_eq!(report["synced"].as_array().unwrap().len(), 0, "{report}");
            assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");
            assert_eq!(report["skipped"].as_array().unwrap().len(), 1, "{report}");
            let skipped = &report["skipped"][0];
            assert_eq!(skipped["reasonCode"], REASON_PREVIEW_STALE, "{report}");
            assert_eq!(skipped["verdict"], "diverge", "{report}");
            assert!(
                skipped["message"]
                    .as_str()
                    .unwrap()
                    .contains("目标账号的内容已变化"),
                "{skipped}"
            );
        }
        assert_eq!(
            dir_snapshot(&target_dir),
            before,
            "被跳过时目标目录必须完全未被写入"
        );
        assert_eq!(records(&target_dir).len(), 2, "目标记录数不得变化");
    }

    /// design §6.1 步 6 / §9：「索引已改、基线未提交」的窗口（进程被 kill、没走回滚）
    /// → 下次判定内容一致，且不需要任何修复动作。
    #[test]
    fn index_written_before_baseline_commit_settles_identical() {
        let fixture = Fixture::new("index-ahead-of-baseline");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");
        let target_dir = fixture.dst_conv_dir(&target_conv);
        let store_backup = fixture.base.join("store-before-sync");
        copy_tree(&fixture.store, &store_backup);

        // 正常同步一次：索引补齐到 4 条、基线同步提交。
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward");
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert_eq!(records(&target_dir).len(), 4, "造数：第一次同步成功");

        // 只把关联存储回滚到同步前：索引已含新记录、基线还是旧的 2 条
        // ——等价于「索引改完、基线提交前进程被 kill」，且没有任何回滚动作。
        copy_tree(&store_backup, &fixture.store);
        let store_state = dir_snapshot(&fixture.store);

        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(
            group["verdict"], "identical",
            "双方摘要已相等（decide_sync 先比摘要再看基线）：{group}"
        );
        assert_eq!(group["availableModes"], json!([]), "无需动作：不可勾选");
        assert_eq!(group["defaultChecked"], false);
        assert!(group.get("previewToken").is_none(), "不可勾选项不发凭据");
        assert_eq!(
            group["recordCount"],
            json!({"source": 4, "target": 4, "baseline": 2}),
            "基线仍是旧的 2 条，但判定与它无关"
        );
        // 无需任何修复动作：预览是只读的，不会去「补」那条落后的基线。
        assert_eq!(
            dir_snapshot(&fixture.store),
            store_state,
            "预览不得改动关联存储"
        );
        assert_eq!(records(&target_dir).len(), 4);
        assert_eq!(
            digests(&target_dir, &target_conv),
            digests(&fixture.src_conv_dir(), CONV_SRC)
        );
    }

    /// `already_applied` 的另一半：重跑时**不重写**已存在的消息文件
    /// （`fast_forward_converges_after_interrupted_index_write` 只锁了「不换 salt / 不留孤儿」）。
    ///
    /// 判据用 mtime 而不是 `sleep`：先把两条已落盘文件的 mtime 设成 2000 年，重写会让它变成
    /// 当前时间 —— 与文件系统时钟粒度无关，三平台一致，也不引入等待。
    #[test]
    fn already_applied_does_not_rewrite_existing_message_files() {
        let fixture = Fixture::new("already-applied-mtime");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");
        let target_dir = fixture.dst_conv_dir(&target_conv);
        let orphans = interrupted_index_write_scene(&fixture, &target_conv);

        let landed: Vec<PathBuf> = orphans
            .iter()
            .map(|name| target_dir.join("messages").join(name))
            .collect();
        let old = UNIX_EPOCH + Duration::from_secs(946_684_800); // 2000-01-01
        for path in &landed {
            set_mtime(path, old);
        }

        // 重跑：目标内容仍等于旧基线 → 判定可快进，两条新增文件命中 already_applied。
        let preview =
            links_preview_at(&fixture.root, &fixture.paths(), SRC_UID, &target_acc()).unwrap();
        let group = &preview["groups"][0];
        assert_eq!(group["verdict"], "fastForward");
        let group_id = group["groupId"].as_str().unwrap().to_string();
        let token = group["previewToken"].as_str().unwrap().to_string();
        let report = sync_selected_at(
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(&group_id, &token, SyncMode::FastForward)],
        )
        .unwrap();
        assert_eq!(report["synced"].as_array().unwrap().len(), 1, "{report}");
        assert!(report["errors"].as_array().unwrap().is_empty(), "{report}");

        // 复用而不是重写：mtime 仍是 2000 年（重写会变成当前时间）。
        for path in &landed {
            let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
            assert!(
                mtime < UNIX_EPOCH + Duration::from_secs(1_600_000_000),
                "已存在的消息文件不得被重写：{path:?} 的 mtime 变成了 {mtime:?}"
            );
        }
        assert_eq!(records(&target_dir).len(), 4);
    }

    /// 覆盖模式回滚失败：`restore_dir` 必须上报失败并给出备份路径，调用点的文案不得声称已回滚。
    ///
    /// 造数不依赖平台语义：用一个不存在的备份目录（「备份读不到」是恢复失败的唯一原因，
    /// 与 chmod / 只读句柄这类平台差异无关）。
    #[test]
    fn restore_dir_reports_failure_with_backup_path() {
        let fixture = Fixture::new("restore-failure");
        let target_dir = fixture.base.join("target-conv");
        write(&target_dir.join("index.json"), "{}");
        let missing_backup = fixture.base.join("missing-backup");

        let error = restore_dir(&missing_backup, &target_dir).unwrap_err();
        assert!(
            error.contains("未恢复"),
            "文案必须明说副本目录未恢复：{error}"
        );
        assert!(
            error.contains(&missing_backup.to_string_lossy().to_string()),
            "文案必须给出可手工恢复的备份路径：{error}"
        );
        // 失败的真实后果：目标目录已先被清空，只剩 `copy_dir_recursive` 建出的空目录。
        assert!(
            target_dir.is_dir() && dir_snapshot(&target_dir).is_empty(),
            "恢复失败后副本目录是空壳，所以提示不能写「已回滚」"
        );

        // 调用点合成文案：恢复成功时与原来的文案逐字一致；失败时追加说明、不掩盖。
        let success = "重建后的内容与来源不一致，已整目录回滚";
        assert_eq!(rollback_message(Ok(()), success), success);
        let composed = rollback_message(Err(error.clone()), success);
        assert!(composed.contains(success), "{composed}");
        assert!(composed.contains("未恢复"), "{composed}");
        assert!(
            composed.contains(&missing_backup.to_string_lossy().to_string()),
            "{composed}"
        );
    }

    /// 只为 `rollback_index` 造一个计划：其余字段填空，仅 `target_dir` 指向测试给定路径。
    fn plan_pointing_at(target_dir: &Path) -> SyncItemPlan {
        let member = |uid: &str| LinkMember {
            member_id: format!("{uid}:{CONV_SRC}"),
            account_id: Some(uid.to_string()),
            uid: uid.to_string(),
            session_id: CONV_SRC.to_string(),
            state: MemberState::Active,
            linked_at: 0,
            last_synced_at: None,
        };
        let snapshot = ContentSnapshot {
            text: String::new(),
            full_digest: String::new(),
            normalized: crate::modules::session_link::NormalizedContent {
                record_count: 0,
                line_digests: Vec::new(),
                total_digest: String::new(),
            },
        };
        SyncItemPlan {
            group_id: "group-under-test".to_string(),
            mode: SyncMode::FastForward,
            verdict: SyncVerdict::FastForward,
            source_member: member(SRC_UID),
            target_member: member(DST_UID),
            source_dir: PathBuf::new(),
            target_dir: target_dir.to_path_buf(),
            workspace_hash: WS.to_string(),
            source_conversation_id: CONV_SRC.to_string(),
            target_conversation_id: CONV_SRC.to_string(),
            target_uid: DST_UID.to_string(),
            backup_kind: VSCODE_STORE.backup_kind,
            source_content: snapshot.clone(),
            target_content: snapshot,
        }
    }

    /// 快进回滚失败必须上报：`rollback_index` 返回失败说明（含备份路径），调用点据此追加提示。
    ///
    /// 造数不依赖平台语义：把目标路径做成一个**普通文件**（不是目录），恢复索引时的 `copy`
    /// 在任何平台都无法在文件下建出子路径 → 必败；与 chmod / 只读句柄 / `set_readonly`
    /// 这类跨平台语义不同的手段无关。
    #[test]
    fn rollback_index_reports_failure_with_backup_path() {
        let fixture = Fixture::new("rollback-index-failure");
        let backup_dir = fixture.base.join("index-backup");
        write(&backup_dir.join("index.json"), "{\"messages\":[]}");
        let blocked = fixture.base.join("target-as-file");
        write(&blocked, "不是目录");
        let plan = plan_pointing_at(&blocked);
        let created = vec![blocked.join("messages/never-written.json")];

        let error = rollback_index(&plan, &backup_dir, &created).unwrap_err();
        assert!(error.contains("索引 index.json 未恢复"), "{error}");
        assert!(
            error.contains(&backup_dir.to_string_lossy().to_string()),
            "文案必须给出可手工恢复的备份路径：{error}"
        );

        // 成功路径一：目标与备份都没有索引文件 → 幂等删除（NotFound）不算失败。
        let clean = fixture.base.join("clean-conv");
        std::fs::create_dir_all(&clean).unwrap();
        let empty_backup = fixture.base.join("empty-backup");
        std::fs::create_dir_all(&empty_backup).unwrap();
        let clean_plan = plan_pointing_at(&clean);
        let rolled_back = rollback_index(&clean_plan, &empty_backup, &[]);
        assert!(rolled_back.is_ok(), "文件本就不在不算失败：{rolled_back:?}");

        // 成功路径二：调用点合成文案时，用户可见文案必须逐字不变。
        let original = "写入副本消息失败：磁盘空间不足";
        assert_eq!(
            rollback_message(rollback_index(&clean_plan, &empty_backup, &[]), original),
            original
        );
        let composed = rollback_message(Err(error), original);
        assert!(composed.contains(original), "{composed}");
        assert!(
            composed.contains(&backup_dir.to_string_lossy().to_string()),
            "{composed}"
        );
    }
}
