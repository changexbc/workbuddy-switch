//! CodeBuddy CN IDE Safe Storage 注入（仅 CN）。
//!
//! 把账号会话 JSON 加密写入 `state.vscdb` 的 ItemTable：
//! `secret://{"extensionId":"tencent-cloud.coding-copilot","key":"planning-genie.new.accessTokencn"}`
//!
//! 平台加密模型对齐 Chromium/Electron Safe Storage：
//! - macOS: Keychain「CodeBuddy CN Safe Storage」→ PBKDF2-SHA1(1003) → AES-128-CBC `v10`
//! - Windows: Local State `os_crypt.encrypted_key` + DPAPI → AES-256-GCM `v10`
//! - Linux: secret-tool / peanuts 固定密钥 → AES-128-CBC `v11`/`v10`

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "windows"))]
use aes::Aes128;
#[cfg(target_os = "windows")]
use aes_gcm::aead::generic_array::GenericArray;
#[cfg(target_os = "windows")]
use aes_gcm::aead::{Aead, AeadCore, OsRng};
#[cfg(target_os = "windows")]
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
#[cfg(target_os = "windows")]
use base64::{engine::general_purpose, Engine as _};
#[cfg(not(target_os = "windows"))]
use cbc::cipher::block_padding::Pkcs7;
#[cfg(not(target_os = "windows"))]
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
#[cfg(not(target_os = "windows"))]
use pbkdf2::pbkdf2_hmac;
use rusqlite::Connection;
#[cfg(not(target_os = "windows"))]
use sha1::Sha1;

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{LocalFree, HLOCAL};
#[cfg(target_os = "windows")]
use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

#[cfg(not(target_os = "windows"))]
type Aes128CbcEnc = cbc::Encryptor<Aes128>;
#[cfg(not(target_os = "windows"))]
type Aes128CbcDec = cbc::Decryptor<Aes128>;

const V10_PREFIX: &[u8] = b"v10";
const V11_PREFIX: &[u8] = b"v11";
#[cfg(not(target_os = "windows"))]
const CBC_IV: [u8; 16] = [b' '; 16];
#[cfg(not(target_os = "windows"))]
const SALT: &[u8] = b"saltysalt";

pub const SECRET_EXTENSION_ID: &str = "tencent-cloud.coding-copilot";
pub const SECRET_KEY: &str = "planning-genie.new.accessTokencn";

/// ItemTable 完整 key。
pub fn secret_storage_item_key() -> String {
    format!(
        r#"secret://{{"extensionId":"{}","key":"{}"}}"#,
        SECRET_EXTENSION_ID, SECRET_KEY
    )
}

pub fn codebuddy_cn_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(crate::modules::config::home_dir().join("Library/Application Support/CodeBuddy CN"))
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir().map(|d| d.join("CodeBuddy CN"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir().map(|d| d.join("CodeBuddy CN"))
    }
}

pub fn codebuddy_cn_state_db_path() -> Option<PathBuf> {
    codebuddy_cn_data_dir().map(|d| d.join("User").join("globalStorage").join("state.vscdb"))
}

pub fn resolve_state_db_path(user_data_dir: Option<&Path>) -> Result<PathBuf, String> {
    let root = match user_data_dir {
        Some(p) => p.to_path_buf(),
        None => codebuddy_cn_data_dir().ok_or_else(|| "无法定位 CodeBuddy CN 数据目录".to_string())?,
    };
    let candidates = [
        root.join("User").join("globalStorage").join("state.vscdb"),
        root.join("globalStorage").join("state.vscdb"),
        root.join("state.vscdb"),
    ];
    if let Some(path) = candidates.iter().find(|p| p.exists()) {
        return Ok(path.clone());
    }
    let preferred = candidates[0].clone();
    if let Some(parent) = preferred.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 globalStorage 失败: {e}"))?;
    }
    Ok(preferred)
}

fn data_root_from_db(db_path: &Path) -> Result<&Path, String> {
    db_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| format!("无法从 db 路径推断数据目录: {}", db_path.display()))
}

fn decode_buffer_data(buffer: &serde_json::Value) -> Result<Vec<u8>, String> {
    let data_arr = buffer["data"]
        .as_array()
        .ok_or_else(|| "Secret data is not in Buffer format".to_string())?;
    let mut encrypted_bytes = Vec::with_capacity(data_arr.len());
    for (idx, v) in data_arr.iter().enumerate() {
        let n = v
            .as_u64()
            .ok_or_else(|| format!("Secret data element at index {idx} is not an integer"))?;
        if n > 255 {
            return Err(format!(
                "Secret data element at index {idx} is out of range ({n} > 255)"
            ));
        }
        encrypted_bytes.push(n as u8);
    }
    Ok(encrypted_bytes)
}

