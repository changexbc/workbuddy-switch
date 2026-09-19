//! 云端会话删除（本地新增，上游零冲突）。
//!
//! 背景（2026-09-15 溯源，详见 `reports/session-cloud-delete-mechanism.md`）：
//! 官方「删除对话连带删云端」的实现是 **App 内 edge-sync 扩展**监听本地删除事件
//! （`wb:conversation:deleted`）后调
//! `POST /console/as/conversations/{sid}/delete`，**用当前登录账号的凭证**。
//! 会话瘦身只做本机软删 ⇒ 云端那份留在原地（手机端仍可见、本机又已无入口）。
//! 本模块把「云端删除」补齐，让瘦身能「本地 + 云端一起瘦」。
//!
//! 三条铁律（违反任一条都会复刻「幽灵会话」故障）：
//!   1. **必须用该会话云端归属账号的 token** —— 服务端按 token 的 uid 校验，
//!      用错账号返回 `403 access denied`（实测），绝不会误删，但也删不掉；
//!   2. **404 有三义**（真已删 / 账号不对 / id 不存在）⇒ 调用方必须先按归属筛过，
//!      再谈 404 —— 不可拿 404 当「账号没问题」的证据；
//!   3. **先删云端成功、再本地软删** —— 顺序反转才可回退。
//!
//! 归属判据：`<configDir>/edge-sync-mapping-v4.db` 的
//! `edge_sync_mapping.msg_channel = convmsg:<uid>`。
//! ⚠️ 该表**会记错**（实测 ~20% 偏差）⇒ 只用来缩小范围，真正的把关是服务端 403/200。

use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::modules::account;
use crate::modules::config::home_dir;

/// 官方网关（与 `ui_theme` 一致；本机 WARP 需 `no_proxy` 直连）。
pub const CLOUD_BASE: &str = "https://www.workbuddy.cn";

/// 云端删除结果。调用方按它决定「是否软删本地」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudDelete {
    /// 云端已删（HTTP 200 或业务 code==0）。
    Deleted,
    /// 云端本来就没有（HTTP 404）—— 归属已校验的前提下视为成功。
    AlreadyGone,
    /// `403 conversation access denied`：**该会话不归这个 token 的账号**
    /// ⇒ 调用方应放弃云端删除（但本地归属不受影响，可照常软删）。
    Forbidden,
    /// 网络/服务错误 ⇒ **不要本地软删**，留待下次重试。
    Failed(String),
}

impl CloudDelete {
    /// 是否可视为「云端这一侧已处理完」。
    pub fn ok(&self) -> bool {
        matches!(self, CloudDelete::Deleted | CloudDelete::AlreadyGone)
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        // 本机 WARP 代理会碍事；workbuddy.cn 直连可达
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// 从 HTTP 状态 + 响应体判定结果。抽出来便于单测（不需要真发请求）。
pub fn classify(status: u16, body: &str) -> CloudDelete {
    match status {
        200..=299 => {
            // 官方可能 200 带业务错误码；code 缺失或为 0 才认成功
            let code = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_i64()));
            match code {
                None | Some(0) => CloudDelete::Deleted,
                Some(c) => CloudDelete::Failed(format!("http 200 但业务 code={c}")),
            }
        }
        404 => CloudDelete::AlreadyGone,
        403 => CloudDelete::Forbidden,
        s => CloudDelete::Failed(format!("http {s}")),
    }
}

/// 删除一条云端会话。`sid` 既是 session id 也是 conversation id（实测一致，158/158）。
pub fn delete_conversation(token: &str, sid: &str) -> CloudDelete {
    if token.is_empty() || sid.is_empty() {
        return CloudDelete::Failed("缺 token 或 sid".into());
    }
    let url = format!("{CLOUD_BASE}/console/as/conversations/{}/delete", urlencode(sid));
    let resp = http().post(&url).bearer_auth(token).send();
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            classify(status, &body)
        }
        Err(e) => CloudDelete::Failed(format!("网络错误：{e}")),
    }
}

