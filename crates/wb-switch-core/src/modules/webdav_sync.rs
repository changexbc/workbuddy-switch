//! WebDAV 备份 / 跨设备同步：私有 WebDAV 服务器上的端到端加密账号备份。
//!
//! - 备份内容为完整账号库（含 access/refresh token 与 `auth_raw` 等授权信息）
//!   加各功能配置（签到 / 旅行 / 限额 / 自动轮换 / 更新源 / WebDAV 自身）。
//!   上传前用用户输入的加密密码派生密钥（PBKDF2-HMAC-SHA256 → AES-256-GCM）
//!   加密，WebDAV 服务器与任何中间人都接触不到凭据明文。
//! - 信封自描述：明文头携带格式版本 / 更新时间 / 设备名 / KDF 参数，密文在后；
//!   读取远端元信息无需密码，解密失败即密码错误。
//! - 同步 = 拉取 → 按 uid 合并（复用 `export_import` 的合并语义）→ 推送；
//!   推送带 `If-Match`（ETag）条件，防止覆盖另一台设备刚写入的备份。
//! - 恢复（restore）= 拉取 → 解密 → 按选中索引合并账号库 →（可选）按白名单写回配置。
//!
//! WebDAV 账号口令与加密密码的存储遵循本应用既有约定（`~/.wb-switch` 下明文
//! JSON，与 `accounts.json` 的 token 同级），不引入新的存储机制。加密密码
//! 仅会话内使用，不落盘。

use std::path::PathBuf;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use pbkdf2::pbkdf2_hmac;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::modules::{account, config, export_import, limits, update};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 信封魔数 + 格式版本号：解析时校验，未来格式升级据此分支。
const ENVELOPE_MAGIC: &[u8] = b"WBSWITCH-SYNC-V1\n";

/// KDF：PBKDF2-HMAC-SHA256 迭代次数（OWASP 2023 推荐量级）。
const KDF_ITERATIONS: u32 = 600_000;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// 远端备份文件名（远端目录下的固定文件）。
pub const REMOTE_FILE_NAME: &str = "wb-switch-backup.wbs";

/// 远端默认目录（相对 WebDAV 根）。
pub const DEFAULT_REMOTE_DIR: &str = "wb-switch";

/// 加密密码最小长度。
const MIN_PASSPHRASE_LEN: usize = 8;

/// 备份载荷格式版本。
const PAYLOAD_FORMAT: u32 = 1;

// ---------------------------------------------------------------------------
// 配置（webdav_config.json）
// ---------------------------------------------------------------------------

pub fn webdav_config_file() -> PathBuf {
    config::store_dir().join("webdav_config.json")
}

const WEBDAV_CONFIG_KEYS: [&str; 4] = ["url", "username", "password", "remotePath"];

fn default_webdav_config() -> Value {
    json!({
        "url": "",
        "username": "",
        "password": "",
        "remotePath": DEFAULT_REMOTE_DIR,
    })
}

/// 读取指定的 WebDAV 配置文件（缺失/损坏时合并默认值）。
///
/// 与 `load_webdav_config` 分离只为注入路径：单测不得触碰真实 `~/.wb-switch`。
pub fn load_webdav_config_at(path: &std::path::Path) -> Value {
    let mut cfg = default_webdav_config();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
            for key in WEBDAV_CONFIG_KEYS {
                if let Some(value) = map.get(key).and_then(Value::as_str) {
                    cfg[key] = json!(value);
                }
            }
        }
    }
    cfg
}

/// 读取 WebDAV 配置（缺失/损坏时合并默认值）。
pub fn load_webdav_config() -> Value {
    load_webdav_config_at(&webdav_config_file())
}

