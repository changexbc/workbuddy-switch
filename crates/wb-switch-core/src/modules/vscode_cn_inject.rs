//! Electron/Chromium Safe Storage 注入（参数化：CodeBuddy 桌面 IDE 与 VS Code 扩展共用）。
//!
//! 把账号会话 JSON 加密写入 `state.vscdb` 的 ItemTable，键形如
//! `secret://{"extensionId":"<ext>","key":"<key>"}`。
//!
//! 各目标之间的差异点（数据目录 / secret key / macOS Keychain 服务名 /
//! Linux 密钥环应用名）全部收敛在 [`VscodeSafeStorageTarget`] 描述符，
//! CodeBuddy CN IDE（`codebuddy_cn_ide`）、CodeBuddy 国际版 IDE（`codebuddy_ide`）
//! 与 VS Code CodeBuddy 扩展（`vscode_ext`）复用同一套加解密与读写流程。
//!
//! 桌面 IDE 的「国内 CN / 国际版」档位用 [`CodeBuddyIdeFlavor`] 表达，
//! 并映射到对应的 [`VscodeSafeStorageTarget`] 常量描述符。
//!
//! 平台加密模型对齐 Chromium/Electron Safe Storage：
//! - macOS: Keychain「<app> Safe Storage」→ PBKDF2-SHA1(1003) → AES-128-CBC `v10`
//! - Windows: Local State `os_crypt.encrypted_key` + DPAPI → AES-256-GCM `v10`
//! - Linux: Secret Service（`org.freedesktop.secrets`）密钥 → AES-128-CBC `v11`，
//!   无密钥环时退回 peanuts 固定密钥 `v10`

use std::path::{Path, PathBuf};

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
use rusqlite::{Connection, OpenFlags};
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

/// CodeBuddy 系扩展共用的 marketplace 扩展 id（secret key 中的 `extensionId`）。
pub const SECRET_EXTENSION_ID: &str = "tencent-cloud.coding-copilot";
/// CodeBuddy CN IDE 的 secret key（secret key 中的 `key`）。
pub const SECRET_KEY: &str = "planning-genie.new.accessTokencn";
/// CodeBuddy 国际版 IDE 的 secret key（secret key 中的 `key`）。
pub const INTL_SECRET_KEY: &str = "planning-genie.new.accessToken";

/// 一个「基于 Electron/Chromium Safe Storage」的应用目标描述符。
///
/// 把各目标之间的差异点参数化后，[`read_secret_for`] / [`inject_secret_for`] 等
/// 流程可对任意目标复用。新增目标只需提供一个常量描述符。
pub struct VscodeSafeStorageTarget {
    /// 数据根目录解析函数；返回 `None` 表示当前平台无法定位。
    pub data_dir_resolver: fn() -> Option<PathBuf>,
    /// 目标展示名（用于错误文案）。
    pub display_name: &'static str,
    /// secret key 中的 `extensionId` 字段。
    pub secret_item_prefix_extension_id: &'static str,
    /// secret key 中的 `key` 字段。
    pub secret_key: &'static str,
    /// macOS Keychain 通用密码服务名。
    pub macos_keychain_service: &'static str,
    /// Linux 密钥环应用名候选（按顺序尝试）：Secret Service 按 `application`
    /// 属性检索，`secret-tool` 兜底路径按同一属性查询。
    pub linux_secret_tool_app_names: &'static [&'static str],
}

/// CodeBuddy CN IDE（桌面客户端）目标描述符。
pub const CODEBUDDY_CN_TARGET: VscodeSafeStorageTarget = VscodeSafeStorageTarget {
    data_dir_resolver: codebuddy_cn_data_dir,
    display_name: "CodeBuddy CN",
    secret_item_prefix_extension_id: SECRET_EXTENSION_ID,
    secret_key: SECRET_KEY,
    macos_keychain_service: "CodeBuddy CN Safe Storage",
    linux_secret_tool_app_names: &["CodeBuddy CN", "codebuddy cn", "codebuddy-cn", "codebuddycn"],
};

