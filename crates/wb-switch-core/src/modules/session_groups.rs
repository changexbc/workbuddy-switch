//! Client-scoped session group directory and explicit-member operations.
//!
//! This module is the group-management boundary used by Tauri and WebUI. It reads the
//! existing namespace stores and delegates every write to the client-specific session
//! kernels; it never resolves an operation source from a client's active login state.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::modules::session::{SessionPaths, SyncSelection};
use crate::modules::session_link::{
    ContentState, LinkGroup, LinkMember, MemberState, StoreState, SyncVerdict,
};
use crate::modules::vscode_session::{
    CopyItem, SessionStoreSpec, CODEBUDDY_IDE_STORE, VSCODE_STORE,
};
use crate::modules::{
    account, codebuddy_ide_session, codebuddy_ide_session_sync, config, i18n, process, session,
    session_link, variant::WbVariant, vscode_ext, vscode_session, vscode_session_sync,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionClient {
    Workbuddy,
    CodebuddyIde,
    VscodeExt,
}

impl SessionClient {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "workbuddy" => Ok(Self::Workbuddy),
            "codebuddyIde" => Ok(Self::CodebuddyIde),
            "vscodeExt" => Ok(Self::VscodeExt),
            _ => Err(i18n::tr(
                "Unsupported session client",
                "지원하지 않는 세션 클라이언트입니다",
            )
            .to_string()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workbuddy => "workbuddy",
            Self::CodebuddyIde => "codebuddyIde",
            Self::VscodeExt => "vscodeExt",
        }
    }

    fn store_paths(self) -> SessionPaths {
        match self {
            Self::Workbuddy => SessionPaths::for_variant(WbVariant::Cn),
            Self::CodebuddyIde => SessionPaths::for_codebuddy_ide(),
            Self::VscodeExt => SessionPaths::for_vscode_ext(),
        }
    }

    fn data_root(self) -> Option<PathBuf> {
        match self {
            Self::Workbuddy => None,
            Self::CodebuddyIde => codebuddy_ide_session::ide_data_root(),
            Self::VscodeExt => vscode_session::ext_data_root(),
        }
    }

    fn store_spec(self) -> Option<SessionStoreSpec> {
        match self {
            Self::Workbuddy => None,
            Self::CodebuddyIde => Some(CODEBUDDY_IDE_STORE),
            Self::VscodeExt => Some(VSCODE_STORE),
        }
    }

    fn validate_scope(self, scope: Option<WbVariant>) -> Result<(), String> {
        match (self, scope) {
            (Self::CodebuddyIde, Some(_)) | (Self::Workbuddy | Self::VscodeExt, None) => Ok(()),
            (Self::CodebuddyIde, None) => Err(i18n::tr(
                "CodeBuddy IDE requests must specify the cn or ai edition",
                "CodeBuddy IDE 요청에는 cn 또는 ai 버전을 지정해야 합니다",
            )
            .to_string()),
            (Self::Workbuddy | Self::VscodeExt, Some(_)) => Err(i18n::tr(
                "Only CodeBuddy IDE supports edition filtering",
                "CodeBuddy IDE에서만 버전 필터를 지원합니다",
            )
            .to_string()),
        }
    }
}

pub fn parse_variant_scope(raw: Option<&str>) -> Result<Option<WbVariant>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some("cn") => Ok(Some(WbVariant::Cn)),
        Some("ai") => Ok(Some(WbVariant::Ai)),
        Some(_) => Err(i18n::tr(
            "variantScope only supports cn or ai",
            "variantScope는 cn 또는 ai만 지원합니다",
        )
        .to_string()),
    }
}

#[derive(Debug, Clone)]
struct MemberView {
    member: LinkMember,
    account_key: String,
    account_name: String,
    title: String,
    project_label: String,
    updated_at: i64,
    content: ContentState,
    reason: String,
    version_status: &'static str,
}

/// List lightweight searchable summaries. IDE must include its mandatory variant scope.
pub fn list(client: SessionClient, scope: Option<WbVariant>) -> Result<Value, String> {
    client.validate_scope(scope)?;
    let paths = client.store_paths();
    let mut result = json!({
        "client": client.as_str(),
        "variantScope": scope.map(WbVariant::as_str),
        "storeStatus": "missing",
        "groups": [],
    });
    match session_link::load_store(&paths) {
        StoreState::Missing => {}
        StoreState::Unavailable(reason) => {
            result["storeStatus"] = json!("unavailable");
            result["storeError"] = json!(reason);
        }
        StoreState::Ready(store) => {
            result["storeStatus"] = json!("ready");
            let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
            let mut cache = HashMap::new();
            let groups = store
                .groups
                .iter()
                .filter(|group| group_is_in_scope(client, scope, group))
                .map(|group| {
                    group_payload(client, scope, &paths, group, &accounts, &mut cache, false)
                })
                .collect::<Vec<_>>();
            result["groups"] = json!(groups);
        }
    }
    Ok(result)
}

/// Load one group and all saved members, including stale/superseded entries.
pub fn detail(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
) -> Result<Value, String> {
    client.validate_scope(scope)?;
    let paths = client.store_paths();
    let store = match session_link::load_store(&paths) {
        StoreState::Ready(store) => store,
        StoreState::Missing => {
            return Err(i18n::tr(
                "Session group does not exist",
                "세션 연결 그룹이 존재하지 않습니다",
            )
            .to_string())
        }
        StoreState::Unavailable(reason) => {
            return Err(i18n::pick(
                format!("Session group is temporarily unavailable: {reason}"),
                format!("세션 연결 그룹을 일시적으로 사용할 수 없습니다: {reason}"),
            ))
        }
    };
    let group = store
        .groups
        .iter()
        .find(|group| group.id == group_id && group_is_in_scope(client, scope, group))
        .ok_or_else(|| {
            i18n::tr(
                "Session group does not exist or does not belong to the current client",
                "세션 연결 그룹이 존재하지 않거나 현재 클라이언트의 그룹이 아닙니다",
            )
            .to_string()
        })?;
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    let mut cache = HashMap::new();
    Ok(group_payload(
        client, scope, &paths, group, &accounts, &mut cache, true,
    ))
}

/// Preview one exact source/target member pair using the existing per-client preview-token kernel.
pub fn preview_pair(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    target_member_id: &str,
) -> Result<Value, String> {
    let context = resolve_pair(client, scope, group_id, source_member_id, target_member_id)?;
    preview_resolved_pair(client, scope, &context)
}

/// Sync one explicit pair. The existing kernel revalidates the namespace/group/member/content token.
#[allow(clippy::too_many_arguments)] // 参数都是本次同步的显式输入（与 rotate.rs 同口径）
pub fn sync_pair(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    target_member_id: &str,
    preview_token: &str,
    mode: &str,
    restart: bool,
) -> Result<Value, String> {
    let context = resolve_pair(client, scope, group_id, source_member_id, target_member_id)?;
    if preview_token.trim().is_empty() {
        return Err(i18n::tr(
            "Missing session sync preview credential",
            "세션 동기화 미리보기 자격 증명이 없습니다",
        )
        .to_string());
    }
    let selection = parse_selection(group_id, preview_token, mode)?;
    // 校验（resolve / 凭据 / 模式）全部通过后再关编辑器：为注定失败的请求打断用户是无谓的。
    let guard = prepare_editor_window(
        client,
        restart,
        vscode_sync_running_hint(),
        &[context.target.uid.as_str()],
    )?;
    let result = run_pair_sync(client, scope, &context, &[selection], guard.is_some());
    settle_editor_window(guard, result)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupUnifyTarget {
    pub target_member_id: String,
    pub preview_token: String,
    pub mode: String,
}

/// Apply an explicit group choice in one lifecycle window. WorkBuddy closes only a running
/// variant whose current login is a write target; the VS Code plugin closes the editor when the
/// caller was authorized (`restart`). Only clients closed here are reopened. Pair kernels still
/// validate every preview token.
pub fn sync_unify_batch(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    targets: &[GroupUnifyTarget],
    restart: bool,
) -> Result<Value, String> {
    match client {
        SessionClient::Workbuddy => {
            sync_unify_batch_workbuddy(scope, group_id, source_member_id, targets)
        }
        SessionClient::VscodeExt => {
            sync_unify_batch_vscode(scope, group_id, source_member_id, targets, restart)
        }
        SessionClient::CodebuddyIde => Err(i18n::tr(
            "Only WorkBuddy and CodeBuddy plugin sessions support group-wide unification",
            "WorkBuddy 및 CodeBuddy 플러그인 세션에서만 그룹 전체 통합을 지원합니다",
        )
        .to_string()),
    }
}

/// Resolve + validate every target before anything is closed or written.
fn prepare_unify_targets(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    targets: &[GroupUnifyTarget],
) -> Result<Vec<(PairContext, SyncSelection)>, String> {
    if targets.is_empty() {
        return Err(i18n::tr(
            "There are no target copies to sync",
            "동기화할 대상 복사본이 없습니다",
        )
        .to_string());
    }
    let mut seen = HashSet::new();
    let mut prepared = Vec::with_capacity(targets.len());
    for target in targets {
        if !seen.insert(target.target_member_id.as_str()) {
            return Err(i18n::tr(
                "Duplicate target copies; please re-check the session",
                "대상 복사본이 중복되었습니다. 세션을 다시 확인해 주세요",
            )
            .to_string());
        }
        let pair = resolve_pair(
            client,
            scope,
            group_id,
            source_member_id,
            &target.target_member_id,
        )?;
        let selection = parse_selection(group_id, &target.preview_token, &target.mode)?;
        prepared.push((pair, selection));
    }
    Ok(prepared)
}

/// WorkBuddy：只关闭「运行中且当前登录是写入目标」的档位，只重开本次关闭的档位。
fn sync_unify_batch_workbuddy(
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    targets: &[GroupUnifyTarget],
) -> Result<Value, String> {
    let client = SessionClient::Workbuddy;
    let prepared = prepare_unify_targets(client, scope, group_id, source_member_id, targets)?;

    let restart_variants = WbVariant::ALL
        .into_iter()
        .filter(|variant| {
            prepared.iter().any(|(pair, _)| {
                account::variant_of(&pair.target_account) == *variant
                    && running_target_needs_restart(*variant, &pair.target.uid)
            })
        })
        .collect::<Vec<_>>();
    let mut closed = Vec::new();
    for variant in restart_variants {
        if let Err(error) = process::close_workbuddy(variant, 20) {
            let reopen_errors = reopen_workbuddy_variants(&closed);
            let suffix = if reopen_errors.is_empty() {
                String::new()
            } else {
                i18n::pick(
                    format!(
                        "; previously closed client failed to reopen: {}",
                        reopen_errors.join(i18n::tr("; ", "；"))
                    ),
                    format!(
                        "；이전에 닫은 클라이언트를 다시 열지 못했습니다: {}",
                        reopen_errors.join(i18n::tr("; ", "；"))
                    ),
                )
            };
            return Err(i18n::pick(
                format!(
                    "Failed to close {}: {error}{suffix}",
                    workbuddy_variant_label(variant)
                ),
                format!(
                    "{} 닫기 실패: {error}{suffix}",
                    workbuddy_variant_label(variant)
                ),
            ));
        }
        closed.push(variant);
    }

    let mut combined = json!({ "client": client.as_str(), "groupId": group_id, "synced": [], "skipped": [], "errors": [], "needsRecovery": false, "temporaryFiles": [], "restartedVariants": [] });
    for (pair, selection) in prepared {
        match run_pair_sync(client, scope, &pair, &[selection], false) {
            Ok(report) => {
                merge_sync_report(&mut combined, report);
                if combined["needsRecovery"] == true {
                    break;
                }
            }
            Err(error) => {
                combined["errors"].as_array_mut().unwrap().push(json!({
                    "error": format!("{}：{error}", pair.target.uid),
                }));
                break;
            }
        }
    }

    if combined["needsRecovery"] == true {
        combined["errors"].as_array_mut().unwrap().push(json!({
            "error": i18n::tr("Session write pending recovery; client reopen is paused. Please handle the recovery prompt first", "세션 쓰기 복구가 필요하여 클라이언트 다시 열기를 중지했습니다. 먼저 복구 안내를 처리해 주세요"),
        }));
        return Ok(combined);
    }
    for variant in &closed {
        match session::recover_pending_session_operations(*variant) {
            Ok(recovery) if !crate::modules::switch::recovery_blocks_startup(&recovery) => {}
            Ok(recovery) => {
                combined["needsRecovery"] = json!(true);
                combined["errors"].as_array_mut().unwrap().push(json!({
                    "error": i18n::pick(format!("{} still has session writes pending recovery ({}); client reopen is paused", workbuddy_variant_label(*variant), crate::modules::switch::recovery_blocking_detail(&recovery)), format!("{}에 복구가 필요한 세션 쓰기가 남아 있어({}) 클라이언트 다시 열기를 중지했습니다", workbuddy_variant_label(*variant), crate::modules::switch::recovery_blocking_detail(&recovery))),
                }));
            }
            Err(error) => {
                combined["needsRecovery"] = json!(true);
                combined["errors"].as_array_mut().unwrap().push(json!({
                    "error": i18n::pick(format!("Could not check session recovery status for {} ({error}); client reopen is paused", workbuddy_variant_label(*variant)), format!("{}의 세션 복구 상태를 확인할 수 없어({error}) 클라이언트 다시 열기를 중지했습니다", workbuddy_variant_label(*variant))),
                }));
            }
        }
    }
    if combined["needsRecovery"] == true {
        return Ok(combined);
    }
    let mut reopened = Vec::new();
    for variant in closed {
        if let Err(error) = process::launch_workbuddy(variant, None) {
            combined["errors"].as_array_mut().unwrap().push(json!({
                "error": i18n::pick(format!("{} sessions were processed, but the client failed to reopen: {error}", workbuddy_variant_label(variant)), format!("{} 세션은 처리했지만 클라이언트를 다시 열지 못했습니다: {error}", workbuddy_variant_label(variant))),
            }));
        } else {
            reopened.push(variant.as_str());
        }
    }
    combined["restartedVariants"] = json!(reopened);
    Ok(combined)
}

/// VS Code 插件：全部校验通过后打开编辑器生命周期窗口（`restart=true` 时关闭），
/// 窗口内跑完全部目标再收尾重开；插件侧没有 WorkBuddy 的 `needsRecovery` 语义。
fn sync_unify_batch_vscode(
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    targets: &[GroupUnifyTarget],
    restart: bool,
) -> Result<Value, String> {
    let client = SessionClient::VscodeExt;
    let prepared = prepare_unify_targets(client, scope, group_id, source_member_id, targets)?;
    let target_uids: Vec<&str> = prepared
        .iter()
        .map(|(pair, _)| pair.target.uid.as_str())
        .collect();
    let guard = prepare_editor_window(client, restart, vscode_sync_running_hint(), &target_uids)?;
    let editor_prepared = guard.is_some();
    let mut combined = json!({ "client": client.as_str(), "groupId": group_id, "synced": [], "skipped": [], "errors": [], "needsRecovery": false, "temporaryFiles": [] });
    for (pair, selection) in prepared {
        match run_pair_sync(client, scope, &pair, &[selection], editor_prepared) {
            Ok(report) => merge_sync_report(&mut combined, report),
            Err(error) => {
                combined["errors"].as_array_mut().unwrap().push(json!({
                    "error": format!("{}：{error}", pair.target.uid),
                }));
                break;
            }
        }
    }
    settle_editor_window(guard, Ok(combined))
}

fn running_target_needs_restart(variant: WbVariant, target_uid: &str) -> bool {
    should_restart_running_target(
        process::is_workbuddy_running(variant),
        session::current_user_uid(variant).as_deref(),
        target_uid,
    )
}

fn should_restart_running_target(
    running: bool,
    current_uid: Option<&str>,
    target_uid: &str,
) -> bool {
    running && current_uid.is_none_or(|uid| uid == target_uid)
}

fn workbuddy_variant_label(variant: WbVariant) -> &'static str {
    match variant {
        WbVariant::Cn => i18n::tr("WorkBuddy Domestic", "WorkBuddy 국내판"),
        WbVariant::Ai => i18n::tr("WorkBuddy International", "WorkBuddy 국제판"),
    }
}