/// 保存 WebDAV 配置到指定路径（只保留已知字段，字段缺失回落默认值）。
pub fn save_webdav_config_at(path: &std::path::Path, cfg: &Value) -> std::io::Result<()> {
    let mut merged = default_webdav_config();
    for key in ["url", "username", "remotePath"] {
        if let Some(value) = cfg.get(key).and_then(Value::as_str) {
            merged[key] = json!(value.trim());
        }
    }
    // `password` 例外：提交为空时保留已存值。脱敏视图不回显明文口令，前端只能
    // 原样带空串回来；若空串也覆盖，改任意一项再保存就会悄悄清掉服务器口令。
    if let Some(existing) = load_webdav_config_at(path).get("password").and_then(Value::as_str) {
        merged["password"] = json!(existing);
    }
    if let Some(value) = cfg.get("password").and_then(Value::as_str) {
        if !value.trim().is_empty() {
            merged["password"] = json!(value.trim());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    config::atomic_write(path, &content)
}

/// 保存 WebDAV 配置，返回写入后的完整配置。
pub fn save_webdav_config(cfg: &Value) -> std::io::Result<Value> {
    save_webdav_config_at(&webdav_config_file(), cfg)?;
    Ok(load_webdav_config())
}

/// 返回给前端的脱敏视图：不含 WebDAV 口令明文，只带 `hasPassword` 标记。
pub fn webdav_config_view() -> Value {
    let cfg = load_webdav_config();
    json!({
        "url": cfg["url"],
        "username": cfg["username"],
        "remotePath": cfg["remotePath"],
        "hasPassword": cfg["password"].as_str().map(|v| !v.is_empty()).unwrap_or(false),
    })
}

// ---------------------------------------------------------------------------
// 信封加密（PBKDF2-SHA256 → AES-256-GCM）
// ---------------------------------------------------------------------------

/// 远端备份的明文头信息（不加密，用于展示与冲突判断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHeader {
    pub updated_at: i64,
    pub device_name: String,
    pub app_version: String,
    pub account_count: usize,
}

fn derive_key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
    key
}

fn validate_passphrase(passphrase: &str) -> Result<(), String> {
    if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
        return Err(format!("加密密码至少 {MIN_PASSPHRASE_LEN} 位"));
    }
    Ok(())
}

/// 当前设备名（跨平台尽力而为：Windows 环境变量 → `hostname` 命令 → 兜底文案）。
fn device_name() -> String {
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    "未知设备".to_string()
}

/// 纯函数：把载荷 JSON 加密为信封字节（魔数 + 头长度 + 明文头 + salt + nonce + 密文）。
fn build_envelope(payload: &Value, passphrase: &str, updated_at: i64) -> Result<Vec<u8>, String> {
    validate_passphrase(passphrase)?;
    let account_count = payload
        .get("accounts")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let header = json!({
        "format": PAYLOAD_FORMAT,
        "updatedAt": updated_at,
        "deviceName": device_name(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "accountCount": account_count,
        "kdf": "pbkdf2-hmac-sha256",
        "iterations": KDF_ITERATIONS,
        "cipher": "aes-256-gcm",
    });
    let header_bytes = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
    if header_bytes.len() > u32::MAX as usize {
        return Err("备份头超出长度限制".to_string());
    }

    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);

    let key = derive_key(passphrase, &salt, KDF_ITERATIONS);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|_| "备份加密失败".to_string())?;

    let mut out = Vec::with_capacity(
        ENVELOPE_MAGIC.len() + 4 + header_bytes.len() + SALT_LEN + NONCE_LEN + ciphertext.len(),
    );
    out.extend_from_slice(ENVELOPE_MAGIC);
    out.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// 纯函数：解析信封头（不需要密码），供「查看云端备份信息」使用。
