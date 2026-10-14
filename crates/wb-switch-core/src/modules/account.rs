//! 账号存储：读取/写入 `~/.wb-switch/accounts.json`，与 Python 版共享数据目录。
//!
//! 对照 server.py `load_accounts` / `save_accounts` / `find_account` /
//! `account_display_name` / `account_meta`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::modules::config::{accounts_file, atomic_write, now_ms};
use crate::modules::variant::WbVariant;

/// 判断字段是否为 WorkBuddy 5.6 加密信封对象（`{$wbEncrypted, envelope}`）。
///
/// 导出导入预览与采集共用同一判定，不要在调用方复制实现。
pub fn is_envelope(v: &Value, key: &str) -> bool {
    matches!(v.get(key), Some(Value::Object(map)) if map.contains_key("$wbEncrypted"))
}

/// 是否持有未过期的明文 access_token（OAuth 扫码所得形态）。
/// 无 expiresAt 时视为有效（保守：不因缺字段丢弃明文凭据）。
fn has_unexpired_plain_token(acc: &Value) -> bool {
    let Some(Value::String(s)) = acc.get("access_token") else {
        return false;
    };
    if s.trim().is_empty() {
        return false;
    }
    match acc.get("expiresAt").and_then(|v| v.as_i64()) {
        Some(exp) => exp > now_ms(),
        None => true,
    }
}

fn load_accounts_from_path(path: &Path) -> Vec<Value> {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(Value::Array(accounts)) = serde_json::from_str::<Value>(&text) {
            return accounts;
        }
    }
    vec![]
}

fn save_accounts_to_path(path: &Path, accounts: &[Value]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(accounts).unwrap_or_default();
    atomic_write(path, &content)
}

fn find_account_in(accounts: &[Value], account_id: &str) -> Option<Value> {
    accounts
        .iter()
        .find(|account| {
            account.get("id").and_then(Value::as_str) == Some(account_id)
                || account.get("uid").and_then(Value::as_str) == Some(account_id)
        })
        .cloned()
}

fn delete_account_from_path(path: &Path, account_id: &str) -> Result<(), String> {
    let mut accounts = load_accounts_from_path(path);
    let before = accounts.len();
    accounts.retain(|account| account.get("id").and_then(Value::as_str) != Some(account_id));
    if accounts.len() == before {
        return Err("账号不存在".to_string());
    }
    save_accounts_to_path(path, &accounts).map_err(|error| error.to_string())
}

/// 读取账号库；文件缺失或损坏返回空列表。
pub fn load_accounts() -> Vec<Value> {
    load_accounts_from_path(&accounts_file())
}

/// 指定工具存储根下的账号库路径（会话操作等注入路径的场景用）。
pub fn accounts_file_in(store_root: &Path) -> std::path::PathBuf {
    store_root.join("accounts.json")
}

/// 读取指定账号库文件；文件缺失或损坏返回空列表。
pub fn load_accounts_at(path: &Path) -> Vec<Value> {
    load_accounts_from_path(path)
}

/// 写回账号库（原子写），保持原 JSON 数组结构。
pub fn save_accounts(accounts: &[Value]) -> std::io::Result<()> {
    save_accounts_to_path(&accounts_file(), accounts)
}

/// 按 id 或 uid 查找账号。
pub fn find_account(account_id: &str) -> Option<Value> {
    find_account_in(&load_accounts(), account_id)
}