fn encode_secret_buffer(encrypted: Vec<u8>) -> Result<String, String> {
    let buffer_json = serde_json::json!({
        "type": "Buffer",
        "data": encrypted
    });
    serde_json::to_string(&buffer_json).map_err(|e| format!("Failed to serialize Buffer: {e}"))
}

fn detect_prefix(encrypted: &[u8]) -> Option<&'static str> {
    if encrypted.starts_with(V10_PREFIX) {
        Some("v10")
    } else if encrypted.starts_with(V11_PREFIX) {
        Some("v11")
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
fn pbkdf2_sha1_key(password: &str, iterations: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2_hmac::<Sha1>(password.as_bytes(), SALT, iterations, &mut key);
    key
}

#[cfg(not(target_os = "windows"))]
fn decrypt_cbc_prefixed(
    encrypted: &[u8],
    expected_prefix: &[u8],
    key: &[u8; 16],
) -> Result<Vec<u8>, String> {
    if !encrypted.starts_with(expected_prefix) {
        return Err(format!(
            "Unexpected ciphertext prefix: {:?}",
            &encrypted[..encrypted.len().min(3)]
        ));
    }
    let raw = &encrypted[expected_prefix.len()..];
    let cipher = Aes128CbcDec::new_from_slices(key, &CBC_IV)
        .map_err(|e| format!("Failed to init AES-CBC decryptor: {e}"))?;
    let mut buf = raw.to_vec();
    let plain = cipher
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| format!("AES-CBC decryption failed: {e}"))?
        .to_vec();
    Ok(plain)
}

#[cfg(not(target_os = "windows"))]
fn encrypt_cbc_prefixed(
    prefix: &[u8],
    key: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let cipher = Aes128CbcEnc::new_from_slices(key, &CBC_IV)
        .map_err(|e| format!("Failed to init AES-CBC encryptor: {e}"))?;
    let mut buf = plaintext.to_vec();
    let msg_len = buf.len();
    let pad_len = 16 - (msg_len % 16);
    buf.resize(msg_len + pad_len, 0);
    let ciphertext = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buf, msg_len)
        .map_err(|e| format!("AES-CBC encryption failed: {e}"))?
        .to_vec();
    let mut result = Vec::with_capacity(prefix.len() + ciphertext.len());
    result.extend_from_slice(prefix);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// 运行命令并取 trim 后的 stdout；带超时兜底。
///
/// macOS 上 `security find-generic-password` 可能因 Keychain 授权弹窗而长时间
/// 挂起（甚至无限期等待用户决定），因此不能使用无超时的阻塞式 `.output()`；
/// 子进程输出也需并发读取（复用 process 模块实现），避免写满管道死锁。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_command_get_trimmed(program: &str, args: &[&str], timeout_secs: u64) -> Option<String> {
    let output = crate::modules::process::run_cmd_timeout(program, args, timeout_secs)?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(target_os = "macos")]
fn get_macos_safe_storage_password() -> Result<String, String> {
    // 只查询一次：解密只依赖 password 本身、与 account 属性无关，
    // 单次查询最多触发一次钥匙串授权弹窗（多候选循环会逐次弹窗）。
    run_command_get_trimmed(
        "security",
        &["find-generic-password", "-w", "-s", "CodeBuddy CN Safe Storage"],
        10,
    )
    .ok_or_else(|| {
        "无法从 Keychain 读取 CodeBuddy CN Safe Storage 密码。请先手动打开 CodeBuddy CN 并登录一次。"
            .to_string()
    })
}

#[cfg(target_os = "linux")]
const LINUX_V10_KEY: [u8; 16] = [
    0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27, 0x27, 0x78,
];
#[cfg(target_os = "linux")]
const LINUX_EMPTY_KEY: [u8; 16] = [
    0xd0, 0xd0, 0xec, 0x9c, 0x7d, 0x77, 0xd4, 0x3a, 0xc5, 0x41, 0x87, 0xfa, 0x48, 0x18, 0xd1, 0x7f,
];

