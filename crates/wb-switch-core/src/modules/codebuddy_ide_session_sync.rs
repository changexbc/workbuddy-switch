//! CodeBuddy IDE「关联会话」：复制后登记、切换预览与增量同步。
//!
//! 与 VS Code 插件侧**共用同一份实现**（`vscode_session_sync` 的 `*_in` 变体）；国内版与国际版
//! IDE 也共用同一份（档位差异只有「来源身份读法」与「运行态判定」，见
//! [`codebuddy_ide_session::IdeFlavor`]）。本模块只做三件与目标相关的事：解析数据根与来源身份
//! （当前 IDE 登录 uid）、检查 IDE 运行态、固定 [`SessionPaths::for_codebuddy_ide`] 命名空间。
//! 存储隔离与判定语义见 `session-links.md`。

use serde_json::{json, Value};
use std::path::Path;

use crate::modules::codebuddy_ide_session::{self, IdeFlavor};
use crate::modules::session::{SessionPaths, SyncSelection};
use crate::modules::variant::WbVariant;
use crate::modules::vscode_session::CODEBUDDY_IDE_STORE;
use crate::modules::vscode_session_sync;

/// 为本次复制成功的会话登记「源 ↔ 副本」关联，返回逐条失败原因。
///
/// **登记失败不回滚复制**：复制已完成、文件已落盘；失败原因由调用方并入报告 `linkErrors[]`。
pub fn register_copied_sessions(
    root: &Path,
    paths: &SessionPaths,
    variant: WbVariant,
    report: &Value,
) -> Vec<Value> {
    // 两个 IDE 共用一张关联表：只复用同 variant 的组，不把国际版登记并进国内版的组。
    vscode_session_sync::register_copied_sessions_isolated(
        CODEBUDDY_IDE_STORE,
        root,
        paths,
        variant,
        report,
    )
}

/// 预览「当前国内版 IDE 账号 → 目标账号」可同步的关联会话（只读，不写任何会话正文）。
pub fn links_preview(target_acc: &Value) -> Result<Value, String> {
    links_preview_for(IdeFlavor::Cn, target_acc)
}

/// 预览「当前国际版 IDE 账号 → 目标账号」可同步的关联会话（语义与国内版一致）。
pub fn links_preview_intl(target_acc: &Value) -> Result<Value, String> {
    links_preview_for(IdeFlavor::Intl, target_acc)
}

fn links_preview_for(flavor: IdeFlavor, target_acc: &Value) -> Result<Value, String> {
    let root = codebuddy_ide_session::ide_data_root()
        .ok_or_else(|| "未找到 CodeBuddy IDE 数据目录，无法同步会话".to_string())?;
    let source_uid = active_source_uid(flavor)?;
    // 共用 `codebuddy_ide_session_links.json`：按组的 variant 过滤，国内版组不进国际版预览。
    vscode_session_sync::links_preview_in_for_variant(
        CODEBUDDY_IDE_STORE,
        &root,
        &SessionPaths::for_codebuddy_ide(),
        &source_uid,
        target_acc,
        link_variant(flavor),
    )
}

/// 执行勾选的同步项（两个 IDE 共用；国内版 / 国际版只差来源 uid 与运行态判定）。
///
/// 由切换编排调用（同步只随切换发生，没有独立的宿主入口）。
/// **前提**：调用方已确认 IDE 完全退出（写入会被运行中的客户端覆盖）；本函数自行复查一次。
pub(crate) fn sync_selected_for(
    flavor: IdeFlavor,
    target_acc: &Value,
    selections: &[SyncSelection],
) -> Result<Value, String> {
    if selections.is_empty() {
        return Ok(json!({ "synced": [], "skipped": [], "errors": [] }));
    }
    if flavor.is_running() {
        return Err(
            "检测到 CodeBuddy IDE 正在运行，请先完全退出后再同步会话，否则写入会被 CodeBuddy IDE 覆盖。"
                .to_string(),
        );
    }
    let root = codebuddy_ide_session::ide_data_root()
        .ok_or_else(|| "未找到 CodeBuddy IDE 数据目录，无法同步会话".to_string())?;
    let source_uid = active_source_uid(flavor)?;
    vscode_session_sync::sync_selected_in_for_variant(
        CODEBUDDY_IDE_STORE,
        &root,
        &SessionPaths::for_codebuddy_ide(),
        &source_uid,
        target_acc,
        selections,
        link_variant(flavor),
    )
}