/// 从 profile_raw 读取官方手机号（仅接受非空字符串）。
///
/// 手机号不落库、每次实时读取，避免与官方数据产生第二份副本。
fn profile_phone_number(acc: &Value) -> Option<String> {
    acc.get("profile_raw")
        .and_then(|p| p.get("phoneNumber"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 账号展示名：按本地 `displayField` 选择（note / phone / nickname），
/// 兜底 email → nickname → uid → unknown。
///
/// `displayField` 缺失或取值异常时行为与改造前一致（email → nickname → uid）。
pub fn account_display_name(acc: &Value) -> String {
    let by_field = match get_str(acc, "displayField").as_deref() {
        Some("note") => get_str(acc, "note"),
        Some("phone") => profile_phone_number(acc),
        _ => None,
    };
    by_field
        .or_else(|| get_str(acc, "email"))
        .or_else(|| get_str(acc, "nickname"))
        .or_else(|| get_str(acc, "uid"))
        .unwrap_or_else(|| "unknown".to_string())
}

/// 账号的展示元数据（不泄露 token）。对照 server.py `account_meta`。
/// 展示字段一律走 `display_value`：WorkBuddy 5.6 起 nickname/phoneNumber 可能是
/// 加密信封对象，裸透传会导致前端 React error #31（白屏）。
pub fn account_meta(acc: &Value) -> Value {
    json!({
        "id": display_value(acc, "id"),
        "uid": display_value(acc, "uid"),
        "email": display_value(acc, "email"),
        "nickname": display_value(acc, "nickname"),
        "phoneNumber": profile_phone_number(acc).map(Value::String).unwrap_or(Value::Null),
        "note": display_value(acc, "note"),
        "displayField": display_value(acc, "displayField"),
        "enterpriseName": display_value(acc, "enterpriseName"),
        "expiresAt": display_value(acc, "expiresAt"),
        "refreshExpiresAt": display_value(acc, "refreshExpiresAt"),
        "refreshedAt": display_value(acc, "refreshedAt"),
        "createdAt": display_value(acc, "createdAt"),
        "lastUsedAt": display_value(acc, "lastUsedAt"),
        "lastUsedClient": display_value(acc, "lastUsedClient"),
        "needsRelogin": acc.get("needs_relogin").and_then(|v| v.as_bool()) == Some(true),
        "needsReloginReason": display_value(acc, "needs_relogin_reason"),
        // 企业版标记（issue #167）：读取时判定，供前端展示「企业版」徽标，历史数据零迁移。
        "isEnterprise": is_enterprise_account(acc),
        // 档位随元数据下发，供宿主按档位过滤列表（缺省国内版，历史数据零迁移）。
        "variant": variant_of(acc).as_str(),
    })
}

/// 取非空字符串字段；空/缺失返回 None。
pub fn get_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 展示型字段安全读取：标量原样返回；对象/数组（如 WorkBuddy 5.6 引入的
/// `{$wbEncrypted, envelope}` 加密信封）折叠为 Null，避免对象漏进前端
/// 被当作 React 子节点渲染导致整树卸载（白屏）。
pub fn display_value(acc: &Value, key: &str) -> Value {
    match acc.get(key) {
        Some(v @ (Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null)) => v.clone(),
        _ => Value::Null,
    }
}

/// 字段值读取：接受明文字符串或 WorkBuddy 5.6 加密信封对象，其他类型返回 None。
/// 信封在本机同一 keyblob 下可由 WorkBuddy 自行解密，导入与切换写回时需原样保留。
/// 空白字符串按「没有值」处理（与 `get_str` 的空值语义一致）：否则 auth 文件里的
/// `"accessToken": ""` 会被判为已有 token，导入一条空凭据账号。
pub fn secret_value(v: &Value, key: &str) -> Option<Value> {
    match v.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(Value::String(s.clone())),
        Some(o @ Value::Object(map)) if map.contains_key("$wbEncrypted") => Some(o.clone()),
        _ => None,
    }
}

/// 账号档位：显式 `variant` 字段优先，缺失时按 `domain` 后缀兜底，缺省国内版。
pub fn variant_of(acc: &Value) -> WbVariant {
    WbVariant::from_account(acc)
}

/// 企业 ID 字段读取（兼容 camelCase `enterpriseId` 与 snake_case `enterprise_id`）。
///
/// 官方客户端以该字段是否存在分流个人版 / 企业版积分接口（issue #167）；
/// 请求头（[`build_auth_headers`]）与积分查询共用同一字段口径，不要各写一份。
pub fn enterprise_id_of(account: &Value) -> Option<String> {
    get_str(account, "enterpriseId").or_else(|| get_str(account, "enterprise_id"))
}

/// 是否企业版账号：有企业 ID 直接命中；历史备份可能只带
/// `type ∈ {ultimate, exclusive}` 而缺 ID（对齐 WorkDaddy 兜底判定），
/// 这类账号仍需走企业分流，取数前补拉企业 ID。
pub fn is_enterprise_account(account: &Value) -> bool {
    enterprise_id_of(account).is_some()
        || matches!(
            get_str(account, "type").as_deref(),
            Some("ultimate" | "exclusive")
        )
}

/// 合并采集结果时保留已有档位：已入库的档位不得因再次采集而丢失。
/// 采集结果自带档位时以它为准（记录里的凭据来自该档位）。
fn inherit_existing_variant(existing: &Value, collected: &mut Value) {
    if get_str(collected, "variant").is_none() {
        if let Some(variant) = get_str(existing, "variant") {
            collected["variant"] = Value::String(variant);
        }
    }
}

/// 返回可用于 UID 缺失场景的真实邮箱。历史展示占位值不参与身份匹配。
fn identity_email(account: &Value) -> Option<String> {
    let email = get_str(account, "email")?;
    if !email.contains('@')
        || email.eq_ignore_ascii_case("unknown")
        || email == "手动添加"
        || get_str(account, "nickname").as_deref() == Some(email.as_str())
        || get_str(account, "uid").as_deref() == Some(email.as_str())
    {
        return None;
    }
    Some(email.to_ascii_lowercase())
}

/// 按稳定身份将采集结果合并到账号列表，并返回最终持久化的账号。
///
/// 非空 UID 始终优先；仅当新账号没有 UID 时，才使用真实邮箱兜底。
/// 命中已有身份时保留本地 id，避免调用方持有的账号引用失效。
pub fn upsert_collected_account(accounts: &mut Vec<Value>, mut collected: Value) -> Value {
    let collected_uid = get_str(&collected, "uid");
    let collected_email = identity_email(&collected);
    let matches_identity = |existing: &Value| {
        if let Some(uid) = collected_uid.as_deref() {
            return get_str(existing, "uid").as_deref() == Some(uid);
        }
        collected_email
            .as_deref()
            .is_some_and(|email| identity_email(existing).as_deref() == Some(email))
    };

    let matching_indexes: Vec<usize> = accounts
        .iter()
        .enumerate()
        .filter_map(|(index, existing)| matches_identity(existing).then_some(index))
        .collect();

    if let Some(&first_index) = matching_indexes.first() {
        let existing = &accounts[first_index];

        // WorkBuddy 5.6 加密态保护：本机重导入得到的是加密信封 token；若已有
        // 记录仍持有未过期的明文 token（OAuth 扫码所得），不得让信封覆盖明文
        // —— 否则 UI 每次自动 importLocal 都会把扫码凭据冲掉，签到/积分等
        // 需要明文 token 的功能随之失效。明文过期后才放行信封接管。
        if is_envelope(&collected, "access_token") && has_unexpired_plain_token(existing) {
            if get_str(existing, "id").is_some() {
                return existing.clone();
            }
            // 凭据保护短路时也修复缺失的本地 id，但不动任何 token 或展示字段。
            let mut preserved = existing.clone();
            preserved["id"] = get_str(&collected, "id")
                .map(Value::String)
                .unwrap_or_else(|| Value::String(uuid::Uuid::new_v4().to_string()));
            accounts[first_index] = preserved.clone();
            return preserved;
        }
        // 展示字段兜底：新采集为信封时保留已有记录的明文展示值。
        for key in ["nickname", "email", "enterpriseName"] {
            if is_envelope(&collected, key) {
                if let Some(v) = existing.get(key) {
                    collected[key] = v.clone();
                }
            }
        }
        // 本地字段（备注 / 显示选择 / 最后使用）不来自官方采集：采集结果缺失时保留已有值。
        for key in ["note", "displayField", "lastUsedAt", "lastUsedClient"] {
            if collected.get(key).is_none() {
                if let Some(v) = existing.get(key) {
                    collected[key] = v.clone();
                }
            }
        }

        if let Some(existing_id) = get_str(existing, "id") {
            collected["id"] = Value::String(existing_id);
        } else if get_str(&collected, "id").is_none() {
            // 同 uid 的历史记录也可能缺 id；命中覆盖分支时同样维持账号库不变量。
            collected["id"] = Value::String(uuid::Uuid::new_v4().to_string());
        }
        if get_str(&collected, "uid").is_none() {
            if let Some(existing_uid) = existing.get("uid").cloned() {
                collected["uid"] = existing_uid;
            }
        }
        if let Some(created_at) = existing.get("createdAt").cloned() {
            collected["createdAt"] = created_at;
        }
        inherit_existing_variant(existing, &mut collected);

        for index in matching_indexes.into_iter().rev() {
            accounts.remove(index);
        }
        accounts.insert(first_index.min(accounts.len()), collected.clone());
    } else {
        // 追加分支同样保证入库记录带 id（账号库不允许无 id 记录）。
        if get_str(&collected, "id").is_none() {
            collected["id"] = Value::String(uuid::Uuid::new_v4().to_string());
        }
        accounts.push(collected.clone());
    }

    collected
}

/// 使用统一身份规则保存采集到的账号。
pub fn save_collected_account(collected: Value) -> std::io::Result<Value> {
    let mut accounts = load_accounts();
    let saved = upsert_collected_account(&mut accounts, collected);
    save_accounts(&accounts)?;
    Ok(saved)
}

/// 按 id 覆盖；id 缺失或未命中时按 uid 回退覆盖，仍未命中则补 id 追加。
pub fn upsert_account(updated: &Value) -> std::io::Result<()> {
    let mut accounts = load_accounts();
    upsert_account_in(&mut accounts, updated);
    save_accounts(&accounts)
}

/// 备注长度上限（字符数），与前端输入框 maxLength 保持一致。
const NOTE_MAX_CHARS: usize = 24;

/// 更新账号的本地展示字段：`note`（字符串或 null=清空）与 `displayField`。
///
/// 只触碰本地展示字段，不修改 uid / token / profile_raw 等官方数据。
/// 先整体校验再写入，避免部分更新。返回更新后的 `account_meta`。
pub fn update_account_display(account_id: &str, patch: &Value) -> Result<Value, String> {
    let mut accounts = load_accounts();
    let updated = update_account_display_in(&mut accounts, account_id, patch)?;
    save_accounts(&accounts).map_err(|error| error.to_string())?;
    Ok(account_meta(&updated))
}

/// 更新账号展示字段的内存实现（不落盘，便于单测）。
///
/// 校验失败时不修改任何记录；成功时返回更新后的完整记录。
pub fn update_account_display_in(
    accounts: &mut [Value],
    account_id: &str,
    patch: &Value,
) -> Result<Value, String> {
    let index = accounts
        .iter()
        .position(|a| get_str(a, "id").as_deref() == Some(account_id))
        .ok_or_else(|| "账号不存在".to_string())?;

    let note_update = match patch.get("note") {
        None => None,
        Some(Value::Null) => Some(Value::Null),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Some(Value::Null)
            } else if trimmed.chars().count() > NOTE_MAX_CHARS {
                return Err(format!("备注不能超过 {NOTE_MAX_CHARS} 个字符"));
            } else {
                Some(Value::String(trimmed.to_string()))
            }
        }
        Some(_) => return Err("备注格式无效".to_string()),
    };
    let field_update = match patch.get("displayField") {
        None => None,
        Some(value) => {
            let field = value.as_str().unwrap_or("");
            if !matches!(field, "nickname" | "phone" | "note") {
                return Err("显示字段无效".to_string());
            }
            Some(Value::String(field.to_string()))
        }
    };

    let obj = accounts[index]
        .as_object_mut()
        .ok_or_else(|| "账号记录损坏".to_string())?;
    if let Some(note) = note_update {
        obj.insert("note".to_string(), note);
    }
    if let Some(field) = field_update {
        obj.insert("displayField".to_string(), field);
    }

    Ok(accounts[index].clone())
}

