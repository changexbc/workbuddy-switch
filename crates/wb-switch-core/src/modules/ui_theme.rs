//! UI 主题跟随账号（L6）：**云端为准，本地只做一件事 —— 把云端定下的 key 预写进 Local Storage**。
//!
//! 目标：切号后目标账号的主题与源账号一致。
//!
//! 两层各司其职，缺一不可：
//! - ① 云端（真源）：主题**按账号存在云端**（`/portal/user-asset/appearance` 的 `kind=theme`
//!   `resource_key`）。读 source 云端 key → 与 target 不同则写 target 云端 ⇒ 之后无论在哪台
//!   设备拉回云端拿到的都是继承值（**永久跟随**）。只改本地，几秒后会被云端回写覆盖。
//! - ② 本地（体验层）：云端回写是**异步**的（实测几秒）⇒ 只改云端的话，重启瞬间显示的是
//!   上一个账号留在 leveldb 里的主题。把①的 key 写进 leveldb ⇒ 重启瞬间即对。
//!
//! ★★ 键位（2026-09-17 本机 `.log` 逐字节逆向 + `app.asar` 取证，**改这条之前先读**）：
//! `workbuddy.appearance.state::personal::<uid>`，值 = `0x01` + `{"currentTheme":"<key>"}`。
//! key **原样透传** —— `light` / `dark` / 皮肤 id（如 `theme-tkboqn`）都照写，**不做映射、不做枚举**
//! （主题清单由服务端下发，前端不硬编码；枚举方案会随皮肤上下线过期）。
//! 另有 `workbuddy.appearance.mode::<type>::<eid>::<uid>`（明暗 light/dark/auto）与 legacy 键
//! `agent-ui-theme`（App 仍读作迁移输入）—— **两者都不写**：云端只下发一个 `kind=theme` 的 key，
//! 推不出目标明暗，宁可不写也不猜。
//!
//! **已砍掉的部分（别再加回来，理由都在）**：旧键映射与写入（legacy 键只影响一次性迁移）、
//! 读当前值做「已达标就跳过」（只扫 `.log`，compaction 后读不到是常态 ⇒ 极少生效）、
//! `ui_prefs` 审计（无人消费）。剩下的事就一件：**拿云端的 key，追加一条 record**。
//!
//! 安全设计（append-only，不改写任何现有字节）：
//! - 只向**该目录最新的 `.log`** 末尾追加一条完整 record；LevelDB 打开时按序 replay，
//!   我们这条因 sequence 更大而覆盖旧值。
//! - record 的 CRC 若算错或格式不符，LevelDB 会**丢弃该 record** —— 最坏情况是「预写不生效」，
//!   原有数据不受影响。
//! - 追加前把该 `.log` 备份到 `~/.wb-switch/backups/localstorage/<ts>/`（单文件，实测整个
//!   Local Storage 才 0.5 MB，成本可忽略，恢复只需还原这一个文件）。
//!
//! ⚠️ 采样纪律：**只能读 `.log`**（`.ldb` 有前缀压缩 + snappy，搜不到 ≠ 不存在）；App 运行中会
//! 持续写 + compaction，**键随时被搬走**；读到的值也**无法区分是「App 回写」还是「我们写的」**
//! ⇒ 用途是复验「结果对不对」，不能据此断言「预写生效」。
//!
//! 格式（2026-09-10 / 09-17 两次字节级验证）：
//! - 键前缀 = `_file://\0\x01`（origin 固定为打包渲染进程）；keylen 前置为 varint
//! - batch = seq(u64 le) + count(u32 le) + [type(1) + varint(klen) + key + varint(vlen) + value]
//! - record = crc32c-masked(4 le) + len(2 le) + type(1) + batch（crc 覆盖 type+batch；vlen **含** value 首字节）

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::modules::config::{backup_dir, home_dir, now_ms, utc_iso};