#[cfg(target_os = "linux")]
fn get_linux_v11_key() -> Option<[u8; 16]> {
    for app in [
        "CodeBuddy CN",
        "codebuddy cn",
        "codebuddy-cn",
        "codebuddycn",
    ] {
        if let Some(password) =
            run_command_get_trimmed("secret-tool", &["lookup", "application", app], 10)
        {
            return Some(pbkdf2_sha1_key(&password, 1));
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn get_local_state_path(data_root: &Path) -> Result<PathBuf, String> {
    let path = data_root.join("Local State");
    if path.exists() {
        Ok(path)
    } else {
        Err(format!("未找到 CodeBuddy CN Local State: {}", path.display()))
    }
}

#[cfg(target_os = "windows")]
fn dpapi_decrypt(encrypted: &[u8]) -> Result<Vec<u8>, String> {
    unsafe {
        let mut data_in = CRYPT_INTEGER_BLOB {
            cbData: encrypted.len() as u32,
            pbData: encrypted.as_ptr() as *mut u8,
        };
        let mut data_out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        CryptUnprotectData(
            &mut data_in,
            None,
            None,
            None,
            None,
            0,
            &mut data_out,
        )
        .map_err(|e| format!("DPAPI CryptUnprotectData failed: {e}"))?;
        if data_out.pbData.is_null() || data_out.cbData == 0 {
            return Err("DPAPI returned empty data".to_string());
        }
        let slice = std::slice::from_raw_parts(data_out.pbData, data_out.cbData as usize);
        let result = slice.to_vec();
        let _ = LocalFree(HLOCAL(data_out.pbData as _));
        Ok(result)
    }
}

#[cfg(target_os = "windows")]
fn get_windows_encryption_key(data_root: &Path) -> Result<Vec<u8>, String> {
    let local_state = get_local_state_path(data_root)?;
    let text = std::fs::read_to_string(&local_state)
        .map_err(|e| format!("读取 Local State 失败: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 Local State 失败: {e}"))?;
    let encrypted_key_b64 = json["os_crypt"]["encrypted_key"]
        .as_str()
        .ok_or_else(|| "Local State 缺少 os_crypt.encrypted_key".to_string())?;
    let encrypted_key_bytes = general_purpose::STANDARD
        .decode(encrypted_key_b64)
        .map_err(|e| format!("Base64 decode failed for encrypted_key: {e}"))?;
    if encrypted_key_bytes.len() < 6 {
        return Err("encrypted_key data too short".to_string());
    }
    let prefix = String::from_utf8_lossy(&encrypted_key_bytes[..5]);
    if prefix != "DPAPI" {
        return Err(format!("encrypted_key prefix is not DPAPI, got: {prefix}"));
    }
    dpapi_decrypt(&encrypted_key_bytes[5..])
}

#[cfg(target_os = "windows")]
fn decrypt_windows_gcm_v10(key: &[u8], encrypted: &[u8]) -> Result<Vec<u8>, String> {
    if encrypted.len() < 31 {
        return Err("ciphertext too short for AES-GCM".to_string());
    }
    if &encrypted[..3] != V10_PREFIX {
        return Err(format!(
            "Unexpected ciphertext prefix: {:?}",
            &encrypted[..3]
        ));
    }
    let nonce_bytes = &encrypted[3..15];
    let ciphertext = &encrypted[15..];
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("AES-GCM decryption failed: {e}"))
}

#[cfg(target_os = "windows")]
fn encrypt_windows_gcm_v10(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| format!("AES-GCM encryption failed: {e}"))?;
    let mut result = Vec::with_capacity(3 + nonce.len() + ciphertext.len());
    result.extend_from_slice(V10_PREFIX);
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

fn decrypt_secret_payload(encrypted: &[u8], data_root: &Path) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    {
        let key = get_windows_encryption_key(data_root)?;
        return decrypt_windows_gcm_v10(&key, encrypted);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = data_root;
        let password = get_macos_safe_storage_password()?;
        let key = pbkdf2_sha1_key(&password, 1003);
        return decrypt_cbc_prefixed(encrypted, V10_PREFIX, &key);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = data_root;
        match detect_prefix(encrypted) {
            Some("v11") => {
                let key = get_linux_v11_key().ok_or_else(|| {
                    "无法加载 Linux secret storage key（v11）".to_string()
                })?;
                match decrypt_cbc_prefixed(encrypted, V11_PREFIX, &key) {
                    Ok(value) => Ok(value),
                    Err(_) => decrypt_cbc_prefixed(encrypted, V11_PREFIX, &LINUX_EMPTY_KEY),
                }
            }
            Some("v10") => match decrypt_cbc_prefixed(encrypted, V10_PREFIX, &LINUX_V10_KEY) {
                Ok(value) => Ok(value),
                Err(_) => decrypt_cbc_prefixed(encrypted, V10_PREFIX, &LINUX_EMPTY_KEY),
            },
            _ => Err(format!(
                "Unsupported Linux ciphertext prefix: {:?}",
                &encrypted[..encrypted.len().min(3)]
            )),
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (encrypted, data_root);
        Err("Unsupported platform".to_string())
    }
}

fn encrypt_secret_payload(
    plaintext: &[u8],
    preferred_prefix: Option<&str>,
    data_root: &Path,
) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = preferred_prefix;
        let key = get_windows_encryption_key(data_root)?;
        return encrypt_windows_gcm_v10(&key, plaintext);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (preferred_prefix, data_root);
        let password = get_macos_safe_storage_password()?;
        let key = pbkdf2_sha1_key(&password, 1003);
        return encrypt_cbc_prefixed(V10_PREFIX, &key, plaintext);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = data_root;
        let target_prefix = if let Some(prefix) = preferred_prefix {
            prefix
        } else if get_linux_v11_key().is_some() {
            "v11"
        } else {
            "v10"
        };
        if target_prefix == "v11" {
            let key = get_linux_v11_key()
                .ok_or_else(|| "无法加载 Linux secret storage key（v11）".to_string())?;
            return encrypt_cbc_prefixed(V11_PREFIX, &key, plaintext);
        }
        return encrypt_cbc_prefixed(V10_PREFIX, &LINUX_V10_KEY, plaintext);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (plaintext, preferred_prefix, data_root);
        Err("Unsupported platform".to_string())
    }
}