pub fn parse_envelope_header(bytes: &[u8]) -> Result<RemoteHeader, String> {
    let (header_bytes, _, _, _) = split_envelope(bytes)?;
    let header: Value = serde_json::from_slice(header_bytes)
        .map_err(|_| "备份文件头损坏".to_string())?;
    Ok(RemoteHeader {
        updated_at: header.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
        device_name: header
            .get("deviceName")
            .and_then(Value::as_str)
            .unwrap_or("未知设备")
            .to_string(),
        app_version: header
            .get("appVersion")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        account_count: header
            .get("accountCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
    })
}

/// 校验魔数并切出（明文头, salt, nonce, 密文）。
fn split_envelope(bytes: &[u8]) -> Result<(&[u8], &[u8], &[u8], &[u8]), String> {
    if bytes.len() < ENVELOPE_MAGIC.len() + 4 || !bytes.starts_with(ENVELOPE_MAGIC) {
        return Err("不是 workbuddy-switch 的备份文件".to_string());
    }
    let rest = &bytes[ENVELOPE_MAGIC.len()..];
    let header_len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    let after_len = &rest[4..];
    if after_len.len() < header_len + SALT_LEN + NONCE_LEN {
        return Err("备份文件不完整或已损坏".to_string());
    }
    let header = &after_len[..header_len];
    let after_header = &after_len[header_len..];
    let salt = &after_header[..SALT_LEN];
    let nonce = &after_header[SALT_LEN..SALT_LEN + NONCE_LEN];
    let ciphertext = &after_header[SALT_LEN + NONCE_LEN..];
    Ok((header, salt, nonce, ciphertext))
}

/// 纯函数：解密信封，返回（头, 载荷）。密码错误时报错明确。
fn decrypt_envelope(bytes: &[u8], passphrase: &str) -> Result<(RemoteHeader, Value), String> {
    let (header_bytes, salt, nonce, ciphertext) = split_envelope(bytes)?;
    let header: Value = serde_json::from_slice(header_bytes)
        .map_err(|_| "备份文件头损坏".to_string())?;
    let iterations = header
        .get("iterations")
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .unwrap_or(KDF_ITERATIONS);
    let key = derive_key(passphrase, salt, iterations);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| "解密失败：加密密码不正确".to_string())?;
    let payload: Value = serde_json::from_slice(&plaintext)
        .map_err(|_| "备份载荷不是合法 JSON（文件已损坏）".to_string())?;
    let parsed_header = RemoteHeader {
        updated_at: header.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
        device_name: header
            .get("deviceName")
            .and_then(Value::as_str)
            .unwrap_or("未知设备")
            .to_string(),
        app_version: header
            .get("appVersion")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        account_count: payload
            .get("accounts")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0),
    };
    Ok((parsed_header, payload))
}

/// KDF 是 CPU 密集操作（60 万轮迭代），放到阻塞线程池，避免卡住异步运行时。
async fn encrypt_envelope_async(
    payload: Value,
    passphrase: String,
    updated_at: i64,
) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || build_envelope(&payload, &passphrase, updated_at))
        .await
        .map_err(|e| format!("加密任务失败：{e}"))?
}

async fn decrypt_envelope_async(bytes: Vec<u8>, passphrase: String) -> Result<(RemoteHeader, Value), String> {
    tokio::task::spawn_blocking(move || decrypt_envelope(&bytes, &passphrase))
        .await
        .map_err(|e| format!("解密任务失败：{e}"))?
}

// ---------------------------------------------------------------------------
// WebDAV 目标解析与请求
// ---------------------------------------------------------------------------

/// 解析后的远端目标：目录 URL 与备份文件 URL。
#[derive(Debug)]
struct WebdavTarget {
    dir_url: String,
    file_url: String,
    auth_header: Option<String>,
}

/// 归一化配置并解析远端 URL。
///
/// URL 允许带或不带末尾斜杠；目录名不允许路径穿越。未配置服务器时报错明确。
fn resolve_target(cfg: &Value) -> Result<WebdavTarget, String> {
    let base = cfg
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if base.is_empty() {
        return Err("请先在设置中配置 WebDAV 服务器地址".to_string());
    }
    url_parse_base(base)?;
    let dir = cfg
        .get("remotePath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_REMOTE_DIR);
    if dir.contains("..") || dir.contains('\\') {
        return Err("远端目录名不合法".to_string());
    }
    let dir = dir.trim_matches('/');
    let base = base.trim_end_matches('/');
    let dir_url = format!("{base}/{dir}");
    let file_url = format!("{dir_url}/{REMOTE_FILE_NAME}");
    let username = cfg
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let password = cfg
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("");
    let auth_header = if username.is_empty() && password.is_empty() {
        None
    } else {
        Some(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        ))
    };
    Ok(WebdavTarget {
        dir_url,
        file_url,
        auth_header,
    })
}

