//! VS Code CodeBuddy 插件会话的内容身份（design §3）。
//!
//! WorkBuddy 的记录单位是 JSONL 的一行，本模块把它换成**一条消息**：
//!
//! - 记录序列 = 会话 `index.json` 的 `messages[]` **顺序**（目录顺序不可靠，不用 `read_dir`）；
//! - 单条记录摘要 = `messages/<id>.json` 的**规范化 JSON** 取 SHA-256（长度编码后计算，
//!   与 WorkBuddy 的行摘要同一口径）；
//! - 规范化 = 把「副本特异的 id」替换为占位符：`messages[].id` → `m0`/`m1`…、
//!   `requests[].id` → `r0`/`r1`…（各按索引首次出现顺序编号）、会话自身 id → [`SESSION_ID_MARKER`]。
//!
//! 归一化只替换**已知 id**，其它 32-hex（如 `extra.traceId`）原样保留——沿用 WorkBuddy 的
//! 保守哲学：未知差异宁可作为差异呈现，也不猜。同一逻辑会话的源与副本因此得到逐条相同的
//! 摘要，这是「可快进 / 分叉 / 一致」判定的地基。
//!
//! 反直觉但已实测（design §3）：`isComplete: false` 是助手消息的**常态**（26 条消息的会话里
//! 13 条助手消息全为 false，而其 `requests[].state` 全为 `complete`）。**不得**用它判断
//! 「消息是否写完」——摘要只按字节算。
//!
//! 并发前提：读写这些文件的调用方都必须在「VS Code 已完全退出」之后执行
//! （`vscode-ext-process.md` 的顺序不变量），因此这里不做读期间变化的复查。
//!
//! 已知限制（安全方向）：不排除「易变尾部」。复制时最后一条消息若处于半写入状态、
//! 之后在源侧完成，则源的该位置摘要变化 → 判定为分叉（而非误判可快进），用户可用覆盖模式处理。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

use crate::modules::session_link::{
    self, full_digest_of, line_digest_of, to_hex, ContentSnapshot, ContentState, NormalizedContent,
    SESSION_ID_MARKER,
};
use crate::modules::vscode_session::{read_json, replace_ids_in_extra};

/// 派生 id 的域前缀与版本（公式变化必须升版本，避免新旧 id 混用）。
const DERIVE_PREFIX: &str = "vscode-session-link:v1";

/// 读取会话内容状态：按 `index.json.messages[]` 顺序逐条摘要。
///
/// - `Missing`：会话目录下没有 `index.json`（会话不存在）；
/// - `Unavailable`：索引无法解析 / 缺少消息列表 / 内容为空 / 消息文件缺失或损坏——
///   一律不猜，判定侧会得到 `Unknown`；
/// - `Ready`：`normalized` 为有序记录摘要（判定用），`full_digest` 覆盖索引与全部消息文件的
///   原始字节（预览凭据的版本绑定用：任何字节改动都会让它失配）。
///
/// `text` 存放会话索引原文：VS Code 侧没有单一正文字段，写入以文件为单位（见
/// [`translate_session_index`] / [`translate_message`]），该字段不参与写入。
pub fn read_session_content(conv_dir: &Path, conversation_id: &str) -> ContentState {
    let index_path = conv_dir.join("index.json");
    let index_bytes = match std::fs::read(&index_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ContentState::Missing,
        Err(error) => return ContentState::Unavailable(format!("会话索引无法读取：{error}")),
    };
    let index: Value = match serde_json::from_slice(&index_bytes) {
        Ok(value) => value,
        Err(_) => return ContentState::Unavailable("会话索引无法解析，内容可能不完整".to_string()),
    };
    let Some(messages) = index.get("messages").and_then(Value::as_array) else {
        return ContentState::Unavailable("会话索引缺少消息列表，内容可能不完整".to_string());
    };
    if messages.is_empty() {
        return ContentState::Unavailable("会话内容为空，无法确认".to_string());
    }

    let placeholders = placeholder_map(&index, conversation_id);
    let mut line_digests: Vec<String> = Vec::with_capacity(messages.len());
    let mut raw = Vec::new();
    push_len_prefixed(&mut raw, &index_bytes);

    for message in messages {
        let Some(id) = message_id(message) else {
            return ContentState::Unavailable(
                "会话索引中的消息缺少 id，内容可能不完整".to_string(),
            );
        };
        let file = conv_dir.join("messages").join(format!("{id}.json"));
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ContentState::Unavailable(format!(
                    "消息 {id} 的文件不存在，内容可能不完整"
                ));
            }
            Err(error) => {
                return ContentState::Unavailable(format!("消息 {id} 的文件无法读取：{error}"));
            }
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return ContentState::Unavailable(format!("消息 {id} 的文件不是文本，无法确认"));
        };
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return ContentState::Unavailable(format!("消息 {id} 的文件无法解析，内容可能不完整"));
        };
        line_digests.push(line_digest_of(
            &normalize_message(&value, &placeholders).to_string(),
        ));
        push_len_prefixed(&mut raw, &bytes);
    }

    let normalized = NormalizedContent {
        record_count: line_digests.len(),
        total_digest: session_link::total_digest_of(&line_digests),
        line_digests,
    };
    ContentState::Ready(ContentSnapshot {
        text: String::from_utf8_lossy(&index_bytes).to_string(),
        full_digest: full_digest_of(&raw),
        normalized,
    })
}