fn reopen_workbuddy_variants(variants: &[WbVariant]) -> Vec<String> {
    variants
        .iter()
        .filter_map(|variant| {
            process::launch_workbuddy(*variant, None)
                .err()
                .map(|error| format!("{}：{error}", workbuddy_variant_label(*variant)))
        })
        .collect()
}

/// Recompute the group source on the server and sync only targets proven fast-forward safe.
pub fn sync_safe_batch(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    restart: bool,
) -> Result<Value, String> {
    let detail = detail(client, scope, group_id)?;
    let source_member_id = detail
        .get("safeSourceMemberId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            i18n::tr(
                "Cannot determine a safe sync source for this group",
                "이 그룹의 안전한 동기화 원본을 확인할 수 없습니다",
            )
            .to_string()
        })?
        .to_string();
    let targets = detail
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|member| {
            member.get("memberId").and_then(Value::as_str) != Some(source_member_id.as_str())
                && member.get("versionStatus").and_then(Value::as_str) == Some("behind")
        })
        .filter_map(|member| {
            member
                .get("memberId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    let mut combined = json!({ "client": client.as_str(), "groupId": group_id, "synced": [], "skipped": [], "errors": [], "needsRecovery": false, "temporaryFiles": [] });
    // 准备阶段：全部目标的复核与凭据都在关闭编辑器之前完成（复核失败/不再安全快进的项照旧跳过），
    // 避免为注定失败的请求打断用户。
    let mut prepared = Vec::new();
    for target_member_id in targets {
        let pair = resolve_pair(
            client,
            scope,
            group_id,
            &source_member_id,
            &target_member_id,
        )?;
        let preview = preview_resolved_pair(client, scope, &pair)?;
        if preview.get("verdict").and_then(Value::as_str) != Some("fastForward") {
            combined["skipped"].as_array_mut().unwrap().push(json!({
                "groupId": group_id,
                "targetMemberId": target_member_id,
                "reasonCode": "recheckNotFastForward",
                "message": i18n::tr("After re-checking, this copy is no longer in the safe fast-forward range", "다시 확인한 결과 이 복사본은 더 이상 안전한 빨리 감기 범위에 속하지 않습니다"),
            }));
            continue;
        }
        let token = preview
            .get("previewToken")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                i18n::tr(
                    "Re-preview did not return an executable credential",
                    "다시 미리보기에서 실행 가능한 자격 증명을 반환하지 않았습니다",
                )
                .to_string()
            })?;
        let selection = parse_selection(group_id, token, "fastForward")?;
        prepared.push((pair, selection));
    }
    let target_uids: Vec<&str> = prepared
        .iter()
        .map(|(pair, _)| pair.target.uid.as_str())
        .collect();
    let guard = prepare_editor_window(client, restart, vscode_sync_running_hint(), &target_uids)?;
    let editor_prepared = guard.is_some();
    let result = (|| -> Result<Value, String> {
        for (pair, selection) in prepared {
            let report = run_pair_sync(client, scope, &pair, &[selection], editor_prepared)?;
            merge_sync_report(&mut combined, report);
        }
        Ok(combined)
    })();
    settle_editor_window(guard, result)
}

/// 插件侧「复制 + 登记」共享段：复制给定会话到目标账号，并登记关联（无现成组则新建关联组）。
///
/// 供 `add_member`（往已有组加成员）与 `copy_linked_sessions`（从零复制并建组）共用；
/// 只做复制与登记，不做运行门禁与收尾——生命周期窗口由调用方负责。
fn plugin_copy_and_register(
    spec: SessionStoreSpec,
    root: &Path,
    paths: &SessionPaths,
    variant: WbVariant,
    source_uid: &str,
    target_uid: &str,
    items: &[CopyItem],
) -> Result<(Value, Vec<Value>), String> {
    let backup_root = paths
        .backup_root()
        .join(spec.backup_kind)
        .join(config::utc_iso());
    let report =
        vscode_session::copy_sessions_in(root, &backup_root, source_uid, target_uid, items)?;
    let link_errors = vscode_session_sync::register_copied_sessions_in(
        VSCODE_STORE,
        root,
        paths,
        variant,
        &report,
    );
    Ok((report, link_errors))
}

/// Copy one member into a compatible saved account and register it in the same namespace.
pub fn add_member(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    target_account_id: &str,
    restart: bool,
) -> Result<Value, String> {
    client.validate_scope(scope)?;
    let paths = client.store_paths();
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    let target = accounts
        .iter()
        .find(|account| account.get("id").and_then(Value::as_str) == Some(target_account_id))
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Target account does not exist",
                "대상 계정이 존재하지 않습니다",
            )
            .to_string()
        })?;
    let target_uid = account::get_str(&target, "uid")
        .map(|uid| uid.trim().to_string())
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| {
            i18n::tr(
                "Target account is missing uid",
                "대상 계정에 uid가 없습니다",
            )
            .to_string()
        })?;
    validate_target_compatibility(client, scope, &target)?;

    let store = match session_link::load_store(&paths) {
        StoreState::Ready(store) => store,
        StoreState::Missing => {
            return Err(i18n::tr(
                "Session group does not exist",
                "세션 연결 그룹이 존재하지 않습니다",
            )
            .to_string())
        }
        StoreState::Unavailable(reason) => {
            return Err(i18n::pick(
                format!("Session group is temporarily unavailable: {reason}"),
                format!("세션 연결 그룹을 일시적으로 사용할 수 없습니다: {reason}"),
            ))
        }
    };
    let group = store
        .groups
        .iter()
        .find(|group| group.id == group_id && group_is_in_scope(client, scope, group))
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Session group does not exist or does not belong to the current client",
                "세션 연결 그룹이 존재하지 않거나 현재 클라이언트의 그룹이 아닙니다",
            )
            .to_string()
        })?;
    let source = group
        .members
        .iter()
        .find(|member| member.member_id == source_member_id && member.state == MemberState::Active)
        .cloned()
        .ok_or_else(|| i18n::tr("Source member is no longer valid; please refresh the session group and retry", "원본 구성원이 더 이상 유효하지 않습니다. 세션 그룹을 새로 고친 후 다시 시도해 주세요").to_string())?;
    if source.uid == target_uid {
        return Err(i18n::tr(
            "Target account is already a member of this session group",
            "대상 계정은 이미 이 세션 그룹의 구성원입니다",
        )
        .to_string());
    }
    if group
        .members
        .iter()
        .any(|member| member.uid == target_uid && member.state == MemberState::Active)
    {
        return Ok(
            json!({ "status": "alreadyLinked", "client": client.as_str(), "groupId": group_id, "targetAccountId": target_account_id }),
        );
    }

    match client {
        SessionClient::Workbuddy => {
            let source_acc = account_by_uid(&accounts, &source.uid).ok_or_else(|| {
                i18n::tr(
                    "Source account is no longer in the local account library",
                    "원본 계정이 더 이상 이 컴퓨터의 계정 목록에 없습니다",
                )
                .to_string()
            })?;
            let report = session::copy_sessions_cross(
                &source_acc,
                &target,
                std::slice::from_ref(&source.session_id),
            )?;
            let copied = report
                .get("copied")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let linked = report
                .get("alreadyLinked")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Ok(json!({
                "status": if copied > 0 { "linked" } else if linked > 0 { "alreadyLinked" } else { "failed" },
                "client": client.as_str(),
                "groupId": group_id,
                "report": report,
            }))
        }
        SessionClient::CodebuddyIde | SessionClient::VscodeExt => {
            let variant = scope.unwrap_or_else(|| account::variant_of(&target));
            let _operation_lock = session_link::try_acquire_client_ops_lock(&paths)?;
            // Recheck under the operation lock: the underlying IDE/plugin copy kernels are not idempotent.
            let current = match session_link::load_store(&paths) {
                StoreState::Ready(store) => store,
                StoreState::Missing => {
                    return Err(i18n::tr(
                        "Session group does not exist",
                        "세션 연결 그룹이 존재하지 않습니다",
                    )
                    .to_string())
                }
                StoreState::Unavailable(reason) => {
                    return Err(i18n::pick(
                        format!("Session group is temporarily unavailable: {reason}"),
                        format!("세션 연결 그룹을 일시적으로 사용할 수 없습니다: {reason}"),
                    ))
                }
            };
            let current_group = current
                .groups
                .iter()
                .find(|candidate| {
                    candidate.id == group_id && group_is_in_scope(client, scope, candidate)
                })
                .ok_or_else(|| {
                    i18n::tr(
                        "The session group has changed; please refresh and retry",
                        "세션 연결 그룹이 변경되었습니다. 새로 고친 후 다시 시도해 주세요",
                    )
                    .to_string()
                })?;
            if current_group
                .members
                .iter()
                .any(|member| member.uid == target_uid && member.state == MemberState::Active)
            {
                return Ok(
                    json!({ "status": "alreadyLinked", "client": client.as_str(), "groupId": group_id, "targetAccountId": target_account_id }),
                );
            }
            if client == SessionClient::CodebuddyIde {
                let flavor = ide_flavor(variant);
                if flavor.is_running() {
                    return Err(
                        i18n::tr("CodeBuddy IDE is running. Please fully quit it before adding a linked account.", "CodeBuddy IDE가 실행 중입니다. 완전히 종료한 후 연결 계정을 추가해 주세요.").to_string(),
                    );
                }
            }
            let root = client.data_root().ok_or_else(|| {
                i18n::tr(
                    "Client session data directory not found",
                    "클라이언트 세션 데이터 디렉터리를 찾을 수 없습니다",
                )
                .to_string()
            })?;
            let spec = client.store_spec().ok_or_else(|| {
                i18n::tr(
                    "Client data store is unavailable",
                    "클라이언트 데이터 저장소를 사용할 수 없습니다",
                )
                .to_string()
            })?;
            let (workspace_hash, _, content) = vscode_session_sync::session_location_and_content(
                spec,
                &root,
                &source.uid,
                &source.session_id,
            )
            .ok_or_else(|| {
                i18n::tr(
                    "Source session index no longer exists; please refresh and retry",
                    "원본 세션 색인이 더 이상 존재하지 않습니다. 새로 고친 후 다시 시도해 주세요",
                )
                .to_string()
            })?;
            if !matches!(content, ContentState::Ready(_)) {
                return Err(i18n::tr(
                    "Source session content is missing or unverifiable; cannot copy",
                    "원본 세션 내용이 없거나 확인할 수 없어 복사할 수 없습니다",
                )
                .to_string());
            }
            let item = CopyItem {
                workspace_hash,
                conversation_id: source.session_id.clone(),
            };
            // 校验（含来源内容）全部完成后，插件侧才打开编辑器生命周期窗口：为注定失败的请求关编辑器是无谓的。
            let guard = prepare_editor_window(
                client,
                restart,
                vscode_add_member_running_hint(),
                &[target_uid.as_str()],
            )?;
            let result = (|| -> Result<Value, String> {
                let (report, link_errors) = match client {
                    SessionClient::CodebuddyIde => {
                        let backup_root = paths
                            .backup_root()
                            .join(spec.backup_kind)
                            .join(config::utc_iso());
                        let report = codebuddy_ide_session::copy_codebuddy_ide_sessions_in(
                            &root,
                            &backup_root,
                            &source.uid,
                            &target_uid,
                            &[item],
                        )?;
                        let link_errors = codebuddy_ide_session_sync::register_copied_sessions(
                            &root, &paths, variant, &report,
                        );
                        (report, link_errors)
                    }
                    SessionClient::VscodeExt => plugin_copy_and_register(
                        spec,
                        &root,
                        &paths,
                        variant,
                        &source.uid,
                        &target_uid,
                        &[item],
                    )?,
                    SessionClient::Workbuddy => unreachable!(),
                };
                let copied = report
                    .get("copied")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                if copied == 0 {
                    return Ok(
                        json!({ "status": "failed", "client": client.as_str(), "groupId": group_id, "report": report }),
                    );
                }
                if !link_errors.is_empty() {
                    return Ok(json!({
                        "status": "copiedUnlinked",
                        "client": client.as_str(),
                        "groupId": group_id,
                        "report": report,
                        "linkErrors": link_errors,
                    }));
                }
                Ok(
                    json!({ "status": "linked", "client": client.as_str(), "groupId": group_id, "report": report, "linkErrors": [] }),
                )
            })();
            settle_editor_window(guard, result)
        }
    }
}