/// CodeBuddy 国际版 IDE（桌面客户端）目标描述符。
pub const CODEBUDDY_INTL_TARGET: VscodeSafeStorageTarget = VscodeSafeStorageTarget {
    data_dir_resolver: codebuddy_intl_data_dir,
    display_name: "CodeBuddy",
    secret_item_prefix_extension_id: SECRET_EXTENSION_ID,
    secret_key: INTL_SECRET_KEY,
    macos_keychain_service: "CodeBuddy Safe Storage",
    linux_secret_tool_app_names: &["CodeBuddy", "codebuddy"],
};

/// CodeBuddy 桌面 IDE 档位：国内 CN 与国际版共用加密，密钥/目录不同。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeBuddyIdeFlavor {
    Cn,
    Intl,
}

impl CodeBuddyIdeFlavor {
    pub fn product_label(self) -> &'static str {
        match self {
            Self::Cn => "CodeBuddy CN",
            Self::Intl => "CodeBuddy",
        }
    }

    pub fn data_dir_name(self) -> &'static str {
        self.product_label()
    }

    pub fn secret_key(self) -> &'static str {
        match self {
            Self::Cn => SECRET_KEY,
            Self::Intl => INTL_SECRET_KEY,
        }
    }

    pub fn keychain_service(self) -> &'static str {
        match self {
            Self::Cn => "CodeBuddy CN Safe Storage",
            Self::Intl => "CodeBuddy Safe Storage",
        }
    }

    pub fn linux_secret_apps(self) -> &'static [&'static str] {
        match self {
            Self::Cn => &[
                "CodeBuddy CN",
                "codebuddy cn",
                "codebuddy-cn",
                "codebuddycn",
            ],
            Self::Intl => &["CodeBuddy", "codebuddy"],
        }
    }

    /// 映射到对应的静态目标描述符。
    pub fn target(self) -> &'static VscodeSafeStorageTarget {
        match self {
            Self::Cn => &CODEBUDDY_CN_TARGET,
            Self::Intl => &CODEBUDDY_INTL_TARGET,
        }
    }
}

/// ItemTable 完整 key（默认目标 = CodeBuddy CN，保持既有行为）。
pub fn secret_storage_item_key() -> String {
    secret_storage_item_key_for(&CODEBUDDY_CN_TARGET)
}

/// 构造指定目标的 ItemTable 完整 key。
pub fn secret_storage_item_key_for(target: &VscodeSafeStorageTarget) -> String {
    format!(
        r#"secret://{{"extensionId":"{}","key":"{}"}}"#,
        target.secret_item_prefix_extension_id, target.secret_key
    )
}

/// CodeBuddy 桌面 IDE 数据目录（按档位区分 CN / 国际版）。
pub fn codebuddy_ide_data_dir(flavor: CodeBuddyIdeFlavor) -> Option<PathBuf> {
    let name = flavor.data_dir_name();
    #[cfg(target_os = "macos")]
    {
        Some(
            crate::modules::config::home_dir()
                .join("Library/Application Support")
                .join(name),
        )
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir().map(|d| d.join(name))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir().map(|d| d.join(name))
    }
}

/// CodeBuddy CN 数据目录（保持既有行为）。
pub fn codebuddy_cn_data_dir() -> Option<PathBuf> {
    codebuddy_ide_data_dir(CodeBuddyIdeFlavor::Cn)
}

/// CodeBuddy 国际版数据目录。
pub fn codebuddy_intl_data_dir() -> Option<PathBuf> {
    codebuddy_ide_data_dir(CodeBuddyIdeFlavor::Intl)
}

/// CodeBuddy CN state.vscdb 路径（保持既有行为）。
pub fn codebuddy_cn_state_db_path() -> Option<PathBuf> {
    state_db_path_for(&CODEBUDDY_CN_TARGET)
}

/// 指定目标的 state.vscdb 路径。
pub fn state_db_path_for(target: &VscodeSafeStorageTarget) -> Option<PathBuf> {
    (target.data_dir_resolver)().map(|d| d.join("User").join("globalStorage").join("state.vscdb"))
}