/// 长度编码后拼接：避免「两段内容边界不同」产生同一个原始摘要。
fn push_len_prefixed(buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    buffer.extend_from_slice(bytes);
}

/// 索引里一条消息条目的 id（空白视为缺失）。
fn message_id(message: &Value) -> Option<String> {
    message
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// 规范化占位符表：按索引顺序给消息 / 请求 id 编号，会话自身 id 用固定标记。
///
/// 同一逻辑会话的源与副本 id 不同但**顺序一致**，因此两侧得到同一套占位符，逐条摘要可比。
fn placeholder_map(index: &Value, conversation_id: &str) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    if let Some(messages) = index.get("messages").and_then(Value::as_array) {
        for (index, message) in messages.iter().enumerate() {
            if let Some(id) = message_id(message) {
                map.entry(id).or_insert_with(|| format!("m{index}"));
            }
        }
    }
    if let Some(requests) = index.get("requests").and_then(Value::as_array) {
        for (index, request) in requests.iter().enumerate() {
            let Some(id) = request
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            map.entry(id.to_string())
                .or_insert_with(|| format!("r{index}"));
        }
    }
    let own_id = conversation_id.trim();
    if !own_id.is_empty() {
        map.insert(own_id.to_string(), SESSION_ID_MARKER.to_string());
    }
    map
}

