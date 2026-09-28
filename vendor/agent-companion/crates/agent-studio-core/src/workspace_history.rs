//! 会话 → 工程目录的反查。
//!
//! CodeBuddy 系客户端（IDE 与 VS Code 插件共用的 `tencent-cloud.coding-copilot`）
//! 会把每个会话归档成
//! `<用户数据>/User/globalStorage/tencent-cloud.coding-copilot/genie-history/<bucket>/conversations/<会话 id>`，
//! 其中 `<bucket> = base64(工程目录).replace(/[/+=]/g,'_').substring(0,64)`。
//!
//! Hook payload 里的 `cwd` **不可靠**：IDE 在窗口没有打开目录时会给出自己的安装目录
//! （实测 `D:\Program Files\CodeBuddy CN`），照它跳转会打开一个毫不相干的目录、还可能
//! 顶掉用户原来的窗口。因此这里按会话 id 反查真正的工程目录，作为会话 `cwd` 的来源。
//!
//! 与 `agent-studio-desktop` 里的同类解析保持同一算法（桶名把 `/`、`+`、`=` 都压成了
//! `_` 并在 64 字符处截断，所以只有「解码后重新编码能复现该桶名」的候选才被接受）。
//!
//! 限制：桶名截断意味着**目录路径超过约 48 个字符时无法完整还原**（base64 后超 64），
//! 这类会话查不到工程目录，调用方会保留原始 `cwd`。

use std::path::{Path, PathBuf};

/// 已知的插件历史根目录（按平台列出 CodeBuddy / VS Code 系的用户数据目录）。
pub fn history_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |base: PathBuf| {
        for name in [
            "CodeBuddy CN",
            "CodeBuddy",
            "Code",
            "Code - Insiders",
            "VSCodium",
            "Cursor",
            "Windsurf",
        ] {
            roots.push(
                base.join(name)
                    .join("User/globalStorage/tencent-cloud.coding-copilot/genie-history"),
            );
        }
    };
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            push(PathBuf::from(appdata));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            push(PathBuf::from(home).join("Library/Application Support"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            push(PathBuf::from(home).join(".config"));
        }
    }
    roots
}

/// 会话归档所在的工程目录；查不到返回 `None`。
pub fn resolve_session_folder(session: &str) -> Option<PathBuf> {
    if !usable_session_id(session) {
        return None;
    }
    resolve_session_folder_in(&history_roots(), session)
}

/// `roots` 里第一个归档了该会话、且桶名能还原成真实目录的工程目录。
pub fn resolve_session_folder_in(roots: &[PathBuf], session: &str) -> Option<PathBuf> {
    if !usable_session_id(session) {
        return None;
    }
    for root in roots {
        if let Some(folder) = bucket_folder_for_session(root, session) {
            return Some(folder);
        }
    }
    None
}

fn bucket_folder_for_session(root: &Path, session: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        if !entry.path().join("conversations").join(session).is_dir() {
            continue;
        }
        if let Some(bucket) = entry.file_name().to_str() {
            if let Some(folder) = decode_bucket_folder(bucket) {
                return Some(folder);
            }
        }
    }
    None
}

/// `sanitizeWorkspaceId(p) = base64(p).replace(/[/+=]/g,'_').substring(0,64)`（与插件一致）。
fn sanitize_workspace_id(path: &str) -> String {
    base64_encode(path.as_bytes())
        .chars()
        .map(|byte| if matches!(byte, '/' | '+' | '=') { '_' } else { byte })
        .take(64)
        .collect()
}

/// 桶名还原成的工程目录。只接受「重新编码能复现桶名」且真实存在的目录。
fn decode_bucket_folder(bucket: &str) -> Option<PathBuf> {
    if !bucket.is_ascii() {
        return None;
    }
    for candidate in bucket_candidates(bucket) {
        let Ok(text) = std::str::from_utf8(&candidate) else {
            continue;
        };
        if !bucket_matches_folder(bucket, text) {
            continue;
        }
        let path = Path::new(text);
        if path.is_absolute() && path != Path::new("/") && path.is_dir() {
            return Some(path.to_path_buf());
        }
    }
    None
}