fn decode_secret_storage_value(raw_value: &str, data_root: &Path) -> Result<String, String> {
    let parsed: serde_json::Value = match serde_json::from_str(raw_value) {
        Ok(value) => value,
        Err(_) => return Ok(raw_value.to_string()),
    };
    if parsed.get("data").is_some() {
        let encrypted_bytes = decode_buffer_data(&parsed)?;
        let decrypted = decrypt_secret_payload(&encrypted_bytes, data_root)?;
        return String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted data is not valid UTF-8: {e}"));
    }
    if let Some(value) = parsed.as_str() {
        return Ok(value.to_string());
    }
    Ok(raw_value.to_string())
}

/// 读取并解密 CodeBuddy CN 当前登录 secret（明文 JSON 字符串）。
pub fn read_codebuddy_cn_secret(user_data_dir: Option<&Path>) -> Result<Option<String>, String> {
    let db_path = resolve_state_db_path(user_data_dir)?;
    if !db_path.exists() {
        return Ok(None);
    }
    let data_root = data_root_from_db(&db_path)?.to_path_buf();
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("打开 state.vscdb 失败: {e}"))?;
    let key = secret_storage_item_key();
    let raw_value: Option<String> = match conn.query_row(
        "SELECT value FROM ItemTable WHERE key = ?1",
        [key.as_str()],
        |row| row.get(0),
    ) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => return Err(format!("查询 CodeBuddy CN secret 失败: {err}")),
    };
    match raw_value {
        Some(value) => decode_secret_storage_value(&value, &data_root).map(Some),
        None => Ok(None),
    }
}

/// 等待 `state.vscdb` 不再被其他进程独占占用。
///
/// 场景：Windows 上刚 `taskkill` 结束 CodeBuddy CN 后，进程句柄可能尚未完全
/// 释放，立即以 SQLite 打开会因文件被独占而失败；这里轮询到可写打开为止。
/// 文件尚不存在（首次注入）时直接放行。
fn wait_for_db_released(db_path: &Path, timeout: Duration) -> Result<(), String> {
    if !db_path.exists() {
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(db_path)
        {
            Ok(_) => return Ok(()),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                return Err(format!(
                    "等待 state.vscdb 可访问超时（可能仍被 CodeBuddy CN 占用，请先完全退出 IDE）: {e}"
                ));
            }
        }
    }
}

/// 用 SQLite `VACUUM INTO` 生成一致快照备份（比文件复制更安全，天然包含 WAL 内容）。
/// 返回备份文件路径。
fn snapshot_backup(conn: &Connection) -> Result<PathBuf, String> {
    let dir = crate::modules::config::backup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let backup = dir.join(format!(
        "state.vscdb.{}.bak",
        crate::modules::config::now_ms()
    ));
    let _ = std::fs::remove_file(&backup);
    let escaped = backup.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))
        .map_err(|e| format!("生成 state.vscdb 备份失败: {e}"))?;
    if !backup.exists() {
        return Err("生成 state.vscdb 备份失败：备份文件未生成".to_string());
    }
    Ok(backup)
}