/// 档位对应的关联组 variant：国内版 `cn`，国际版 `ai`。
fn link_variant(flavor: IdeFlavor) -> WbVariant {
    match flavor {
        IdeFlavor::Cn => WbVariant::Cn,
        IdeFlavor::Intl => WbVariant::Ai,
    }
}

/// 来源账号 uid：当前 IDE 登录 secret（回退本地状态文件）；非法 uid（`default` / `Public` / 路径穿越）视为未登录。
fn active_source_uid(flavor: IdeFlavor) -> Result<String, String> {
    flavor.active_uid().ok_or_else(|| {
        "未检测到 CodeBuddy IDE 当前登录账号，无法同步会话。请先在 CodeBuddy IDE 中登录后重试。"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::session_link::{self, MemberState, StoreState, SyncMode};
    use crate::modules::vscode_session::{history_root_in, read_json, CopyItem};
    use std::path::{Path, PathBuf};

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

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        backup: PathBuf,
        store: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "wb_switch_codebuddy_ide_sync_{}_{name}",
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
            SessionPaths::for_codebuddy_ide_at(self.store.clone())
        }

        fn src_conv_dir(&self) -> PathBuf {
            history_root_in(CODEBUDDY_IDE_STORE, &self.root, SRC_UID)
                .join(WS)
                .join(CONV_SRC)
        }

        fn dst_conv_dir(&self, conv_id: &str) -> PathBuf {
            history_root_in(CODEBUDDY_IDE_STORE, &self.root, DST_UID)
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
                "extra": json!({"requestId": request_id, "modelId": "hy3"}).to_string(),
                "createdAt": "2026-09-23T16:23:11.563Z",
            })
            .to_string(),
        );
    }

    /// 造源账号的会话：工作区索引 + 2 条记录（user + assistant）。
    fn seed_source(fixture: &Fixture) {
        let ws_dir = history_root_in(CODEBUDDY_IDE_STORE, &fixture.root, SRC_UID).join(WS);
        write(
            &ws_dir.join("index.json"),
            &json!({
                "conversations": [{
                    "id": CONV_SRC, "type": "craft", "name": "测试会话",
                    "createdAt": "2026-09-23T16:22:52.034Z",
                    "lastMessageAt": "2026-09-23T16:23:14.088Z",
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
                    {"id": MSG_2, "type": "text", "role": "assistant", "isComplete": false},
                ],
                "requests": [{
                    "id": REQ_1, "type": "craft", "messages": [MSG_1, MSG_2],
                    "state": "complete", "startedAt": 1790180582570_i64,
                }],
            })
            .to_string(),
        );
        write_message(&conv_dir, MSG_1, "user", REQ_1, "你好");
        write_message(&conv_dir, MSG_2, "assistant", REQ_1, "在的");
    }

    /// 源账号追加一轮对话（user + assistant + 一条请求）。
    fn append_round(
        fixture: &Fixture,
        user_id: &str,
        assistant_id: &str,
        request_id: &str,
        text: &str,
    ) {
        let conv_dir = fixture.src_conv_dir();
        let mut index = read_json(&conv_dir.join("index.json")).unwrap();
        index["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": user_id, "type": "text", "role": "user", "isComplete": true}));
        index["messages"].as_array_mut().unwrap().push(
            json!({"id": assistant_id, "type": "text", "role": "assistant", "isComplete": false}),
        );
        index["requests"].as_array_mut().unwrap().push(json!({
            "id": request_id, "type": "craft", "messages": [user_id, assistant_id],
            "state": "complete", "startedAt": 1790180590000_i64,
        }));
        write(&conv_dir.join("index.json"), &index.to_string());
        write_message(&conv_dir, user_id, "user", request_id, text);
        write_message(&conv_dir, assistant_id, "assistant", request_id, "收到");
    }

    /// 走 IDE 复制内核复制一次，返回报告。
    fn copy_once(fixture: &Fixture) -> Value {
        crate::modules::codebuddy_ide_session::copy_codebuddy_ide_sessions_in(
            &fixture.root,
            &fixture.backup,
            SRC_UID,
            DST_UID,
            &[CopyItem {
                workspace_hash: WS.to_string(),
                conversation_id: CONV_SRC.to_string(),
            }],
        )
        .unwrap()
    }

    /// 复制 + 登记，返回副本会话 id（登记失败即断言失败）。
    fn copy_and_register(fixture: &Fixture) -> String {
        let report = copy_once(fixture);
        let errors =
            register_copied_sessions(&fixture.root, &fixture.paths(), WbVariant::Cn, &report);
        assert!(errors.is_empty(), "登记失败：{errors:?}");
        report["copied"][0]["newId"].as_str().unwrap().to_string()
    }

    fn preview_of(fixture: &Fixture) -> Value {
        vscode_session_sync::links_preview_in(
            CODEBUDDY_IDE_STORE,
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
        )
        .unwrap()
    }

    fn selection(group_id: &str, token: &str, mode: SyncMode) -> SyncSelection {
        SyncSelection {
            group_id: group_id.to_string(),
            preview_token: token.to_string(),
            mode,
        }
    }

    /// 复制成功后出现 1 个组、2 个 active 成员、1 条配对基线，且只写 IDE 命名空间。
    #[test]
    fn copy_registers_group_in_ide_namespace() {
        let fixture = Fixture::new("register");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        assert_eq!(target_conv, CONV_SRC, "无冲突时沿用源会话 id");

        assert!(fixture
            .store
            .join("codebuddy_ide_session_links.json")
            .is_file());
        assert!(fixture
            .store
            .join("codebuddy-ide-session-links/baselines")
            .is_dir());
        assert!(!fixture.store.join("session_links.json").exists());
        assert!(!fixture.store.join("vscode_session_links.json").exists());
        assert!(!fixture.store.join("session-links").exists());
        assert!(!fixture.store.join("vscode-session-links").exists());

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

        // 双方内容一致 → identical（不可勾选、无可用模式）。
        let preview = preview_of(&fixture);
        assert_eq!(preview["storeStatus"], "ready");
        let groups = preview["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["verdict"], "identical");
        assert_eq!(groups[0]["availableModes"], json!([]));
        assert_eq!(groups[0]["defaultChecked"], false);
        assert_eq!(groups[0]["recordCount"]["source"], 2);
    }

    /// 冲突复制（目标已有同 id）也会登记为「源 ↔ 新副本」，副本 id 取新值。
    #[test]
    fn conflict_copy_registers_new_id_pair() {
        let fixture = Fixture::new("conflict-register");
        seed_source(&fixture);
        // 目标工作区预置同 id 的会话（模拟用户此前自己复制过一份）。
        write(
            &fixture.dst_conv_dir(CONV_SRC).join("index.json"),
            r#"{"messages":[],"requests":[]}"#,
        );
        let target_conv = copy_and_register(&fixture);
        assert_ne!(target_conv, CONV_SRC);

        let preview = preview_of(&fixture);
        let groups = preview["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["verdict"], "identical");
        assert_eq!(
            groups[0]["target"]["sessionId"].as_str(),
            Some(target_conv.as_str())
        );
    }

    /// 第二次切换：只剩来源新增内容可快进，同步后双方一致（幂等）。
    #[test]
    fn second_switch_syncs_only_new_records() {
        let fixture = Fixture::new("fast-forward");
        seed_source(&fixture);
        let target_conv = copy_and_register(&fixture);
        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview = preview_of(&fixture);
        let groups = preview["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["verdict"], "fastForward");
        assert_eq!(groups[0]["defaultChecked"], true);
        assert_eq!(groups[0]["availableModes"], json!(["fastForward"]));
        assert_eq!(groups[0]["recordCount"]["source"], 4);
        assert_eq!(groups[0]["recordCount"]["target"], 2);
        assert_eq!(groups[0]["recordCount"]["baseline"], 2);
        let group_id = groups[0]["groupId"].as_str().unwrap().to_string();
        let token = groups[0]["previewToken"].as_str().unwrap().to_string();

        let report = vscode_session_sync::sync_selected_in(
            CODEBUDDY_IDE_STORE,
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

        // 副本记录数与源一致（保留原来的会话 id）。
        let target_index =
            read_json(&fixture.dst_conv_dir(&target_conv).join("index.json")).unwrap();
        assert_eq!(
            target_index
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(4)
        );
        // 同步备份落在 IDE 专属目录名（不与插件侧混）。
        let backup = report["synced"][0]["backup"].as_str().unwrap();
        assert!(backup.contains("codebuddy-ide-sessions"), "{backup}");

        // 再预览：双方一致，无可用操作。
        let preview = preview_of(&fixture);
        assert_eq!(preview["groups"][0]["verdict"], "identical");
        assert_eq!(preview["groups"][0]["availableModes"], json!([]));
    }

    /// 同一张关联表里国内版 / 国际版各一组：预览与同步都不得把另一档的组混进来。
    #[test]
    fn shared_namespace_filters_groups_by_variant() {
        let fixture = Fixture::new("variant-split");
        seed_source(&fixture);
        let report = copy_once(&fixture);
        let cn_errors =
            register_copied_sessions(&fixture.root, &fixture.paths(), WbVariant::Cn, &report);
        let ai_errors =
            register_copied_sessions(&fixture.root, &fixture.paths(), WbVariant::Ai, &report);
        assert!(cn_errors.is_empty(), "{cn_errors:?}");
        assert!(ai_errors.is_empty(), "{ai_errors:?}");

        let store = match session_link::load_store(&fixture.paths()) {
            StoreState::Ready(store) => store,
            other => panic!("关联表不可用：{other:?}"),
        };
        assert_eq!(
            store.groups.len(),
            2,
            "同 variant 才复用组，cn 与 ai 必须各一组"
        );
        assert!(store
            .groups
            .iter()
            .any(|group| group.variant == WbVariant::Cn));
        assert!(store
            .groups
            .iter()
            .any(|group| group.variant == WbVariant::Ai));

        append_round(&fixture, MSG_3, MSG_4, REQ_2, "新增内容");

        let preview_for = |variant: WbVariant| {
            vscode_session_sync::links_preview_in_for_variant(
                CODEBUDDY_IDE_STORE,
                &fixture.root,
                &fixture.paths(),
                SRC_UID,
                &target_acc(),
                variant,
            )
            .unwrap()
        };
        let cn_groups = preview_for(WbVariant::Cn)["groups"]
            .as_array()
            .unwrap()
            .clone();
        let ai_groups = preview_for(WbVariant::Ai)["groups"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(cn_groups.len(), 1, "国内版预览不得带上国际版组");
        assert_eq!(ai_groups.len(), 1, "国际版预览不得带上国内版组");
        assert_ne!(cn_groups[0]["groupId"], ai_groups[0]["groupId"]);
        assert_eq!(cn_groups[0]["verdict"], "fastForward");
        assert_eq!(ai_groups[0]["verdict"], "fastForward");

        // 国际版凭据交给国内版同步：拒绝，且不改目标正文。
        let rejected = vscode_session_sync::sync_selected_in_for_variant(
            CODEBUDDY_IDE_STORE,
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(
                ai_groups[0]["groupId"].as_str().unwrap(),
                ai_groups[0]["previewToken"].as_str().unwrap(),
                SyncMode::FastForward,
            )],
            WbVariant::Cn,
        )
        .unwrap();
        assert!(
            rejected["synced"].as_array().unwrap().is_empty(),
            "{rejected}"
        );
        assert_eq!(
            rejected["errors"].as_array().unwrap().len(),
            1,
            "{rejected}"
        );
        let target_conv = report["copied"][0]["newId"].as_str().unwrap();
        let unchanged = read_json(&fixture.dst_conv_dir(target_conv).join("index.json")).unwrap();
        assert_eq!(unchanged["messages"].as_array().unwrap().len(), 2);

        let synced = vscode_session_sync::sync_selected_in_for_variant(
            CODEBUDDY_IDE_STORE,
            &fixture.root,
            &fixture.paths(),
            SRC_UID,
            &target_acc(),
            &[selection(
                cn_groups[0]["groupId"].as_str().unwrap(),
                cn_groups[0]["previewToken"].as_str().unwrap(),
                SyncMode::FastForward,
            )],
            WbVariant::Cn,
        )
        .unwrap();
        assert_eq!(synced["synced"].as_array().unwrap().len(), 1, "{synced}");
        assert!(synced["errors"].as_array().unwrap().is_empty(), "{synced}");
    }

    /// 关联表不存在时预览为 missing（前端据此提示「先复制一次」）。
    #[test]
    fn preview_reports_missing_store() {
        let fixture = Fixture::new("missing-store");
        seed_source(&fixture);
        let preview = preview_of(&fixture);
        assert_eq!(preview["storeStatus"], "missing");
        assert_eq!(preview["supported"], true);
        assert_eq!(preview["groups"].as_array().unwrap().len(), 0);
    }
}