/// 插件：把来源账号的勾选会话复制到目标账号并登记关联（无现成组则新建关联组）。
///
/// 仅 `VscodeExt` 支持（其他客户端尚未接入该入口）；运行门禁与「确认后关闭重开」沿用插件侧
/// 统一生命周期：全部校验通过后才关闭编辑器，失败路径尽力开回。
pub fn copy_linked_sessions(
    client: SessionClient,
    source_account_id: &str,
    target_account_id: &str,
    session_ids: &[String],
    restart: bool,
) -> Result<Value, String> {
    if client != SessionClient::VscodeExt {
        return Err(i18n::tr(
            "The current client does not support adding linked sessions yet",
            "현재 클라이언트는 아직 연결 세션 추가를 지원하지 않습니다",
        )
        .to_string());
    }
    if session_ids.is_empty() {
        return Err(i18n::tr("Missing sessions to copy", "복사할 세션이 없습니다").to_string());
    }
    let paths = client.store_paths();
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    let source = accounts
        .iter()
        .find(|item| account::get_str(item, "id").as_deref() == Some(source_account_id))
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Source account does not exist",
                "원본 계정이 존재하지 않습니다",
            )
            .to_string()
        })?;
    let target = accounts
        .iter()
        .find(|item| account::get_str(item, "id").as_deref() == Some(target_account_id))
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Target account does not exist",
                "대상 계정이 존재하지 않습니다",
            )
            .to_string()
        })?;
    let source_uid = account::get_str(&source, "uid")
        .map(|uid| uid.trim().to_string())
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| {
            i18n::tr(
                "Source account is missing uid",
                "원본 계정에 uid가 없습니다",
            )
            .to_string()
        })?;
    let target_uid = account::get_str(&target, "uid")
        .map(|uid| uid.trim().to_string())
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| {
            i18n::tr(
                "Target account is missing uid",
                "대상 계정에 uid가 없습니다",
            )
            .to_string()
        })?;
    if !vscode_session::is_safe_uid(&target_uid) {
        return Err(i18n::tr(
            "Target account uid is invalid; refusing to write",
            "대상 계정 uid가 올바르지 않아 쓰기를 거부합니다",
        )
        .to_string());
    }
    if source_uid == target_uid {
        return Err(i18n::tr(
            "Source and target accounts are the same; no need to copy sessions",
            "원본 계정과 대상 계정이 같아 세션을 복사할 필요가 없습니다",
        )
        .to_string());
    }
    let root = client.data_root().ok_or_else(|| {
        i18n::tr(
            "VS Code CodeBuddy extension data directory not found",
            "VS Code CodeBuddy 확장 데이터 디렉터리를 찾을 수 없습니다",
        )
        .to_string()
    })?;
    let spec = client.store_spec().ok_or_else(|| {
        i18n::tr(
            "Extension data store is unavailable",
            "확장 데이터 저장소를 사용할 수 없습니다",
        )
        .to_string()
    })?;
    let variant = account::variant_of(&target);
    // 逐条校验来源内容：无内容 / 索引缺失的跳过并如实报告；全部无效才拒绝
    // （不为注定失败的请求关编辑器）。
    let mut items = Vec::with_capacity(session_ids.len());
    let mut skipped: Vec<Value> = Vec::new();
    for session_id in session_ids {
        match vscode_session_sync::session_location_and_content(
            spec,
            &root,
            &source_uid,
            session_id,
        ) {
            Some((workspace_hash, _, ContentState::Ready(_))) => items.push(CopyItem {
                workspace_hash,
                conversation_id: session_id.clone(),
            }),
            Some(_) => skipped.push(json!({
                "id": session_id,
                "error": i18n::tr("Source session content is missing or unverifiable", "원본 세션 내용이 없거나 확인할 수 없습니다"),
            })),
            None => skipped.push(json!({
                "id": session_id,
                "error": i18n::tr("Source session index no longer exists", "원본 세션 색인이 더 이상 존재하지 않습니다"),
            })),
        }
    }
    if items.is_empty() {
        return Err(i18n::tr(
            "None of the selected sessions have content to copy",
            "선택한 세션 중 복사할 내용이 있는 세션이 없습니다",
        )
        .to_string());
    }
    let _operation_lock = session_link::try_acquire_client_ops_lock(&paths)?;
    let guard = prepare_editor_window(
        client,
        restart,
        vscode_add_member_running_hint(),
        &[target_uid.as_str()],
    )?;
    let result = (|| -> Result<Value, String> {
        let (report, link_errors) = plugin_copy_and_register(
            spec,
            &root,
            &paths,
            variant,
            &source_uid,
            &target_uid,
            &items,
        )?;
        let copied = report
            .get("copied")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if copied == 0 {
            return Ok(json!({
                "status": "failed",
                "client": client.as_str(),
                "report": report,
                "skipped": skipped,
            }));
        }
        if !link_errors.is_empty() {
            return Ok(json!({
                "status": "copiedUnlinked",
                "client": client.as_str(),
                "report": report,
                "linkErrors": link_errors,
                "skipped": skipped,
            }));
        }
        Ok(json!({
            "status": "linked",
            "client": client.as_str(),
            "report": report,
            "linkErrors": [],
            "skipped": skipped,
        }))
    })();
    settle_editor_window(guard, result)
}

/// Remove one member from a group, keeping the account's session content untouched.
///
/// Group-management data only: no client kernel runs and no session file is written. Pair
/// baselines that reference the member are dropped together with it (`validate_store` requires
/// pair references to stay inside the group); a group left without members is deleted.
pub fn remove_member(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    member_id: &str,
) -> Result<Value, String> {
    client.validate_scope(scope)?;
    let paths = client.store_paths();
    // 单次存储锁内完成读改写：失败不写盘、不推进 revision（`with_link_store_write` 契约）。
    let (group_removed, remaining) = session_link::with_link_store_write(&paths, |store| {
        remove_member_in(store, client, scope, group_id, member_id)
    })?;
    Ok(json!({
        "status": if group_removed { "groupRemoved" } else { "removed" },
        "client": client.as_str(),
        "groupId": group_id,
        "memberId": member_id,
        "remaining": remaining,
    }))
}

/// [`remove_member`] 的纯变更部分：与存储 IO 分离，便于用固定夹具覆盖不变量。
fn remove_member_in(
    store: &mut crate::modules::session_link::LinkStore,
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    member_id: &str,
) -> Result<(bool, usize), String> {
    let group = store
        .groups
        .iter_mut()
        .find(|group| group.id == group_id && group_is_in_scope(client, scope, group))
        .ok_or_else(|| {
            i18n::tr(
                "Session group does not exist or does not belong to the current client",
                "세션 연결 그룹이 존재하지 않거나 현재 클라이언트의 그룹이 아닙니다",
            )
            .to_string()
        })?;
    if !group
        .members
        .iter()
        .any(|member| member.member_id == member_id)
    {
        return Err(i18n::tr(
            "Member does not exist; please refresh and retry",
            "구성원이 존재하지 않습니다. 새로 고친 후 다시 시도해 주세요",
        )
        .to_string());
    }
    group.members.retain(|member| member.member_id != member_id);
    group
        .pair_bases
        .retain(|pair| !pair.member_ids.iter().any(|id| id == member_id));
    let remaining = group.members.len();
    let group_removed = remaining == 0;
    if group_removed {
        store.groups.retain(|group| group.id != group_id);
    }
    Ok((group_removed, remaining))
}

/// 删除整个会话组：组内所有成员一起解除关联，只解除管理关系，不删除账号内的会话内容。
pub fn delete_group(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
) -> Result<Value, String> {
    client.validate_scope(scope)?;
    let paths = client.store_paths();
    // 单次存储锁内完成读改写：失败不写盘、不推进 revision（`with_link_store_write` 契约）。
    let removed = session_link::with_link_store_write(&paths, |store| {
        delete_group_in(store, client, scope, group_id)
    })?;
    Ok(json!({
        "status": "groupRemoved",
        "client": client.as_str(),
        "groupId": group_id,
        "removed": removed,
    }))
}

/// [`delete_group`] 的纯变更部分：与存储 IO 分离，便于用固定夹具覆盖不变量。
fn delete_group_in(
    store: &mut crate::modules::session_link::LinkStore,
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
) -> Result<usize, String> {
    let removed = store
        .groups
        .iter()
        .find(|group| group.id == group_id && group_is_in_scope(client, scope, group))
        .map(|group| group.members.len())
        .ok_or_else(|| {
            i18n::tr(
                "Session group does not exist or does not belong to the current client",
                "세션 연결 그룹이 존재하지 않거나 현재 클라이언트의 그룹이 아닙니다",
            )
            .to_string()
        })?;
    store.groups.retain(|group| group.id != group_id);
    Ok(removed)
}

#[derive(Debug, Clone)]
struct PairContext {
    group: LinkGroup,
    source: LinkMember,
    target: LinkMember,
    source_account: Value,
    target_account: Value,
}

fn resolve_pair(
    client: SessionClient,
    scope: Option<WbVariant>,
    group_id: &str,
    source_member_id: &str,
    target_member_id: &str,
) -> Result<PairContext, String> {
    client.validate_scope(scope)?;
    let paths = client.store_paths();
    let store = match session_link::load_store(&paths) {
        StoreState::Ready(store) => store,
        StoreState::Missing => {
            return Err(i18n::tr(
                "Session group does not exist",
                "세션 연결 그룹이 존재하지 않습니다",
            )
            .to_string())
        }
        StoreState::Unavailable(reason) => {
            return Err(i18n::pick(
                format!("Session group is temporarily unavailable: {reason}"),
                format!("세션 연결 그룹을 일시적으로 사용할 수 없습니다: {reason}"),
            ))
        }
    };
    let group = store
        .groups
        .iter()
        .find(|group| group.id == group_id && group_is_in_scope(client, scope, group))
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Session group does not exist or does not belong to the current client",
                "세션 연결 그룹이 존재하지 않거나 현재 클라이언트의 그룹이 아닙니다",
            )
            .to_string()
        })?;
    let source = group
        .members
        .iter()
        .find(|member| member.member_id == source_member_id && member.state == MemberState::Active)
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Source member is no longer valid; please refresh the session group",
                "원본 구성원이 더 이상 유효하지 않습니다. 세션 그룹을 새로 고쳐 주세요",
            )
            .to_string()
        })?;
    let target = group
        .members
        .iter()
        .find(|member| member.member_id == target_member_id && member.state == MemberState::Active)
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "Target member is no longer valid; please refresh the session group",
                "대상 구성원이 더 이상 유효하지 않습니다. 세션 그룹을 새로 고쳐 주세요",
            )
            .to_string()
        })?;
    if source.member_id == target.member_id || source.uid == target.uid {
        return Err(i18n::tr(
            "Source and target must be members of different accounts",
            "원본과 대상은 서로 다른 계정의 구성원이어야 합니다",
        )
        .to_string());
    }
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    let source_account = account_by_uid(&accounts, &source.uid).ok_or_else(|| {
        i18n::tr(
            "Source account is no longer in the local account library",
            "원본 계정이 더 이상 이 컴퓨터의 계정 목록에 없습니다",
        )
        .to_string()
    })?;
    let target_account = account_by_uid(&accounts, &target.uid).ok_or_else(|| {
        i18n::tr(
            "Target account is no longer in the local account library",
            "대상 계정이 더 이상 이 컴퓨터의 계정 목록에 없습니다",
        )
        .to_string()
    })?;
    validate_target_compatibility(client, scope, &target_account)?;
    Ok(PairContext {
        group,
        source,
        target,
        source_account,
        target_account,
    })
}

