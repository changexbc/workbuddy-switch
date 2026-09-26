//! CodeBuddy IDE（国内版桌面客户端）会话复制：切换账号时把勾选会话复制到目标账号。
//!
//! 会话树与 VS Code 插件同根同构（`<数据根>/<uid>/CodeBuddyIDE/<uid>/history/<工作区目录名>/`），
//! 复制内核、索引合并与关联登记都复用 `vscode_session` / `vscode_session_sync`，本模块只负责：
//!
//! - 数据根与源账号 uid 的解析（源 uid = 当前 IDE 登录 secret 里的 uid，回退本地状态文件）；
//! - 「沿用源会话 id、冲突才重随机」的复制策略（[`vscode_session::CopyIdPolicy`]）；
//! - 切换编排水位：**校验（关闭前只挡确定失败） → 关闭 IDE → 再读源 uid → 复制 → 登记关联 → 同步 → 注入凭证 → 重启**。
//!
//! 写入前提：IDE 必须已关闭（复制与同步都会被运行中的客户端覆盖）。`restart = true` 且当时
//! 在运行时由本模块负责关闭与重开；`restart = false` 时 IDE 正在运行则直接拒绝，不做「假装写入」。

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::modules::account::{self, get_str};
use crate::modules::codebuddy_cn_ide;
use crate::modules::codebuddy_ide_session_sync;
use crate::modules::config::{backup_dir, utc_iso};
use crate::modules::session::{SessionPaths, SyncSelection};
use crate::modules::vscode_session::{self, CopyItem, CODEBUDDY_IDE_STORE, IDE_COPY};

/// IDE 会话树的数据根（`…/CodeBuddyExtension/Data`）；目录不存在时 `None`。
pub fn ide_data_root() -> Option<PathBuf> {
    vscode_session::store_data_root(CODEBUDDY_IDE_STORE)
}

/// 列出某账号可复制的 IDE 会话（按工作区 hash 分桶）。
///
/// 未找到数据根时 `dataRoot` 为 `null`，调用方据此区分「未找到数据目录」与「该账号无会话」。
pub fn list_codebuddy_ide_sessions(uid: &str) -> Value {
    match ide_data_root() {
        Some(root) => vscode_session::list_sessions_in_store(CODEBUDDY_IDE_STORE, &root, uid),
        None => empty_session_list(Some(uid), None),
    }
}

/// 列出**当前 IDE 登录账号**可复制的会话（宿主列表入口）。
///
/// 未登录 / uid 非法时 `sourceUid` 为 `null`；数据根不存在时 `dataRoot` 为 `null`。
/// 两个字段独立出现，前端据此区分「请先登录」与「未找到数据目录」。
pub fn list_current_codebuddy_ide_sessions() -> Value {
    match codebuddy_cn_ide::active_cn_ide_uid().filter(|uid| vscode_session::is_safe_uid(uid)) {
        Some(uid) => list_codebuddy_ide_sessions(&uid),
        None => empty_session_list(None, ide_data_root().as_deref()),
    }
}

/// 空列表的固定形状：始终带 `sourceUid` / `sessions` / `skipped` / `dataRoot`。
fn empty_session_list(uid: Option<&str>, data_root: Option<&std::path::Path>) -> Value {
    json!({
        "sourceUid": uid,
        "sessions": [],
        "skipped": 0,
        "dataRoot": data_root.map(|path| path.to_string_lossy().into_owned()),
    })
}

/// 解析复制源端：数据根 + 当前 IDE 登录账号 uid。
///
/// 两个都会在关闭 IDE **之前**校验，避免「注定失败却已关掉用户的 IDE」。
pub(crate) fn copy_source() -> Result<(PathBuf, String), String> {
    let root = ide_data_root().ok_or_else(|| {
        "未找到 CodeBuddy IDE 数据目录，无法复制会话。请先打开 CodeBuddy IDE 并登录一次。"
            .to_string()
    })?;
    let uid = codebuddy_cn_ide::active_cn_ide_uid()
        .filter(|uid| vscode_session::is_safe_uid(uid))
        .ok_or_else(|| {
            "未检测到 CodeBuddy IDE 当前登录账号，无法定位源会话。请先在 CodeBuddy IDE 中登录后重试。"
                .to_string()
        })?;
    Ok((root, uid))
}