/// 主题状态键前缀（完整键 = 前缀 + uid）。
const APPEARANCE_STATE_PREFIX: &[u8] = b"_file://\x00\x01workbuddy.appearance.state::personal::";
/// Chromium localStorage value 首字节（0x01 = UTF-8 文本）。
const VALUE_PREFIX: u8 = 0x01;
/// WorkBuddy userData 下的 Local Storage 位置。
const LEVELDB_SUBDIR: &str = "app/session/Local Storage/leveldb";
/// LevelDB log 常量。
const BLOCK: usize = 32 * 1024;
const RECORD_HEADER: usize = 7;
const RECORD_FULL: u8 = 1;
const BATCH_TYPE_VALUE: u8 = 1;

fn leveldb_dir() -> PathBuf {
    home_dir().join(".workbuddy").join(LEVELDB_SUBDIR)
}

// ---------------------------------------------------------------------------
// CRC32C（Castagnoli，LevelDB 标准校验）
// ---------------------------------------------------------------------------

fn crc32c_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

fn crc32c(data: &[u8]) -> u32 {
    let t = crc32c_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = t[(crc ^ b as u32) as usize & 0xFF] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// LevelDB 的 crc 掩码：rotate right 15 再加固定常数。
fn mask_crc(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xA282_EAD8)
}

// ---------------------------------------------------------------------------
// 编码
// ---------------------------------------------------------------------------

fn put_varint32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        if v < 0x80 {
            out.push(v as u8);
            break;
        }
        out.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
}

/// 构造一条含单条 put 的 WriteBatch payload。
fn build_batch(seq: u64, key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + key.len() + value.len() + 6);
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes()); // count = 1
    p.push(BATCH_TYPE_VALUE);
    put_varint32(&mut p, key.len() as u32);
    p.extend_from_slice(key);
    put_varint32(&mut p, value.len() as u32);
    p.extend_from_slice(value);
    p
}

/// 把 batch payload 封成一条 FULL log record。
fn build_log_record(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() + RECORD_HEADER <= BLOCK, "单条 record 必须放进一个 block");
    let mut crc_input = Vec::with_capacity(1 + payload.len());
    crc_input.push(RECORD_FULL);
    crc_input.extend_from_slice(payload);
    let masked = mask_crc(crc32c(&crc_input));

    let mut out = Vec::with_capacity(RECORD_HEADER + payload.len());
    out.extend_from_slice(&masked.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.push(RECORD_FULL);
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// 写入 leveldb
// ---------------------------------------------------------------------------

/// 目录里编号最大的 `.log`（LevelDB 永远只往它追加）。
fn latest_log(dir: &Path) -> Result<PathBuf, String> {
    let mut logs: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let num: u64 = name.strip_suffix(".log")?.parse().ok()?;
            Some((num, e.path()))
        })
        .collect();
    logs.sort_by_key(|(n, _)| *n);
    logs.pop()
        .map(|(_, p)| p)
        .ok_or_else(|| "leveldb 无 .log 文件（可能刚被 compaction），跳过主题预写".to_string())
}

/// 完整状态键（前缀 + uid）。
fn appearance_state_key(uid: &str) -> Vec<u8> {
    let mut k = APPEARANCE_STATE_PREFIX.to_vec();
    k.extend_from_slice(uid.as_bytes());
    k
}

/// 状态键的 value：云端 key **原样**，不猜、不改写。
fn appearance_state_json(resource_key: &str) -> String {
    json!({ "currentTheme": resource_key }).to_string()
}