fn preview_resolved_pair(
    client: SessionClient,
    scope: Option<WbVariant>,
    pair: &PairContext,
) -> Result<Value, String> {
    let paths = client.store_paths();
    let report = match client {
        SessionClient::Workbuddy => {
            session::session_links_preview_cross(&pair.source_account, &pair.target_account)?
        }
        SessionClient::CodebuddyIde => {
            let variant = scope.ok_or_else(|| {
                i18n::tr(
                    "CodeBuddy IDE is missing the edition",
                    "CodeBuddy IDE에 버전 정보가 없습니다",
                )
                .to_string()
            })?;
            let root = client.data_root().ok_or_else(|| {
                i18n::tr(
                    "CodeBuddy IDE session data directory not found",
                    "CodeBuddy IDE 세션 데이터 디렉터리를 찾을 수 없습니다",
                )
                .to_string()
            })?;
            vscode_session_sync::links_preview_in_for_variant(
                CODEBUDDY_IDE_STORE,
                &root,
                &paths,
                &pair.source.uid,
                &pair.target_account,
                variant,
            )?
        }
        SessionClient::VscodeExt => {
            let root = client.data_root().ok_or_else(|| {
                i18n::tr(
                    "VS Code extension session data directory not found",
                    "VS Code 확장 세션 데이터 디렉터리를 찾을 수 없습니다",
                )
                .to_string()
            })?;
            vscode_session_sync::links_preview_in(
                VSCODE_STORE,
                &root,
                &paths,
                &pair.source.uid,
                &pair.target_account,
            )?
        }
    };
    let preview = report
        .get("groups")
        .and_then(Value::as_array)
        .and_then(|groups| {
            groups.iter().find(|item| {
                item.get("groupId").and_then(Value::as_str) == Some(pair.group.id.as_str())
            })
        })
        .cloned()
        .ok_or_else(|| {
            i18n::tr(
                "This group currently has no source and target copies to preview",
                "이 그룹에는 현재 미리 볼 원본 및 대상 복사본이 없습니다",
            )
            .to_string()
        })?;
    let mut modes = preview.get("availableModes").cloned().unwrap_or(json!([]));
    if preview.get("verdict").and_then(Value::as_str) == Some("ahead")
        && preview
            .get("previewToken")
            .and_then(Value::as_str)
            .is_some()
    {
        modes = json!(["unifyOverwrite"]);
    }
    Ok(json!({
        "client": client.as_str(),
        "variantScope": scope.map(WbVariant::as_str),
        "groupId": pair.group.id,
        "sourceMemberId": pair.source.member_id,
        "targetMemberId": pair.target.member_id,
        "verdict": preview.get("verdict").cloned().unwrap_or(Value::Null),
        "availableModes": modes,
        "previewToken": preview.get("previewToken").cloned().unwrap_or(Value::Null),
        "reason": preview
            .get("reason")
            .cloned()
            .unwrap_or(json!(i18n::tr("Unable to confirm", "확인할 수 없습니다"))),
        "recordCount": preview.get("recordCount").cloned().unwrap_or(Value::Null),
        "extraTargetCount": preview.get("extraB").cloned().unwrap_or(json!(0)),
    }))
}

/// 插件侧同步入口运行中的报错文案（`restart=false`，逐字保持既有文案）。
fn vscode_sync_running_hint() -> &'static str {
    i18n::tr(
        "VS Code is running. Please fully quit it before syncing sessions.",
        "VS Code가 실행 중입니다. 완전히 종료한 후 세션을 동기화해 주세요.",
    )
}

/// 插件侧「添加关联账号」运行中的报错文案（`restart=false`，逐字保持既有文案）。
fn vscode_add_member_running_hint() -> &'static str {
    i18n::tr(
        "VS Code is running. Please fully quit it before adding a linked account.",
        "VS Code가 실행 중입니다. 완전히 종료한 후 연결 계정을 추가해 주세요.",
    )
}

/// 关闭决策（纯函数）：`Ok(true)` = 需要关闭编辑器，`Ok(false)` = 未运行或不涉及、不关不拉。
///
/// 运行中的插件只会写「当前登录账号」的数据，因此仅当**本次写入目标包含当前登录账号**时才需要
/// 编辑器生命周期；`current_uid = None`（未登录/状态缺失）时保守按涉及处理——与 WorkBuddy 的
/// `current_uid.map_or(true, …)` 同口径。
fn plan_editor_restart(
    running: bool,
    current_uid: Option<&str>,
    target_uids: &[&str],
    restart: bool,
    manual_hint: &str,
) -> Result<bool, String> {
    if !running {
        return Ok(false);
    }
    let involved = current_uid.is_none_or(|uid| target_uids.contains(&uid));
    if !involved {
        return Ok(false);
    }
    if !restart {
        return Err(manual_hint.to_string());
    }
    Ok(true)
}

/// 预检：当前是否需要关闭 VS Code 才能安全执行写操作（运行中 且 目标账号含当前登录账号）。
///
/// 供前端决定「是否先弹确认框」；判定与写入口共用 [`plan_editor_restart`]，最终把关仍在各写入口。
pub fn vscode_restart_precheck(target_account_ids: &[String]) -> Value {
    let paths = SessionClient::VscodeExt.store_paths();
    let accounts = account::load_accounts_at(&account::accounts_file_in(&paths.store_root));
    let target_uids: Vec<String> = target_account_ids
        .iter()
        .filter_map(|id| {
            accounts
                .iter()
                .find(|item| account::get_str(item, "id").as_deref() == Some(id.as_str()))
        })
        .filter_map(|item| account::get_str(item, "uid"))
        .collect();
    let target_refs: Vec<&str> = target_uids.iter().map(String::as_str).collect();
    let running = vscode_ext::is_vscode_running();
    let current_uid = vscode_ext::active_ext_uid();
    // 以 `restart = true` 走同一决策（不会产生文案错误）：`Ok(true)` 即「需要关闭」。
    let required = plan_editor_restart(running, current_uid.as_deref(), &target_refs, true, "")
        .unwrap_or(false);
    json!({ "required": required, "running": running })
}

/// 插件侧写操作的编辑器生命周期窗口（仅 `SessionClient::VscodeExt` 使用）。
///
/// - 未运行 → 不关闭、不主动拉起；
/// - 运行中 + `restart=false` → `Err(manual_hint)`（各入口保留自己的既有文案）；
/// - 运行中 + `restart=true` → 复用 [`vscode_ext::close_vscode_for_switch`] 优雅关闭，
///   窗口结束后 best-effort 重开。
///
/// **单窗口**：调用方在窗口内跑完全部目标才收尾，全程只关 / 开各一次；任何失败路径都必须
/// 经 [`fail`](Self::fail) 收尾，保证「关了的要开回来」，且不对未运行者主动拉起。
struct VscodeEditorGuard {
    /// 本次由我们关闭的编辑器的重开动作；`None` = 本来就没运行（或关闭前已自行退出）。
    relaunch: Option<Box<dyn FnOnce() -> Result<(), String>>>,
}

impl VscodeEditorGuard {
    /// 打开窗口：判定 →（必要时）关闭。校验类错误必须在此前完成，避免为注定失败的请求关编辑器。
    fn prepare(restart: bool, manual_hint: &str, target_uids: &[&str]) -> Result<Self, String> {
        let running = vscode_ext::is_vscode_running();
        let current_uid = vscode_ext::active_ext_uid();
        if !plan_editor_restart(
            running,
            current_uid.as_deref(),
            target_uids,
            restart,
            manual_hint,
        )? {
            return Ok(Self { relaunch: None });
        }
        match vscode_ext::close_vscode_for_switch(true)? {
            None => Ok(Self { relaunch: None }),
            Some(closed) => Ok(Self {
                relaunch: Some(Box::new(move || {
                    vscode_ext::relaunch_closed_editor(&closed)
                })),
            }),
        }
    }

    /// 本次窗口是否真的关闭了编辑器（报告字段只认「我们关的 + 重开成功」）。
    fn closed_by_us(&self) -> bool {
        self.relaunch.is_some()
    }

    /// 成功收尾：重开（仅本次由我们关闭时）。`Err` = 重开失败，操作本身已成功。
    fn finish(self) -> Result<(), String> {
        match self.relaunch {
            None => Ok(()),
            Some(relaunch) => relaunch(),
        }
    }

    /// 失败收尾：best-effort 重开，并把重开失败信息追加到原错误之后（不吞原错误）。
    fn fail(self, error: String) -> String {
        match self.finish() {
            Ok(()) => error,
            Err(launch_error) => format!("{error}\n\n{launch_error}"),
        }
    }
}

/// 非插件侧不涉及编辑器生命周期（`Ok(None)`）；插件侧按 `restart` 打开窗口。
fn prepare_editor_window(
    client: SessionClient,
    restart: bool,
    manual_hint: &str,
    target_uids: &[&str],
) -> Result<Option<VscodeEditorGuard>, String> {
    match client {
        SessionClient::VscodeExt => {
            VscodeEditorGuard::prepare(restart, manual_hint, target_uids).map(Some)
        }
        SessionClient::Workbuddy | SessionClient::CodebuddyIde => Ok(None),
    }
}

/// 插件侧写操作的统一收尾：重开编辑器并把结果并入报告。
///
/// - 重开成功且本次确实关过 → 报告补 `restartedEditor: true`；
/// - 重开失败不进整体 `Err`（操作本身已成功）：能进 `errors[]` 就进，否则挂 `editorError`。
fn settle_editor_window(
    guard: Option<VscodeEditorGuard>,
    result: Result<Value, String>,
) -> Result<Value, String> {
    let Some(guard) = guard else {
        return result;
    };
    match result {
        Ok(mut report) => {
            let closed_by_us = guard.closed_by_us();
            match guard.finish() {
                Ok(()) => {
                    if closed_by_us {
                        report["restartedEditor"] = json!(true);
                    }
                }
                Err(error) => {
                    let message = i18n::pick(
                        format!("Session was processed, but VS Code failed to reopen: {error}"),
                        format!("세션은 처리했지만 VS Code를 다시 열지 못했습니다: {error}"),
                    );
                    match report.get_mut("errors").and_then(Value::as_array_mut) {
                        Some(errors) => errors.push(json!({ "error": message })),
                        None => report["editorError"] = json!(message),
                    }
                }
            }
            Ok(report)
        }
        Err(error) => Err(guard.fail(error)),
    }
}

fn run_pair_sync(
    client: SessionClient,
    scope: Option<WbVariant>,
    pair: &PairContext,
    selections: &[SyncSelection],
    editor_prepared: bool,
) -> Result<Value, String> {
    let paths = client.store_paths();
    let report = match client {
        SessionClient::Workbuddy => {
            session::sync_sessions_cross(&pair.source_account, &pair.target_account, selections)?
        }
        SessionClient::CodebuddyIde => {
            let variant = scope.ok_or_else(|| {
                i18n::tr(
                    "CodeBuddy IDE is missing the edition",
                    "CodeBuddy IDE에 버전 정보가 없습니다",
                )
                .to_string()
            })?;
            let _operation_lock = session_link::try_acquire_client_ops_lock(&paths)?;
            if ide_flavor(variant).is_running() {
                return Err(i18n::tr(
                    "CodeBuddy IDE is running. Please fully quit it before syncing sessions.",
                    "CodeBuddy IDE가 실행 중입니다. 완전히 종료한 후 세션을 동기화해 주세요.",
                )
                .to_string());
            }
            let root = client.data_root().ok_or_else(|| {
                i18n::tr(
                    "CodeBuddy IDE session data directory not found",
                    "CodeBuddy IDE 세션 데이터 디렉터리를 찾을 수 없습니다",
                )
                .to_string()
            })?;
            vscode_session_sync::sync_selected_in_for_variant(
                CODEBUDDY_IDE_STORE,
                &root,
                &paths,
                &pair.source.uid,
                &pair.target_account,
                selections,
                variant,
            )?
        }
        SessionClient::VscodeExt => {
            let _operation_lock = session_link::try_acquire_client_ops_lock(&paths)?;
            // `editor_prepared` = 调用方已在生命周期窗口内处理过编辑器（已关闭或本来未运行）。
            if !editor_prepared && vscode_ext::is_vscode_running() {
                return Err(vscode_sync_running_hint().to_string());
            }
            let root = client.data_root().ok_or_else(|| {
                i18n::tr(
                    "VS Code extension session data directory not found",
                    "VS Code 확장 세션 데이터 디렉터리를 찾을 수 없습니다",
                )
                .to_string()
            })?;
            vscode_session_sync::sync_selected_in(
                VSCODE_STORE,
                &root,
                &paths,
                &pair.source.uid,
                &pair.target_account,
                selections,
            )?
        }
    };
    Ok(json!({
        "client": client.as_str(),
        "groupId": pair.group.id,
        "sourceMemberId": pair.source.member_id,
        "targetMemberId": pair.target.member_id,
        "synced": report.get("synced").cloned().unwrap_or(json!([])),
        "skipped": report.get("skipped").cloned().unwrap_or(json!([])),
        "errors": report.get("errors").cloned().unwrap_or(json!([])),
        "needsRecovery": report.get("needsRecovery").cloned().unwrap_or(json!(false)),
        "temporaryFiles": report.get("temporaryFiles").cloned().unwrap_or(json!([])),
    }))
}