/// 解析目标 state.vscdb 路径（保持既有行为）。
pub fn resolve_state_db_path(user_data_dir: Option<&Path>) -> Result<PathBuf, String> {
    resolve_state_db_path_for(&CODEBUDDY_CN_TARGET, user_data_dir)
}

/// 解析指定目标的 state.vscdb 路径；优先返回已存在的候选，否则准备首选路径。
pub fn resolve_state_db_path_for(
    target: &VscodeSafeStorageTarget,
    user_data_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let root = match user_data_dir {
        Some(p) => p.to_path_buf(),
        None => (target.data_dir_resolver)()
            .ok_or_else(|| format!("无法定位 {} 数据目录", target.display_name))?,
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

/// 只挑已存在的 state.vscdb 候选路径；找不到返回 `None`，**不创建任何目录或文件**。
///
/// 只读探测（读 secret、查行存在性）必须走这里，不要复用
/// [`resolve_state_db_path_for`]——它有 `create_dir_all` 副作用，会让
/// `installed`（依赖数据目录存在性）形成自我维持的误报（issue #91）。
fn find_existing_state_db_path(root: &Path) -> Option<PathBuf> {
    [
        root.join("User").join("globalStorage").join("state.vscdb"),
        root.join("globalStorage").join("state.vscdb"),
        root.join("state.vscdb"),
    ]
    .into_iter()
    .find(|path| path.exists())
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
fn get_macos_safe_storage_password(service: &str) -> Result<String, String> {
    // 只查询一次：解密只依赖 password 本身、与 account 属性无关，
    // 单次查询最多触发一次钥匙串授权弹窗（多候选循环会逐次弹窗）。
    run_command_get_trimmed("security", &["find-generic-password", "-w", "-s", service], 10)
        .ok_or_else(|| {
            format!(
                "无法从 Keychain 读取「{service}」密码。请先手动打开对应应用并登录一次。"
            )
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
fn get_linux_v11_key(app_names: &[&str]) -> Option<[u8; 16]> {
    // 优先原生 D-Bus（Secret Service）：`secret-tool` 属于 libsecret-tools，多数发行版
    // 默认不安装，而 Electron 早把密码写进了 gnome-keyring，只差一个读得到的客户端。
    if let Some(password) = crate::modules::linux_keyring::find_password(app_names) {
        return Some(pbkdf2_sha1_key(&password, 1));
    }
    // 兜底：极少数只装了 libsecret-tools 的环境，按老路子再试一次。
    for app in app_names {
        if let Some(password) =
            run_command_get_trimmed("secret-tool", &["lookup", "application", app], 10)
        {
            return Some(pbkdf2_sha1_key(&password, 1));
        }
    }
    None
}

/// v11 密钥缺失时的提示：直接说清「谁去开、怎么开」，不要只说一句加载失败。
///
/// 文案刻意不出现 "Safe Storage" / "Keychain"：cn-ide 的注入失败提示会按这两个词
/// 追加 macOS 钥匙串（Keychain）指引，那是平台错位的建议。
#[cfg(target_os = "linux")]
fn linux_v11_key_error(target: &VscodeSafeStorageTarget) -> String {
    format!(
        "无法从系统密钥环读取「{}」的登录凭证密钥（v11）。请确认 gnome-keyring / KWallet 已启动、登录密钥环已解锁，并先用 {} 手动登录一次。",
        target.display_name, target.display_name
    )
}

#[cfg(target_os = "windows")]
fn get_local_state_path(data_root: &Path) -> Result<PathBuf, String> {
    let path = data_root.join("Local State");
    if path.exists() {
        Ok(path)
    } else {
        Err(format!("未找到 Local State: {}", path.display()))
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

fn decrypt_secret_payload(
    encrypted: &[u8],
    data_root: &Path,
    target: &VscodeSafeStorageTarget,
) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = target;
        let key = get_windows_encryption_key(data_root)?;
        return decrypt_windows_gcm_v10(&key, encrypted);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = data_root;
        let password = get_macos_safe_storage_password(target.macos_keychain_service)?;
        let key = pbkdf2_sha1_key(&password, 1003);
        return decrypt_cbc_prefixed(encrypted, V10_PREFIX, &key);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = data_root;
        match detect_prefix(encrypted) {
            Some("v11") => {
                let key = get_linux_v11_key(target.linux_secret_tool_app_names)
                    .ok_or_else(|| linux_v11_key_error(target))?;
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
        let _ = (encrypted, data_root, target);
        Err("Unsupported platform".to_string())
    }
}

fn encrypt_secret_payload(
    plaintext: &[u8],
    preferred_prefix: Option<&str>,
    data_root: &Path,
    target: &VscodeSafeStorageTarget,
) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = (preferred_prefix, target);
        let key = get_windows_encryption_key(data_root)?;
        return encrypt_windows_gcm_v10(&key, plaintext);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (preferred_prefix, data_root);
        let password = get_macos_safe_storage_password(target.macos_keychain_service)?;
        let key = pbkdf2_sha1_key(&password, 1003);
        return encrypt_cbc_prefixed(V10_PREFIX, &key, plaintext);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = data_root;
        let target_prefix = if let Some(prefix) = preferred_prefix {
            prefix
        } else if get_linux_v11_key(target.linux_secret_tool_app_names).is_some() {
            "v11"
        } else {
            "v10"
        };
        if target_prefix == "v11" {
            let key = get_linux_v11_key(target.linux_secret_tool_app_names)
                .ok_or_else(|| linux_v11_key_error(target))?;
            return encrypt_cbc_prefixed(V11_PREFIX, &key, plaintext);
        }
        return encrypt_cbc_prefixed(V10_PREFIX, &LINUX_V10_KEY, plaintext);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (plaintext, preferred_prefix, data_root, target);
        Err("Unsupported platform".to_string())
    }
}

fn decode_secret_storage_value(
    raw_value: &str,
    data_root: &Path,
    target: &VscodeSafeStorageTarget,
) -> Result<String, String> {
    let parsed: serde_json::Value = match serde_json::from_str(raw_value) {
        Ok(value) => value,
        Err(_) => return Ok(raw_value.to_string()),
    };
    if parsed.get("data").is_some() {
        let encrypted_bytes = decode_buffer_data(&parsed)?;
        let decrypted = decrypt_secret_payload(&encrypted_bytes, data_root, target)?;
        return String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted data is not valid UTF-8: {e}"));
    }
    if let Some(value) = parsed.as_str() {
        return Ok(value.to_string());
    }
    Ok(raw_value.to_string())
}

/// 读取并解密 CodeBuddy CN 当前登录 secret（明文 JSON 字符串）；保持既有行为。
pub fn read_codebuddy_cn_secret(user_data_dir: Option<&Path>) -> Result<Option<String>, String> {
    read_secret_for(&CODEBUDDY_CN_TARGET, user_data_dir)
}

/// 读取并解密指定桌面 IDE 档位的 secret（明文 JSON 字符串）。
pub fn read_codebuddy_ide_secret(
    flavor: CodeBuddyIdeFlavor,
    user_data_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    read_secret_for(flavor.target(), user_data_dir)
}

/// 读取并解密指定目标的 secret（明文 JSON 字符串）。
///
/// 只读探测：即使 db 不存在也只返回 `Ok(None)`，绝不创建目录或文件（issue #91）。
pub fn read_secret_for(
    target: &VscodeSafeStorageTarget,
    user_data_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    let root = match user_data_dir {
        Some(p) => p.to_path_buf(),
        None => (target.data_dir_resolver)()
            .ok_or_else(|| format!("无法定位 {} 数据目录", target.display_name))?,
    };
    let Some(db_path) = find_existing_state_db_path(&root) else {
        return Ok(None);
    };
    let data_root = data_root_from_db(&db_path)?.to_path_buf();
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("打开 state.vscdb 失败: {e}"))?;
    let key = secret_storage_item_key_for(target);
    let raw_value: Option<String> = match conn.query_row(
        "SELECT value FROM ItemTable WHERE key = ?1",
        [key.as_str()],
        |row| row.get(0),
    ) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => return Err(format!("查询 {} secret 失败: {err}", target.display_name)),
    };
    match raw_value {
        Some(value) => decode_secret_storage_value(&value, &data_root, target).map(Some),
        None => Ok(None),
    }
}

/// 目标 `state.vscdb` 中是否存在该 secret 行（只读查询、**不解密**）。
///
/// 供账号页轮询的状态接口使用：macOS 上解密会触发钥匙串授权弹窗，绝不能进轮询路径，
/// 因此这里只判断 key 是否存在（不读值、不解密）。
///
/// 与 [`resolve_state_db_path_for`] 的关键差异：**不创建任何目录或文件**——
/// 三个候选路径都不存在时直接返回 `Ok(false)`（`resolve_state_db_path_for` 会
/// `create_dir_all`，不能用于只读探测）。
pub fn has_secret_row_for(
    target: &VscodeSafeStorageTarget,
    user_data_dir: Option<&Path>,
) -> Result<bool, String> {
    let root = match user_data_dir {
        Some(path) => path.to_path_buf(),
        None => match (target.data_dir_resolver)() {
            Some(dir) => dir,
            None => return Ok(false),
        },
    };
    let Some(db_path) = find_existing_state_db_path(&root) else {
        return Ok(false);
    };
    // 只读打开：避免在「文件不存在」等边缘情况下由 SQLite 兜底新建空库。
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开 state.vscdb 失败: {e}"))?;
    let key = secret_storage_item_key_for(target);
    match conn.query_row(
        "SELECT 1 FROM ItemTable WHERE key = ?1 LIMIT 1",
        [key.as_str()],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        // 空库 / 尚未初始化 ItemTable：按「无该行」处理，不是失败。
        Err(err) if is_missing_table_error(&err) => Ok(false),
        Err(err) => Err(format!("查询 {} secret 行失败: {err}", target.display_name)),
    }
}

/// SQLite「表不存在」（`no such table`）：空库或尚未初始化 ItemTable。
fn is_missing_table_error(err: &rusqlite::Error) -> bool {
    err.to_string()
        .to_ascii_lowercase()
        .contains("no such table")
}

/// 加密并写入 CodeBuddy CN secret；保持既有行为。
pub fn inject_codebuddy_cn_secret(
    plaintext: &str,
    user_data_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    inject_secret_for(&CODEBUDDY_CN_TARGET, plaintext, user_data_dir)
}

/// 加密并写入指定桌面 IDE 档位的 secret。
pub fn inject_codebuddy_ide_secret(
    flavor: CodeBuddyIdeFlavor,
    plaintext: &str,
    user_data_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    inject_secret_for(flavor.target(), plaintext, user_data_dir)
}

/// 加密并写入指定目标 secret。
pub fn inject_secret_for(
    target: &VscodeSafeStorageTarget,
    plaintext: &str,
    user_data_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let db_path = resolve_state_db_path_for(target, user_data_dir)?;
    let data_root = data_root_from_db(&db_path)?.to_path_buf();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 state.vscdb 父目录失败: {e}"))?;
    }
    let conn = Connection::open(&db_path).map_err(|e| format!("打开 state.vscdb 失败: {e}"))?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .map_err(|e| format!("初始化 ItemTable 失败: {e}"))?;

    let db_key = secret_storage_item_key_for(target);
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

    let encrypted = encrypt_secret_payload(
        plaintext.as_bytes(),
        existing_prefix.as_deref(),
        &data_root,
        target,
    )?;
    let buffer_str = encode_secret_buffer(encrypted)?;
    conn.execute(
        "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)",
        rusqlite::params![db_key, buffer_str],
    )
    .map_err(|e| format!("写入 state.vscdb 失败: {e}"))?;

    // 写后校验：行存在且为 Buffer JSON
    let written: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?",
            [db_key.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| format!("写后校验失败: {e}"))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&written).map_err(|e| format!("写后校验 JSON 失败: {e}"))?;
    if parsed.get("type").and_then(|v| v.as_str()) != Some("Buffer") {
        return Err("写后校验失败：value 不是 Buffer".to_string());
    }
    Ok(db_path)
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
        assert_eq!(
            secret_storage_item_key_for(&CODEBUDDY_INTL_TARGET),
            r#"secret://{"extensionId":"tencent-cloud.coding-copilot","key":"planning-genie.new.accessToken"}"#
        );
        assert_eq!(
            CodeBuddyIdeFlavor::Intl.keychain_service(),
            "CodeBuddy Safe Storage"
        );
        assert_ne!(
            CodeBuddyIdeFlavor::Cn.secret_key(),
            CodeBuddyIdeFlavor::Intl.secret_key()
        );
    }

    #[test]
    fn flavor_target_matches_flavor_fields() {
        for flavor in [CodeBuddyIdeFlavor::Cn, CodeBuddyIdeFlavor::Intl] {
            let target = flavor.target();
            assert_eq!(target.secret_key, flavor.secret_key());
            assert_eq!(target.macos_keychain_service, flavor.keychain_service());
            assert_eq!(target.linux_secret_tool_app_names, flavor.linux_secret_apps());
            assert_eq!(target.display_name, flavor.product_label());
        }
    }

    #[test]
    fn secret_key_for_custom_target_uses_its_fields() {
        let target = VscodeSafeStorageTarget {
            data_dir_resolver: codebuddy_cn_data_dir,
            display_name: "Test",
            secret_item_prefix_extension_id: "tencent-cloud.coding-copilot",
            secret_key: "Tencent-Cloud.coding-copilot.new.accessToken",
            macos_keychain_service: "Code Safe Storage",
            linux_secret_tool_app_names: &["Code"],
        };
        assert_eq!(
            secret_storage_item_key_for(&target),
            r#"secret://{"extensionId":"tencent-cloud.coding-copilot","key":"Tencent-Cloud.coding-copilot.new.accessToken"}"#
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
        let Some(intl) = codebuddy_ide_data_dir(CodeBuddyIdeFlavor::Intl) else {
            return;
        };
        let intl_s = intl.to_string_lossy();
        assert!(
            intl_s.contains("CodeBuddy") && !intl_s.contains("CodeBuddy CN"),
            "intl data dir should be CodeBuddy not CN: {intl_s}"
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

    /// `has_secret_row_for`：只看 key 是否存在（有行 / 无行 / 无文件三态），
    /// 且不读值、不解密、不创建任何目录或文件。
    #[test]
    fn has_secret_row_detects_row_without_creating_files() {
        let dir =
            std::env::temp_dir().join(format!("wb-cn-secret-row-test-{}", uuid::Uuid::new_v4()));
        let db = dir.join("User").join("globalStorage").join("state.vscdb");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();

        // ① 库文件不存在 → false，且不得顺手创建目录/文件
        assert!(!has_secret_row_for(&CODEBUDDY_CN_TARGET, Some(&dir)).unwrap());
        assert!(!db.exists(), "只读探测不得创建 state.vscdb");

        let missing_root = dir.join("no-such-data-dir");
        assert!(!has_secret_row_for(&CODEBUDDY_CN_TARGET, Some(&missing_root)).unwrap());
        assert!(!missing_root.exists(), "只读探测不得创建数据目录");

        // ①b 空库（0 字节文件）：ItemTable 尚未初始化 → false，且不得报错
        std::fs::write(&db, b"").unwrap();
        assert!(!has_secret_row_for(&CODEBUDDY_CN_TARGET, Some(&dir)).unwrap());

        // ② 表存在但无该 key → false
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('other', 'x')",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(!has_secret_row_for(&CODEBUDDY_CN_TARGET, Some(&dir)).unwrap());

        // ③ 有该 key → true（值是不可解密的占位串，证明只查存在性、不走解密）
        let key = secret_storage_item_key();
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, 'not-a-ciphertext')",
            [key.as_str()],
        )
        .unwrap();
        drop(conn);
        assert!(has_secret_row_for(&CODEBUDDY_CN_TARGET, Some(&dir)).unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 回归 issue #91：只读探测（`read_secret_for`）不得有任何建目录副作用，
    /// 否则 `installed` 会把探测自建的空数据目录误判为「已接入」，且用户删掉后
    /// 下次探测又会重建，形成自我维持的误报。国内版与国际版 target 一并断言。
    #[test]
    fn read_secret_probe_creates_nothing() {
        let base =
            std::env::temp_dir().join(format!("wb-cn-readonly-probe-{}", uuid::Uuid::new_v4()));

        for (label, target, root) in [
            (
                "cn/数据目录整体缺失",
                &CODEBUDDY_CN_TARGET,
                base.join("cn-missing"),
            ),
            (
                "cn/目录存在但无 globalStorage",
                &CODEBUDDY_CN_TARGET,
                base.join("cn-empty-root"),
            ),
            (
                "intl/数据目录整体缺失",
                &CODEBUDDY_INTL_TARGET,
                base.join("intl-missing"),
            ),
        ] {
            if label.ends_with("无 globalStorage") {
                std::fs::create_dir_all(&root).unwrap();
            }
            assert_eq!(
                read_secret_for(target, Some(&root)).unwrap(),
                None,
                "{label}: 无 db 时应返回 Ok(None)"
            );
            assert!(!root.join("User").exists(), "{label}: 不得创建 User/");
            assert!(
                !root.join("User").join("globalStorage").exists(),
                "{label}: 不得创建 User/globalStorage/"
            );
            assert!(
                !root.join("globalStorage").exists(),
                "{label}: 不得创建 globalStorage/"
            );
            assert!(!root.join("state.vscdb").exists(), "{label}: 不得创建 state.vscdb");
        }

        std::fs::remove_dir_all(base).ok();
    }

    /// 回归 issue #91（写入路径不受影响）：db 不存在时注入仍会自动准备目录并写库成功。
    #[test]
    fn inject_still_prepares_directory() {
        let dir = std::env::temp_dir().join(format!("wb-cn-inject-mkdir-{}", uuid::Uuid::new_v4()));
        let plaintext = r#"{"token":"tok","accessToken":"uid+tok"}"#;
        let result = inject_secret_for(&CODEBUDDY_CN_TARGET, plaintext, Some(&dir));
        // Windows 上加密依赖系统 Safe Storage，无该条目会失败；只断言目录已准备好。
        let db = dir.join("User").join("globalStorage").join("state.vscdb");
        if result.is_ok() {
            assert!(db.exists(), "注入应创建 state.vscdb");
            let read = read_secret_for(&CODEBUDDY_CN_TARGET, Some(&dir)).unwrap();
            assert_eq!(read.as_deref(), Some(plaintext));
        } else {
            assert!(
                db.parent().map(|p| p.exists()).unwrap_or(false),
                "注入路径解析应准备 globalStorage 目录"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    /// 回归 issue #80：密钥环里确实有密码时，读本机登录信息不能再报
    /// 「无法加载 Linux secret storage key（v11）」。
    ///
    /// 只在本机既有密钥环条目、又有 CodeBuddy CN 数据目录时才验证，
    /// 无桌面环境（CI）直接跳过。
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_v11_secret_is_readable_when_keyring_has_password() {
        let Some(_password) = crate::modules::linux_keyring::find_password(
            CODEBUDDY_CN_TARGET.linux_secret_tool_app_names,
        ) else {
            return;
        };
        let Some(db_path) = codebuddy_cn_state_db_path() else {
            return;
        };
        if !db_path.exists() {
            return;
        }
        read_secret_for(&CODEBUDDY_CN_TARGET, None)
            .expect("密钥环里有密码时，读取 CodeBuddy CN secret 不应失败");
    }

    /// Linux 写入 → 读回必须还原原文：有密钥环走 v11、没有则退回 peanuts 的 v10，
    /// 两条路都要能自洽。
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_inject_then_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wb-linux-inject-{}", uuid::Uuid::new_v4()));
        let plaintext = r#"{"token":"tok","accessToken":"uid+tok"}"#;
        inject_secret_for(&CODEBUDDY_CN_TARGET, plaintext, Some(&dir)).unwrap();
        let read = read_secret_for(&CODEBUDDY_CN_TARGET, Some(&dir)).unwrap();
        assert_eq!(read.as_deref(), Some(plaintext));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