/// 把勾选的会话复制到目标账号（默认沿用源会话 id，目标已有同 id 时才重随机）。返回复制报告。
///
/// 源端（数据根 + 当前登录 uid）由 [`copy_source`] 在关闭 IDE **之前**解析后传入；
/// 备份根固定为 `<工具存储根>/backups/<backup_kind>/<utc_iso>/`（供人工回滚）。
pub(crate) fn copy_codebuddy_ide_sessions(
    root: &std::path::Path,
    source_uid: &str,
    target_uid: &str,
    items: &[CopyItem],
) -> Result<Value, String> {
    let backup_root = backup_dir()
        .join(CODEBUDDY_IDE_STORE.backup_kind)
        .join(utc_iso());
    copy_codebuddy_ide_sessions_in(root, &backup_root, source_uid, target_uid, items)
}

/// 复制内核入口（源端与备份根已解析）：固定数据仓与复制策略，交给共用内核执行。
pub(crate) fn copy_codebuddy_ide_sessions_in(
    root: &std::path::Path,
    backup_root: &std::path::Path,
    source_uid: &str,
    target_uid: &str,
    items: &[CopyItem],
) -> Result<Value, String> {
    vscode_session::copy_sessions_in_with(
        CODEBUDDY_IDE_STORE,
        IDE_COPY,
        root,
        backup_root,
        source_uid,
        target_uid,
        items,
    )
}