fn parse_selection(
    group_id: &str,
    preview_token: &str,
    mode: &str,
) -> Result<SyncSelection, String> {
    if !matches!(mode, "fastForward" | "overwrite" | "unifyOverwrite") {
        return Err(i18n::tr(
            "Unsupported session sync mode",
            "지원하지 않는 세션 동기화 모드입니다",
        )
        .to_string());
    }
    session::parse_sync_selections(Some(&json!([{
        "groupId": group_id,
        "previewToken": preview_token,
        "mode": mode,
    }])))?
    .into_iter()
    .next()
    .ok_or_else(|| {
        i18n::tr(
            "Session sync selection is empty",
            "세션 동기화 선택 항목이 비어 있습니다",
        )
        .to_string()
    })
}

fn merge_sync_report(into: &mut Value, item: Value) {
    let Value::Object(combined) = into else {
        return;
    };
    for key in ["synced", "skipped", "errors", "temporaryFiles"] {
        let values = item
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // 汇总对象的初始化 schema 可能与合并键列表漂移：缺键时 IndexMut 只会补成 Null，
        // 直接 as_array_mut().unwrap() 会 panic。这里把「缺失或不是数组」的槽位补成
        // 数组再合并 values —— 不 panic，也不丢数据。
        match combined.entry(key.to_owned()).or_insert_with(|| json!([])) {
            Value::Array(target) => target.extend(values),
            slot => *slot = json!(values),
        }
    }
    if item.get("needsRecovery").and_then(Value::as_bool) == Some(true) {
        combined.insert("needsRecovery".to_owned(), json!(true));
    }
}

fn validate_target_compatibility(
    client: SessionClient,
    scope: Option<WbVariant>,
    target: &Value,
) -> Result<(), String> {
    match client {
        SessionClient::Workbuddy => Ok(()),
        SessionClient::CodebuddyIde => {
            let expected = scope.ok_or_else(|| {
                i18n::tr(
                    "CodeBuddy IDE is missing the edition",
                    "CodeBuddy IDE에 버전 정보가 없습니다",
                )
                .to_string()
            })?;
            if account::variant_of(target) != expected {
                return Err(i18n::tr(
                    "The target account is not compatible with the current CodeBuddy IDE edition",
                    "대상 계정이 현재 CodeBuddy IDE 버전과 호환되지 않습니다",
                )
                .to_string());
            }
            Ok(())
        }
        // The plugin uses one account-scoped store across account regions.
        SessionClient::VscodeExt => Ok(()),
    }
}

fn group_is_in_scope(client: SessionClient, scope: Option<WbVariant>, group: &LinkGroup) -> bool {
    match client {
        SessionClient::CodebuddyIde => Some(group.variant) == scope,
        SessionClient::Workbuddy | SessionClient::VscodeExt => true,
    }
}

fn account_by_uid(accounts: &[Value], uid: &str) -> Option<Value> {
    accounts
        .iter()
        .find(|account| account::get_str(account, "uid").as_deref() == Some(uid))
        .cloned()
}

fn ide_flavor(variant: WbVariant) -> codebuddy_ide_session::IdeFlavor {
    match variant {
        WbVariant::Cn => codebuddy_ide_session::IdeFlavor::Cn,
        WbVariant::Ai => codebuddy_ide_session::IdeFlavor::Intl,
    }
}

fn group_payload(
    client: SessionClient,
    scope: Option<WbVariant>,
    paths: &SessionPaths,
    group: &LinkGroup,
    accounts: &[Value],
    cache: &mut HashMap<(String, String), Vec<Value>>,
    include_detail: bool,
) -> Value {
    let root = client.data_root();
    let spec = client.store_spec();
    let mut views = group
        .members
        .iter()
        .map(|member| {
            let account = account_by_uid(accounts, &member.uid);
            let account_name = account
                .as_ref()
                .map(account::account_display_name)
                .unwrap_or_else(|| member.uid.clone());
            let account_key = account
                .as_ref()
                .and_then(|account| account::get_str(account, "id"))
                .or_else(|| member.account_id.clone())
                .unwrap_or_else(|| member.uid.clone());
            let (title, project_label, updated_at, content) =
                member_snapshot(client, &root, spec, group, member, cache);
            let mut member = member.clone();
            // Correct legacy plugin labels from the account library; region never selects its data root.
            if client == SessionClient::VscodeExt {
                if let Some(account) = account.as_ref() {
                    member.variant = Some(account::variant_of(account));
                }
            }
            MemberView {
                member,
                account_key,
                account_name,
                title,
                project_label,
                updated_at,
                content,
                reason: String::new(),
                version_status: "unknown",
            }
        })
        .collect::<Vec<_>>();
    views.sort_by(|left, right| {
        left.account_key
            .cmp(&right.account_key)
            .then_with(|| left.member.uid.cmp(&right.member.uid))
            .then_with(|| left.member.member_id.cmp(&right.member.member_id))
    });

    let active = views
        .iter()
        .enumerate()
        .filter(|(_, view)| view.member.state == MemberState::Active)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut matrix: HashMap<(usize, usize), (SyncVerdict, String)> = HashMap::new();
    for &source_index in &active {
        for &target_index in &active {
            if source_index == target_index {
                continue;
            }
            let source = &views[source_index];
            let target = &views[target_index];
            let baseline = session_link::load_pair_baseline(
                paths,
                group,
                &source.member.member_id,
                &target.member.member_id,
            );
            let decision = session_link::decide_sync(&source.content, &target.content, &baseline);
            matrix.insert(
                (source_index, target_index),
                (decision.verdict, decision.reason),
            );
        }
    }
    let (safe_source, aggregate_status) = aggregate_group_state(&mut views, &active, &matrix);
    let divergence = common_base_branches(&views, &active);
    let summary_status = if divergence.is_some() {
        "diverge"
    } else {
        aggregate_status
    };
    let title = views
        .iter()
        .find(|view| !view.title.trim().is_empty())
        .map(|view| view.title.clone())
        .unwrap_or_else(|| i18n::tr("Session group", "세션 그룹").to_string());
    let project_label = views
        .iter()
        .find(|view| !view.project_label.is_empty())
        .map(|view| view.project_label.clone())
        .unwrap_or_default();
    let latest_activity_at = views.iter().map(|view| view.updated_at).max().unwrap_or(0);
    let account_names = views
        .iter()
        .map(|view| view.account_name.clone())
        .collect::<Vec<_>>();
    let group_key = format!(
        "{}:{}:{}",
        client.as_str(),
        scope.map(WbVariant::as_str).unwrap_or("all"),
        group.id
    );
    let mut result = json!({
        "key": group_key,
        "client": client.as_str(),
        "variantScope": scope.map(WbVariant::as_str),
        "groupId": group.id,
        "groupVariant": group.variant.as_str(),
        "title": title,
        "projectLabel": project_label,
        "latestActivityAt": latest_activity_at,
        "memberCount": group.members.len(),
        "activeMemberCount": active.len(),
        "accountNames": account_names,
        "summaryStatus": summary_status,
        "summaryText": summary_text(summary_status),
        "safeSourceMemberId": safe_source.map(|index| views[index].member.member_id.clone()),
        "hasSafeSource": safe_source.is_some(),
    });
    if include_detail {
        if let Some(divergence) = divergence {
            result["divergence"] = json!({
                "commonMemberIds": divergence.common.iter().map(|&index| &views[index].member.member_id).collect::<Vec<_>>(),
                "branches": divergence.branches.iter().map(|branch| branch.iter().map(|&index| &views[index].member.member_id).collect::<Vec<_>>()).collect::<Vec<_>>(),
            });
        }
        let member_payloads = views.iter().map(|view| json!({
            "memberId": view.member.member_id,
            "accountId": view.member.account_id,
            "uid": view.member.uid,
            "sessionId": view.member.session_id,
            "accountName": view.account_name,
            "variant": session_link::member_variant(group, &view.member).as_str(),
            "linkState": view.member.state.as_str(),
            "versionStatus": view.version_status,
            "title": view.title,
            "projectLabel": view.project_label,
            "updatedAt": view.updated_at,
            "recordCount": content_count(&view.content),
            "contentPreview": content_preview(&view.content),
            "contentState": content_state_name(&view.content),
            "reason": view.reason,
            "canBeSource": view.member.state == MemberState::Active && matches!(view.content, ContentState::Ready(_)),
        })).collect::<Vec<_>>();
        let target_options = compatible_accounts(client, scope, group, accounts);
        result["members"] = json!(member_payloads);
        result["addTargets"] = json!(target_options);
    }
    result
}

/// Recognize one present shared snapshot followed by independent append-only branches.
/// This uses normalized ordered content, so timestamps and record counts cannot invent ancestry.
/// A more complex history stays in the generic divergence state.
#[derive(Debug, PartialEq, Eq)]
struct CommonBaseBranches {
    common: Vec<usize>,
    branches: Vec<Vec<usize>>,
}

fn common_base_branches(views: &[MemberView], active: &[usize]) -> Option<CommonBaseBranches> {
    if active.len() < 3 || active.len() != views.len() {
        return None;
    }
    let lines = |index: usize| match &views[index].content {
        ContentState::Ready(snapshot) => Some(snapshot.normalized.line_digests.as_slice()),
        _ => None,
    };
    let mut versions: Vec<Vec<usize>> = Vec::new();
    for &index in active {
        let content = lines(index)?;
        if let Some(version) = versions
            .iter_mut()
            .find(|version| lines(version[0]) == Some(content))
        {
            version.push(index);
        } else {
            versions.push(vec![index]);
        }
    }
    if versions.len() < 3 {
        return None;
    }
    let common_index = versions.iter().position(|version| {
        let common = lines(version[0]).expect("active members have readable content");
        versions
            .iter()
            .filter(|other| *other != version)
            .all(|other| {
                let branch = lines(other[0]).expect("active members have readable content");
                branch.len() > common.len() && branch.starts_with(common)
            })
    })?;
    let common = versions.remove(common_index);
    // Each remaining version must be independently advanced from the shared snapshot.
    if versions.iter().enumerate().any(|(index, branch)| {
        versions.iter().skip(index + 1).any(|other| {
            let left = lines(branch[0]).expect("active members have readable content");
            let right = lines(other[0]).expect("active members have readable content");
            left.starts_with(right) || right.starts_with(left)
        })
    }) {
        return None;
    }
    Some(CommonBaseBranches {
        common,
        branches: versions,
    })
}

/// Reduce pairwise decisions into stable member states and a group summary.
///
/// `views` are already ordered by account key, UID, and member ID. The reducer is deliberately
/// independent of storage and process state so the conservative source-selection contract can be
/// covered with deterministic unit tests.
fn aggregate_group_state(
    views: &mut [MemberView],
    active: &[usize],
    matrix: &HashMap<(usize, usize), (SyncVerdict, String)>,
) -> (Option<usize>, &'static str) {
    let mut safe_candidates = active
        .iter()
        .copied()
        .filter(|&candidate| {
            if !matches!(&views[candidate].content, ContentState::Ready(_)) {
                return false;
            }
            active
                .iter()
                .copied()
                .filter(|&other| other != candidate)
                .all(|other| {
                    matrix.get(&(candidate, other)).is_some_and(|(verdict, _)| {
                        matches!(verdict, SyncVerdict::Identical | SyncVerdict::FastForward)
                    })
                })
        })
        .collect::<Vec<_>>();
    safe_candidates.sort_by(|&left, &right| {
        views[left]
            .account_key
            .cmp(&views[right].account_key)
            .then_with(|| views[left].member.uid.cmp(&views[right].member.uid))
            .then_with(|| {
                views[left]
                    .member
                    .member_id
                    .cmp(&views[right].member.member_id)
            })
    });
    let safe_source = safe_candidates.first().copied();

    let mut has_behind = false;
    let mut has_diverge = false;
    let mut has_missing = false;
    for (index, view) in views.iter_mut().enumerate() {
        match view.member.state {
            MemberState::Stale => {
                view.version_status = "stale";
                view.reason = i18n::tr(
                    "This copy has been marked as invalid",
                    "이 복사본은 유효하지 않음으로 표시되었습니다",
                )
                .to_string();
            }
            MemberState::Superseded => {
                view.version_status = "superseded";
                view.reason = i18n::tr(
                    "This copy has been superseded by a newer member",
                    "이 복사본은 더 최신 구성원으로 대체되었습니다",
                )
                .to_string();
            }
            MemberState::Active => match &view.content {
                ContentState::Missing => {
                    view.version_status = "missing";
                    view.reason = i18n::tr(
                        "Session content does not exist; cannot sync",
                        "세션 내용이 존재하지 않아 동기화할 수 없습니다",
                    )
                    .to_string();
                    has_missing = true;
                }
                ContentState::Unavailable(reason) => {
                    view.version_status = "unknown";
                    view.reason = reason.clone();
                }
                ContentState::Ready(_) => {
                    if let Some(source_index) = safe_source {
                        if index == source_index
                            || matrix
                                .get(&(source_index, index))
                                .is_some_and(|(verdict, _)| *verdict == SyncVerdict::Identical)
                        {
                            view.version_status = "latest";
                            if index != source_index {
                                view.reason = i18n::tr(
                                    "Content matches the operation source",
                                    "내용이 작업 원본과 일치합니다",
                                )
                                .to_string();
                            }
                        } else if let Some((SyncVerdict::FastForward, reason)) =
                            matrix.get(&(source_index, index))
                        {
                            view.version_status = "behind";
                            view.reason = reason.clone();
                            has_behind = true;
                        } else {
                            view.version_status = "unknown";
                            view.reason = i18n::tr(
                                "Could not confirm this member's state after re-checking",
                                "다시 확인한 후에도 이 구성원의 상태를 확인할 수 없습니다",
                            )
                            .to_string();
                        }
                    } else {
                        let reasons = active
                            .iter()
                            .filter_map(|other| matrix.get(&(*other, index)))
                            .collect::<Vec<_>>();
                        if reasons
                            .iter()
                            .any(|(verdict, _)| *verdict == SyncVerdict::Diverge)
                        {
                            view.version_status = "diverge";
                            view.reason = reasons
                                .iter()
                                .find(|(verdict, _)| *verdict == SyncVerdict::Diverge)
                                .map(|(_, reason)| reason.clone())
                                .unwrap_or_default();
                            has_diverge = true;
                        } else {
                            view.version_status = "unknown";
                            view.reason = reasons
                                .first()
                                .map(|(_, reason)| reason.clone())
                                .unwrap_or_else(|| {
                                    i18n::tr(
                                        "No comparable linked member",
                                        "비교할 수 있는 연결 구성원이 없습니다",
                                    )
                                    .to_string()
                                });
                        }
                    }
                }
            },
        }
    }

    let summary_status = if safe_source.is_some() {
        if has_behind {
            "behind"
        } else {
            "latest"
        }
    } else if has_diverge {
        "diverge"
    } else if has_missing {
        "missing"
    } else {
        // 无任何可判定的状态标记时同样回退为「无法确认」（原 `has_unknown` 分支与 else 同体）。
        "unknown"
    };

    (safe_source, summary_status)
}