/// URL 校验（必须 http/https）。不引入 url crate，用现有解析能力：
/// 先交由 `reqwest::Url` 解析校验，再回传原串。
fn url_parse_base(base: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(base).map_err(|_| {
        "WebDAV 服务器地址不合法，应为 http(s):// 开头的完整 URL".to_string()
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => Err(format!("WebDAV 服务器地址不支持 {other}: 协议，请使用 http/https")),
    }
}

/// 备份上传 / 下载可能远超共享客户端的 30 秒上限，给独立的宽松超时。
const DAV_TRANSFER_TIMEOUT_SECS: u64 = 300;

/// 发送一个 WebDAV 请求（自动带 Basic 认证）。
async fn dav_request(
    target: &WebdavTarget,
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: Option<Vec<u8>>,
    extra_headers: &[(&str, String)],
) -> DavResponse {
    let mut headers = std::collections::HashMap::new();
    if let Some(auth) = &target.auth_header {
        headers.insert("Authorization".to_string(), auth.clone());
    }
    for (key, value) in extra_headers {
        headers.insert(key.to_string(), value.clone());
    }
    // 带传输体的请求（PUT 上传 / GET 下载）用独立长超时；控制面请求（PROPFIND / MKCOL）保持 30 秒。
    let timeout_secs = if matches!(method, "PUT" | "GET") {
        Some(DAV_TRANSFER_TIMEOUT_SECS)
    } else {
        None
    };
    let (status, resp_headers, body) =
        config::http_request_bytes(url, method, content_type, body, Some(&headers), true, timeout_secs)
            .await;
    DavResponse {
        status,
        headers: resp_headers,
        body,
    }
}

fn describe_status(status: u16, body: &[u8]) -> String {
    let detail = String::from_utf8_lossy(body);
    let detail = detail.trim();
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("：{}", detail.chars().take(200).collect::<String>())
    };
    format!("WebDAV 服务器返回 {status}{detail}")
}

/// 一个 WebDAV 响应：状态码 + 响应头 + 响应体字节。
pub struct DavResponse {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Vec<u8>,
}

impl DavResponse {
    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn error_text(&self) -> String {
        if self.status == 401 {
            return "WebDAV 用户名或密码错误".to_string();
        }
        describe_status(self.status, &self.body)
    }
}

/// 确保远端目录存在：PROPFIND 探测，404 时 MKCOL 创建（405 = 已存在，同样视为成功）。
async fn ensure_remote_dir(target: &WebdavTarget) -> Result<(), String> {
    let prop = dav_request(
        target,
        "PROPFIND",
        &target.dir_url,
        Some("application/xml"),
        None,
        &[("Depth", "0".to_string())],
    )
    .await;
    if prop.status == 207 || prop.status == 200 {
        return Ok(());
    }
    if prop.status == 404 {
        let mk = dav_request(target, "MKCOL", &target.dir_url, None, None, &[]).await;
        if mk.status == 201 || mk.status == 200 || mk.status == 405 {
            return Ok(());
        }
        return Err(mk.error_text());
    }
    Err(prop.error_text())
}

/// 下载远端备份文件；404（云端还没有备份）返回 `None`。
async fn download_backup(target: &WebdavTarget) -> Result<Option<DavResponse>, String> {
    let resp = dav_request(target, "GET", &target.file_url, None, None, &[]).await;
    if resp.status == 404 {
        return Ok(None);
    }
    if !resp.is_success() {
        return Err(resp.error_text());
    }
    Ok(Some(resp))
}