/// 向指定 `.log` 末尾追加一条 record（处理 block 剩余空间）。
fn append_record(log: &Path, seq: u64, key: &[u8], value: &[u8]) -> Result<(), String> {
    let payload = build_batch(seq, key, value);
    let record = build_log_record(&payload);

    let len = fs::metadata(log).map_err(|e| e.to_string())?.len() as usize;
    let rem = BLOCK - (len % BLOCK);
    let mut out: Vec<u8> = Vec::with_capacity(RECORD_HEADER + record.len() + rem);
    if rem < RECORD_HEADER || rem < RECORD_HEADER + record.len() {
        out.extend(std::iter::repeat_n(0u8, rem)); // trailer / 本 block 放不下，另起 block
    }
    out.extend_from_slice(&record);

    let mut f = fs::OpenOptions::new().append(true).open(log).map_err(|e| e.to_string())?;
    f.write_all(&out).map_err(|e| e.to_string())
}

/// 备份单个 `.log`（我们只改它 ⇒ 恢复只需还原它），返回备份路径。
/// `root` 由调用方给（生产 = `~/.wb-switch/backups/localstorage`），便于测试不碰用户目录。
fn backup_log(log: &Path, root: &Path) -> Result<PathBuf, String> {
    let name = log
        .file_name()
        .ok_or_else(|| "leveldb .log 路径异常".to_string())?
        .to_string_lossy()
        .to_string();
    let dst_dir = root.join(utc_iso());
    fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;
    let dst = dst_dir.join(name);
    fs::copy(log, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// 本地层：备份 .log → 追加状态记录。返回备份路径。
fn write_local_state(dir: &Path, uid: &str, resource_key: &str, backup_root: &Path) -> Result<PathBuf, String> {
    let log = latest_log(dir)?;
    let backup = backup_log(&log, backup_root)?; // 先备份，写失败可原地还原
    let value: Vec<u8> = std::iter::once(VALUE_PREFIX)
        .chain(appearance_state_json(resource_key).into_bytes())
        .collect();
    append_record(&log, now_ms() as u64, &appearance_state_key(uid), &value)?;
    Ok(backup)
}

// ---------------------------------------------------------------------------
// 云端层：WorkBuddy 把明暗/皮肤选择按账号存云端（/portal/user-asset/appearance），
// 切号启动后由外观运行时拉回——本地注入只能保证启动瞬间，云端改掉才能永久跟随。
// 接口规格 2026-09-11 从 app.asar 渲染层逆向 + 本机实测（Bearer access_token）。
// ---------------------------------------------------------------------------

const APPEARANCE_GET_URL: &str = "https://www.workbuddy.cn/portal/user-asset/appearance/get";
const APPEARANCE_SET_URL: &str = "https://www.workbuddy.cn/portal/user-asset/appearance/set";

fn cloud_http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        // 本机 WARP 代理会碍事，workbuddy.cn 直连可达
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// 读账号云端 theme 选择（resource_key：`light` / `dark` / 皮肤 id）。
fn fetch_cloud_theme(token: &str) -> Option<String> {
    let resp: Value = cloud_http()
        .get(APPEARANCE_GET_URL)
        .bearer_auth(token)
        .send()
        .ok()?
        .json()
        .ok()?;
    if resp.get("code").and_then(|c| c.as_i64()) != Some(0) {
        return None;
    }
    let items = resp.get("data")?.get("items")?.as_array()?;
    items
        .iter()
        .filter_map(|i| i.as_object())
        .find(|o| o.get("kind").and_then(|k| k.as_str()) == Some("theme"))
        .and_then(|o| o.get("resource_key"))
        .and_then(|k| k.as_str())
        .map(|s| s.to_string())
}

/// 写账号云端 theme 选择。
fn push_cloud_theme(token: &str, resource_key: &str) -> Result<(), String> {
    let resp: Value = cloud_http()
        .post(APPEARANCE_SET_URL)
        .bearer_auth(token)
        .json(&json!({ "kind": "theme", "resource_key": resource_key }))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    match resp.get("code").and_then(|c| c.as_i64()) {
        Some(0) => Ok(()),
        _ => Err(format!("set 响应异常: {resp}")),
    }
}

/// 云端继承：把 source 账号的 theme 选择写到 target 账号云端，返回**最终生效的 key**。
/// 任一步失败返回 Err（不阻断切号）。
fn inherit_cloud_theme(source_token: &str, target_token: &str) -> Result<Value, String> {
    let source_theme = fetch_cloud_theme(source_token)
        .ok_or_else(|| "读取 source 云端主题失败".to_string())?;
    let target_theme = fetch_cloud_theme(target_token);
    if target_theme.as_deref() == Some(source_theme.as_str()) {
        return Ok(json!({ "changed": false, "theme": source_theme }));
    }
    push_cloud_theme(target_token, &source_theme)?;
    Ok(json!({ "changed": true, "theme": source_theme, "previous": target_theme }))
}

// ---------------------------------------------------------------------------
// 对外入口
// ---------------------------------------------------------------------------

/// 切号时同步主题：① 云端继承（永久跟随）→ ② 用①的 key 预写本地（重启瞬间即对）。
///
/// ①失败 ⇒ **不写本地**（宁可不写，也不写错值）。本地层失败不影响①的结果。
/// 必须在 WorkBuddy 完全关闭后、写 auth/重启前调用。
pub fn sync_theme_for_switch(
    source_uid: Option<&str>,
    target_uid: &str,
    source_token: Option<&str>,
    target_token: Option<&str>,
) -> Value {
    let dir = leveldb_dir();
    if target_uid.is_empty() {
        return json!({ "inherited": false, "cloud": null, "skipped": true });
    }

    // ① 云端继承（真源）
    let cloud = match (source_token, target_token) {
        (Some(src), Some(tgt)) if !src.is_empty() && !tgt.is_empty() => match inherit_cloud_theme(src, tgt) {
            Ok(v) => v,
            Err(e) => json!({ "changed": false, "error": e }),
        },
        _ => json!({ "changed": false, "reason": "缺少 source/target token" }),
    };

    // ② 本地预写（值取自①）
    let final_key = cloud
        .get("theme")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let local = match final_key.as_deref() {
        None => json!({
            "inherited": false,
            "reason": "云端主题未取到，跳过本地预写（避免写入不确定的值）",
        }),
        Some(key) if !dir.is_dir() => json!({
            "inherited": false,
            "theme": key,
            "reason": "找不到 Local Storage 目录，跳过本地预写",
        }),
        Some(key) => match write_local_state(&dir, target_uid, key, &backup_dir().join("localstorage")) {
            Ok(backup) => json!({
                "inherited": true,
                "theme": key,
                "resourceKey": key,
                "backup": backup,
            }),
            Err(e) => json!({ "inherited": false, "theme": key, "error": e }),
        },
    };

    json!({
        "local": local,
        "cloud": cloud,
        // 继承语义下源身份经 token 体现，uid 仅留作排障线索
        "sourceUid": source_uid,
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const UID: &str = "00000000-0000-0000-0000-000000000000";
    const UID2: &str = "11111111-1111-1111-1111-111111111111";

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wbs-theme-{tag}-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn expected_record(seq: u64, uid: &str, key: &str) -> Vec<u8> {
        let value: Vec<u8> = std::iter::once(VALUE_PREFIX)
            .chain(appearance_state_json(key).into_bytes())
            .collect();
        build_log_record(&build_batch(seq, &appearance_state_key(uid), &value))
    }

    #[test]
    fn crc32c_matches_known_vectors() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0x0000_0000);
        assert_eq!(crc32c(b"a"), 0xC1D0_4330);
    }

    #[test]
    fn varint_encodes_expected_bytes() {
        let mut buf = Vec::new();
        put_varint32(&mut buf, 84);
        assert_eq!(buf, vec![0x54]); // 实测 keylen
        let mut buf = Vec::new();
        put_varint32(&mut buf, 32);
        assert_eq!(buf, vec![0x20]); // 实测 vlen
        let mut buf = Vec::new();
        put_varint32(&mut buf, 300);
        assert_eq!(buf, vec![0xAC, 0x02]);
    }

    /// 键/值的形状必须与实测字节一致（keylen `0x54`=84、vlen `0x20`=32）。
    /// 对不上说明前缀或 JSON 形状漂了 —— 这是防「静默写错键」的第一道闸。
    #[test]
    fn appearance_state_shape_matches_observed_bytes() {
        let key = appearance_state_key(UID);
        assert_eq!(key.len(), 84);
        assert!(key.starts_with(b"_file://\x00\x01workbuddy.appearance.state::personal::"));
        assert!(key.ends_with(UID.as_bytes()));

        let json = appearance_state_json("theme-tkboqn");
        assert_eq!(json, r#"{"currentTheme":"theme-tkboqn"}"#);
        assert_eq!(json.len() + 1, 32);
    }

    /// 写入的字节必须与手工构造的 record 完全一致（vlen 含类型字节、CRC 覆盖 type+batch）。
    #[test]
    fn append_writes_expected_bytes() {
        let dir = tmp("bytes");
        let log = dir.join("000007.log");
        fs::write(&log, build_log_record(&build_batch(1, b"_file://\x00\x01seed", b"\x01{}"))).unwrap();
        let before = fs::metadata(&log).unwrap().len() as usize;

        let value: Vec<u8> = std::iter::once(VALUE_PREFIX)
            .chain(appearance_state_json("theme-tkboqn").into_bytes())
            .collect();
        append_record(&log, 42, &appearance_state_key(UID), &value).unwrap();

        let data = fs::read(&log).unwrap();
        assert_eq!(&data[before..], &expected_record(42, UID, "theme-tkboqn")[..]);
        fs::remove_dir_all(&dir).ok();
    }

    /// block 边界：剩余 < record 头时先补 trailer 再另起 block（record 不跨 block 起始）。
    #[test]
    fn append_handles_block_boundary() {
        let dir = tmp("blk");
        let log = dir.join("000003.log");
        let prefix_len = BLOCK - 3;
        fs::write(&log, vec![0xABu8; prefix_len]).unwrap();

        let value: Vec<u8> = std::iter::once(VALUE_PREFIX)
            .chain(appearance_state_json("dark").into_bytes())
            .collect();
        append_record(&log, 7, &appearance_state_key(UID), &value).unwrap();

        let data = fs::read(&log).unwrap();
        let expected = expected_record(7, UID, "dark");
        assert_eq!(data.len(), prefix_len + 3 + expected.len());
        assert_eq!(&data[prefix_len + 3..], &expected[..]);
        fs::remove_dir_all(&dir).ok();
    }

    /// 按账号分键 + 只碰**最新**的那个 .log + 备份落盘。
    #[test]
    fn writes_latest_log_for_the_right_account() {
        let dir = tmp("uid");
        let bk = tmp("bk");
        fs::write(dir.join("000002.log"), Vec::<u8>::new()).unwrap();
        fs::write(dir.join("000009.log"), Vec::<u8>::new()).unwrap();

        let backup = write_local_state(&dir, UID, "dark", &bk).unwrap();
        assert!(backup.exists(), "备份必须落盘");
        assert!(backup.starts_with(&bk), "备份应落在调用方给的根目录下");

        assert!(fs::read(dir.join("000002.log")).unwrap().is_empty(), "旧的 .log 不该被写");
        let latest = fs::read(dir.join("000009.log")).unwrap();
        assert!(
            latest.windows(appearance_state_key(UID).len()).any(|w| w == appearance_state_key(UID)),
            "状态键必须出现在最新 .log 里"
        );
        let needle = br#"{"currentTheme":"dark"}"#;
        assert!(
            latest.windows(needle.len()).any(|w| w == needle),
            "值必须原样写入"
        );

        assert_ne!(appearance_state_key(UID), appearance_state_key(UID2));
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&bk).ok();
    }
}