/// 「最后使用」记账的来源端标识，取值与前端 `ToolId`（`src/lib/supported-tools.ts`）一致。
pub const USED_CLIENT_WORKBUDDY: &str = "workbuddy";
pub const USED_CLIENT_CODEBUDDY_CLI: &str = "codebuddyCli";
pub const USED_CLIENT_CODEBUDDY_IDE: &str = "codebuddyIde";
pub const USED_CLIENT_VSCODE_EXT: &str = "vscodeExt";
pub const USED_CLIENT_JETBRAINS: &str = "jetbrains";

/// 记录一次「该账号被切为某端的当前账号」：写入 `lastUsedAt`（毫秒）与 `lastUsedClient`。
///
/// 只补丁这两个本地字段，不触碰 token 等官方数据。由各端的切换收尾调用，
/// 此时切换已经生效，记账失败只影响「最后使用」展示，调用方忽略返回值即可。
pub fn mark_used(account_id: &str, client: &str) -> std::io::Result<()> {
    let mut accounts = load_accounts();
    if !mark_used_in(&mut accounts, account_id, client, now_ms()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "账号不存在",
        ));
    }
    save_accounts(&accounts)
}

/// 「最后使用」记账的内存实现（不落盘，便于单测）。返回是否命中账号。
pub fn mark_used_in(
    accounts: &mut [Value],
    account_id: &str,
    client: &str,
    timestamp: i64,
) -> bool {
    let index = accounts
        .iter()
        .position(|a| get_str(a, "id").as_deref() == Some(account_id));
    let Some(index) = index else {
        return false;
    };
    let Some(obj) = accounts[index].as_object_mut() else {
        return false;
    };
    obj.insert("lastUsedAt".to_string(), json!(timestamp));
    obj.insert(
        "lastUsedClient".to_string(),
        Value::String(client.to_string()),
    );
    true
}