/// 只对 id 里可能出现的字符做最小转义（sid 是 uuid，通常无需变）。
/// 全账枚举端点（2026-09-15 实测：与 `CLOUD_BASE` **不同域**，是另一套网关）。
///
/// `GET {CLOUD_API_BASE}/v2/as/conversations/?type=all&page=N&size=M` + **归属账号** Bearer
/// ⇒ 该账号名下**跨设备**全量会话（含他机创建与云端自动化）。
/// ⚠️ 别把路径换到 `CLOUD_BASE` 的 `/console/as/conversations`：同 token 恒 403。
pub const CLOUD_API_BASE: &str = "https://copilot.tencent.com";

/// 列表页 size 上限（`size=200` 服务端直接 400）。
pub const CLOUD_LIST_PAGE_SIZE: usize = 100;

/// 解析一页列表响应 → (本页 sid 列表, 是否还有下一页)。纯函数，便于单测。
pub fn parse_conversation_page(body: &str) -> Result<(Vec<String>, bool), String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("响应不是 JSON: {e}"))?;
    let data = v.get("data").ok_or_else(|| "响应缺 data 字段".to_string())?;
    let sids = data
        .get("conversations")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("id").and_then(|i| i.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let has_next = data
        .get("pagination")
        .and_then(|p| p.get("hasNext"))
        .and_then(|h| h.as_bool())
        .unwrap_or(false);
    Ok((sids, has_next))
}

/// 拉取账号全域会话 id（**只读**，自动分页）。
///
/// 失败一律 `Err` —— 调用方据此把全账巡检降级为「不启用」，**绝不影响瘦身主流程**。
pub fn list_conversation_ids(token: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut page = 1usize;
    loop {
        let url = format!(
            "{CLOUD_API_BASE}/v2/as/conversations/?type=all&page={page}&size={CLOUD_LIST_PAGE_SIZE}"
        );
        let resp = http()
            .get(&url)
            .bearer_auth(token)
            .send()
            .map_err(|e| format!("请求失败: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(format!(
                "http {status}: {}",
                body.chars().take(120).collect::<String>()
            ));
        }
        let (mut sids, has_next) = parse_conversation_page(&body)?;
        out.append(&mut sids);
        if !has_next || page >= 50 {
            break;
        }
        page += 1;
    }
    Ok(out)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `convmsg:<uid>` → `<uid>`（不是该前缀就原样返回）。
pub fn channel_uid(ch: &str) -> String {
    ch.strip_prefix("convmsg:").unwrap_or(ch).to_string()
}

/// 探测当前生效的映射库：**v4 → v3 → v2 → 无名**，取第一个存在的。
///
/// ⚠️ 官方 `extensions/edge-sync/server/index.cjs` 里**硬编码 `edge-sync-mapping-v4.db`**；
/// 本仓早期代码写死 v2，导致「复制会话注册云端归属」静默写进了 App 不读的旧库（HANDOFF 坑位 40）。
/// 这里改为探测，跟随官方换代。
pub fn latest_mapping_db() -> Option<PathBuf> {
    let root = home_dir().join(".workbuddy");
    for name in [
        "edge-sync-mapping-v4.db",
        "edge-sync-mapping-v3.db",
        "edge-sync-mapping-v2.db",
        "edge-sync-mapping.db",
    ] {
        let p = root.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 读映射表的 `session_id → msg_channel` 全量。读不到就返回空表（调用方退化为「只本地瘦身」）。
pub fn mapping_channels() -> HashMap<String, String> {
    let Some(db) = latest_mapping_db() else {
        return HashMap::new();
    };
    mapping_channels_from(&db)
}

/// 映射行全量（含 conversation_id——删除接口的唯一钥匙）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingEntry {
    pub session_id: String,
    pub conversation_id: String,
    pub channel: String,
}

/// 读映射表全量行（只读，绝不写删——坑位 41）。空表 = 无法对账，调用方跳过该阶段。
pub fn mapping_rows() -> Vec<MappingEntry> {
    let Some(db) = latest_mapping_db() else {
        return Vec::new();
    };
    mapping_rows_from(&db)
}

/// 供测试注入路径用。
pub fn mapping_rows_from(db: &std::path::Path) -> Vec<MappingEntry> {
    let Ok(conn) = Connection::open(db) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT session_id, conversation_id, COALESCE(msg_channel, '') FROM edge_sync_mapping",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok(MappingEntry {
            session_id: r.get(0)?,
            conversation_id: r.get(1)?,
            channel: r.get(2)?,
        })
    }) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// 供测试注入路径用。