fn member_snapshot(
    client: SessionClient,
    root: &Option<PathBuf>,
    spec: Option<SessionStoreSpec>,
    group: &LinkGroup,
    member: &LinkMember,
    cache: &mut HashMap<(String, String), Vec<Value>>,
) -> (String, String, i64, ContentState) {
    match client {
        SessionClient::Workbuddy => {
            let variant = session_link::member_variant(group, member);
            let rows = cache
                .entry((variant.as_str().to_string(), member.uid.clone()))
                .or_insert_with(|| {
                    session::list_sessions_for_user(variant, &member.uid)
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                });
            let row = rows.iter().find(|row| {
                row.get("id").and_then(Value::as_str) == Some(member.session_id.as_str())
            });
            let title = row
                .and_then(|row| row.get("title"))
                .and_then(Value::as_str)
                .unwrap_or(i18n::tr(
                    "Session title is temporarily unavailable",
                    "세션 제목을 일시적으로 사용할 수 없습니다",
                ))
                .to_string();
            let cwd = row
                .and_then(|row| row.get("cwd"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let project = basename(cwd);
            let updated_at = row
                .and_then(|row| row.get("updatedAt"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let member_paths = SessionPaths::for_variant(variant);
            let content = if row.is_none() {
                ContentState::Missing
            } else {
                session::member_content_state(&member_paths, &member.session_id)
            };
            (
                title,
                if project.is_empty() {
                    String::new()
                } else {
                    i18n::pick(format!("Project {project}"), format!("프로젝트 {project}"))
                },
                updated_at,
                content,
            )
        }
        SessionClient::CodebuddyIde | SessionClient::VscodeExt => {
            let variant = session_link::member_variant(group, member);
            let rows = cache
                .entry((variant.as_str().to_string(), member.uid.clone()))
                .or_insert_with(|| {
                    let Some(root) = root.as_ref() else {
                        return Vec::new();
                    };
                    vscode_session::list_sessions_in_store(spec.unwrap(), root, &member.uid)
                        .get("sessions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                });
            let row = rows.iter().find(|row| {
                row.get("id").and_then(Value::as_str) == Some(member.session_id.as_str())
            });
            let title = row
                .and_then(|row| row.get("title"))
                .and_then(Value::as_str)
                .unwrap_or(i18n::tr(
                    "Session title is temporarily unavailable",
                    "세션 제목을 일시적으로 사용할 수 없습니다",
                ))
                .to_string();
            let hash = row
                .and_then(|row| row.get("workspaceHash"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let project = if hash.is_empty() {
                String::new()
            } else {
                i18n::pick(
                    format!("Workspace {}", hash.chars().take(8).collect::<String>()),
                    format!("워크스페이스 {}", hash.chars().take(8).collect::<String>()),
                )
            };
            let updated_at = row
                .and_then(|row| row.get("updatedAt"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let content = match (root.as_ref(), spec) {
                (Some(root), Some(spec)) => vscode_session_sync::session_location_and_content(
                    spec,
                    root,
                    &member.uid,
                    &member.session_id,
                )
                .map(|(_, _, content)| content)
                .unwrap_or(ContentState::Missing),
                _ => ContentState::Unavailable(
                    i18n::tr(
                        "Client session data directory not found",
                        "클라이언트 세션 데이터 디렉터리를 찾을 수 없습니다",
                    )
                    .to_string(),
                ),
            };
            (title, project, updated_at, content)
        }
    }
}

fn compatible_accounts(
    client: SessionClient,
    scope: Option<WbVariant>,
    group: &LinkGroup,
    accounts: &[Value],
) -> Vec<Value> {
    accounts
        .iter()
        .filter(|target| {
            let Some(uid) = account::get_str(target, "uid") else {
                return false;
            };
            if group
                .members
                .iter()
                .any(|member| member.uid == uid && member.state == MemberState::Active)
            {
                return false;
            }
            validate_target_compatibility(client, scope, target).is_ok()
                && (client == SessionClient::Workbuddy || vscode_session::is_safe_uid(&uid))
        })
        .map(account::account_meta)
        .collect()
}

fn content_count(content: &ContentState) -> Option<usize> {
    match content {
        ContentState::Ready(snapshot) => Some(snapshot.normalized.record_count),
        _ => None,
    }
}

fn content_preview(content: &ContentState) -> Vec<Value> {
    let ContentState::Ready(snapshot) = content else {
        return Vec::new();
    };
    let mut recent = snapshot
        .text
        .lines()
        .rev()
        .filter_map(|line| {
            let record: Value = serde_json::from_str(line).ok()?;
            let text = record_text(&record)?;
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            let speaker = match record
                .get("role")
                .or_else(|| record.get("type"))
                .and_then(Value::as_str)
            {
                Some("user") => i18n::tr("User", "사용자"),
                Some("assistant") => i18n::tr("Assistant", "어시스턴트"),
                _ => i18n::tr("Record", "기록"),
            };
            Some(json!({ "speaker": speaker, "text": text.chars().take(240).collect::<String>() }))
        })
        .take(6)
        .collect::<Vec<_>>();
    recent.reverse();
    recent
}

fn record_text(record: &Value) -> Option<String> {
    match record {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items.iter().filter_map(record_text).collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        Value::Object(fields) => ["text", "content", "message", "parts"]
            .iter()
            .find_map(|key| fields.get(*key).and_then(record_text)),
        _ => None,
    }
}

fn content_state_name(content: &ContentState) -> &'static str {
    match content {
        ContentState::Ready(_) => "ready",
        ContentState::Missing => "missing",
        ContentState::Unavailable(_) => "unavailable",
    }
}

fn summary_text(status: &str) -> &'static str {
    match status {
        "latest" => i18n::tr(
            "Linked copies have identical content",
            "연결된 복사본의 내용이 동일합니다",
        ),
        "behind" => i18n::tr(
            "Some copies are behind and can be safely synced",
            "일부 복사본이 뒤처져 있으며 안전하게 동기화할 수 있습니다",
        ),
        "diverge" => i18n::tr(
            "Copies have different updates; choose a source",
            "여러 복사본에 서로 다른 업데이트가 있습니다. 원본을 선택해야 합니다",
        ),
        "missing" => i18n::tr(
            "Some copies have missing content",
            "일부 복사본의 내용이 없습니다",
        ),
        _ => i18n::tr(
            "Cannot confirm copy status right now",
            "현재 복사본 상태를 확인할 수 없습니다",
        ),
    }
}

fn basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::session_link::{
        full_digest_of, normalize_jsonl, ContentSnapshot, SyncMode,
    };

    #[test]
    fn restart_only_when_running_client_may_be_using_write_target() {
        assert!(should_restart_running_target(
            true,
            Some("target"),
            "target"
        ));
        assert!(!should_restart_running_target(
            true,
            Some("other"),
            "target"
        ));
        assert!(should_restart_running_target(true, None, "target"));
        assert!(!should_restart_running_target(
            false,
            Some("target"),
            "target"
        ));
    }

    #[test]
    fn plan_editor_restart_only_closes_when_target_involves_current_login() {
        let targets = ["uid-a", "uid-b"];
        // 未运行：无论是否授权、是否涉及，都不关（也不主动拉起）。
        assert!(!plan_editor_restart(false, Some("uid-a"), &targets, true, "hint").unwrap());
        assert!(!plan_editor_restart(false, Some("uid-a"), &targets, false, "hint").unwrap());
        // 运行中但不涉及当前登录账号：不关、不报错（即使未授权）。
        assert!(!plan_editor_restart(true, Some("uid-other"), &targets, false, "hint").unwrap());
        assert!(!plan_editor_restart(true, Some("uid-other"), &targets, true, "hint").unwrap());
        // 空目标集合 + 读得到当前账号：同样不涉及。
        assert!(!plan_editor_restart(true, Some("uid-a"), &[], false, "hint").unwrap());
        // 运行中且涉及：未授权 → 原文案报错（逐字对齐既有入口文案）。
        assert_eq!(
            plan_editor_restart(
                true,
                Some("uid-a"),
                &targets,
                false,
                vscode_sync_running_hint()
            )
            .unwrap_err(),
            "VS Code is running. Please fully quit it before syncing sessions."
        );
        assert_eq!(
            plan_editor_restart(
                true,
                Some("uid-b"),
                &targets,
                false,
                vscode_add_member_running_hint()
            )
            .unwrap_err(),
            "VS Code is running. Please fully quit it before adding a linked account."
        );
        // 运行中且涉及 + 已授权：关闭。
        assert!(plan_editor_restart(true, Some("uid-a"), &targets, true, "hint").unwrap());
        // 读不到当前登录账号：保守按涉及处理（未授权 → 报错；已授权 → 关闭）。
        assert_eq!(
            plan_editor_restart(true, None, &targets, false, vscode_sync_running_hint())
                .unwrap_err(),
            "VS Code is running. Please fully quit it before syncing sessions."
        );
        assert!(plan_editor_restart(true, None, &targets, true, "hint").unwrap());
    }

    #[test]
    fn settle_editor_window_reports_restart_only_when_closed_here() {
        // 未关过（本来没运行或关闭前已退出）：不加字段、不调用重开。
        let report = settle_editor_window(
            Some(VscodeEditorGuard { relaunch: None }),
            Ok(json!({ "errors": [] })),
        )
        .unwrap();
        assert!(report.get("restartedEditor").is_none());
        assert!(report["errors"].as_array().unwrap().is_empty());

        // 本次关过且重开成功：restartedEditor = true。
        let report = settle_editor_window(
            Some(VscodeEditorGuard {
                relaunch: Some(Box::new(|| Ok(()))),
            }),
            Ok(json!({ "errors": [] })),
        )
        .unwrap();
        assert_eq!(report["restartedEditor"], json!(true));
    }

    #[test]
    fn settle_editor_window_degrades_restart_failure_without_failing_operation() {
        // 重开失败：操作仍返回 Ok，失败信息进 errors[]（不能只报成功）。
        let report = settle_editor_window(
            Some(VscodeEditorGuard {
                relaunch: Some(Box::new(|| Err("open 超时".to_string()))),
            }),
            Ok(json!({ "errors": [] })),
        )
        .unwrap();
        assert!(report.get("restartedEditor").is_none());
        let errors = report["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        let message = errors[0]["error"].as_str().unwrap();
        assert!(message.contains("VS Code failed to reopen"), "{message}");
        assert!(message.contains("open 超时"), "{message}");

        // 没有 errors 数组（add_member 形态）：失败信息挂顶层 editorError。
        let report = settle_editor_window(
            Some(VscodeEditorGuard {
                relaunch: Some(Box::new(|| Err("open 超时".to_string()))),
            }),
            Ok(json!({ "status": "linked" })),
        )
        .unwrap();
        assert_eq!(report["status"], json!("linked"));
        assert!(report["editorError"]
            .as_str()
            .unwrap()
            .contains("VS Code failed to reopen"));
    }

    #[test]
    fn settle_editor_window_relaunches_on_failure_and_keeps_original_error() {
        // 操作失败：必须 best-effort 重开，原错误保留。
        let error = settle_editor_window(
            Some(VscodeEditorGuard {
                relaunch: Some(Box::new(|| Ok(()))),
            }),
            Err("同步失败".to_string()),
        )
        .unwrap_err();
        assert_eq!(error, "同步失败");

        // 重开也失败：两条信息都要给用户（他不知道编辑器还关着，也不知道为什么）。
        let error = settle_editor_window(
            Some(VscodeEditorGuard {
                relaunch: Some(Box::new(|| Err("open 超时".to_string()))),
            }),
            Err("同步失败".to_string()),
        )
        .unwrap_err();
        assert!(error.starts_with("同步失败"), "{error}");
        assert!(error.contains("open 超时"), "{error}");
    }

    #[test]
    fn settle_editor_window_is_noop_for_non_plugin_clients() {
        let report = settle_editor_window(None, Ok(json!({ "synced": [] }))).unwrap();
        assert!(report.get("restartedEditor").is_none());
        assert!(report.get("editorError").is_none());
        assert_eq!(
            settle_editor_window(None, Err("原错误".to_string())).unwrap_err(),
            "原错误"
        );
    }

    #[test]
    fn copy_linked_sessions_rejects_invalid_requests_before_touching_editor() {
        // 非插件客户端：直接拒绝（该入口尚未接入）。
        let error = copy_linked_sessions(
            SessionClient::Workbuddy,
            "source",
            "target",
            &["conv".to_string()],
            false,
        )
        .unwrap_err();
        assert!(
            error.contains("does not support adding linked sessions"),
            "{error}"
        );

        // 空勾选：直接拒绝。
        let error = copy_linked_sessions(SessionClient::VscodeExt, "source", "target", &[], false)
            .unwrap_err();
        assert!(error.contains("Missing sessions to copy"), "{error}");

        // 账号不存在：直接拒绝（不会走到编辑器生命周期窗口）。
        let error = copy_linked_sessions(
            SessionClient::VscodeExt,
            "no-such-source",
            "no-such-target",
            &["conv".to_string()],
            true,
        )
        .unwrap_err();
        assert!(error.contains("Source account does not exist"), "{error}");
    }

    #[test]
    fn explicit_group_unify_mode_reaches_sync_kernel() {
        let selection = parse_selection("group", "token", "unifyOverwrite").unwrap();
        assert_eq!(selection.mode, SyncMode::UnifyOverwrite);
        assert!(parse_selection("group", "token", "unsupported").is_err());
        // 会话组统一入口只提交正文模式：只同步归档由切号预览的 archiveAction 表达，
        // 不在统一入口的模式白名单里，也不影响它对 ahead 的 unifyOverwrite 改写。
        assert!(parse_selection("group", "token", "statusOnly").is_err());
    }

    fn member_view(
        member_id: &str,
        account_key: &str,
        state: MemberState,
        content: ContentState,
    ) -> MemberView {
        MemberView {
            member: LinkMember {
                member_id: member_id.to_string(),
                account_id: Some(account_key.to_string()),
                uid: format!("uid-{member_id}"),
                session_id: format!("session-{member_id}"),
                variant: Some(WbVariant::Cn),
                state,
                linked_at: 0,
                last_synced_at: None,
            },
            account_key: account_key.to_string(),
            account_name: account_key.to_string(),
            title: member_id.to_string(),
            project_label: String::new(),
            updated_at: 0,
            content,
            reason: String::new(),
            version_status: "unknown",
        }
    }

    fn ready_content(member_id: &str) -> ContentState {
        let text = format!(r#"{{"id":"{member_id}"}}"#);
        ContentState::Ready(ContentSnapshot {
            full_digest: full_digest_of(text.as_bytes()),
            normalized: normalize_jsonl(&text, &format!("session-{member_id}")).unwrap(),
            text,
        })
    }

    fn ready_lines(ids: &[&str]) -> ContentState {
        let text = ids
            .iter()
            .map(|id| format!(r#"{{"id":"{id}"}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        ContentState::Ready(ContentSnapshot {
            full_digest: full_digest_of(text.as_bytes()),
            normalized: normalize_jsonl(&text, "unrelated-session-id").unwrap(),
            text,
        })
    }

    fn active_indexes(views: &[MemberView]) -> Vec<usize> {
        views
            .iter()
            .enumerate()
            .filter(|(_, view)| view.member.state == MemberState::Active)
            .map(|(index, _)| index)
            .collect()
    }

    fn pair(
        matrix: &mut HashMap<(usize, usize), (SyncVerdict, String)>,
        source: usize,
        target: usize,
        verdict: SyncVerdict,
    ) {
        matrix.insert((source, target), (verdict, format!("{verdict:?}")));
    }

    #[test]
    fn client_namespaces_and_scope_are_explicit() {
        assert_eq!(
            SessionClient::parse("workbuddy")
                .unwrap()
                .store_paths()
                .link_namespace,
            crate::modules::session::LinkNamespace::WorkBuddy
        );
        assert!(SessionClient::parse("CodeBuddy").is_err());
        assert!(SessionClient::Workbuddy
            .validate_scope(Some(WbVariant::Ai))
            .is_err());
        assert!(SessionClient::CodebuddyIde.validate_scope(None).is_err());
        assert!(SessionClient::CodebuddyIde
            .validate_scope(Some(WbVariant::Ai))
            .is_ok());
    }

    #[test]
    fn plugin_accepts_both_regions_while_ide_requires_matching_region() {
        for variant in [WbVariant::Cn, WbVariant::Ai] {
            let target = json!({ "uid": "target", "variant": variant.as_str() });
            assert!(validate_target_compatibility(SessionClient::VscodeExt, None, &target).is_ok());
            for scope in [WbVariant::Cn, WbVariant::Ai] {
                assert_eq!(
                    validate_target_compatibility(
                        SessionClient::CodebuddyIde,
                        Some(scope),
                        &target
                    )
                    .is_ok(),
                    scope == variant
                );
            }
        }
    }

    #[test]
    fn workspace_label_never_exposes_or_guesses_a_path() {
        assert_eq!(basename("C:\\users\\private\\project"), "project");
        assert_eq!(basename("/users/private/project/"), "project");
    }

    /// TempDir + injected store root: link-store tests must never touch the real `~/.wb-switch`.
    struct TempStore {
        dir: PathBuf,
        paths: SessionPaths,
    }

    impl TempStore {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wb_switch_groups_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let paths = SessionPaths {
                store_root: dir.join("store"),
                data_root: dir.join("data"),
                auth_file: dir.join("auth.info"),
                link_namespace: crate::modules::session::LinkNamespace::WorkBuddy,
            };
            std::fs::create_dir_all(&paths.store_root).unwrap();
            Self { dir, paths }
        }

        fn seed(&self, groups: Vec<LinkGroup>) {
            let store = crate::modules::session_link::LinkStore {
                version: crate::modules::session_link::LINK_STORE_VERSION,
                revision: 1,
                groups,
            };
            std::fs::write(
                self.paths.session_links_file(),
                serde_json::to_string_pretty(&store).unwrap(),
            )
            .unwrap();
        }

        fn store(&self) -> crate::modules::session_link::LinkStore {
            match session_link::load_store(&self.paths) {
                StoreState::Ready(store) => store,
                other => panic!("存储应为可读状态：{other:?}"),
            }
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn stored_member(member_id: &str, uid: &str, state: MemberState) -> LinkMember {
        LinkMember {
            member_id: member_id.to_string(),
            account_id: Some(format!("account-{uid}")),
            uid: uid.to_string(),
            session_id: format!("session-{uid}"),
            variant: Some(WbVariant::Cn),
            state,
            linked_at: 1,
            last_synced_at: None,
        }
    }

    fn remove_member_at(paths: &SessionPaths, group_id: &str, member_id: &str) -> Value {
        let (group_removed, remaining) = session_link::with_link_store_write(paths, |store| {
            super::remove_member_in(store, SessionClient::Workbuddy, None, group_id, member_id)
        })
        .unwrap();
        json!({
            "status": if group_removed { "groupRemoved" } else { "removed" },
            "remaining": remaining,
        })
    }

    fn delete_group_at(paths: &SessionPaths, group_id: &str) -> Result<usize, String> {
        session_link::with_link_store_write(paths, |store| {
            super::delete_group_in(store, SessionClient::Workbuddy, None, group_id)
        })
    }

    #[test]
    fn removing_a_member_drops_its_pair_bases_and_keeps_the_rest() {
        let store = TempStore::new("remove-member");
        let mut group = LinkGroup {
            id: "g-1".to_string(),
            variant: WbVariant::Cn,
            created_at: 1,
            members: vec![
                stored_member("m-a", "uid-a", MemberState::Active),
                stored_member("m-b", "uid-b", MemberState::Active),
                stored_member("m-c", "uid-c", MemberState::Stale),
            ],
            pair_bases: Vec::new(),
        };
        for (left, right, ref_name) in [
            ("m-a", "m-b", "base-ab"),
            ("m-a", "m-c", "base-ac"),
            ("m-b", "m-c", "base-bc"),
        ] {
            let (first, second) = session_link::pair_key(left, right);
            group
                .pair_bases
                .push(crate::modules::session_link::PairBase {
                    member_ids: [first, second],
                    baseline_ref: ref_name.to_string(),
                    normalization_version: 1,
                });
        }
        store.seed(vec![group]);

        let result = remove_member_at(&store.paths, "g-1", "m-a");

        assert_eq!(result["status"], "removed");
        assert_eq!(result["remaining"], 2);
        let saved = store.store();
        assert_eq!(saved.groups.len(), 1);
        let members = &saved.groups[0].members;
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|member| member.member_id != "m-a"));
        let refs = saved.groups[0]
            .pair_bases
            .iter()
            .map(|pair| pair.baseline_ref.as_str())
            .collect::<Vec<_>>();
        assert_eq!(refs, vec!["base-bc"]);
        assert_eq!(saved.revision, 2);
        assert!(session_link::validate_store(&saved).is_ok());
    }

    #[test]
    fn removing_a_superseded_member_keeps_the_group() {
        // 共同旧版 / 已替代成员同样可移除：组保留，remaining 是剩余人数，引用它的配对基线被清掉。
        let store = TempStore::new("remove-superseded");
        let mut group = LinkGroup {
            id: "g-1".to_string(),
            variant: WbVariant::Cn,
            created_at: 1,
            members: vec![
                stored_member("m-a", "uid-a", MemberState::Active),
                stored_member("m-b", "uid-b", MemberState::Active),
                stored_member("m-old", "uid-old", MemberState::Superseded),
            ],
            pair_bases: Vec::new(),
        };
        for (left, right, ref_name) in [
            ("m-a", "m-b", "base-ab"),
            ("m-a", "m-old", "base-a-old"),
            ("m-b", "m-old", "base-b-old"),
        ] {
            let (first, second) = session_link::pair_key(left, right);
            group
                .pair_bases
                .push(crate::modules::session_link::PairBase {
                    member_ids: [first, second],
                    baseline_ref: ref_name.to_string(),
                    normalization_version: 1,
                });
        }
        store.seed(vec![group]);

        let result = remove_member_at(&store.paths, "g-1", "m-old");

        assert_eq!(result["status"], "removed");
        assert_eq!(result["remaining"], 2);
        let saved = store.store();
        assert_eq!(saved.groups.len(), 1);
        assert_eq!(saved.groups[0].id, "g-1");
        assert!(saved.groups[0]
            .members
            .iter()
            .all(|member| member.member_id != "m-old"));
        let refs = saved.groups[0]
            .pair_bases
            .iter()
            .map(|pair| pair.baseline_ref.as_str())
            .collect::<Vec<_>>();
        assert_eq!(refs, vec!["base-ab"]);
        assert!(session_link::validate_store(&saved).is_ok());
    }

    #[test]
    fn removing_the_last_member_deletes_the_group() {
        let store = TempStore::new("remove-last");
        store.seed(vec![
            LinkGroup {
                id: "g-1".to_string(),
                variant: WbVariant::Cn,
                created_at: 1,
                members: vec![stored_member("m-a", "uid-a", MemberState::Active)],
                pair_bases: Vec::new(),
            },
            LinkGroup {
                id: "g-2".to_string(),
                variant: WbVariant::Cn,
                created_at: 1,
                members: vec![stored_member("m-z", "uid-z", MemberState::Active)],
                pair_bases: Vec::new(),
            },
        ]);

        let result = remove_member_at(&store.paths, "g-1", "m-a");

        assert_eq!(result["status"], "groupRemoved");
        assert_eq!(result["remaining"], 0);
        let saved = store.store();
        assert_eq!(saved.groups.len(), 1);
        assert_eq!(saved.groups[0].id, "g-2");
        assert_eq!(saved.revision, 2);
        assert!(session_link::validate_store(&saved).is_ok());
    }

    #[test]
    fn removing_an_unknown_group_or_member_reports_a_readable_error() {
        let store = TempStore::new("remove-unknown");
        store.seed(vec![LinkGroup {
            id: "g-1".to_string(),
            variant: WbVariant::Cn,
            created_at: 1,
            members: vec![stored_member("m-a", "uid-a", MemberState::Active)],
            pair_bases: Vec::new(),
        }]);

        let missing_group = session_link::with_link_store_write(&store.paths, |link_store| {
            super::remove_member_in(link_store, SessionClient::Workbuddy, None, "missing", "m-a")
        })
        .unwrap_err();
        assert_eq!(
            missing_group,
            "Session group does not exist or does not belong to the current client"
        );

        let missing_member = session_link::with_link_store_write(&store.paths, |link_store| {
            super::remove_member_in(link_store, SessionClient::Workbuddy, None, "g-1", "nope")
        })
        .unwrap_err();
        assert_eq!(
            missing_member,
            "Member does not exist; please refresh and retry"
        );

        // 失败的读改写不得落盘，更不得推进 revision。
        let saved = store.store();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.groups[0].members.len(), 1);
        assert!(session_link::validate_store(&saved).is_ok());
    }

    #[test]
    fn deleting_a_group_drops_all_members_and_keeps_the_others() {
        let store = TempStore::new("delete-group");
        let mut group = LinkGroup {
            id: "g-1".to_string(),
            variant: WbVariant::Cn,
            created_at: 1,
            members: vec![
                stored_member("m-a", "uid-a", MemberState::Active),
                stored_member("m-b", "uid-b", MemberState::Stale),
            ],
            pair_bases: Vec::new(),
        };
        let (first, second) = session_link::pair_key("m-a", "m-b");
        group
            .pair_bases
            .push(crate::modules::session_link::PairBase {
                member_ids: [first, second],
                baseline_ref: "base-ab".to_string(),
                normalization_version: 1,
            });
        store.seed(vec![
            group,
            LinkGroup {
                id: "g-2".to_string(),
                variant: WbVariant::Cn,
                created_at: 1,
                members: vec![stored_member("m-z", "uid-z", MemberState::Active)],
                pair_bases: Vec::new(),
            },
        ]);

        let removed = delete_group_at(&store.paths, "g-1").unwrap();

        assert_eq!(removed, 2);
        let saved = store.store();
        assert_eq!(saved.groups.len(), 1);
        assert_eq!(saved.groups[0].id, "g-2");
        assert_eq!(saved.groups[0].members.len(), 1);
        assert_eq!(saved.revision, 2);
        assert!(session_link::validate_store(&saved).is_ok());
    }

    #[test]
    fn deleting_an_unknown_group_reports_a_readable_error_without_writing() {
        let store = TempStore::new("delete-unknown");
        store.seed(vec![LinkGroup {
            id: "g-1".to_string(),
            variant: WbVariant::Cn,
            created_at: 1,
            members: vec![stored_member("m-a", "uid-a", MemberState::Active)],
            pair_bases: Vec::new(),
        }]);

        let error = delete_group_at(&store.paths, "missing").unwrap_err();

        assert_eq!(
            error,
            "Session group does not exist or does not belong to the current client"
        );
        // 失败的读改写不得落盘，更不得推进 revision。
        let saved = store.store();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.groups.len(), 1);
        assert!(session_link::validate_store(&saved).is_ok());
    }

    #[test]
    fn delete_group_rejects_invalid_scope_combinations() {
        let vscode =
            delete_group(SessionClient::VscodeExt, Some(WbVariant::Ai), "g-1").unwrap_err();
        assert!(vscode.contains("Only CodeBuddy IDE"));
        let workbuddy =
            delete_group(SessionClient::Workbuddy, Some(WbVariant::Cn), "g-1").unwrap_err();
        assert!(workbuddy.contains("Only CodeBuddy IDE"));
        let ide = delete_group(SessionClient::CodebuddyIde, None, "g-1").unwrap_err();
        assert!(ide.contains("must specify the cn or ai edition"));
    }

    #[test]
    fn unlink_rejects_invalid_scope_combinations() {
        let vscode =
            remove_member(SessionClient::VscodeExt, Some(WbVariant::Ai), "g-1", "m-a").unwrap_err();
        assert!(vscode.contains("Only CodeBuddy IDE"));
        let workbuddy =
            remove_member(SessionClient::Workbuddy, Some(WbVariant::Cn), "g-1", "m-a").unwrap_err();
        assert!(workbuddy.contains("Only CodeBuddy IDE"));
        let ide = remove_member(SessionClient::CodebuddyIde, None, "g-1", "m-a").unwrap_err();
        assert!(ide.contains("must specify the cn or ai edition"));
    }

    #[test]
    fn aggregate_selects_a_safe_dominant_source_and_marks_only_fast_forward_targets_behind() {
        let mut views = vec![
            member_view(
                "source",
                "account-a",
                MemberState::Active,
                ready_content("a"),
            ),
            member_view(
                "behind",
                "account-b",
                MemberState::Active,
                ready_content("b"),
            ),
        ];
        let mut matrix = HashMap::new();
        pair(&mut matrix, 0, 1, SyncVerdict::FastForward);
        pair(&mut matrix, 1, 0, SyncVerdict::Ahead);

        let active = active_indexes(&views);
        let (safe_source, summary) = aggregate_group_state(&mut views, &active, &matrix);

        assert_eq!(safe_source, Some(0));
        assert_eq!(summary, "behind");
        assert_eq!(views[0].version_status, "latest");
        assert_eq!(views[1].version_status, "behind");
    }

    #[test]
    fn aggregate_keeps_equal_latest_copies_and_uses_stable_account_order_for_operations() {
        // Input order differs from the stable account ordering on purpose.
        let mut views = vec![
            member_view(
                "latest-z",
                "account-z",
                MemberState::Active,
                ready_content("z"),
            ),
            member_view(
                "latest-a",
                "account-a",
                MemberState::Active,
                ready_content("a"),
            ),
            member_view(
                "behind",
                "account-m",
                MemberState::Active,
                ready_content("m"),
            ),
        ];
        let mut matrix = HashMap::new();
        for (source, target, verdict) in [
            (0, 1, SyncVerdict::Identical),
            (1, 0, SyncVerdict::Identical),
            (0, 2, SyncVerdict::FastForward),
            (1, 2, SyncVerdict::FastForward),
            (2, 0, SyncVerdict::Ahead),
            (2, 1, SyncVerdict::Ahead),
        ] {
            pair(&mut matrix, source, target, verdict);
        }

        let active = active_indexes(&views);
        let (safe_source, summary) = aggregate_group_state(&mut views, &active, &matrix);

        assert_eq!(safe_source, Some(1));
        assert_eq!(views[0].version_status, "latest");
        assert_eq!(views[1].version_status, "latest");
        assert_eq!(views[2].version_status, "behind");
        assert_eq!(summary, "behind");
    }

    #[test]
    fn aggregate_reports_divergence_without_inventing_a_safe_source() {
        let mut views = vec![
            member_view("a", "account-a", MemberState::Active, ready_content("a")),
            member_view("b", "account-b", MemberState::Active, ready_content("b")),
        ];
        let mut matrix = HashMap::new();
        pair(&mut matrix, 0, 1, SyncVerdict::Diverge);
        pair(&mut matrix, 1, 0, SyncVerdict::Diverge);

        let active = active_indexes(&views);
        let (safe_source, summary) = aggregate_group_state(&mut views, &active, &matrix);

        assert_eq!(safe_source, None);
        assert_eq!(summary, "diverge");
        assert_eq!(views[0].version_status, "diverge");
        assert_eq!(views[1].version_status, "diverge");
    }

    #[test]
    fn recognizes_three_equal_old_copies_and_two_independent_extensions() {
        let views = vec![
            member_view("old-a", "a", MemberState::Active, ready_lines(&["base"])),
            member_view(
                "tip-a",
                "b",
                MemberState::Active,
                ready_lines(&["base", "alice"]),
            ),
            member_view("old-b", "c", MemberState::Active, ready_lines(&["base"])),
            member_view(
                "tip-b",
                "d",
                MemberState::Active,
                ready_lines(&["base", "bob"]),
            ),
            member_view("old-c", "e", MemberState::Active, ready_lines(&["base"])),
        ];
        assert_eq!(
            common_base_branches(&views, &active_indexes(&views)),
            Some(CommonBaseBranches {
                common: vec![0, 2, 4],
                branches: vec![vec![1], vec![3]]
            })
        );
    }

    #[test]
    fn does_not_call_a_linear_history_independent_branches() {
        let views = vec![
            member_view("old", "a", MemberState::Active, ready_lines(&["base"])),
            member_view(
                "mid",
                "b",
                MemberState::Active,
                ready_lines(&["base", "alice"]),
            ),
            member_view(
                "new",
                "c",
                MemberState::Active,
                ready_lines(&["base", "alice", "bob"]),
            ),
        ];
        assert_eq!(common_base_branches(&views, &active_indexes(&views)), None);
    }

    #[test]
    fn aggregate_keeps_unknown_and_missing_distinct() {
        let mut unknown_views = vec![
            member_view("a", "account-a", MemberState::Active, ready_content("a")),
            member_view(
                "unknown",
                "account-b",
                MemberState::Active,
                ContentState::Unavailable("正文读取失败".to_string()),
            ),
        ];
        let mut unknown_matrix = HashMap::new();
        pair(&mut unknown_matrix, 0, 1, SyncVerdict::Unknown);
        pair(&mut unknown_matrix, 1, 0, SyncVerdict::Unknown);
        let active = active_indexes(&unknown_views);
        let (safe_source, summary) =
            aggregate_group_state(&mut unknown_views, &active, &unknown_matrix);
        assert_eq!(safe_source, None);
        assert_eq!(summary, "unknown");
        assert_eq!(unknown_views[1].version_status, "unknown");

        let mut missing_views = vec![
            member_view("a", "account-a", MemberState::Active, ready_content("a")),
            member_view(
                "missing",
                "account-b",
                MemberState::Active,
                ContentState::Missing,
            ),
        ];
        let mut missing_matrix = HashMap::new();
        pair(&mut missing_matrix, 0, 1, SyncVerdict::Unknown);
        pair(&mut missing_matrix, 1, 0, SyncVerdict::Unknown);
        let active = active_indexes(&missing_views);
        let (safe_source, summary) =
            aggregate_group_state(&mut missing_views, &active, &missing_matrix);
        assert_eq!(safe_source, None);
        assert_eq!(summary, "missing");
        assert_eq!(missing_views[1].version_status, "missing");
    }

    #[test]
    fn aggregate_preserves_stale_and_superseded_members_outside_active_source_selection() {
        let mut views = vec![
            member_view(
                "source",
                "account-a",
                MemberState::Active,
                ready_content("a"),
            ),
            member_view(
                "behind",
                "account-b",
                MemberState::Active,
                ready_content("b"),
            ),
            member_view(
                "stale",
                "account-c",
                MemberState::Stale,
                ContentState::Missing,
            ),
            member_view(
                "superseded",
                "account-d",
                MemberState::Superseded,
                ContentState::Missing,
            ),
        ];
        let mut matrix = HashMap::new();
        pair(&mut matrix, 0, 1, SyncVerdict::FastForward);
        pair(&mut matrix, 1, 0, SyncVerdict::Ahead);

        let active = active_indexes(&views);
        let (safe_source, summary) = aggregate_group_state(&mut views, &active, &matrix);

        assert_eq!(safe_source, Some(0));
        assert_eq!(summary, "behind");
        assert_eq!(views[2].version_status, "stale");
        assert_eq!(views[3].version_status, "superseded");
    }

    /// 汇总对象初始化形状：与 `sync_safe_batch` / `sync_unify_batch_vscode` 一致。
    fn combined_report() -> Value {
        json!({
            "client": "workbuddy",
            "groupId": "g-1",
            "synced": [],
            "skipped": [],
            "errors": [],
            "needsRecovery": false,
            "temporaryFiles": [],
        })
    }

    /// 单个目标项的报告：只给 `key` 灌入一条记录，其余合并键为空。
    fn sync_report_item(key: &str) -> Value {
        let mut item = json!({
            "synced": [],
            "skipped": [],
            "errors": [],
            "temporaryFiles": [],
        });
        item[key] = json!([{"targetMemberId": "m-1"}]);
        item
    }

    #[test]
    fn merge_sync_report_merges_every_key_and_propagates_needs_recovery() {
        let mut combined = combined_report();
        combined["synced"] = json!([{"targetMemberId": "m-0"}]);
        combined["temporaryFiles"] = json!([{"targetMemberId": "m-0", "path": "/tmp/first"}]);

        merge_sync_report(
            &mut combined,
            json!({
                "synced": [{"targetMemberId": "m-1"}],
                "skipped": [{"targetMemberId": "m-1", "reasonCode": "previewStale"}],
                "errors": [{"error": "m-1：写入失败"}],
                "needsRecovery": true,
                "temporaryFiles": [{"targetMemberId": "m-1", "path": "/tmp/second"}],
            }),
        );

        assert_eq!(combined["synced"].as_array().map(Vec::len), Some(2));
        assert_eq!(combined["skipped"].as_array().map(Vec::len), Some(1));
        assert_eq!(combined["errors"].as_array().map(Vec::len), Some(1));
        assert_eq!(combined["temporaryFiles"].as_array().map(Vec::len), Some(2));
        assert_eq!(combined["temporaryFiles"][0]["path"], json!("/tmp/first"));
        assert_eq!(combined["temporaryFiles"][1]["path"], json!("/tmp/second"));
        assert_eq!(combined["needsRecovery"], json!(true));
    }

    #[test]
    fn merge_sync_report_initializes_missing_key_instead_of_panicking() {
        for key in ["synced", "skipped", "errors", "temporaryFiles"] {
            let mut combined = combined_report();
            combined.as_object_mut().unwrap().remove(key);
            assert!(combined.get(key).is_none());

            merge_sync_report(&mut combined, sync_report_item(key));

            assert_eq!(
                combined[key],
                json!([{"targetMemberId": "m-1"}]),
                "{key} 缺失时应补成数组并保留合并内容"
            );
        }
    }

    #[test]
    fn merge_sync_report_replaces_null_key_with_merged_array() {
        for key in ["synced", "skipped", "errors", "temporaryFiles"] {
            let mut combined = combined_report();
            combined[key] = Value::Null;
            assert!(combined[key].is_null());

            merge_sync_report(&mut combined, sync_report_item(key));

            assert_eq!(
                combined[key],
                json!([{"targetMemberId": "m-1"}]),
                "{key} 为 Null 时应补成数组并保留合并内容"
            );
        }
    }
}