/// 覆盖写入的内存实现：按 id 覆盖；id 缺失或未命中时回退按 uid 收敛到已有
/// 记录；仍无归属才追加，且追加前补 id。
///
/// 无 id 但有 uid 的历史记录按 uid 覆盖，并在原记录上补 id；新追加记录也补 id。
/// 两种身份都缺失的旧记录无法在此处安全关联到原行，导入入口会先生成 id。
fn upsert_account_in(accounts: &mut Vec<Value>, updated: &Value) {
    let id = get_str(updated, "id");
    let uid = get_str(updated, "uid");
    let id_matched_index = id.as_deref().and_then(|id| {
        accounts
            .iter()
            .position(|a| get_str(a, "id").as_deref() == Some(id))
    });
    let uid_matched_index = id_matched_index
        .is_none()
        .then(|| {
            uid.as_deref().and_then(|uid| {
                accounts
                    .iter()
                    .position(|a| get_str(a, "uid").as_deref() == Some(uid))
            })
        })
        .flatten();
    let matched_index = id_matched_index.or(uid_matched_index);

    if let Some(index) = matched_index {
        let mut next = updated.clone();
        inherit_existing_variant(&accounts[index], &mut next);
        // 本地字段（备注 / 显示选择 / 最后使用）不从官方数据来：updated 缺失时保留已有值。
        if let Some(obj) = next.as_object_mut() {
            for key in ["note", "displayField", "lastUsedAt", "lastUsedClient"] {
                if !obj.contains_key(key) {
                    if let Some(v) = accounts[index].get(key) {
                        obj.insert(key.to_string(), v.clone());
                    }
                }
            }
        }
        // uid 回退时保留本地稳定 id，即使刷新对象携带了另一 id；历史脏数据
        // 的空 id 不继承，优先保留刷新对象的有效 id，否则生成新 id。
        if uid_matched_index.is_some() {
            if let Some(existing_id) = get_str(&accounts[index], "id") {
                next["id"] = Value::String(existing_id);
            }
        }
        if get_str(&next, "id").is_none() {
            next["id"] = Value::String(uuid::Uuid::new_v4().to_string());
        }
        accounts[index] = next;
        return;
    }

    let mut appended = updated.clone();
    if get_str(&appended, "id").is_none() {
        appended["id"] = Value::String(uuid::Uuid::new_v4().to_string());
    }
    accounts.push(appended);
}

/// WorkBuddy 5.6 加密信封凭据的可读错误：`access_token` 为信封形态时返回提示文案。
///
/// 信封 token 无法解出明文，不能用于签到 / 积分 / 旅行等 API 请求；此前会经
/// [`build_auth_headers`] 的 `unwrap_or_default()` 兜底成空 `Bearer`，被网关
/// 401 后再把 HTML 错误页原样回显到界面（issue #94）。需要账号身份的请求
/// 发出前应先用本函数短路。
pub fn envelope_token_error(account: &Value) -> Option<String> {
    if is_envelope(account, "access_token") {
        return Some(
            "该账号凭据为 WorkBuddy 加密信封态，无法直接调用签到 / 积分 / Token 统计等接口；\
             切换功能不受影响，如需上述功能请删除该账号后改用「OAuth 扫码添加」获取明文凭据。"
                .to_string(),
        );
    }
    None
}

/// 构造与官方对齐的请求头。对照 server.py `build_auth_headers`。
///
/// 注意：`access_token` 为加密信封对象时 `get_str` 取不到值，这里会产出空
/// `Bearer`——调用方必须先用 [`envelope_token_error`] 拦截，不要把空凭据
/// 真的发出去（issue #94）。
pub fn build_auth_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!(
            "Bearer {}",
            get_str(account, "access_token").unwrap_or_default()
        ),
    );
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Some(uid) = get_str(account, "uid") {
        headers.insert("X-User-Id".to_string(), uid);
    }
    if let Some(eid) = enterprise_id_of(account) {
        headers.insert("X-Enterprise-Id".to_string(), eid.clone());
        headers.insert("X-Tenant-Id".to_string(), eid);
    }
    if let Some(domain) = get_str(account, "domain") {
        headers.insert("X-Domain".to_string(), domain);
    }
    headers
}

/// 删除账号（按 id）。
pub fn delete_account(account_id: &str) -> Result<(), String> {
    delete_account_from_path(&accounts_file(), account_id)
}