/// 归一化一条消息文件：顶层 `id` 与 `extra` 内的 id 引用换成占位符，其余字段原样保留。
///
/// 与复制路径的写入范围严格一致（`id` + `extra`）：复制不改 `message` 正文，
/// 归一化同样不改——两侧口径一致，摘要才可能相等。
fn normalize_message(value: &Value, placeholders: &BTreeMap<String, String>) -> Value {
    let mut out = value.clone();
    if let Some(object) = out.as_object_mut() {
        if let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_string) {
            if let Some(marker) = placeholders.get(&id) {
                object.insert("id".to_string(), json!(marker));
            }
        }
        if let Some(extra) = object.get("extra").cloned() {
            object.insert(
                "extra".to_string(),
                replace_ids_in_extra(&extra, placeholders),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 确定性 id 派生（design §6.1：幂等的地基）
// ---------------------------------------------------------------------------

/// 派生副本消息 id：`sha256("vscode-session-link:v1" + 目标 uid + 目标会话 id + 序号 + 源记录摘要)`
/// 取前 32 位小写 hex。
///
/// 同一目标会话、同一序号、同一源内容 → 同一个 id，因此重复执行只会覆盖同一批文件；
/// 序号进公式是为了让「内容完全相同的重复消息」不互相撞 id。
pub fn derive_message_id(
    target_uid: &str,
    target_conversation_id: &str,
    seq_index: usize,
    source_digest: &str,
    salt: u32,
) -> String {
    derive_id(&[
        target_uid,
        target_conversation_id,
        &seq_index.to_string(),
        source_digest,
        &salt.to_string(),
    ])
}

/// 派生副本请求 id：与消息 id 同公式，用请求序号与请求摘要，并加 `req` 段区分。
pub fn derive_request_id(
    target_uid: &str,
    target_conversation_id: &str,
    request_index: usize,
    source_digest: &str,
    salt: u32,
) -> String {
    derive_id(&[
        target_uid,
        target_conversation_id,
        "req",
        &request_index.to_string(),
        source_digest,
        &salt.to_string(),
    ])
}

/// 派生实现：各段长度编码后拼接再取 SHA-256 前 32 位（段内容含分隔符也不会歧义）。
fn derive_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DERIVE_PREFIX.as_bytes());
    hasher.update([0u8]);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    to_hex(&hasher.finalize())[..32].to_string()
}

/// 请求条目的规范化摘要（派生请求 id 用；与消息摘要无关）。
///
/// 只做占位符替换，不参与记录序列摘要——请求不是记录，改动请求不改变判定。
pub fn request_digest(request: &Value, placeholders: &BTreeMap<String, String>) -> String {
    line_digest_of(&replace_ids_in_extra(request, placeholders).to_string())
}

/// 按索引顺序取出请求的规范化摘要序列，供派生请求 id 使用。
pub fn request_digests(index: &Value, conversation_id: &str) -> Vec<String> {
    let placeholders = placeholder_map(index, conversation_id);
    index
        .get("requests")
        .and_then(Value::as_array)
        .map(|requests| {
            requests
                .iter()
                .map(|request| request_digest(request, &placeholders))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 位置对齐与引用翻译（design §6.1）
// ---------------------------------------------------------------------------

/// 位置对齐的翻译表：源记录序列的前 `target_ids.len()` 条与目标记录一一对应。
///
/// 判定保证「目标等于基线、源是基线的严格有序追加」，因此前缀逐位置对应是成立的；
/// 目标多出的部分不会翻译（不会凭空造出目标 id）。
pub fn positional_translation(
    source_ids: &[String],
    target_ids: &[String],
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (source_id, target_id) in source_ids.iter().zip(target_ids.iter()) {
        map.insert(source_id.clone(), target_id.clone());
    }
    map
}

/// 按翻译表重写会话索引里的一条消息条目（只换 id，其余字段原样）。
pub fn translate_message_entry(entry: &Value, translation: &BTreeMap<String, String>) -> Value {
    let mut out = entry.clone();
    if let Some(object) = out.as_object_mut() {
        if let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_string) {
            if let Some(new_id) = translation.get(&id) {
                object.insert("id".to_string(), json!(new_id));
            }
        }
    }
    out
}

/// 按翻译表重写会话索引里的一条请求条目：`id` 与 `messages[]` 引用。
pub fn translate_request_entry(request: &Value, translation: &BTreeMap<String, String>) -> Value {
    let mut out = request.clone();
    let Some(object) = out.as_object_mut() else {
        return out;
    };
    if let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_string) {
        if let Some(new_id) = translation.get(&id) {
            object.insert("id".to_string(), json!(new_id));
        }
    }
    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            if let Some(id) = message.as_str().map(str::to_string) {
                if let Some(new_id) = translation.get(&id) {
                    *message = json!(new_id);
                }
            }
        }
    }
    out
}

/// 按翻译表重写整份会话索引（`messages[].id` + `requests[].id` + `requests[].messages[]`）。
pub fn translate_session_index(index: &Value, translation: &BTreeMap<String, String>) -> Value {
    let mut out = index.clone();
    if let Some(messages) = out.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            *message = translate_message_entry(message, translation);
        }
    }
    if let Some(requests) = out.get_mut("requests").and_then(Value::as_array_mut) {
        for request in requests.iter_mut() {
            *request = translate_request_entry(request, translation);
        }
    }
    out
}

/// 按翻译表重写一条消息文件：顶层 `id` 与 `extra` 内的 id 引用（与复制路径同范围）。
pub fn translate_message(value: &Value, translation: &BTreeMap<String, String>) -> Value {
    let mut out = value.clone();
    if let Some(object) = out.as_object_mut() {
        if let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_string) {
            if let Some(new_id) = translation.get(&id) {
                object.insert("id".to_string(), json!(new_id));
            }
        }
        if let Some(extra) = object.get("extra").cloned() {
            object.insert(
                "extra".to_string(),
                replace_ids_in_extra(&extra, translation),
            );
        }
    }
    out
}