pub fn mapping_channels_from(db: &std::path::Path) -> HashMap<String, String> {
    let Ok(conn) = Connection::open(db) else {
        return HashMap::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT session_id, msg_channel FROM edge_sync_mapping")
    else {
        return HashMap::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    }) else {
        return HashMap::new();
    };
    rows.flatten()
        .filter_map(|(sid, ch)| ch.map(|c| (sid, c)))
        .collect()
}

/// 从账号记录里取 uid（空串 = 该账号缺 uid）。
fn account_uid(acc: &Value) -> String {
    acc.get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 按 uid 从 `accounts.json` 找 `access_token`。
pub fn token_of(uid: &str) -> Option<String> {
    account::load_accounts()
        .into_iter()
        .find(|a| account_uid(a) == uid)
        .and_then(|a| account::get_str(&a, "access_token"))
}

/// 本机 device-id（`~/.workbuddy/device-id`，uuid 格式）。读不到返回空串。
///
/// App edge-sync 的 `CREATE` / `MIGRATE` 请求都带 `hostId=<deviceId>`；
/// 服务端会把 `source_device_id` 覆盖成这个值（手机端「XX 设备」的来源）。
pub fn device_id() -> String {
    std::fs::read_to_string(home_dir().join(".workbuddy").join("device-id"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 为目标账号创建云端会话（conv），返回 `Ok(conv_id)`。
///
/// 背景（2026-09-16 实锤，见 `reports/hardlink-mobile-invisible-diagnosis-2026-09-16.md`）：
/// **写映射行 ≠ 云端建 conv**——edge-sync 启动时把映射库行当「已上云」，
/// 手工 register 反而阻断 App 补建（坑 48）。外部造的会话（复制/硬链接）
/// 必须主动调这条接口，且**顺序铁律：先建 conv 成功，再 register 映射行**。
///
/// body 逐字段模仿 App `syncCreateConversation`（index.cjs）：
/// `type=local`、`conversationOrigin=legacy_workbuddy_local`、
/// `clientContext.hostId=<device-id>` 等；成功后 conv id == sid（云端统一）。
pub fn create_conversation(
    token: &str,
    sid: &str,
    title: &str,
    cwd: &str,
    ts_ms: i64,
) -> Result<String, String> {
    if token.is_empty() {
        return Err("缺目标账号 token，无法在云端建会话".into());
    }
    if sid.is_empty() {
        return Err("缺 session id".into());
    }
    let host = device_id();
    let body = serde_json::json!({
        "type": "local",
        "sessionId": sid,
        "name": title,
        "conversationOrigin": "legacy_workbuddy_local",
        "workDir": cwd,
        "isPlayground": 0,
        "createdAtMs": ts_ms,
        "updatedAtMs": ts_ms,
        "lastActivityAtMs": ts_ms,
        "clientContext": {
            "localStatus": "active",
            "hostId": host,
            "sessionKind": "manual",
            "model": "",
            "sourceMode": "",
            "permissionMode": "",
            "expert": { "id": "", "locale": "", "runtimeIdentity": "", "marketplace": "" }
        }
    });
    let url = format!("{CLOUD_BASE}/console/as/conversations/v2");
    let resp = http()
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    match classify(status, &text) {
        CloudDelete::Deleted => {
            // conv id 实测与 sid 同值；服务端若返回独立 id，以返回值为准
            let cid = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("data")
                        .and_then(|d| d.get("id"))
                        .and_then(|i| i.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| sid.to_string());
            Ok(cid)
        }
        other => Err(format!("云端建会话失败（http {status}）: {other:?}")),
    }
}

/// 批量迁移的单条 item（逐字段模仿 App MIGRATE_SESSION payload，index.cjs @42419）。
pub fn migrate_item(sid: &str, title: &str, cwd: &str, ts_ms: i64) -> Value {
    let host = device_id();
    serde_json::json!({
        "sessionId": sid,
        // 服务端 migrationConversationForceUpdateCols 会覆盖 source_device_id 列，
        // 端上冗余传（批次顶层 deviceId + 每条内层 sourceDeviceId），与 App 一致。
        "sourceDeviceId": host,
        "conversationTitle": title,
        "conversationOrigin": "legacy_workbuddy_local",
        "status": "completed",
        "workDir": cwd,
        "createdAtMs": ts_ms,
        "updatedAtMs": ts_ms,
        "lastActivityAtMs": ts_ms,
        "isPlayground": 0,
        "clientContext": {
            "localStatus": "completed",
            "sessionKind": "manual",
            "model": "",
            "sourceMode": "",
            "permissionMode": "",
            "expert": { "id": "", "locale": "", "runtimeIdentity": "", "marketplace": "" }
        }
    })
}

/// 批量迁移端点（App edge-sync MIGRATE 同款）：
/// `POST /console/as/conversation-sync/migrations/legacy`（index.cjs:42370）。
///
/// 与单建 `conversations/v2` 的关键差异（2026-09-16 逆向 + 实测）：
/// - **一次请求打包 ≤500 条**（App `MIGRATE_BATCH_SIZE=500`），服务端逐条建 conv；
///   限流按请求数计（单建实测 10 条/窗口），批量端点 1 请求即可绕开；
/// - 实测 `convId == sessionId`（与单建一致），归属 = token 账号；
/// - ⚠️ `forceUpdate=true` 会覆盖已存在 conv 的 source_device_id 等列
///   ⇒ **只对「云端必无」的新 sid 用**（我们的共享场景天然满足）。
///
/// 返回 `data`（含 `results: {sid: {status, conversationId}}` 与
/// `importedCount/skippedCount/failedCount`），调用方按逐条结果 register/回滚。
pub fn migrate_conversations(token: &str, sessions: &[Value]) -> Result<Value, String> {
    if token.is_empty() {
        return Err("缺目标账号 token，无法批量上云".into());
    }
    if sessions.is_empty() {
        return Ok(serde_json::json!({ "results": {} }));
    }
    let host = device_id();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let body = serde_json::json!({
        "migrationId": format!("{host}-{now_ms}-b0"),
        "deviceId": host,
        "schemaVersion": 0,
        "forceUpdate": true,
        "sessions": sessions,
    });
    let url = format!("{CLOUD_BASE}/console/as/conversation-sync/migrations/legacy");
    let resp = http()
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    match classify(status, &text) {
        CloudDelete::Deleted => {
            let data = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("data").cloned())
                .unwrap_or_else(|| serde_json::json!({}));
            Ok(data)
        }
        other => Err(format!("批量迁移失败（http {status}）: {other:?}")),
    }
}

/// 判定云端 conv 是否存在（GET /v2/as/conversations/{sid}）。
/// 200 = 存在；404 = 不存在；403 = 存在但属别的账号（按存在处理，保守不重建）。
pub fn conversation_exists(token: &str, sid: &str) -> Result<bool, String> {
    if token.is_empty() {
        return Err("缺目标账号 token".into());
    }
    let url = format!("{CLOUD_BASE}/v2/as/conversations/{sid}");
    let resp = http()
        .get(&url)
        .bearer_auth(token)
        .send()
        .map_err(|e| format!("请求失败: {e}"))?;
    match resp.status().as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        403 => Ok(true),
        s => Err(format!("http {s}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_http_status() {
        assert_eq!(classify(200, r#"{"code":0,"msg":"OK"}"#), CloudDelete::Deleted);
        assert_eq!(classify(200, ""), CloudDelete::Deleted);
        assert_eq!(
            classify(404, r#"{"code":14284,"msg":"conversation not found"}"#),
            CloudDelete::AlreadyGone
        );
        assert_eq!(
            classify(403, r#"{"code":14287,"msg":"conversation access denied"}"#),
            CloudDelete::Forbidden
        );
        assert!(matches!(classify(500, "boom"), CloudDelete::Failed(_)));
        // 200 但业务错码 ⇒ 不能当成功
        assert!(matches!(
            classify(200, r#"{"code":1,"msg":"x"}"#),
            CloudDelete::Failed(_)
        ));
    }

    #[test]
    fn ok_covers_deleted_and_gone_only() {
        assert!(CloudDelete::Deleted.ok());
        assert!(CloudDelete::AlreadyGone.ok());
        assert!(!CloudDelete::Forbidden.ok());
        assert!(!CloudDelete::Failed("x".into()).ok());
    }

    #[test]
    fn channel_uid_strips_prefix() {
        assert_eq!(channel_uid("convmsg:uid-a"), "uid-a");
        assert_eq!(channel_uid("uid-a"), "uid-a");
        assert_eq!(channel_uid(""), "");
    }

    #[test]
    fn urlencode_keeps_uuid_and_escapes_others() {
        assert_eq!(
            urlencode("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
            "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
        );
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn missing_input_never_touches_network() {
        assert!(matches!(delete_conversation("", "x"), CloudDelete::Failed(_)));
        assert!(matches!(delete_conversation("t", ""), CloudDelete::Failed(_)));
    }

    #[test]
    fn mapping_channels_from_reads_table() {
        let db = std::env::temp_dir().join(format!(
            "wb_cloudconv_{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE edge_sync_mapping (session_id TEXT PRIMARY KEY, \
             conversation_id TEXT NOT NULL, msg_channel TEXT NOT NULL DEFAULT '');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edge_sync_mapping VALUES ('s1','s1','convmsg:uid-a')",
            [],
        )
        .unwrap();
        drop(conn);
        let m = mapping_channels_from(&db);
        assert_eq!(m.get("s1").map(|s| s.as_str()), Some("convmsg:uid-a"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn mapping_channels_from_missing_db_is_empty() {
        let m = mapping_channels_from(std::path::Path::new("C:/__no_such_dir__/x.db"));
        assert!(m.is_empty());
    }

    #[test]
    fn parse_conversation_page_reads_ids_and_hasnext() {
        let body = r#"{"code":0,"msg":"OK","data":{"conversations":[{"id":"a"},{"id":"b"}],"total":2,"pagination":{"page":1,"size":100,"total":2,"totalPages":1,"hasNext":false,"hasPrev":false}}}"#;
        let (sids, more) = parse_conversation_page(body).unwrap();
        assert_eq!(sids, vec!["a".to_string(), "b".to_string()]);
        assert!(!more);
    }

    #[test]
    fn parse_conversation_page_hasnext_true() {
        let body = r#"{"data":{"conversations":[],"pagination":{"hasNext":true}}}"#;
        let (sids, more) = parse_conversation_page(body).unwrap();
        assert!(sids.is_empty());
        assert!(more);
    }

    #[test]
    fn parse_conversation_page_rejects_non_json_and_missing_data() {
        // 网关 403/401 会回 HTML 或 {"error":...}，必须当失败而不是当空列表
        assert!(parse_conversation_page("<html>401 Authorization Required</html>").is_err());
        assert!(parse_conversation_page(r#"{"error":"access_denied"}"#).is_err());
    }
}