/// 切换 CodeBuddy IDE 账号，可选「先复制会话」与「把关联会话的新增内容同步过去」。
///
/// 时序：空参数直通 [`codebuddy_cn_ide::switch_account`]（行为与现网一致）→ 校验目标
/// （账号 / `access_token` / 数据目录；复制则再校验数据根，能读到 uid 时挡源=目标）
/// → 关闭（仅 `restart = true` **且当时在运行**）→ 再读源 uid 并复制、登记关联
/// → 执行勾选的同步 → 注入凭证 → 重启（`restart = true` 时）。
///
/// 复制与同步都逐条隔离：单条失败不影响其余条目与后续切换，失败原因写在报告里
/// （`errors` / `linkErrors`）。复制本身致命失败（如数据根缺失、目标不可写）时：IDE 是本次
/// 由我们关闭的就 best-effort 开回来，再返回错误，避免「IDE 关了、会话也没复制成」的双输。
pub fn switch_codebuddy_cn_ide_with_copy(
    account_id: &str,
    restart: bool,
    items: &[CopyItem],
    sync_selections: &[SyncSelection],
) -> Result<Value, String> {
    if items.is_empty() && sync_selections.is_empty() {
        return codebuddy_cn_ide::switch_account(account_id, restart);
    }
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let target_uid = get_str(&acc, "uid")
        .ok_or_else(|| "账号缺少 uid，无法定位 CodeBuddy IDE 会话目录".to_string())?;

    // 与 [`codebuddy_cn_ide::switch_account`] 同序：先把「注定失败」的目标挡在关闭之前。
    let (_, data_dir) = codebuddy_cn_ide::validate_switch_target(account_id)?;

    // 复制与同步都拒绝「IDE 正在运行」：手动模式（`restart = false`）直接报错，不假装写入。
    let ide_running = codebuddy_cn_ide::is_codebuddy_cn_running();
    if !restart && ide_running {
        return Err(
            "检测到 CodeBuddy IDE 正在运行，请先完全退出后再操作，否则写入会被 IDE 覆盖。"
                .to_string(),
        );
    }

    // 关闭前只做确定失败的检查。源 uid 的权威读取放在关闭之后：IDE 运行时
    // `state.vscdb` 可能被占用，此时回退状态文件会误判成「上次用本工具切过的账号」。
    if !items.is_empty() {
        if ide_data_root().is_none() {
            return Err(
                "未找到 CodeBuddy IDE 数据目录，无法复制会话。请先打开 CodeBuddy IDE 并登录一次。"
                    .to_string(),
            );
        }
        match codebuddy_cn_ide::active_cn_ide_uid().filter(|uid| vscode_session::is_safe_uid(uid)) {
            Some(uid) if uid == target_uid => {
                return Err("源账号与目标账号相同，无需复制会话".to_string());
            }
            None if !ide_running => {
                return Err(
                    "未检测到 CodeBuddy IDE 当前登录账号，无法定位源会话。请先在 CodeBuddy IDE 中登录后重试。"
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    // 仅当 IDE 当时在跑且本次要重启时才关。`closed` 表示「是我们关掉的」：
    // 失败才负责开回来；`restart = true` 但本来没运行，失败路径不得把没开过的 IDE 拉起来。
    let closed = restart && ide_running;
    if closed {
        eprintln!("[codebuddy-ide-session] closing CodeBuddy CN…");
        codebuddy_cn_ide::close_codebuddy_cn(20)?;
    }

    let copy_report = if items.is_empty() {
        None
    } else {
        let (root, source_uid) = match copy_source() {
            Ok(source) => source,
            Err(error) => {
                relaunch_if_closed(closed);
                return Err(error);
            }
        };
        if source_uid == target_uid {
            relaunch_if_closed(closed);
            return Err("源账号与目标账号相同，无需复制会话".to_string());
        }
        match copy_codebuddy_ide_sessions(&root, &source_uid, &target_uid, items) {
            Ok(mut report) => {
                // 复制成功即登记「源 ↔ 副本」关联；登记失败不回滚复制，只写进报告。
                let link_errors = codebuddy_ide_session_sync::register_copied_sessions(
                    &root,
                    &SessionPaths::for_codebuddy_ide(),
                    account::variant_of(&acc),
                    &report,
                );
                if !link_errors.is_empty() {
                    report["linkErrors"] = json!(link_errors);
                }
                Some(report)
            }
            Err(error) => {
                relaunch_if_closed(closed);
                return Err(error);
            }
        }
    };

    let sync_report = if sync_selections.is_empty() {
        None
    } else {
        // 同步失败不阻断切换：报告形状与成功路径一致，错误挂在 `errors` 里。
        Some(
            match codebuddy_ide_session_sync::sync_selected(&acc, sync_selections) {
                Ok(report) => report,
                Err(error) => json!({
                    "synced": [],
                    "skipped": [],
                    "errors": [{ "error": error }],
                }),
            },
        )
    };

    let copied = copy_report
        .as_ref()
        .map(|report| count_items(report, "copied"))
        .unwrap_or(0);
    let synced = sync_report
        .as_ref()
        .map(|report| count_items(report, "synced"))
        .unwrap_or(0);

    match codebuddy_cn_ide::inject_session_and_finish(account_id, &acc, &data_dir, restart) {
        Ok(mut result) => {
            if let Some(report) = copy_report {
                result["sessionCopy"] = report;
            }
            if let Some(report) = sync_report {
                result["sessionSync"] = report;
            }
            Ok(result)
        }
        Err(error) => {
            let mut done: Vec<String> = Vec::new();
            if copied > 0 {
                done.push(format!("已成功复制 {copied} 个会话到目标账号"));
            }
            if synced > 0 {
                done.push(format!("已成功同步 {synced} 个会话"));
            }
            let error = if done.is_empty() {
                error
            } else {
                format!(
                    "{error}（注意：{}，但账号切换未完成，请重试切换）",
                    done.join("、")
                )
            };
            // 注入失败且 IDE 是本次我们关闭的：best-effort 开回来再报错，别让用户两头落空。
            if closed {
                match codebuddy_cn_ide::launch_codebuddy_cn() {
                    Ok(()) => Err(error),
                    Err(launch_error) => Err(format!("{error}\n\n{launch_error}")),
                }
            } else {
                Err(error)
            }
        }
    }
}

/// 报告里某一类条目的数量（缺字段按 0）。
fn count_items(report: &Value, key: &str) -> usize {
    report
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

/// 失败兜底：只在本次确实关掉 IDE 时才 best-effort 开回来。
fn relaunch_if_closed(closed: bool) {
    if closed {
        let _ = codebuddy_cn_ide::launch_codebuddy_cn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::vscode_session::{history_root_in, read_json};
    use std::collections::BTreeSet;
    use std::path::Path;

    const WS: &str = "0123456789abcdef0123456789abcdef";
    const WS2: &str = "fedcba9876543210fedcba9876543210";
    const CONV_OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CONV_EXISTING: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const MSG_1: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MSG_2: &str = "cccccccccccccccccccccccccccccccc";
    const REQ_1: &str = "dddddddddddddddddddddddddddddddd";
    const SRC_UID: &str = "uid-src-0001";
    const DST_UID: &str = "uid-dst-0002";

    struct Fixture {
        root: PathBuf,
        backup: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "wb_switch_codebuddy_ide_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            let root = base.join("Data");
            let backup = base.join("backup");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&backup).unwrap();
            Self { root, backup }
        }

        fn src_ws_dir(&self) -> PathBuf {
            ide_ws_dir(&self.root, SRC_UID, WS)
        }

        fn dst_ws_dir(&self) -> PathBuf {
            ide_ws_dir(&self.root, DST_UID, WS)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(base) = self.root.parent() {
                let _ = std::fs::remove_dir_all(base);
            }
        }
    }

    /// 测试内定位工作区目录：与生产同构（`<root>/<uid>/CodeBuddyIDE/<uid>/history/<ws>`）。
    fn ide_ws_dir(root: &Path, uid: &str, workspace_hash: &str) -> PathBuf {
        history_root_in(CODEBUDDY_IDE_STORE, root, uid).join(workspace_hash)
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// 造一条源会话：`index.json` + 两条消息（含 `extra.requestId` 引用）+ 一个附件。
    fn seed_source(fixture: &Fixture) {
        let ws_dir = fixture.src_ws_dir();
        write(
            &ws_dir.join("index.json"),
            &format!(
                r#"{{"conversations":[{{"id":"{CONV_OLD}","type":"craft","name":"测试会话","createdAt":"2026-09-23T16:22:52.034Z","lastMessageAt":"2026-09-23T16:23:14.088Z"}}],"current":"{CONV_OLD}"}}"#
            ),
        );
        let conv_dir = ws_dir.join(CONV_OLD);
        write(
            &conv_dir.join("index.json"),
            &format!(
                r#"{{"messages":[{{"id":"{MSG_1}","type":"text","role":"user","isComplete":true}},{{"id":"{MSG_2}","type":"text","role":"assistant","isComplete":false}}],"requests":[{{"id":"{REQ_1}","type":"craft","messages":["{MSG_1}","{MSG_2}"],"state":"complete","startedAt":1790180582570}}]}}"#
            ),
        );
        write(
            &conv_dir.join(format!("messages/{MSG_1}.json")),
            &format!(
                r#"{{"role":"user","message":"{{\"role\":\"user\",\"content\":\"你好\"}}","id":"{MSG_1}","extra":"{{\"requestId\":\"{REQ_1}\",\"modelId\":\"hy3\"}}","createdAt":"2026-09-23T16:23:11.563Z"}}"#
            ),
        );
        write(
            &conv_dir.join(format!("messages/{MSG_2}.json")),
            &format!(
                r#"{{"role":"assistant","message":"{{\"role\":\"assistant\",\"content\":\"在的\"}}","id":"{MSG_2}","extra":"{{\"requestId\":\"{REQ_1}\",\"modelId\":\"hy3\"}}","createdAt":"2026-09-23T16:23:12.001Z"}}"#
            ),
        );
        std::fs::create_dir_all(conv_dir.join("assets")).unwrap();
        std::fs::write(conv_dir.join("assets/图片.1.jpeg"), b"\x01\x02\x03binary").unwrap();
    }

    /// 造目标账号既有的会话（另一条 id）。
    fn seed_target_index(fixture: &Fixture) {
        write(
            &fixture.dst_ws_dir().join("index.json"),
            &format!(
                r#"{{"conversations":[{{"id":"{CONV_EXISTING}","type":"craft","name":"目标既有"}}],"current":"{CONV_EXISTING}"}}"#
            ),
        );
    }

    fn copy_once(fixture: &Fixture, workspace_hash: &str) -> Value {
        copy_codebuddy_ide_sessions_in(
            &fixture.root,
            &fixture.backup,
            SRC_UID,
            DST_UID,
            &[CopyItem {
                workspace_hash: workspace_hash.to_string(),
                conversation_id: CONV_OLD.to_string(),
            }],
        )
        .expect("copy ok")
    }

    #[test]
    fn list_reports_sessions_and_skips_corrupted_index() {
        let fixture = Fixture::new("list");
        seed_source(&fixture);
        // 第二个工作区：损坏的 index.json（应计入 skipped）；第三个：合法但无 current。
        let broken = history_root_in(CODEBUDDY_IDE_STORE, &fixture.root, SRC_UID).join(WS2);
        write(&broken.join("index.json"), "{ not json");
        let no_current = history_root_in(CODEBUDDY_IDE_STORE, &fixture.root, SRC_UID)
            .join("11111111111111111111111111111111");
        write(
            &no_current.join("index.json"),
            &format!(r#"{{"conversations":[{{"id":"{CONV_EXISTING}","name":"无 current"}}]}}"#),
        );

        let result =
            vscode_session::list_sessions_in_store(CODEBUDDY_IDE_STORE, &fixture.root, SRC_UID);
        let sessions = result.get("sessions").and_then(Value::as_array).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(result.get("skipped").and_then(Value::as_u64), Some(1));
        let first = sessions
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(CONV_OLD))
            .expect("含正文的会话在列表里");
        assert_eq!(first.get("workspaceHash").and_then(Value::as_str), Some(WS));
        assert_eq!(first.get("title").and_then(Value::as_str), Some("测试会话"));
        assert_eq!(first.get("hasHistory").and_then(Value::as_bool), Some(true));
        assert!(first.get("updatedAt").and_then(Value::as_i64).unwrap() > 0);
    }

    #[test]
    fn list_rejects_default_and_public_uid() {
        let fixture = Fixture::new("list-bad-uid");
        seed_source(&fixture);
        for uid in ["default", "Public", "a/b", "..", ""] {
            let result =
                vscode_session::list_sessions_in_store(CODEBUDDY_IDE_STORE, &fixture.root, uid);
            assert_eq!(
                result
                    .get("sessions")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                Some(0),
                "{uid} 不应列出会话"
            );
        }
    }

    /// 未登录空列表也必须带 `dataRoot`（与「已登录但无会话」区分）；缺字段会让前端落到错误空态。
    #[test]
    fn empty_list_payload_always_includes_data_root() {
        let payload = empty_session_list(None, None);
        assert!(payload.get("sourceUid").unwrap().is_null());
        assert!(payload.get("dataRoot").unwrap().is_null());
        assert_eq!(
            payload
                .get("sessions")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        assert_eq!(payload.get("skipped").and_then(Value::as_u64), Some(0));

        let root = std::env::temp_dir();
        let payload = empty_session_list(None, Some(root.as_path()));
        assert!(payload.get("sourceUid").unwrap().is_null());
        assert_eq!(
            payload.get("dataRoot").and_then(Value::as_str),
            Some(root.to_string_lossy().as_ref())
        );
    }

    /// 空目标工作区：沿用源会话 id，直接可读；合并后必须写入 `current`。
    #[test]
    fn copy_into_empty_workspace_keeps_id_and_writes_current() {
        let fixture = Fixture::new("empty");
        seed_source(&fixture);

        let report = copy_once(&fixture, WS);
        let copied = report.get("copied").and_then(Value::as_array).unwrap();
        assert_eq!(copied.len(), 1);
        assert_eq!(
            copied[0].get("oldId").and_then(Value::as_str),
            Some(CONV_OLD)
        );
        assert_eq!(
            copied[0].get("newId").and_then(Value::as_str),
            Some(CONV_OLD),
            "无冲突时沿用源会话 id"
        );
        assert_eq!(copied[0].get("messages").and_then(Value::as_u64), Some(2));

        let dst_index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        assert_eq!(
            dst_index.get("current").and_then(Value::as_str),
            Some(CONV_OLD)
        );
        assert_eq!(
            dst_index
                .get("conversations")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        // IDE 侧不写工作区级 `.index_bak.json`（目标原本没有就不新建）。
        assert!(!fixture.dst_ws_dir().join(".index_bak.json").exists());
    }

    /// 沿用 id 路径必须**字节级**复制：会话索引与消息文件内容与源逐字节一致。
    #[test]
    fn copy_keeps_conversation_files_byte_exact() {
        let fixture = Fixture::new("byte-exact");
        seed_source(&fixture);
        copy_once(&fixture, WS);

        let src_conv = fixture.src_ws_dir().join(CONV_OLD);
        let dst_conv = fixture.dst_ws_dir().join(CONV_OLD);
        for relative in [
            "index.json",
            &format!("messages/{MSG_1}.json"),
            &format!("messages/{MSG_2}.json"),
            "assets/图片.1.jpeg",
        ] {
            let src = std::fs::read(src_conv.join(relative)).unwrap();
            let dst = std::fs::read(dst_conv.join(relative)).unwrap();
            assert_eq!(src, dst, "{relative} 不是字节级复制");
        }
        // 源账号树保持不变。
        let src_index = read_json(&fixture.src_ws_dir().join("index.json")).unwrap();
        assert_eq!(
            src_index.get("current").and_then(Value::as_str),
            Some(CONV_OLD)
        );
    }

    /// 目标已有同 id：复制体取新 id、改写内部引用；目标既有会话与源会话都不被覆盖。
    #[test]
    fn copy_on_conflict_reassigns_id_and_rewrites_references() {
        let fixture = Fixture::new("conflict");
        seed_source(&fixture);
        // 目标工作区已有同 id 会话（内容与源不同，用于确认不被覆盖），且预置一条引用旧 id 的痕迹。
        write(
            &fixture.dst_ws_dir().join(CONV_OLD).join("index.json"),
            &format!(r#"{{"messages":[],"requests":[],"sessionId":"{CONV_OLD}"}}"#),
        );
        let target_original =
            std::fs::read(fixture.dst_ws_dir().join(CONV_OLD).join("index.json")).unwrap();
        seed_target_index(&fixture);

        let report = copy_once(&fixture, WS);
        let copied = report.get("copied").and_then(Value::as_array).unwrap();
        let new_id = copied[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_ne!(new_id, CONV_OLD, "冲突时必须重随机会话 id");
        assert!(vscode_session::is_hex32(&new_id));

        // 目标原有会话目录（同 id）保持原样。
        assert_eq!(
            std::fs::read(fixture.dst_ws_dir().join(CONV_OLD).join("index.json")).unwrap(),
            target_original
        );
        // 目标工作区索引：既有条目 + 新条目，`current` 保留目标原值。
        let dst_index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        let ids: Vec<String> = dst_index
            .get("conversations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|entry| entry.get("id").and_then(Value::as_str).unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().any(|id| id == CONV_EXISTING));
        assert!(ids.contains(&new_id));
        assert_eq!(
            dst_index.get("current").and_then(Value::as_str),
            Some(CONV_EXISTING)
        );

        // 复制体内的 id 引用被改写到新 id，消息文件仍一一对应。
        let new_conv_dir = fixture.dst_ws_dir().join(&new_id);
        let conv_index = read_json(&new_conv_dir.join("index.json")).unwrap();
        let message_ids: Vec<String> = conv_index
            .get("messages")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|m| m.get("id").and_then(Value::as_str).unwrap().to_string())
            .collect();
        assert!(new_conv_dir.join("index.json").is_file());
        for id in &message_ids {
            assert!(new_conv_dir.join(format!("messages/{id}.json")).is_file());
        }
        // 源会话目录原封不动（真源只读）。
        assert!(fixture
            .src_ws_dir()
            .join(CONV_OLD)
            .join("index.json")
            .is_file());
    }

    /// 「重写内部引用」的定向性：副本内只有真正出现旧会话 id 的文件被改写，其余字节不变。
    #[test]
    fn conflict_rewrite_only_touches_files_with_references() {
        let fixture = Fixture::new("conflict-rewrite");
        seed_source(&fixture);
        let src_conv = fixture.src_ws_dir().join(CONV_OLD);
        // 让消息文件里真的带一处会话 id 引用（字符串化 JSON 的 extra）。
        let msg2 = format!(
            r#"{{"role":"assistant","message":"{{\"role\":\"assistant\",\"content\":\"在的\"}}","id":"{MSG_2}","extra":"{{\"requestId\":\"{REQ_1}\",\"sessionId\":\"{CONV_OLD}\"}}","createdAt":"2026-09-23T16:23:12.001Z"}}"#
        );
        write(&src_conv.join(format!("messages/{MSG_2}.json")), &msg2);
        // 目标同 id 冲突。
        write(
            &fixture.dst_ws_dir().join(CONV_OLD).join("index.json"),
            r#"{"messages":[],"requests":[]}"#,
        );

        let report = copy_once(&fixture, WS);
        let new_id = report.get("copied").and_then(Value::as_array).unwrap()[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let dst_conv = fixture.dst_ws_dir().join(&new_id);

        // 有引用的文件：引用改成新 id，其余字段保留。
        let rewritten: Value = serde_json::from_str(
            &std::fs::read_to_string(dst_conv.join(format!("messages/{MSG_2}.json"))).unwrap(),
        )
        .unwrap();
        let extra: Value =
            serde_json::from_str(rewritten.get("extra").and_then(Value::as_str).unwrap()).unwrap();
        assert_eq!(
            extra.get("sessionId").and_then(Value::as_str),
            Some(new_id.as_str())
        );
        assert_eq!(extra.get("requestId").and_then(Value::as_str), Some(REQ_1));
        // 无引用的文件：逐字节与源一致。
        assert_eq!(
            std::fs::read(dst_conv.join(format!("messages/{MSG_1}.json"))).unwrap(),
            std::fs::read(src_conv.join(format!("messages/{MSG_1}.json"))).unwrap()
        );
    }

    /// 目标工作区已有自己的会话：合并是加法，保留目标 `current`。
    #[test]
    fn copy_is_additive_and_preserves_target_state() {
        let fixture = Fixture::new("additive");
        seed_source(&fixture);
        seed_target_index(&fixture);
        write(
            &fixture.dst_ws_dir().join(".index_bak.json"),
            r#"{"conversations":[],"current":"keep-me"}"#,
        );

        copy_once(&fixture, WS);
        let dst_index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        assert_eq!(
            dst_index.get("current").and_then(Value::as_str),
            Some(CONV_EXISTING)
        );
        assert_eq!(
            dst_index
                .get("conversations")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        // 目标侧 `.index_bak.json` 保持原样（IDE 侧不写工作区索引备份）。
        assert_eq!(
            std::fs::read_to_string(fixture.dst_ws_dir().join(".index_bak.json")).unwrap(),
            r#"{"conversations":[],"current":"keep-me"}"#
        );
        // 备份根目录留下被修改前的目标索引，供人工回滚。
        let backup_index = read_json(&fixture.backup.join(WS).join("index.json")).unwrap();
        assert_eq!(
            backup_index.get("current").and_then(Value::as_str),
            Some(CONV_EXISTING)
        );
    }

    /// 源会话缺失 / id 非法：逐条记错，不创建目标目录。
    #[test]
    fn failed_items_leave_no_target_directory() {
        let fixture = Fixture::new("missing");
        seed_source(&fixture);
        let report = copy_codebuddy_ide_sessions_in(
            &fixture.root,
            &fixture.backup,
            SRC_UID,
            DST_UID,
            &[
                CopyItem {
                    workspace_hash: WS.to_string(),
                    conversation_id: "99999999999999999999999999999999".to_string(),
                },
                CopyItem {
                    workspace_hash: WS.to_string(),
                    conversation_id: "bad-id".to_string(),
                },
            ],
        )
        .expect("returns report");
        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        assert_eq!(
            report.get("errors").and_then(Value::as_array).map(Vec::len),
            Some(2)
        );
        assert!(!fixture.dst_ws_dir().exists(), "全部失败时不应建出目标目录");
    }

    /// 目标工作区被一个同名文件占位（无法建目录）：该条失败、错误可见，其余条目不受影响。
    #[test]
    fn unwritable_target_workspace_records_error() {
        let fixture = Fixture::new("unwritable");
        seed_source(&fixture);
        let target_history = history_root_in(CODEBUDDY_IDE_STORE, &fixture.root, DST_UID);
        std::fs::create_dir_all(&target_history).unwrap();
        // `<ws>` 位置放一个文件：写临时目录必然失败。
        std::fs::write(target_history.join(WS), b"not a directory").unwrap();

        let report = copy_codebuddy_ide_sessions_in(
            &fixture.root,
            &fixture.backup,
            SRC_UID,
            DST_UID,
            &[CopyItem {
                workspace_hash: WS.to_string(),
                conversation_id: CONV_OLD.to_string(),
            }],
        )
        .expect("returns report");
        assert_eq!(
            report.get("copied").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        let errors = report.get("errors").and_then(Value::as_array).unwrap();
        assert_eq!(errors.len(), 1);
        let message = errors[0].get("error").and_then(Value::as_str).unwrap();
        assert!(message.contains("写入临时会话目录失败"), "{message}");
    }

    /// 时序：目标校验必须在关闭 IDE **之前**失败——账号不存在时不触发任何进程操作。
    /// （关闭/注入本身要跑真实进程与 Keychain，单测不触碰；顺序由代码结构与这两条断言锁定。）
    #[test]
    fn switch_with_copy_validates_before_closing_ide() {
        let missing = format!("no-such-account-{}", uuid::Uuid::new_v4().simple());
        let items = [CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        let error = switch_codebuddy_cn_ide_with_copy(&missing, true, &items, &[]).unwrap_err();
        assert!(error.contains("账号不存在"), "{error}");
    }

    /// 只勾同步（不勾复制）时也走编排 wrapper，且同样先校验账号再决定关闭。
    #[test]
    fn switch_with_sync_only_validates_before_closing_ide() {
        let missing = format!("no-such-account-{}", uuid::Uuid::new_v4().simple());
        let selections = [SyncSelection {
            group_id: "group-1".to_string(),
            preview_token: "00000000-0000-4000-8000-000000000000".to_string(),
            mode: crate::modules::session_link::SyncMode::FastForward,
        }];
        let error =
            switch_codebuddy_cn_ide_with_copy(&missing, true, &[], &selections).unwrap_err();
        assert!(error.contains("账号不存在"), "{error}");
    }

    #[test]
    fn copy_rejects_unsafe_or_same_uids() {
        let fixture = Fixture::new("bad-uid");
        let items = [CopyItem {
            workspace_hash: WS.to_string(),
            conversation_id: CONV_OLD.to_string(),
        }];
        // uid 白名单 / 路径穿越：`default` / `Public` / 分隔符 / `..` / 空串一律拒绝。
        for uid in ["default", "Public", "a/b", "a\\b", "..", ""] {
            let error = copy_codebuddy_ide_sessions_in(
                &fixture.root,
                &fixture.backup,
                SRC_UID,
                uid,
                &items,
            )
            .unwrap_err();
            assert!(error.contains("非法"), "{uid}: {error}");
        }
        let error = copy_codebuddy_ide_sessions_in(
            &fixture.root,
            &fixture.backup,
            SRC_UID,
            SRC_UID,
            &items,
        )
        .unwrap_err();
        assert!(error.contains("相同"), "{error}");
    }

    /// 多次复制同一会话：第一次沿用 id，之后每次都取新 id（加法，不覆盖既有副本）。
    #[test]
    fn repeated_copy_never_overwrites_existing_copy() {
        let fixture = Fixture::new("repeat");
        seed_source(&fixture);
        let first = copy_once(&fixture, WS);
        let second = copy_once(&fixture, WS);

        let first_id = first.get("copied").and_then(Value::as_array).unwrap()[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let second_id = second.get("copied").and_then(Value::as_array).unwrap()[0]
            .get("newId")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_ne!(first_id, second_id);
        let index = read_json(&fixture.dst_ws_dir().join("index.json")).unwrap();
        let ids: BTreeSet<String> = index
            .get("conversations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|entry| entry.get("id").and_then(Value::as_str).unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 2, "两次复制生成两条独立条目");
        assert!(ids.contains(&first_id) && ids.contains(&second_id));
    }
}
