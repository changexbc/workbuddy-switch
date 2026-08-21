//! 账号导出/导入：解析导入文件、按 uid 去重合并、计数。
//!
//! 纯逻辑（`parse_accounts_json` / `merge_import_records` / `select_export_records`）
//! 不依赖文件系统，便于无 UI 环境单测；`export_accounts` / `import_accounts`
//! 负责读写账号库（`~/.wb-switch/accounts.json`）。

use serde_json::{json, Value};

use crate::modules::account;

/// 导入结果计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportResult {
    /// 成功合入账号库的数量（含覆盖与新增）。
    pub imported: usize,
    /// 未导入的数量（缺 access_token / 索引越界）。
    pub skipped: usize,
    /// 其中覆盖了同 uid 本地账号的数量。
    pub overwritten: usize,
}

/// 解析导出/导入文件文本：必须是 JSON 数组，且每项为 JSON 对象。
///
/// 失败时返回带位置的明确错误文案（非法 JSON / 非数组 / 元素不是对象）。
pub fn parse_accounts_json(text: &str) -> Result<Vec<Value>, String> {
    if text.trim().is_empty() {
        return Err("文件内容为空".to_string());
    }
    let parsed: Value = serde_json::from_str(text)
        .map_err(|e| format!("文件不是合法的 JSON：{e}"))?;
    let array = parsed
        .as_array()
        .ok_or_else(|| "文件内容应为 JSON 数组（账号列表）".to_string())?;
    for (index, item) in array.iter().enumerate() {
        if !item.is_object() {
            return Err(format!("文件第 {} 项不是合法的账号对象", index + 1));
        }
    }
    Ok(array.clone())
}

/// 生成导入文件的脱敏预览（含文件内索引，不含 token）。
pub fn preview_accounts(text: &str) -> Result<Value, String> {
    let array = parse_accounts_json(text)?;
    let items: Vec<Value> = array
        .iter()
        .enumerate()
        .map(|(index, item)| {
            json!({
                "index": index,
                "uid": item.get("uid"),
                "nickname": item.get("nickname"),
                "email": item.get("email"),
                "hasToken": account::get_str(item, "access_token").is_some(),
            })
        })
        .collect();
    Ok(json!({ "accounts": items, "total": array.len() }))
}

/// 单条导入记录的合并动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeOutcome {
    /// 追加为新账号。
    Appended,
    /// 覆盖同 uid 的本地账号（保留导入记录原样）。
    Overwritten,
    /// 缺少 access_token，跳过。
    Skipped,
}

/// 纯函数：把一条导入记录合并进账号列表。
///
/// 按 uid 去重：同 uid 覆盖（保留导入记录原样）；uid 缺失或无法匹配则追加。
/// 缺少 access_token 的记录跳过，不进入账号库。
fn merge_import_record(accounts: &mut Vec<Value>, item: &Value) -> MergeOutcome {
    if account::get_str(item, "access_token").is_none() {
        return MergeOutcome::Skipped;
    }
    if let Some(uid) = account::get_str(item, "uid").as_deref() {
        if let Some(existing) = accounts
            .iter_mut()
            .find(|a| account::get_str(a, "uid").as_deref() == Some(uid))
        {
            let mut replaced = item.clone();
            // 导入记录缺 id 时保留本地 id：账号库不允许出现无 id 记录
            //（删除按 id、列表 key、导出选择都依赖 id）。
            if account::get_str(&replaced, "id").is_none() {
                if let Some(id) = existing.get("id").cloned() {
                    replaced["id"] = id;
                }
            }
            *existing = replaced;
            return MergeOutcome::Overwritten;
        }
    }
    accounts.push(item.clone());
    MergeOutcome::Appended
}

/// 纯函数：解析文件文本并按选中索引把记录合并进账号列表，返回计数。
///
/// 不做文件读写，便于单测。索引越界视为跳过。
pub fn merge_import_records(
    accounts: &mut Vec<Value>,
    text: &str,
    indexes: &[usize],
) -> Result<ImportResult, String> {
    let array = parse_accounts_json(text)?;
    let mut result = ImportResult::default();
    for &index in indexes {
        match array.get(index) {
            None => result.skipped += 1,
            Some(item) => match merge_import_record(accounts, item) {
                MergeOutcome::Appended => result.imported += 1,
                MergeOutcome::Overwritten => {
                    result.imported += 1;
                    result.overwritten += 1;
                }
                MergeOutcome::Skipped => result.skipped += 1,
            },
        }
    }
    Ok(result)
}