/// 导入本机当前账号（从该档位的登录态文件读取）。
pub fn import_local(variant: WbVariant) -> Result<Value, String> {
    let acc = crate::modules::auth_file::import_from_auth_file(variant)
        .ok_or("未读取到本地 WorkBuddy 登录信息")?;
    let saved = save_collected_account(acc).map_err(|e| e.to_string())?;
    Ok(account_meta(&saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 回归 issue #94：信封凭据要能被识别并给出可读错误，明文/缺字段不误报。
    #[test]
    fn envelope_token_error_only_fires_on_envelope_access_token() {
        let envelope = json!({
            "id": "a1",
            "access_token": {"$wbEncrypted": true, "envelope": "…"},
            "refresh_token": {"$wbEncrypted": true, "envelope": "…"},
        });
        let err = envelope_token_error(&envelope).expect("信封 access_token 应返回错误");
        assert!(err.contains("信封"), "错误文案应可读：{err}");
        assert!(err.contains("OAuth"), "应给出扫码重新添加的指引：{err}");

        let plain = json!({"id": "a2", "access_token": "SECRET", "refresh_token": "R"});
        assert!(envelope_token_error(&plain).is_none(), "明文凭据不应报错");

        let legacy = json!({"id": "a3"});
        assert!(
            envelope_token_error(&legacy).is_none(),
            "缺 access_token 的历史账号不在此拦截（保持既有行为）"
        );
    }

    #[test]
    fn account_meta_strips_tokens() {
        let acc = json!({
            "id": "a1",
            "uid": "u1",
            "email": "x@y.z",
            "nickname": "小明",
            "enterpriseName": "某公司",
            "access_token": "SECRET_ACCESS",
            "refresh_token": "SECRET_REFRESH",
            "expiresAt": 123456,
            "needs_relogin": true,
            "needs_relogin_reason": "刷新失败",
        });
        let meta = account_meta(&acc);
        assert_eq!(meta["id"], "a1");
        assert_eq!(meta["needsRelogin"], true);
        assert_eq!(meta["needsReloginReason"], "刷新失败");
        assert!(meta.get("access_token").is_none(), "不得泄露 token");
        assert!(meta.get("refresh_token").is_none(), "不得泄露 token");
    }

    #[test]
    fn account_meta_carries_variant_for_filtering() {
        // 无字段的历史账号按国内版解释，前端仍能按 variant 过滤。
        assert_eq!(account_meta(&json!({"id": "a1"}))["variant"], "cn");
        assert_eq!(
            account_meta(&json!({"id": "a2", "variant": "ai"}))["variant"],
            "ai"
        );
        // 域名为空的账号同样落回国内版（domain 兜底见 variant 单测）。
        assert_eq!(
            account_meta(&json!({"id": "a3", "domain": "", "variant": ""}))["variant"],
            "cn"
        );
    }

    /// 回归 issue #167：企业版判定矩阵（enterpriseId / type 兜底 / 普通账号不命中）。
    #[test]
    fn enterprise_account_detection_matrix() {
        assert!(is_enterprise_account(&json!({"enterpriseId": "e-1"})));
        assert!(is_enterprise_account(&json!({"enterprise_id": "e-2"})));
        assert_eq!(
            enterprise_id_of(&json!({"enterpriseId": "  e-3  "})),
            Some("e-3".to_string()),
            "企业 ID 应 trim"
        );
        // 老备份可能只带 type 而缺 ID：仍判定为企业版（取数前补拉 ID）。
        assert!(is_enterprise_account(&json!({"type": "ultimate"})));
        assert!(is_enterprise_account(&json!({"type": "exclusive"})));
        // 普通个人版账号不命中。
        assert!(!is_enterprise_account(&json!({"type": "personal"})));
        assert!(!is_enterprise_account(&json!({"type": ""})));
        assert!(!is_enterprise_account(&json!({})));
        // 空串企业 ID 不算命中。
        assert!(!is_enterprise_account(&json!({"enterpriseId": "  "})));
        assert_eq!(enterprise_id_of(&json!({"enterprise_id": ""})), None);
    }

    /// account_meta 下发企业标记，供前端展示「企业版」徽标。
    #[test]
    fn account_meta_exposes_enterprise_flag() {
        assert_eq!(
            account_meta(&json!({"id": "a1", "enterpriseId": "e-1"}))["isEnterprise"],
            true
        );
        assert_eq!(
            account_meta(&json!({"id": "a2", "type": "ultimate"}))["isEnterprise"],
            true
        );
        assert_eq!(
            account_meta(&json!({"id": "a3", "type": "personal"}))["isEnterprise"],
            false
        );
        assert_eq!(account_meta(&json!({"id": "a4"}))["isEnterprise"], false);
    }

    #[test]
    fn account_display_name_priority() {
        assert_eq!(
            account_display_name(&json!({"email": "a@b.c", "nickname": "n"})),
            "a@b.c"
        );
        assert_eq!(
            account_display_name(&json!({"nickname": "n", "uid": "u"})),
            "n"
        );
        assert_eq!(account_display_name(&json!({"uid": "u"})), "u");
        assert_eq!(account_display_name(&json!({})), "unknown");
    }

    #[test]
    fn get_str_trims_and_filters_empty() {
        assert_eq!(get_str(&json!({"k": "  v  "}), "k"), Some("v".to_string()));
        assert_eq!(get_str(&json!({"k": "  "}), "k"), None);
        assert_eq!(get_str(&json!({"k": 123}), "k"), None);
    }

    fn account(id: &str, uid: Option<&str>, nickname: &str, email: Option<&str>) -> Value {
        json!({
            "id": id,
            "uid": uid,
            "nickname": nickname,
            "email": email,
            "access_token": format!("token-{id}"),
            "createdAt": 1,
        })
    }

    #[test]
    fn same_nickname_with_different_uids_is_retained() {
        let mut accounts = vec![account("old", Some("uid-1"), "同名", Some("同名"))];
        let saved =
            upsert_collected_account(&mut accounts, account("new", Some("uid-2"), "同名", None));

        assert_eq!(accounts.len(), 2);
        assert_eq!(saved["id"], "new");
    }

    #[test]
    fn same_uid_refresh_preserves_local_id_and_removes_duplicates() {
        let mut accounts = vec![
            account("stable", Some("uid-1"), "旧名称", Some("old@example.com")),
            account("duplicate", Some("uid-1"), "重复记录", None),
        ];
        let saved = upsert_collected_account(
            &mut accounts,
            account("generated", Some("uid-1"), "新名称", None),
        );

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["id"], "stable");
        assert_eq!(saved["nickname"], "新名称");
        assert_eq!(saved["access_token"], "token-generated");
    }

    #[test]
    fn different_uids_with_same_real_email_are_retained() {
        let mut accounts = vec![account(
            "old",
            Some("uid-1"),
            "账号一",
            Some("shared@example.com"),
        )];
        upsert_collected_account(
            &mut accounts,
            account("new", Some("uid-2"), "账号二", Some("shared@example.com")),
        );

        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn real_email_is_fallback_only_when_collected_uid_is_missing() {
        let mut accounts = vec![account("stable", None, "旧名称", Some("user@example.com"))];
        let saved = upsert_collected_account(
            &mut accounts,
            account("generated", None, "新名称", Some("USER@example.com")),
        );

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["id"], "stable");
        assert_eq!(saved["nickname"], "新名称");
    }

    #[test]
    fn legacy_synthetic_email_does_not_merge_accounts() {
        let mut accounts = vec![account("old", None, "同名", Some("同名"))];
        upsert_collected_account(&mut accounts, account("new", None, "同名", Some("同名")));

        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn variant_of_defaults_to_cn_and_reads_domain_fallback() {
        assert_eq!(variant_of(&json!({"uid": "u-1"})), WbVariant::Cn);
        assert_eq!(
            variant_of(&json!({"uid": "u-1", "variant": "ai"})),
            WbVariant::Ai
        );
        assert_eq!(
            variant_of(&json!({"uid": "u-1", "domain": "www.workbuddy.ai"})),
            WbVariant::Ai
        );
    }

    #[test]
    fn upsert_keeps_variant_of_recollected_account() {
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "variant": "ai",
            "access_token": "old-token",
        })];
        // 不含档位字段的再次采集（如刷新）不得丢掉已入库档位
        let saved =
            upsert_collected_account(&mut accounts, account("a-1", Some("uid-1"), "新名称", None));

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["variant"], "ai");
        assert_eq!(accounts[0]["variant"], "ai");
    }

    #[test]
    fn envelope_reimport_keeps_unexpired_plain_oauth_token() {
        let envelope = json!({"$wbEncrypted": 1, "envelope": "enc"});
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "nickname": "明文昵称",
            "access_token": "plain-token",
            "refresh_token": "plain-refresh",
            "expiresAt": crate::modules::config::now_ms() + 86_400_000_i64,
        })];
        // UI 自动 importLocal 会拿本机加密态重采集同一 uid：
        // 不得让信封覆盖仍未过期的明文 token（否则签到/积分失效）。
        let collected = json!({
            "uid": "uid-1",
            "nickname": envelope,
            "access_token": {"$wbEncrypted": 1, "envelope": "a"},
            "refresh_token": {"$wbEncrypted": 1, "envelope": "r"},
        });
        let saved = upsert_collected_account(&mut accounts, collected);

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["access_token"], "plain-token");
        assert_eq!(saved["refresh_token"], "plain-refresh");
        assert_eq!(saved["nickname"], "明文昵称");
    }

    #[test]
    fn protected_envelope_reimport_repairs_missing_local_id_without_changing_tokens() {
        let mut accounts = vec![json!({
            "uid": "uid-1",
            "access_token": "plain-token",
            "refresh_token": "plain-refresh",
            "expiresAt": crate::modules::config::now_ms() + 86_400_000_i64,
        })];
        let collected = json!({
            "uid": "uid-1",
            "access_token": {"$wbEncrypted": 1, "envelope": "a"},
            "refresh_token": {"$wbEncrypted": 1, "envelope": "r"},
        });

        let saved = upsert_collected_account(&mut accounts, collected);

        assert!(get_str(&saved, "id").is_some());
        assert_eq!(saved["access_token"], "plain-token");
        assert_eq!(saved["refresh_token"], "plain-refresh");
        assert_eq!(accounts[0]["id"], saved["id"]);
    }

    #[test]
    fn envelope_reimport_takes_over_after_plain_token_expired() {
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "access_token": "stale-plain",
            "expiresAt": crate::modules::config::now_ms() - 1_000_i64,
        })];
        let collected = json!({
            "uid": "uid-1",
            "access_token": {"$wbEncrypted": 1, "envelope": "a"},
        });
        let saved = upsert_collected_account(&mut accounts, collected);

        assert_eq!(accounts.len(), 1);
        // 明文已过期：信封接管（切换仍可用，由 WorkBuddy 自解）。
        assert!(saved.get("access_token").and_then(|v| v.as_str()).is_none());
    }

    #[test]
    fn fresh_plain_oauth_token_replaces_envelope_record() {
        let mut accounts = vec![json!({
            "id": "a-1",
            "uid": "uid-1",
            "access_token": {"$wbEncrypted": 1, "envelope": "old"},
        })];
        // 重新扫码得到新明文：应正常替换。
        let collected = json!({
            "uid": "uid-1",
            "access_token": "fresh-plain",
            "expiresAt": crate::modules::config::now_ms() + 86_400_000_i64,
        });
        let saved = upsert_collected_account(&mut accounts, collected);

        assert_eq!(accounts.len(), 1);
        assert_eq!(saved["access_token"], "fresh-plain");
    }

    #[test]
    fn upsert_accepts_caller_supplied_variant_for_new_account() {
        let mut accounts = vec![];
        let saved = upsert_collected_account(
            &mut accounts,
            json!({"id": "a-ai", "uid": "uid-ai", "variant": "ai", "access_token": "t"}),
        );

        assert_eq!(saved["variant"], "ai");
        assert_eq!(variant_of(&accounts[0]), WbVariant::Ai);
    }

    #[test]
    fn upsert_account_keeps_existing_variant_when_updated_lacks_it() {
        let mut accounts =
            vec![json!({"id": "a-1", "uid": "uid-1", "variant": "ai", "access_token": "old"})];

        let mut refreshed = accounts[0].clone();
        refreshed["access_token"] = json!("new");
        refreshed.as_object_mut().unwrap().remove("variant");
        upsert_account_in(&mut accounts, &refreshed);

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["access_token"], "new");
        assert_eq!(accounts[0]["variant"], "ai", "覆盖写入不得丢档位");

        // 追加新账号时按调用方给定的档位入库
        upsert_account_in(
            &mut accounts,
            &json!({"id": "a-2", "uid": "uid-2", "variant": "cn", "access_token": "t2"}),
        );
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1]["variant"], "cn");
    }

    /// 回归 issue #111：无 id 记录刷新回写时按 uid 收敛，不得追加副本。
    #[test]
    fn upsert_without_id_falls_back_to_uid_and_keeps_store_size() {
        let mut accounts = vec![json!({
            "id": "local-id",
            "uid": "uid-1",
            "access_token": "old",
        })];
        let mut refreshed = accounts[0].clone();
        refreshed.as_object_mut().unwrap().remove("id");
        refreshed["access_token"] = json!("new");

        upsert_account_in(&mut accounts, &refreshed);

        assert_eq!(accounts.len(), 1, "缺 id 刷新不得追加副本");
        assert_eq!(accounts[0]["id"], "local-id", "按 uid 命中时保留库中 id");
        assert_eq!(accounts[0]["access_token"], "new");
    }

    #[test]
    fn uid_fallback_keeps_local_id_when_updated_has_a_different_id() {
        let mut accounts = vec![json!({
            "id": "local-id",
            "uid": "uid-1",
            "access_token": "old",
        })];

        upsert_account_in(
            &mut accounts,
            &json!({
                "id": "foreign-id",
                "uid": "uid-1",
                "access_token": "new",
            }),
        );

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["id"], "local-id");
        assert_eq!(accounts[0]["access_token"], "new");
    }

    #[test]
    fn uid_fallback_repairs_empty_local_id() {
        let mut accounts = vec![json!({
            "id": "  ",
            "uid": "uid-1",
            "access_token": "old",
        })];

        upsert_account_in(
            &mut accounts,
            &json!({
                "id": "",
                "uid": "uid-1",
                "access_token": "new",
            }),
        );

        assert_eq!(accounts.len(), 1);
        assert!(get_str(&accounts[0], "id").is_some());
        assert_eq!(accounts[0]["access_token"], "new");
    }

    #[test]
    fn collected_uid_match_repairs_missing_local_id() {
        let mut accounts = vec![json!({
            "id": "",
            "uid": "uid-1",
            "access_token": "old",
        })];

        let saved = upsert_collected_account(
            &mut accounts,
            json!({"uid": "uid-1", "access_token": "new"}),
        );

        assert_eq!(accounts.len(), 1);
        assert!(get_str(&saved, "id").is_some());
        assert_eq!(accounts[0]["id"], saved["id"]);
    }

    /// 既无 id 也无 uid 的记录：首次追加时补 id，之后按 id 覆盖不追加。
    #[test]
    fn upsert_without_id_or_uid_appends_once_with_generated_id() {
        let mut accounts: Vec<Value> = vec![];
        upsert_account_in(&mut accounts, &json!({"access_token": "t1"}));

        assert_eq!(accounts.len(), 1);
        let id = accounts[0]["id"].as_str().unwrap_or_default().to_string();
        assert!(!id.is_empty(), "追加时必须补 id");

        let mut again = accounts[0].clone();
        again["access_token"] = json!("t2");
        upsert_account_in(&mut accounts, &again);

        assert_eq!(accounts.len(), 1, "补 id 后必须按 id 覆盖");
        assert_eq!(accounts[0]["access_token"], "t2");
        assert_eq!(accounts[0]["id"], id, "覆盖不得改变 id");
    }

    /// 回归 issue #111 的增长曲线：模拟刷新回写循环，无 id 记录连续多轮
    /// upsert 后账号库长度恒为 1，且首轮即自愈出 id。
    #[test]
    fn repeated_refresh_upsert_of_id_less_record_does_not_grow_store() {
        let mut accounts = vec![json!({"uid": "uid-1", "access_token": "t0"})];
        for round in 0..5 {
            let mut refreshed = accounts[0].clone();
            refreshed["access_token"] = json!(format!("t{round}"));
            upsert_account_in(&mut accounts, &refreshed);
        }

        assert_eq!(accounts.len(), 1, "刷新循环不得复制无 id 记录");
        assert!(
            accounts[0]["id"]
                .as_str()
                .is_some_and(|id| !id.trim().is_empty()),
            "首轮覆盖后必须自愈出 id"
        );
        assert_eq!(accounts[0]["access_token"], "t4");
    }

    /// 采集路径的追加分支同样保证入库记录带 id（纵深防御）。
    #[test]
    fn collected_account_without_identity_gets_generated_id() {
        let mut accounts: Vec<Value> = vec![];
        let saved = upsert_collected_account(&mut accounts, json!({"access_token": "t"}));

        assert_eq!(accounts.len(), 1);
        let id = saved["id"].as_str().unwrap_or_default();
        assert!(!id.is_empty(), "采集追加分支必须补 id");
        assert_eq!(accounts[0]["id"], saved["id"], "返回值与库中记录一致");
    }

    #[test]
    fn persisted_same_name_accounts_can_be_found_and_deleted_independently() {
        let test_dir = std::env::temp_dir().join(format!(
            "wb-switch-same-name-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = test_dir.join("accounts.json");
        let mut accounts = vec![];
        upsert_collected_account(
            &mut accounts,
            account("account-1", Some("uid-1"), "同名用户", None),
        );
        upsert_collected_account(
            &mut accounts,
            account("account-2", Some("uid-2"), "同名用户", None),
        );
        save_accounts_to_path(&path, &accounts).expect("same-name accounts should persist");

        let persisted = load_accounts_from_path(&path);
        assert_eq!(
            find_account_in(&persisted, "account-1").unwrap()["uid"],
            "uid-1"
        );
        assert_eq!(
            find_account_in(&persisted, "account-2").unwrap()["uid"],
            "uid-2"
        );

        delete_account_from_path(&path, "account-1").expect("first account should delete");
        let after_first_delete = load_accounts_from_path(&path);
        assert!(find_account_in(&after_first_delete, "account-1").is_none());
        assert_eq!(
            find_account_in(&after_first_delete, "account-2").unwrap()["uid"],
            "uid-2"
        );

        delete_account_from_path(&path, "account-2").expect("second account should delete");
        assert!(load_accounts_from_path(&path).is_empty());
        std::fs::remove_dir_all(&test_dir).expect("temporary account store should clean up");
    }

    // -----------------------------------------------------------------------
    // 本地展示字段：备注与显示字段选择
    // -----------------------------------------------------------------------

    /// displayField 缺失时展示名保持改造前行为（email → nickname → uid）。
    #[test]
    fn display_name_without_display_field_keeps_legacy_fallback() {
        let with_email = account("a-1", Some("uid-1"), "小明", Some("a@b.c"));
        assert_eq!(account_display_name(&with_email), "a@b.c");
        let no_email = account("a-2", Some("uid-2"), "小明", None);
        assert_eq!(account_display_name(&no_email), "小明");
    }

    /// displayField = note / phone 时优先取对应字段，取不到回退原兜底链。
    #[test]
    fn display_name_follows_display_field_with_fallback() {
        let mut acc = account("a-1", Some("uid-1"), "小明", None);
        acc["note"] = json!("工作号");
        acc["displayField"] = json!("note");
        assert_eq!(account_display_name(&acc), "工作号");

        acc["note"] = Value::Null;
        assert_eq!(account_display_name(&acc), "小明", "备注为空应回退昵称");

        acc["displayField"] = json!("phone");
        assert_eq!(account_display_name(&acc), "小明", "无手机号应回退昵称");
        acc["profile_raw"] = json!({"phoneNumber": "13800138000"});
        assert_eq!(account_display_name(&acc), "13800138000");

        acc["displayField"] = json!("nickname");
        assert_eq!(account_display_name(&acc), "小明");
    }

    /// account_meta 下发手机号（实时读 profile_raw）、备注与显示字段。
    #[test]
    fn account_meta_exposes_phone_note_and_display_field() {
        let mut acc = account("a-1", Some("uid-1"), "小明", None);
        acc["profile_raw"] = json!({"phoneNumber": "13800138000"});
        acc["note"] = json!("工作号");
        acc["displayField"] = json!("phone");
        let meta = account_meta(&acc);
        assert_eq!(meta["phoneNumber"], "13800138000");
        assert_eq!(meta["note"], "工作号");
        assert_eq!(meta["displayField"], "phone");

        // profile_raw 缺失或异常形态：手机号折叠为 null，不泄露对象。
        let bare = account("a-2", Some("uid-2"), "小红", None);
        let meta = account_meta(&bare);
        assert!(meta["phoneNumber"].is_null());
        assert!(meta["note"].is_null());
        assert!(meta["displayField"].is_null());
    }

    /// 更新接口：设置 / 清空备注、切换显示字段、只更新传入项。
    #[test]
    fn update_account_display_sets_note_and_field() {
        let mut accounts = vec![account("a-1", Some("uid-1"), "小明", None)];

        let updated = update_account_display_in(
            &mut accounts,
            "a-1",
            &json!({"note": "  工作号  ", "displayField": "note"}),
        )
        .expect("valid patch should apply");
        assert_eq!(updated["note"], "工作号", "备注应 trim");
        assert_eq!(updated["displayField"], "note");

        // 只改显示字段：备注保持。
        update_account_display_in(&mut accounts, "a-1", &json!({"displayField": "nickname"}))
            .expect("field-only patch should apply");
        assert_eq!(accounts[0]["note"], "工作号");
        assert_eq!(accounts[0]["displayField"], "nickname");

        // 显式 null 与纯空白都等价于清空。
        update_account_display_in(&mut accounts, "a-1", &json!({"note": null}))
            .expect("null note should clear");
        assert!(accounts[0]["note"].is_null());
        update_account_display_in(&mut accounts, "a-1", &json!({"note": "   "}))
            .expect("blank note should clear");
        assert!(accounts[0]["note"].is_null());
    }

    /// 更新接口的校验：非法字段 / 未知 id / 超长备注都不写入。
    #[test]
    fn update_account_display_rejects_invalid_patch() {
        let mut accounts = vec![account("a-1", Some("uid-1"), "小明", None)];

        assert!(
            update_account_display_in(&mut accounts, "a-1", &json!({"displayField": "email"}))
                .is_err(),
            "非白名单显示字段应拒绝"
        );
        assert!(
            update_account_display_in(&mut accounts, "a-1", &json!({"note": 42})).is_err(),
            "非字符串备注应拒绝"
        );
        assert!(
            update_account_display_in(&mut accounts, "missing", &json!({"note": "x"})).is_err(),
            "未知账号应报错"
        );
        let long = "字".repeat(NOTE_MAX_CHARS + 1);
        assert!(
            update_account_display_in(&mut accounts, "a-1", &json!({"note": long})).is_err(),
            "超长备注应拒绝"
        );
        // 任一失败都不应留下部分更新。
        assert!(accounts[0].get("note").is_none());
        assert!(accounts[0].get("displayField").is_none());
    }

    /// 采集合并（扫码重登）保留本地备注与显示选择。
    #[test]
    fn collected_upsert_preserves_local_display_fields() {
        let mut existing = account("a-1", Some("uid-1"), "小明", None);
        existing["note"] = json!("工作号");
        existing["displayField"] = json!("note");
        let mut accounts = vec![existing];

        let collected = account("a-1", Some("uid-1"), "小明（官方更新）", None);
        let saved = upsert_collected_account(&mut accounts, collected);
        assert_eq!(saved["note"], "工作号");
        assert_eq!(saved["displayField"], "note");
        assert_eq!(saved["nickname"], "小明（官方更新）");
    }

    /// token 刷新路径（upsert_account_in）保留本地展示字段。
    #[test]
    fn refresh_upsert_preserves_local_display_fields() {
        let mut existing = account("a-1", Some("uid-1"), "小明", None);
        existing["note"] = json!("工作号");
        existing["displayField"] = json!("phone");
        let mut accounts = vec![existing];

        let refreshed = account("a-1", Some("uid-1"), "小明", None);
        upsert_account_in(&mut accounts, &refreshed);
        assert_eq!(accounts[0]["note"], "工作号");
        assert_eq!(accounts[0]["displayField"], "phone");
    }

    // -----------------------------------------------------------------------
    // 最后使用记账
    // -----------------------------------------------------------------------

    #[test]
    fn mark_used_records_timestamp_and_client() {
        let mut accounts = vec![account("a-1", Some("uid-1"), "小明", None)];

        assert!(mark_used_in(
            &mut accounts,
            "a-1",
            USED_CLIENT_WORKBUDDY,
            1_700_000_000_000
        ));
        assert_eq!(accounts[0]["lastUsedAt"], 1_700_000_000_000_i64);
        assert_eq!(accounts[0]["lastUsedClient"], "workbuddy");
        let meta = account_meta(&accounts[0]);
        assert_eq!(meta["lastUsedAt"], 1_700_000_000_000_i64);
        assert_eq!(meta["lastUsedClient"], "workbuddy");

        // 再次切到另一端：时间与来源端一起更新，其余字段不动。
        assert!(mark_used_in(
            &mut accounts,
            "a-1",
            USED_CLIENT_VSCODE_EXT,
            1_700_000_009_999
        ));
        assert_eq!(accounts[0]["lastUsedClient"], "vscodeExt");
        assert_eq!(accounts[0]["nickname"], "小明");

        assert!(
            !mark_used_in(&mut accounts, "missing", USED_CLIENT_WORKBUDDY, 1),
            "未知账号不记账"
        );
    }

    #[test]
    fn account_meta_without_last_used_is_null() {
        let meta = account_meta(&account("a-1", Some("uid-1"), "小明", None));
        assert!(meta["lastUsedAt"].is_null());
        assert!(meta["lastUsedClient"].is_null());
    }

    /// 扫码重登 / 本机导入重建的记录不得丢「最后使用」。
    #[test]
    fn collected_upsert_preserves_last_used() {
        let mut existing = account("a-1", Some("uid-1"), "小明", None);
        existing["lastUsedAt"] = json!(1_700_000_000_000_i64);
        existing["lastUsedClient"] = json!("workbuddy");
        let mut accounts = vec![existing];

        let saved = upsert_collected_account(
            &mut accounts,
            account("a-1", Some("uid-1"), "小明（官方更新）", None),
        );
        assert_eq!(saved["lastUsedAt"], 1_700_000_000_000_i64);
        assert_eq!(saved["lastUsedClient"], "workbuddy");
    }
}

// 手动添加账号（token 方式）已随 UI 入口「手动添加」一并下线；
// `identity_email` 中的 "手动添加" 占位过滤保留，用于兼容历史手动添加的旧账号。