/// 会话索引里的消息 id 序列（按索引顺序，空白 id 跳过）。
pub fn message_ids_of(index: &Value) -> Vec<String> {
    index
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.iter().filter_map(message_id).collect())
        .unwrap_or_default()
}

/// 读会话索引（文件缺失或损坏返回 `None`）：翻译与写入前的定位用。
pub fn read_session_index(conv_dir: &Path) -> Option<Value> {
    read_json(&conv_dir.join("index.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::vscode_session::is_hex32;
    use std::path::PathBuf;

    const WS: &str = "0123456789abcdef0123456789abcdef";
    const SRC_UID: &str = "uid-src-0001";
    const DST_UID: &str = "uid-dst-0002";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wb_switch_vscode_link_{}_{name}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// 造一个会话目录：`conv_id` 与各类 id 都由调用方给定，用来模拟「同一会话的源与副本」。
    struct Seed<'a> {
        conv_id: &'a str,
        msg_ids: [&'a str; 2],
        req_id: &'a str,
        /// 第一条消息 `extra.traceId`（不在「副本特异 id」集合内的 32-hex）。
        trace_id: &'a str,
        /// 追加到第一条消息 `extra` 的额外字段（例如会话自身 id 的引用）。
        extra_fields: Vec<(&'a str, &'a str)>,
        /// 第二条消息的正文，用于「改写一条消息」的用例。
        second_message: &'a str,
    }

    impl<'a> Default for Seed<'a> {
        fn default() -> Self {
            Self {
                conv_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                msg_ids: [
                    "11111111111111111111111111111111",
                    "22222222222222222222222222222222",
                ],
                req_id: "33333333333333333333333333333333",
                trace_id: "ffffffffffffffffffffffffffffffff",
                extra_fields: Vec::new(),
                second_message: "在的",
            }
        }
    }

    fn seed_conversation(root: &Path, seed: &Seed<'_>) -> PathBuf {
        let dir = root.join(WS).join(seed.conv_id);
        let index = json!({
            "messages": [
                {"id": seed.msg_ids[0], "type": "text", "role": "user", "isComplete": true},
                {"id": seed.msg_ids[1], "type": "text", "role": "assistant", "isComplete": false},
            ],
            "requests": [{
                "id": seed.req_id,
                "type": "craft",
                "messages": [seed.msg_ids[0], seed.msg_ids[1]],
                "state": "complete",
                "startedAt": 1789532362052_i64,
            }],
        });
        write(&dir.join("index.json"), &index.to_string());

        // `extra` 用字符串化 JSON（实测形态）：复制路径会解析后再序列化，归一化走同一处理。
        let mut first_extra = json!({
            "requestId": seed.req_id,
            "responseId": seed.msg_ids[0],
            "modelId": "deepseek-v4",
            "traceId": seed.trace_id,
        });
        for (key, value) in &seed.extra_fields {
            first_extra[*key] = json!(value);
        }
        write(
            &dir.join(format!("messages/{}.json", seed.msg_ids[0])),
            &json!({
                "role": "user",
                // `message` 是字符串化 JSON：复制路径不改它，归一化同样不改（两侧口径一致）。
                "message": json!({"role": "user", "content": "你好"}).to_string(),
                "id": seed.msg_ids[0],
                "extra": first_extra.to_string(),
                "createdAt": "2026-09-16T05:05:29.751Z",
            })
            .to_string(),
        );
        let second_extra = json!({
            "requestId": seed.req_id,
            "responseId": seed.msg_ids[1],
            "modelId": "deepseek-v4",
        });
        write(
            &dir.join(format!("messages/{}.json", seed.msg_ids[1])),
            &json!({
                "role": "assistant",
                "message": json!({"role": "assistant", "content": seed.second_message}).to_string(),
                "id": seed.msg_ids[1],
                "extra": second_extra.to_string(),
                "createdAt": "2026-09-16T05:05:31.001Z",
            })
            .to_string(),
        );
        dir
    }

    fn digest_of(state: &ContentState) -> Vec<String> {
        match state {
            ContentState::Ready(snapshot) => snapshot.normalized.line_digests.clone(),
            other => panic!("期望 Ready，实际 {other:?}"),
        }
    }

    /// 判定的地基：同一逻辑会话的源与副本（id 全不同）归一化后逐条摘要相等。
    #[test]
    fn copies_normalize_to_identical_digests() {
        let root = temp_dir("identical");
        let source = seed_conversation(&root, &Seed::default());
        let copy = seed_conversation(
            &root,
            &Seed {
                conv_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                msg_ids: [
                    "44444444444444444444444444444444",
                    "55555555555555555555555555555555",
                ],
                req_id: "66666666666666666666666666666666",
                ..Seed::default()
            },
        );
        let source_state = read_session_content(&source, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let copy_state = read_session_content(&copy, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(digest_of(&source_state), digest_of(&copy_state));
        // 原始字节摘要必须能区分源与副本（预览凭据按副本校验）。
        let (ContentState::Ready(source_snapshot), ContentState::Ready(copy_snapshot)) =
            (&source_state, &copy_state)
        else {
            panic!("双方内容都应可读");
        };
        assert_ne!(source_snapshot.full_digest, copy_snapshot.full_digest);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 会话自身 id 参与归一化：消息里引用了会话 id 时，源与副本（id 全不同）摘要仍相等。
    #[test]
    fn conversation_id_is_normalized_too() {
        let root = temp_dir("conv-id");
        let source = seed_conversation(
            &root,
            &Seed {
                extra_fields: vec![("sessionId", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")],
                ..Seed::default()
            },
        );
        let copy = seed_conversation(
            &root,
            &Seed {
                conv_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                msg_ids: [
                    "44444444444444444444444444444444",
                    "55555555555555555555555555555555",
                ],
                req_id: "66666666666666666666666666666666",
                extra_fields: vec![("sessionId", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")],
                ..Seed::default()
            },
        );
        let a = read_session_content(&source, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = read_session_content(&copy, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(digest_of(&a), digest_of(&b));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 未列入 id 集合的 32-hex（如 `extra.traceId`）原样保留：两侧不同即视为差异（保守方向）。
    #[test]
    fn unknown_hex_ids_are_kept_and_make_digests_differ() {
        let root = temp_dir("trace-id");
        let source = seed_conversation(&root, &Seed::default());
        let copy = seed_conversation(
            &root,
            &Seed {
                conv_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                msg_ids: [
                    "44444444444444444444444444444444",
                    "55555555555555555555555555555555",
                ],
                req_id: "66666666666666666666666666666666",
                // traceId 是真实数据里存在的 32-hex，但不在「副本特异 id」集合内。
                trace_id: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                ..Seed::default()
            },
        );
        let a = read_session_content(&source, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = read_session_content(&copy, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(digest_of(&a), digest_of(&b));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 改写一条消息内容 → 该条摘要变化（其它条不变）。
    #[test]
    fn changed_message_changes_only_that_digest() {
        let root = temp_dir("changed");
        let source = seed_conversation(&root, &Seed::default());
        let before = digest_of(&read_session_content(
            &source,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ));
        let dir = seed_conversation(
            &root,
            &Seed {
                second_message: "在的，改过了",
                ..Seed::default()
            },
        );
        let after = digest_of(&read_session_content(
            &dir,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ));
        assert_eq!(before[0], after[0]);
        assert_ne!(before[1], after[1]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缺消息文件 / 缺索引 / 空会话一律 `Unavailable` 或 `Missing`，不猜。
    #[test]
    fn missing_pieces_are_not_guessed() {
        let root = temp_dir("missing");
        let dir = seed_conversation(&root, &Seed::default());
        let conv_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert!(matches!(
            read_session_content(&dir, conv_id),
            ContentState::Ready(_)
        ));
        std::fs::remove_file(dir.join("messages/22222222222222222222222222222222.json")).unwrap();
        assert!(matches!(
            read_session_content(&dir, conv_id),
            ContentState::Unavailable(_)
        ));
        std::fs::remove_file(dir.join("index.json")).unwrap();
        assert!(matches!(
            read_session_content(&dir, conv_id),
            ContentState::Missing
        ));
        write(&dir.join("index.json"), r#"{"messages":[],"requests":[]}"#);
        assert!(matches!(
            read_session_content(&dir, conv_id),
            ContentState::Unavailable(_)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 派生 id：32 位小写 hex、确定性（同输入同结果）、序号区分重复内容、salt 换值。
    #[test]
    fn derived_ids_are_deterministic_hex32() {
        let digest = "d".repeat(64);
        let first = derive_message_id(DST_UID, "conv-b", 7, &digest, 0);
        assert_eq!(first.len(), 32);
        assert!(is_hex32(&first));
        assert_eq!(first, derive_message_id(DST_UID, "conv-b", 7, &digest, 0));
        // 内容相同的重复消息：序号不同 → id 不同（不撞车）。
        assert_ne!(first, derive_message_id(DST_UID, "conv-b", 8, &digest, 0));
        // 换目标会话 / 换 salt / 换内容摘要都会换 id。
        assert_ne!(first, derive_message_id(DST_UID, "conv-c", 7, &digest, 0));
        assert_ne!(first, derive_message_id(DST_UID, "conv-b", 7, &digest, 1));
        assert_ne!(
            first,
            derive_message_id(DST_UID, "conv-b", 7, &"e".repeat(64), 0)
        );
        assert_ne!(first, derive_message_id(SRC_UID, "conv-b", 7, &digest, 0));
        // 请求 id 与消息 id 在同一序号上不撞。
        let request = derive_request_id(DST_UID, "conv-b", 7, &digest, 0);
        assert!(is_hex32(&request));
        assert_ne!(first, request);
    }

    /// 位置对齐只翻译前缀，不做「按内容猜 id」。
    #[test]
    fn positional_translation_covers_prefix_only() {
        let source = vec!["s0".to_string(), "s1".to_string(), "s2".to_string()];
        let target = vec!["t0".to_string(), "t1".to_string()];
        let map = positional_translation(&source, &target);
        assert_eq!(map.get("s0"), Some(&"t0".to_string()));
        assert_eq!(map.get("s1"), Some(&"t1".to_string()));
        assert_eq!(map.get("s2"), None);
    }

    /// 索引 / 消息 / 请求的重写范围与复制路径一致（只动 id 与引用）。
    #[test]
    fn translation_touches_ids_and_references_only() {
        let root = temp_dir("translate");
        let dir = seed_conversation(&root, &Seed::default());
        let index = read_session_index(&dir).unwrap();
        let translation: BTreeMap<String, String> = [
            (
                "11111111111111111111111111111111".to_string(),
                "aaaa1111111111111111111111111111".to_string(),
            ),
            (
                "22222222222222222222222222222222".to_string(),
                "aaaa2222222222222222222222222222".to_string(),
            ),
            (
                "33333333333333333333333333333333".to_string(),
                "aaaa3333333333333333333333333333".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let translated = translate_session_index(&index, &translation);
        assert_eq!(
            message_ids_of(&translated),
            vec![
                "aaaa1111111111111111111111111111".to_string(),
                "aaaa2222222222222222222222222222".to_string()
            ]
        );
        let request = translated
            .get("requests")
            .and_then(Value::as_array)
            .unwrap()[0]
            .clone();
        assert_eq!(
            request.get("id").and_then(Value::as_str),
            Some("aaaa3333333333333333333333333333")
        );
        assert_eq!(
            request
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            request.get("startedAt").and_then(Value::as_i64),
            Some(1789532362052),
            "非 id 字段必须原样保留"
        );

        let message_path = dir.join("messages/11111111111111111111111111111111.json");
        let message: Value =
            serde_json::from_str(&std::fs::read_to_string(&message_path).unwrap()).unwrap();
        let translated_message = translate_message(&message, &translation);
        assert_eq!(
            translated_message.get("id").and_then(Value::as_str),
            Some("aaaa1111111111111111111111111111")
        );
        let extra: Value = serde_json::from_str(
            translated_message
                .get("extra")
                .and_then(Value::as_str)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            extra.get("requestId").and_then(Value::as_str),
            Some("aaaa3333333333333333333333333333")
        );
        assert_eq!(
            extra.get("traceId").and_then(Value::as_str),
            Some("ffffffffffffffffffffffffffffffff"),
            "不在翻译表里的 hex 原样保留"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 请求摘要：请求序号不同 / 内容不同都会改变派生 id 的输入。
    #[test]
    fn request_digests_follow_index_order() {
        let root = temp_dir("req-digest");
        let dir = seed_conversation(&root, &Seed::default());
        let index = read_session_index(&dir).unwrap();
        let digests = request_digests(&index, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(digests.len(), 1);
        assert_eq!(digests[0].len(), 64);
        assert!(request_digests(&json!({}), "x").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