/// 导入：读账号库 → 合并 → 写回，返回计数。
pub fn import_accounts(text: &str, indexes: &[usize]) -> Result<ImportResult, String> {
    let mut accounts = account::load_accounts();
    let result = merge_import_records(&mut accounts, text, indexes)?;
    account::save_accounts(&accounts).map_err(|e| format!("保存账号库失败：{e}"))?;
    Ok(result)
}

/// 纯函数：从账号列表中挑出 id 命中的完整记录（含 token）。
pub fn select_export_records(accounts: &[Value], ids: &[String]) -> Result<Vec<Value>, String> {
    let ids: Vec<&str> = ids
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        return Err("请先选择要导出的账号".to_string());
    }
    let mut exported: Vec<Value> = Vec::new();
    for id in ids {
        if let Some(acc) = accounts
            .iter()
            .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id))
        {
            exported.push(acc.clone());
        }
    }
    if exported.is_empty() {
        return Err("未找到要导出的账号".to_string());
    }
    Ok(exported)
}

/// 导出：按账号 id 列表返回完整记录（含 token）。
pub fn export_accounts(ids: &[String]) -> Result<Vec<Value>, String> {
    select_export_records(&account::load_accounts(), ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, uid: Option<&str>, nickname: &str, email: Option<&str>, token: bool) -> Value {
        json!({
            "id": id,
            "uid": uid,
            "nickname": nickname,
            "email": email,
            "access_token": if token { format!("token-{id}") } else { String::new() },
            "createdAt": 1,
        })
    }

    #[test]
    fn parse_rejects_empty_invalid_and_non_array() {
        assert!(parse_accounts_json("").is_err());
        assert!(parse_accounts_json("not json").is_err());
        assert!(parse_accounts_json(r#"{ "a": 1 }"#).is_err());
    }

    #[test]
    fn parse_rejects_non_object_element() {
        let err = parse_accounts_json(r#"[{ "uid": "u1" }, 42]"#).unwrap_err();
        assert!(err.contains("第 2 项"), "错误应带位置：{err}");
    }

    #[test]
    fn parse_accepts_object_array() {
        let parsed = parse_accounts_json(r#"[{ "uid": "u1" }, {}]"#).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn merge_overwrites_same_uid_preserving_imported_record() {
        let mut accounts = vec![record("local", Some("u1"), "旧名称", None, true)];
        let text = r#"[{ "id": "imported", "uid": "u1", "nickname": "新名称", "access_token": "tok-new" }]"#;
        let result = merge_import_records(&mut accounts, text, &[0]).unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(result.overwritten, 1);
        assert_eq!(result.skipped, 0);
        assert_eq!(accounts.len(), 1, "同 uid 应覆盖而不是新增");
        assert_eq!(accounts[0]["id"], "imported", "覆盖保留导入记录原样");
        assert_eq!(accounts[0]["nickname"], "新名称");
        assert_eq!(accounts[0]["access_token"], "tok-new");
    }

    #[test]
    fn merge_preserves_local_id_when_imported_record_has_none() {
        let mut accounts = vec![record("local-id", Some("u1"), "旧名称", None, true)];
        let text = r#"[{ "uid": "u1", "nickname": "新名称", "access_token": "tok" }]"#;
        let result = merge_import_records(&mut accounts, text, &[0]).unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(result.overwritten, 1);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["id"], "local-id", "导入记录缺 id 时保留本地 id");
        assert_eq!(accounts[0]["nickname"], "新名称");
        assert_eq!(accounts[0]["access_token"], "tok");
    }

    #[test]
    fn merge_appends_when_uid_missing_or_unmatched() {
        let mut accounts = vec![record("local", Some("u1"), "甲", None, true)];
        let text = r#"[
            { "id": "n1", "nickname": "无uid", "access_token": "t1" },
            { "id": "n2", "uid": "u-other", "nickname": "uid不匹配", "access_token": "t2" }
        ]"#;
        let result = merge_import_records(&mut accounts, text, &[0, 1]).unwrap();
        assert_eq!(result.imported, 2);
        assert_eq!(result.overwritten, 0);
        assert_eq!(accounts.len(), 3);
    }

    #[test]
    fn merge_skips_missing_token() {
        let mut accounts: Vec<Value> = vec![];
        let text = r#"[{ "id": "no-token", "uid": "u9", "nickname": "缺token" }]"#;
        let result = merge_import_records(&mut accounts, text, &[0]).unwrap();
        assert_eq!(result.imported, 0);
        assert_eq!(result.skipped, 1);
        assert!(accounts.is_empty(), "缺 token 的记录不得进入账号库");
    }

    #[test]
    fn merge_counts_out_of_range_index_as_skipped() {
        let mut accounts: Vec<Value> = vec![];
        let result = merge_import_records(&mut accounts, r#"[{ "access_token": "t" }]"#, &[5]).unwrap();
        assert_eq!(result.imported, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn merge_deduplicates_within_file() {
        let mut accounts: Vec<Value> = vec![];
        let text = r#"[
            { "id": "f1", "uid": "u9", "access_token": "t1" },
            { "id": "f2", "uid": "u9", "access_token": "t2" }
        ]"#;
        let result = merge_import_records(&mut accounts, text, &[0, 1]).unwrap();
        assert_eq!(result.imported, 2);
        assert_eq!(result.overwritten, 1);
        assert_eq!(accounts.len(), 1, "文件内重复 uid 也不得产生重复账号");
        assert_eq!(accounts[0]["id"], "f2");
    }

    #[test]
    fn export_requires_selection() {
        let accounts = vec![record("a1", Some("u1"), "甲", None, true)];
        assert!(select_export_records(&accounts, &[]).is_err());
        assert!(select_export_records(&accounts, &["missing".to_string()]).is_err());
    }

    #[test]
    fn export_returns_full_records_with_tokens() {
        let accounts = vec![
            record("a1", Some("u1"), "甲", Some("a@b.c"), true),
            record("a2", Some("u2"), "乙", None, true),
        ];
        let exported =
            select_export_records(&accounts, &["a1".to_string(), "missing".to_string()]).unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0]["id"], "a1");
        assert_eq!(exported[0]["access_token"], "token-a1");
    }

    #[test]
    fn exported_records_roundtrip_import() {
        let accounts = vec![json!({
            "id": "a1",
            "uid": "u1",
            "nickname": "甲",
            "email": "a@b.c",
            "access_token": "tok-1",
            "refresh_token": "ref-1",
            "auth_raw": { "k": "v" },
            "createdAt": 1,
        })];
        let exported = select_export_records(&accounts, &["a1".to_string()]).unwrap();
        let text = serde_json::to_string(&exported).unwrap();
        let mut target: Vec<Value> = vec![];
        let result = merge_import_records(&mut target, &text, &[0]).unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(target.len(), 1);
        assert_eq!(target[0]["access_token"], "tok-1", "round-trip 保留 access_token");
        assert_eq!(target[0]["refresh_token"], "ref-1");
        assert_eq!(target[0]["auth_raw"]["k"], "v");
    }

    #[test]
    fn preview_exposes_only_desensitized_fields() {
        let text = r#"[{
            "id": "a1",
            "uid": "u1",
            "nickname": "小明",
            "email": "x@y.z",
            "access_token": "SECRET"
        }]"#;
        let preview = preview_accounts(text).unwrap();
        assert_eq!(preview["total"], 1);
        assert_eq!(preview["accounts"][0]["index"], 0);
        assert_eq!(preview["accounts"][0]["uid"], "u1");
        assert_eq!(preview["accounts"][0]["nickname"], "小明");
        assert_eq!(preview["accounts"][0]["email"], "x@y.z");
        assert_eq!(preview["accounts"][0]["hasToken"], true);
        assert!(preview["accounts"][0].get("access_token").is_none(), "预览不得泄露 token");
    }
}