/// 校验：解密回读刚写入的 secret，必须与目标明文一致。
fn verify_written_secret(
    conn: &Connection,
    db_key: &str,
    data_root: &Path,
    expected: &str,
) -> Result<(), String> {
    let written: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?",
            [db_key],
            |row| row.get(0),
        )
        .map_err(|e| format!("写后回读失败: {e}"))?;
    let plain = decode_secret_storage_value(&written, data_root)
        .map_err(|e| format!("写后解密回读失败: {e}"))?;
    if plain != expected {
        return Err("写后校验失败：解密回读内容与目标账号不一致".to_string());
    }
    Ok(())
}

/// 加密并写入 CodeBuddy CN secret。
pub fn inject_codebuddy_cn_secret(
    plaintext: &str,
    user_data_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let db_path = resolve_state_db_path(user_data_dir)?;
    let data_root = data_root_from_db(&db_path)?.to_path_buf();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 state.vscdb 父目录失败: {e}"))?;
    }

    // 1) 句柄握手：等待 DB 不被其它进程独占（Windows 强杀后句柄释放有延迟）。
    wait_for_db_released(&db_path, Duration::from_secs(5))?;

    let conn = Connection::open(&db_path).map_err(|e| format!("打开 state.vscdb 失败: {e}"))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("设置 busy_timeout 失败: {e}"))?;

    // 2) 写前生成一致快照备份；失败回滚用。
    let backup = snapshot_backup(&conn)?;

    let db_path_for_result = db_path.clone();
    let result: Result<PathBuf, String> = (|| {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .map_err(|e| format!("初始化 ItemTable 失败: {e}"))?;

        let db_key = secret_storage_item_key();
        let existing_prefix: Option<String> = match conn.query_row(
            "SELECT value FROM ItemTable WHERE key = ?",
            [db_key.as_str()],
            |row| row.get::<_, String>(0),
        ) {
            Ok(val) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&val) {
                    if let Ok(bytes) = decode_buffer_data(&parsed) {
                        detect_prefix(&bytes).map(|s| s.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Err(_) => None,
        };

        let encrypted =
            encrypt_secret_payload(plaintext.as_bytes(), existing_prefix.as_deref(), &data_root)?;
        let buffer_str = encode_secret_buffer(encrypted)?;
        conn.execute(
            "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)",
            rusqlite::params![db_key, buffer_str],
        )
        .map_err(|e| format!("写入 state.vscdb 失败: {e}"))?;

        // 3) 写后解密回读，确认写入内容与目标账号一致。
        verify_written_secret(&conn, &db_key, &data_root, plaintext)
            .map_err(|e| {
                let _ = std::fs::copy(&backup, &db_path_for_result);
                e
            })?;
        Ok(db_path_for_result)
    })();

    if result.is_err() {
        // 兜底：确保连接已关闭后再尝试恢复备份。
        drop(conn);
        let _ = std::fs::copy(&backup, &db_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_matches_verified_format() {
        let key = secret_storage_item_key();
        assert_eq!(
            key,
            r#"secret://{"extensionId":"tencent-cloud.coding-copilot","key":"planning-genie.new.accessTokencn"}"#
        );
    }

    #[test]
    fn data_dir_contains_codebuddy_cn() {
        let Some(dir) = codebuddy_cn_data_dir() else {
            return;
        };
        let s = dir.to_string_lossy();
        assert!(
            s.contains("CodeBuddy CN"),
            "data dir should contain CodeBuddy CN: {s}"
        );
    }

    #[test]
    fn state_db_path_ends_with_state_vscdb() {
        let Some(db) = codebuddy_cn_state_db_path() else {
            return;
        };
        assert!(db.ends_with("state.vscdb"));
        assert!(db.to_string_lossy().contains("globalStorage"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cbc_roundtrip_with_known_password() {
        let key = pbkdf2_sha1_key("test-password", 1003);
        let plain = br#"{"token":"abc","accessToken":"uid+abc"}"#;
        let encrypted = encrypt_cbc_prefixed(V10_PREFIX, &key, plain).unwrap();
        assert!(encrypted.starts_with(V10_PREFIX));
        let decrypted = decrypt_cbc_prefixed(&encrypted, V10_PREFIX, &key).unwrap();
        assert_eq!(decrypted, plain);
    }

    #[test]
    fn resolve_prefers_existing_candidate() {
        let dir = std::env::temp_dir().join(format!(
            "wb-cn-ide-path-test-{}",
            uuid::Uuid::new_v4()
        ));
        let db = dir.join("User").join("globalStorage").join("state.vscdb");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        let resolved = resolve_state_db_path(Some(&dir)).unwrap();
        assert_eq!(resolved, db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