/// 上传信封；`if_match` 非空时带 `If-Match` 条件（ETag），412 视为冲突。
async fn upload_backup(
    target: &WebdavTarget,
    envelope: Vec<u8>,
    if_match: Option<&str>,
) -> Result<(), String> {
    ensure_remote_dir(target).await?;
    let mut extra: Vec<(&str, String)> = Vec::new();
    if let Some(tag) = if_match {
        extra.push(("If-Match", tag.to_string()));
    }
    let resp = dav_request(
        target,
        "PUT",
        &target.file_url,
        Some("application/octet-stream"),
        Some(envelope),
        &extra,
    )
    .await;
    if resp.status == 412 {
        return Err("云端备份刚被其他设备更新，为避免覆盖请重试同步".to_string());
    }
    if !resp.is_success() {
        return Err(resp.error_text());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 备份载荷
// ---------------------------------------------------------------------------

/// 固定收录的功能配置白名单（恢复时按同一名单逐项写回）。
///
/// 各模块的 `load_*` 已合并默认值，`save_*` 只保留已知键并原子写；未知键
/// （版本漂移 / 未来新增）不会在恢复时被误写。
fn load_configs_snapshot() -> Value {
    json!({
        "checkin": config::load_checkin_config(),
        "travel": config::load_travel_config(),
        "rateLimit": config::load_rate_limit_config(),
        "autoRotate": config::load_auto_rotate_config(),
        "github": update::load_github_config(),
        // 完整配置（含 WebDAV 服务器口令）：备份整包经用户加密密码保护，
        // 新设备恢复一次即可连上同一服务器，不必逐台重填口令。
        "webdav": load_webdav_config(),
    })
}

/// 构造备份载荷：完整账号库 + 功能配置（含 token 等授权信息）。
fn build_payload(accounts: &[Value], configs: &Value, exported_at: i64) -> Value {
    json!({
        "format": PAYLOAD_FORMAT,
        "exportedAt": exported_at,
        "accounts": accounts,
        "configs": configs,
    })
}

/// 把备份载荷里的 `configs` 按白名单写回本机配置。
///
/// 各 `save_*` 只保留已知键并原子写，未知键（旧版 / 未来版本）自动丢弃，
/// 与账号库的「同 uid 覆盖、缺凭据跳过」语义对齐，不会误删本地新增项。
fn apply_configs_snapshot(configs: &Value) -> std::io::Result<()> {
    let get = |key: &str| configs.get(key);
    if let Some(v) = get("checkin") {
        config::save_checkin_config(v)?;
    }
    if let Some(v) = get("travel") {
        config::save_travel_config(v)?;
    }
    if let Some(v) = get("rateLimit") {
        limits::save_rate_limit_config(v)?;
    }
    if let Some(v) = get("autoRotate") {
        config::save_auto_rotate_config(v)?;
    }
    if let Some(v) = get("github") {
        update::save_github_config(v)?;
    }
    // WebDAV 自身配置（含服务器口令，与加密密钥同保护级别）：写回后新设备
    // 一次恢复即可连上同一服务器。未知键（如脱敏视图的 `hasPassword`）会被
    // save_webdav_config_at 丢弃。
    if let Some(v) = get("webdav") {
        let _ = save_webdav_config(v);
    }
    Ok(())
}

fn load_accounts_or_err() -> Result<Vec<Value>, String> {
    let accounts = account::load_accounts();
    if accounts.is_empty() {
        return Err("本机账号库为空，没有可备份的账号".to_string());
    }
    Ok(accounts)
}

// ---------------------------------------------------------------------------
// 对外操作：测试连接 / 备份 / 恢复预览 / 恢复 / 同步
// ---------------------------------------------------------------------------

/// 测试连接：探测（必要时创建）远端目录。
pub async fn test_connection() -> Result<Value, String> {
    let target = resolve_target(&load_webdav_config())?;
    ensure_remote_dir(&target).await?;
    Ok(json!({ "ok": true, "dirUrl": target.dir_url }))
}

/// 备份：整库 + 配置加密后 PUT 覆盖远端（用户主动备份，无条件覆盖）。
pub async fn backup_now(passphrase: &str) -> Result<Value, String> {
    let target = resolve_target(&load_webdav_config())?;
    let accounts = load_accounts_or_err()?;
    let count = accounts.len();
    let configs = load_configs_snapshot();
    let envelope = encrypt_envelope_async(
        build_payload(&accounts, &configs, config::now_ms()),
        passphrase.to_string(),
        config::now_ms(),
    )
    .await?;
    upload_backup(&target, envelope, None).await?;
    Ok(json!({ "ok": true, "count": count, "updatedAt": config::now_ms() }))
}

/// 拉取并解密远端备份，返回脱敏预览（不含 token）与远端信息。
pub async fn restore_preview(passphrase: &str) -> Result<Value, String> {
    let target = resolve_target(&load_webdav_config())?;
    let resp = download_backup(&target)
        .await?
        .ok_or("云端还没有备份，无法恢复")?;
    let (_, payload) = decrypt_envelope_async(resp.body, passphrase.to_string()).await?;
    let accounts_text = payload_accounts_text(&payload)?;
    let mut result = export_import::preview_accounts(&accounts_text)?;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("remote".to_string(), remote_info_value(&target));
        // 备份是否携带配置快照（新版含 configs 字段），供前端展示「同时恢复配置」选项。
        let has_configs = payload
            .get("configs")
            .and_then(Value::as_object)
            .is_some_and(|m| !m.is_empty());
        obj.insert("hasConfigs".to_string(), json!(has_configs));
    }
    Ok(result)
}

/// 恢复：拉取解密后按选中索引合并账号库；`restore_configs` 为真且备份携带
/// 配置快照时，按白名单写回功能配置。
pub async fn restore(passphrase: &str, indexes: &[usize], restore_configs: bool) -> Result<Value, String> {
    let target = resolve_target(&load_webdav_config())?;
    let resp = download_backup(&target)
        .await?
        .ok_or("云端还没有备份，无法恢复")?;
    let (_, payload) = decrypt_envelope_async(resp.body, passphrase.to_string()).await?;
    let accounts_text = payload_accounts_text(&payload)?;
    let result = export_import::import_accounts(&accounts_text, indexes)?;
    let mut restored_configs = false;
    if restore_configs {
        if let Some(configs) = payload.get("configs").and_then(Value::as_object) {
            if !configs.is_empty() {
                apply_configs_snapshot(payload.get("configs").expect("checked above"))
                    .map_err(|e| format!("写回备份配置失败：{e}"))?;
                restored_configs = true;
            }
        }
    }
    Ok(json!({
        "ok": true,
        "imported": result.imported,
        "skipped": result.skipped,
        "overwritten": result.overwritten,
        "restoredConfigs": restored_configs,
        "remote": remote_info_value(&target),
    }))
}

/// 同步：拉取 → 按 uid 全量合并 → 推送。
///
/// 推送带 `If-Match`（拉取时记录的 ETag）：拉取与推送之间若另一台设备已写入
/// 新备份，服务器返回 412，此时报错让用户重试，绝不盲目覆盖。远端没有备份时
/// 直接推送本地整库。服务器不支持 ETag 时退化为无条件覆盖（语义与手动备份一致）。
pub async fn sync_now(passphrase: &str) -> Result<Value, String> {
    let target = resolve_target(&load_webdav_config())?;
    let mut if_match: Option<String> = None;

    if let Some(resp) = download_backup(&target).await? {
        if_match = resp.headers.get("etag").cloned();
        let (_, payload) = decrypt_envelope_async(resp.body, passphrase.to_string()).await?;
        let text = payload_accounts_text(&payload)?;
        let indexes: Vec<usize> = (0..count_accounts(&text)).collect();
        export_import::import_accounts(&text, &indexes)?;
    }

    // 推送内容是合并后的整库 + 本机配置快照（首次同步即本地现有库）。
    let accounts = load_accounts_or_err()?;
    let count = accounts.len();
    let configs = load_configs_snapshot();
    let envelope = encrypt_envelope_async(
        build_payload(&accounts, &configs, config::now_ms()),
        passphrase.to_string(),
        config::now_ms(),
    )
    .await?;
    upload_backup(&target, envelope, if_match.as_deref()).await?;
    Ok(json!({ "ok": true, "count": count, "updatedAt": config::now_ms() }))
}

/// 查看云端备份的元信息（不需要加密密码）。
pub async fn remote_info() -> Result<Value, String> {
    let target = resolve_target(&load_webdav_config())?;
    match download_backup(&target).await? {
        None => Ok(json!({
            "ok": true,
            "exists": false,
            "remote": remote_info_value(&target),
        })),
        Some(resp) => {
            let header = parse_envelope_header(&resp.body)?;
            Ok(json!({
                "ok": true,
                "exists": true,
                "remote": remote_info_value(&target),
                "updatedAt": header.updated_at,
                "deviceName": header.device_name,
                "appVersion": header.app_version,
                "accountCount": header.account_count,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn payload_accounts_text(payload: &Value) -> Result<String, String> {
    let accounts = payload
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or("备份载荷缺少账号列表（文件已损坏或版本不兼容）")?;
    serde_json::to_string(accounts).map_err(|e| e.to_string())
}

fn count_accounts(text: &str) -> usize {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0)
}

fn remote_info_value(target: &WebdavTarget) -> Value {
    json!({ "dirUrl": target.dir_url, "fileUrl": target.file_url })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 备份/恢复配置快照会经各模块 `load_*/save_*` 读写 `~/.wb-switch`；
    /// 测试用 `WB_SWITCH_HOME` 把 store 重定向到临时目录。store 路径按调用读取
    /// 环境变量（`config::home_dir`），切换前后需锁住避免并行测试互踩。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_isolated_store<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let guard = ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("wbsync-test-{}", uuid::Uuid::new_v4().simple()));
        std::env::set_var(config::HOME_ENV_VAR, &home);
        let result = f(&home);
        std::env::remove_var(config::HOME_ENV_VAR);
        let _ = std::fs::remove_dir_all(&home);
        drop(guard);
        result
    }

    fn sample_payload() -> Value {
        json!({
            "format": PAYLOAD_FORMAT,
            "exportedAt": 1_700_000_000_000i64,
            "accounts": [
                { "id": "a1", "uid": "u1", "nickname": "甲", "access_token": "tok-1" },
                { "id": "a2", "uid": "u2", "nickname": "乙", "access_token": "tok-2" }
            ],
        })
    }

    #[test]
    fn envelope_roundtrip_preserves_payload() {
        let payload = sample_payload();
        let envelope = build_envelope(&payload, "correct horse", 1_700_000_000_001).unwrap();
        let (header, decrypted) = decrypt_envelope(&envelope, "correct horse").unwrap();
        assert_eq!(decrypted, payload, "解密后载荷应与原文一致");
        assert_eq!(header.account_count, 2);
        assert_eq!(header.updated_at, 1_700_000_000_001);
        assert!(!header.device_name.is_empty());
    }

    #[test]
    fn envelope_wrong_passphrase_fails_with_clear_error() {
        let envelope = build_envelope(&sample_payload(), "correct horse", 1).unwrap();
        let err = decrypt_envelope(&envelope, "wrong passphrase").unwrap_err();
        assert!(err.contains("加密密码不正确"), "错误应指明密码错误：{err}");
    }

    #[test]
    fn envelope_header_readable_without_passphrase() {
        let envelope = build_envelope(&sample_payload(), "correct horse", 1_700_000_000_002).unwrap();
        let header = parse_envelope_header(&envelope).unwrap();
        assert_eq!(header.account_count, 2);
        assert_eq!(header.app_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn envelope_rejects_foreign_bytes() {
        assert!(parse_envelope_header(b"not a backup").is_err());
        assert!(parse_envelope_header(b"").is_err());
        // 魔数对但长度字段谎报：不得 panic。
        let mut truncated = ENVELOPE_MAGIC.to_vec();
        truncated.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_envelope_header(&truncated).is_err());
    }

    #[test]
    fn envelope_requires_strong_enough_passphrase() {
        let err = build_envelope(&sample_payload(), "1234567", 1).unwrap_err();
        assert!(err.contains("至少 8 位"), "短密码应被拒绝：{err}");
    }

    #[test]
    fn envelope_salt_and_nonce_are_fresh_per_call() {
        let a = build_envelope(&sample_payload(), "correct horse", 1).unwrap();
        let b = build_envelope(&sample_payload(), "correct horse", 1).unwrap();
        assert_ne!(a, b, "相同输入两次加密必须产生不同密文（随机 salt/nonce）");
    }

    #[test]
    fn target_resolution_normalizes_url_and_dir() {
        let mk = |url: &str, dir: &str| {
            json!({ "url": url, "username": "u", "password": "p", "remotePath": dir })
        };
        let t = resolve_target(&mk("https://dav.example.com/dav/", "wb-switch")).unwrap();
        assert_eq!(t.dir_url, "https://dav.example.com/dav/wb-switch");
        assert_eq!(
            t.file_url,
            "https://dav.example.com/dav/wb-switch/wb-switch-backup.wbs"
        );
        let t = resolve_target(&mk("https://dav.example.com", "/nested/dir/")).unwrap();
        assert_eq!(t.dir_url, "https://dav.example.com/nested/dir");
        assert!(t.auth_header.is_some());
    }

    #[test]
    fn target_resolution_requires_server_url() {
        let err = resolve_target(&json!({ "url": "" })).unwrap_err();
        assert!(err.contains("请先"), "未配置时应报可操作错误：{err}");
        assert!(resolve_target(&json!({ "url": "ftp://x" })).is_err());
        assert!(resolve_target(&json!({ "url": "not a url" })).is_err());
    }

    #[test]
    fn target_resolution_rejects_path_traversal_in_dir() {
        let cfg = json!({ "url": "https://dav.example.com", "remotePath": "../escape" });
        assert!(resolve_target(&cfg).is_err());
    }

    #[test]
    fn target_resolution_allows_missing_credentials() {
        let t = resolve_target(&json!({ "url": "https://dav.example.com/dav" })).unwrap();
        assert!(t.auth_header.is_none(), "无凭据时不应带认证头");
        assert_eq!(t.dir_url, "https://dav.example.com/dav/wb-switch");
    }

    #[test]
    fn webdav_config_roundtrip_and_unknown_key_filtering() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("webdav-config-{}.json", uuid::Uuid::new_v4().simple()));
        let cfg = json!({
            "url": " https://dav.example.com/ ",
            "username": "user",
            "password": "pass",
            "remotePath": "backup",
            "evil": true,
        });
        save_webdav_config_at(&path, &cfg).unwrap();
        let loaded = load_webdav_config_at(&path);
        assert_eq!(loaded["url"], "https://dav.example.com/", "URL 应去首尾空白");
        assert_eq!(loaded["remotePath"], "backup");
        assert!(loaded.get("evil").is_none(), "未知字段不得落盘");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn webdav_config_defaults_fill_missing_fields() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("webdav-config-{}.json", uuid::Uuid::new_v4().simple()));
        save_webdav_config_at(&path, &json!({ "url": "https://dav.example.com" })).unwrap();
        let loaded = load_webdav_config_at(&path);
        assert_eq!(loaded["remotePath"], DEFAULT_REMOTE_DIR, "缺省目录回落默认值");
        assert_eq!(loaded["username"], "");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn configs_snapshot_roundtrips_and_preserves_stored_password() {
        with_isolated_store(|_home| {
            // 预置一份带口令的 WebDAV 配置。
            save_webdav_config(&json!({
                "url": "https://dav.example.com",
                "username": "user",
                "password": "dav-pass",
                "remotePath": "dir",
            }))
            .unwrap();

            let snapshot = load_configs_snapshot();
            assert!(
                snapshot.get("webdav").and_then(|v| v.get("password")).is_some(),
                "快照应携带完整 WebDAV 配置（含口令，随整包加密保护）"
            );

            // 模拟另一台设备：store 清空后写回快照。
            let _ = std::fs::remove_dir_all(config::store_dir());
            apply_configs_snapshot(&snapshot).unwrap();
            let restored = load_webdav_config();
            assert_eq!(restored["url"], "https://dav.example.com");
            assert_eq!(restored["password"], "dav-pass", "新设备恢复后应能直接连上服务器");
        });
    }

    #[test]
    fn save_retains_stored_password_when_submission_is_blank() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("webdav-config-{}.json", uuid::Uuid::new_v4().simple()));
        save_webdav_config_at(
            &path,
            &json!({ "url": "https://dav.example.com", "username": "user", "password": "secret" }),
        )
        .unwrap();

        // 脱敏视图场景：前端原样带回空口令 + 修改其它项，已存口令不得被清掉。
        save_webdav_config_at(
            &path,
            &json!({ "url": "https://dav.example.com", "username": "user", "password": "", "remotePath": "b2" }),
        )
        .unwrap();
        let loaded = load_webdav_config_at(&path);
        assert_eq!(loaded["password"], "secret", "空口令提交不得清掉已存口令");
        assert_eq!(loaded["remotePath"], "b2");

        save_webdav_config_at(
            &path,
            &json!({ "url": "https://dav.example.com", "username": "user", "password": "newpass" }),
        )
        .unwrap();
        let loaded = load_webdav_config_at(&path);
        assert_eq!(loaded["password"], "newpass", "提交新口令应生效");
        std::fs::remove_file(&path).unwrap();
    }
}