/// 桶名是 `sanitize_workspace_id(folder)` 截断到 64 字符的产物，因此重新编码必须能
/// 复现整个桶名（目录够长被截断时，复现它的前 64 个字符）。
fn bucket_matches_folder(bucket: &str, folder: &str) -> bool {
    let encoded = sanitize_workspace_id(folder);
    if bucket.len() == 64 {
        encoded.starts_with(bucket)
    } else {
        encoded == bucket
    }
}

/// 桶名可能代表的原始 base64 文本：尾部 `_` 当作 padding（1~2 个）、把 64 字符名当
/// 截断的 base64、以及把其中一个 `_` 读回 `+`（`~`、`>` 或 DEL 落在分组边界时会用到）。
fn bucket_candidates(bucket: &str) -> Vec<Vec<u8>> {
    let mut candidates = Vec::new();
    for padding in 0..=2 {
        if padding > 0 && !bucket.ends_with(&"_".repeat(padding)) {
            break;
        }
        let mut text = desanitize(&bucket[..bucket.len() - padding]);
        text.push_str(&"=".repeat(padding));
        candidates.extend(base64_decode(&text));
    }
    for trimmed in 1..=3 {
        if bucket.len() <= trimmed {
            break;
        }
        candidates.extend(base64_decode(&desanitize(&bucket[..bucket.len() - trimmed])));
    }
    for flip in 0..bucket.len() {
        if bucket.as_bytes()[flip] != b'_' {
            continue;
        }
        let text: String = bucket
            .char_indices()
            .map(|(index, byte)| match byte {
                '_' if index == flip => '+',
                '_' => '/',
                other => other,
            })
            .collect();
        candidates.extend(base64_decode(&text));
    }
    candidates
}

/// `_` → `/`：插件把三个 base64 字符压成了一个，除 padding 外只可能是 `/`。
fn desanitize(text: &str) -> String {
    text.chars()
        .map(|byte| if byte == '_' { '/' } else { byte })
        .collect()
}

/// 标准 base64（可带 `=` padding）。非规范尾部一律拒绝，避免误解码被当成数据。
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let body = text
        .strip_suffix("==")
        .or_else(|| text.strip_suffix('='))
        .unwrap_or(text);
    if body.contains('=') || body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in body.bytes() {
        let value = base64_value(byte)? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    (accumulator == 0).then_some(out)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let group = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(ALPHABET[(group >> 18) as usize & 63] as char);
        out.push(ALPHABET[(group >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(group >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[group as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 会话 id 是普通 token：带路径分隔符或 `..` 的都不允许用来走历史目录。
fn usable_session_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains('/') && !id.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 插件真实产生的桶名（本机 `genie-history` 里的一条）。
    #[test]
    fn sanitize_matches_the_plugin_spelling() {
        assert_eq!(
            sanitize_workspace_id("d:/File/code/workbuddy-switch"),
            "ZDovRmlsZS9jb2RlL3dvcmtidWRkeS1zd2l0Y2g_"
        );
    }

    /// 按会话 id 反查工程目录：桶名能还原、且目录真实存在才算数。
    ///
    /// 桶名在 64 字符处被截断，所以这里用一个足够短的既有目录当「工程目录」；
    /// 路径过深时无法反查，属于插件方案的固有限制（见模块注释）。
    #[test]
    fn resolves_the_folder_that_archives_the_session() {
        let folder = std::env::temp_dir();
        let bucket = sanitize_workspace_id(&folder.to_string_lossy());
        if bucket.len() >= 64 {
            return; // 临时目录本身就太深，无法用桶名还原。
        }
        let root = folder.join(format!("wb-history-{}", std::process::id()));
        std::fs::create_dir_all(root.join(&bucket).join("conversations/session-1")).unwrap();
        let roots = vec![root.clone()];
        assert_eq!(
            resolve_session_folder_in(&roots, "session-1"),
            Some(folder.clone())
        );
        // 别的会话不在这个桶里。
        assert_eq!(resolve_session_folder_in(&roots, "session-2"), None);
        // 会话 id 不允许走出历史根目录。
        assert_eq!(resolve_session_folder_in(&roots, "../session-1"), None);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 找不到桶时返回 None（调用方据此保留原始 cwd）。
    #[test]
    fn missing_roots_are_silent() {
        let roots = vec![std::env::temp_dir().join("wb-history-does-not-exist")];
        assert_eq!(resolve_session_folder_in(&roots, "any"), None);
    }
}
