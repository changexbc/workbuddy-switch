//! 多账号数据对齐：把 multi_sync 的 L1/L3/L4/L5 能力内置进 switch。
//!
//! 设计原则（对照 multi_sync.py，规则保持一致）：
//! - 归属层（L3）：「移动」而非「复制」，UPDATE 既有行的 owner/user_id，天然无双跑
//! - 文件层（L4）：settings.json claw.users 深合并，SECRET_KEYS 命中即跳过防串号；
//!   storage/user-* 与画像缓存做「目标缺失才补」的单向对齐
//! - 合并层（L5）：my-files.json 全账号并集，禁止单向覆盖（防丢 favoriteIds 键）
//! - 备份层（L1）：任何落盘前先备份 workbuddy.db + settings.json；storage 被
//!   覆盖的文件在覆盖前另存 backups/storage/<ts>/<user 目录>/<相对路径>（唯一还原点）
//! - dry_run：只统计将要发生的变更，不落盘、不关 App、不写凭据

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::modules::automations;
use crate::modules::discover;
use crate::modules::config::{atomic_write, backup_dir, utc_iso};
use crate::modules::session::{backup_workbuddy_db, open_db, workbuddy_db_path};
use crate::modules::variant::WbVariant;

/// 身份凭据关键词：命中即跳过，绝不跨账号复制（含渠道身份，防止串号）。
const SECRET_KEYS: &[&str] = &[
    "bottoken", "accountid", "token", "credential", "secret", "master.key",
    "appid", "appsecret", "corpid", "apikey", "accesstoken", "refreshtoken",
    "password", "sessionkey", "privatekey", "signature",
    "channelid", "userid", "openid", "unionid", "chatid", "groupid", "roomid",
    "webhook", "robotid", "wxid", "imuid", "ownerid",
];
const SECRET_FILENAMES: &[&str] = &["master.key", "credential", "credentials"];
/// 这些文件必须走并集合并，禁止单向覆盖。
const MERGE_ONLY_FILES: &[&str] = &["my-files.json"];

pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_lowercase().replace(['_', '-'], "");
    SECRET_KEYS.iter().any(|s| k.contains(&s.replace('.', "")))
}

fn is_secret_file(name: &str) -> bool {
    let low = name.to_lowercase();
    SECRET_FILENAMES.iter().any(|s| low.contains(s))
}

fn is_merge_only(name: &str) -> bool {
    MERGE_ONLY_FILES.iter().any(|s| name.eq_ignore_ascii_case(s))
}

/// 对齐选项（dry_run=true 时只统计不落盘）。
///
/// 命名：align_automations = 「带走定时任务」；align_files = 「同步设置与文件」；
/// slim_keep = 「清理旧会话」保留条数（0=关）。
/// 「会话归属对齐」（原 align_sessions）已于 2026-09-17 全链删除——它正是「幽灵会话」
/// 的成因之一（改别人账号的 sessions.user_id），归档见
/// `.memory/archive/align-sessions-removed-2026-09-17.md`。
/// 「同步项目列表」（原 sync_projects）已于 2026-09-17 弃用并清理，见 `projects_anchor`。
#[derive(Debug, Clone, Default)]
pub struct AlignOptions {
    pub align_automations: bool,
    pub align_files: bool,
    /// 会话瘦身：每项目保留最近 N 条存活会话（0 或缺省 = 关闭）。
    pub slim_keep: i64,
    /// 预览模式：只统计变更，不落盘。
    pub dry_run: bool,
}

impl AlignOptions {
    /// 是否有任一对齐项开启（全关时整个对齐流程跳过）。
    fn any_enabled(&self) -> bool {
        self.align_automations || self.align_files || self.slim_keep > 0
    }
}

/// 从账号 JSON 里取 uid（空串表示账号缺 uid，调用方应跳过对齐）。
pub fn account_uid(acc: &Value) -> String {
    acc.get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 备份 settings.json（P1 全量备份的一半；db 备份复用 session::backup_workbuddy_db）。
pub fn backup_settings() -> Option<PathBuf> {
    let src = discover::settings_path();
    if !src.is_file() {
        return None;
    }
    let root = backup_dir().join("settings").join(utc_iso());
    fs::create_dir_all(&root).ok()?;
    let dst = root.join("settings.json");
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// L1：备份 workbuddy.db + settings.json，返回描述。
fn backup_all() -> Value {
    let db = backup_workbuddy_db(WbVariant::Cn, &backup_dir().join("db").join(utc_iso()))
        .map(|p| p.to_string_lossy().to_string());
    let settings = backup_settings().map(|p| p.to_string_lossy().to_string());
    json!({ "db": db, "settings": settings })
}

/// 每类备份保留的最近份数（真实执行落盘后顺带轮转）。
pub const BACKUP_KEEP: usize = 20;

/// 目录名是否为 `utc_iso` 生成的时间戳格式（`%Y-%m-%dT%H-%M-%SZ`，20 字符）。
fn is_backup_ts(name: &str) -> bool {
    let b = name.as_bytes();
    name.len() == 20
        && name.ends_with('Z')
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b'-'
        && b[16] == b'-'
        && b.iter().enumerate().all(|(i, c)| {
            matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit()
        })
}

/// 备份轮转：`root`（= backups/）下每个类别目录只保留最近 [`BACKUP_KEEP`]
/// 份时间戳目录，其余删除。非时间戳命名的条目一律不碰；返回删除的目录数。
/// 时间戳字符串排序即时间序（`utc_iso` 格式天然可排序）。
pub fn prune_backups_at(root: &Path, keep: usize) -> usize {
    let mut removed = 0usize;
    let Ok(categories) = fs::read_dir(root) else {
        return 0;
    };
    for cat in categories.flatten() {
        let cat_path = cat.path();
        if !cat_path.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&cat_path) else {
            continue;
        };
        let mut stamps: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| is_backup_ts(n) && cat_path.join(n).is_dir())
            .collect();
        if stamps.len() <= keep {
            continue;
        }
        stamps.sort();
        stamps.reverse();
        for stale in stamps.into_iter().skip(keep) {
            if fs::remove_dir_all(cat_path.join(&stale)).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// 真实备份落盘后调用 [`prune_backups_at`]，本次时间戳天然在保留范围内。
fn prune_backups(keep: usize) -> usize {
    prune_backups_at(&backup_dir(), keep)
}

// ---------------------------------------------------------------------------
// 对齐源选择
// ---------------------------------------------------------------------------

fn latest_mtime_dir(p: &Path) -> i64 {
    let mut latest = 0i64;
    if let Ok(entries) = fs::read_dir(p) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                latest = latest.max(latest_mtime_dir(&path));
            } else if let Ok(meta) = e.metadata() {
                if let Ok(m) = meta.modified() {
                    if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                        latest = latest.max(d.as_millis() as i64);
                    }
                }
            }
        }
    }
    latest
}

/// 缺省对齐源 = 除目标外最近活跃的账号（按 user-<uid>-personal 最新 mtime）。
pub fn pick_source(target: &str) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for uid in discover::discover_accounts() {
        if uid == target {
            continue;
        }
        let dir = discover::storage_dir().join(format!("user-{uid}-personal"));
        let m = if dir.is_dir() { latest_mtime_dir(&dir) } else { 0 };
        if best.as_ref().is_none_or(|(bm, _)| m > *bm) {
            best = Some((m, uid));
        }
    }
    best.map(|(_, uid)| uid)
}

// ---------------------------------------------------------------------------
// L3 归属层（DB）→ 已拆到 `automations.rs`（2026-09-19，上游 PR 单独走一个）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// L4 文件层
// ---------------------------------------------------------------------------

/// settings.json claw.users 深合并（源补齐到目标），SECRET_KEYS 命中的键整棵跳过。
fn align_settings_claw_users(src: &str, dst: &str, dry_run: bool) -> Value {
    align_settings_claw_users_at(&discover::settings_path(), src, dst, dry_run)
}

/// 注入版：`settings_file` = settings.json 路径（测试传临时文件）。
fn align_settings_claw_users_at(settings_file: &Path, src: &str, dst: &str, dry_run: bool) -> Value {
    let mut changed: Vec<String> = Vec::new();
    let mut skipped = false;
    'outer: {
        let Ok(text) = fs::read_to_string(settings_file) else {
            skipped = true;
            break 'outer;
        };
        let Ok(mut data) = serde_json::from_str::<Value>(&text) else {
            skipped = true;
            break 'outer;
        };
        let Some(users) = data
            .get_mut("claw")
            .and_then(|c| c.get_mut("users"))
            .and_then(|u| u.as_object_mut())
        else {
            skipped = true;
            break 'outer;
        };
        let Some(src_node) = users.get(src).cloned() else {
            skipped = true;
            break 'outer;
        };
        let Some(dst_node) = users.get_mut(dst) else {
            skipped = true;
            break 'outer;
        };
        if let (Value::Object(s), Value::Object(d)) = (&src_node, dst_node) {
            let s = s.clone();
            merge_walk(&s, d, "", &mut changed);
        }
        if !changed.is_empty() && !dry_run {
            if let Ok(out) = serde_json::to_string_pretty(&data) {
                if let Err(e) = atomic_write(settings_file, &out) {
                    return json!({ "changed": 0, "skipped": true, "error": e.to_string() });
                }
            }
        }
    }
    json!({
        "changed": changed.len(),
        "keys": changed.iter().take(8).collect::<Vec<_>>(),
        "skipped": skipped,
    })
}

/// 深合并：src 的键补齐/更新到 dst（secret 键跳过），记录变更路径。
fn merge_walk(
    src: &Map<String, Value>,
    dst: &mut Map<String, Value>,
    path: &str,
    changed: &mut Vec<String>,
) {
    for (k, v) in src {
        if is_secret_key(k) {
            continue;
        }
        let full = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
        match dst.get_mut(k) {
            None => {
                changed.push(format!("+{full}"));
                dst.insert(k.clone(), v.clone());
            }
            Some(Value::Object(d2)) if v.is_object() => {
                if let Value::Object(s2) = v {
                    merge_walk(s2, d2, &full, changed);
                }
            }
            Some(cur) if cur != v => {
                changed.push(format!("~{full}"));
                *cur = v.clone();
            }
            _ => {}
        }
    }
}

/// 递归目录对齐：目标缺失或内容不同才复制；跳过 .bak / 凭据文件；my-files.json 延迟到 L5。
fn align_storage_dir(
    src: &Path,
    dst: &Path,
    backup_root: &Path,
    dry_run: bool,
    copied: &mut usize,
    skipped: &mut usize,
    deferred: &mut usize,
    samples: &mut Vec<String>,
) {
    if !src.is_dir() {
        return;
    }
    if !dst.exists() && !dry_run {
        let _ = fs::create_dir_all(dst);
    }
    let Ok(entries) = fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        let sp = entry.path();
        let dp = dst.join(entry.file_name());
        if sp.is_dir() {
            align_storage_dir(&sp, &dp, backup_root, dry_run, copied, skipped, deferred, samples);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".bak") || is_secret_file(&name) {
            *skipped += 1;
            continue;
        }
        if is_merge_only(&name) {
            *deferred += 1;
            continue;
        }
        // 先比 size：长度不同必不同（免读大文件进内存）；同长才读内容精确比对
        let need = match (fs::metadata(&sp).ok().map(|m| m.len()), fs::metadata(&dp).ok().map(|m| m.len())) {
            (Some(a), Some(b)) if a == b => match (fs::read(&sp), fs::read(&dp)) {
                (Ok(x), Ok(y)) => x != y,
                (Ok(_), Err(_)) => true,
                _ => false,
            },
            (Some(_), None) => true,    // 目标缺失/不可读 → 复制
            (Some(_), Some(_)) => true, // 长度不同 → 复制
            _ => false,
        };
        if !need {
            continue;
        }
        *copied += 1;
        if samples.len() < 6 {
            samples.push(name);
        }
        if !dry_run {
            if let Some(parent) = dp.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // 覆盖前单文件备份（镜像 dst 内相对结构）：<backup_root>/storage/<ts>/<user 目录>/<rel>。
            // L1 备份只保 db + settings.json，这里是 storage 被覆盖文件的唯一还原点。
            // 备份失败 = 无还原点 ⇒ 宁不同步也不裸覆盖（与 ui_theme「宁可不写」同哲学）。
            let mut backed_up = true;
            if dp.is_file() {
                let rel = dp.strip_prefix(dst).unwrap_or(dp.as_path());
                let target = backup_root
                    .join("storage")
                    .join(utc_iso())
                    .join(dst.file_name().unwrap_or_default())
                    .join(rel);
                let bparent_ok = target
                    .parent()
                    .map(|p| fs::create_dir_all(p).is_ok())
                    .unwrap_or(true);
                backed_up = bparent_ok && fs::copy(&dp, &target).is_ok();
            }
            if !backed_up {
                *copied -= 1;
                *skipped += 1;
                continue;
            }
            if fs::copy(&sp, &dp).is_err() {
                *copied -= 1;
                *skipped += 1;
            }
        }
    }
}

/// 云端画像缓存对齐：memory/<uid>_memory.md 源 -> 目标。
/// 注入版：`memory_root` = memory 目录、`backup_root` = backups 根（测试传临时目录）。
fn align_memory_profile_at(
    memory_root: &Path,
    backup_root: &Path,
    src: &str,
    dst: &str,
    dry_run: bool,
) -> Value {
    let sa = memory_root.join(format!("{src}_memory.md"));
    let sb = memory_root.join(format!("{dst}_memory.md"));
    if !sa.is_file() {
        return json!({ "changed": false, "skipped": true });
    }
    let same = match (fs::read(&sa), fs::read(&sb)) {
        (Ok(a), Ok(b)) => a == b,
        (Ok(_), Err(_)) => false,
        _ => true,
    };
    if same {
        return json!({ "changed": false });
    }
    let bytes = fs::metadata(&sa).map(|m| m.len()).unwrap_or(0);
    if !dry_run {
        if sb.is_file() {
            let root = backup_root.join("memory").join(utc_iso());
            let bparent_ok = fs::create_dir_all(&root).is_ok();
            // 备份失败 = 无还原点 ⇒ 宁不同步也不裸覆盖
            if !bparent_ok || fs::copy(&sb, root.join(format!("{dst}_memory.md"))).is_err() {
                return json!({ "changed": false, "skipped": true });
            }
        }
        let _ = fs::copy(&sa, &sb);
    }
    json!({ "changed": true, "bytes": bytes })
}

/// L5：my-files.json 全账号并集合并（列表取并集去重，其余键首个非空值生效）。
/// 注入版：`storage_root` = user-<uid>-personal 的父目录、`backup_root` = backups 根。
fn merge_my_files_all_at(
    storage_root: &Path,
    backup_root: &Path,
    accounts: &[String],
    dry_run: bool,
) -> Value {
    let mut files: Vec<PathBuf> = Vec::new();
    for uid in accounts {
        let scoped = storage_root
            .join(format!("user-{uid}-personal"))
            .join("scoped");
        let Ok(entries) = fs::read_dir(&scoped) else {
            continue;
        };
        for scope in entries.flatten() {
            let f = scope.path().join("my-files.json");
            if f.is_file() {
                files.push(f);
            }
        }
    }
    if files.len() < 2 {
        return json!({ "files": files.len(), "changed": 0 });
    }

    let mut merged = Map::new();
    let mut parsed: Vec<(PathBuf, Map<String, Value>)> = Vec::new();
    for f in &files {
        let parsed_one = fs::read_to_string(f)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
        if let Ok(Value::Object(m)) = parsed_one {
            parsed.push((f.clone(), m));
        }
    }
    for (_, m) in &parsed {
        for (k, v) in m {
            match merged.get_mut(k) {
                None => {
                    merged.insert(k.clone(), v.clone());
                }
                Some(Value::Array(dst_list)) if v.is_array() => {
                    if let Value::Array(src_list) = v {
                        let mut all: Vec<String> = dst_list
                            .iter()
                            .chain(src_list.iter())
                            .map(|x| x.to_string())
                            .collect();
                        all.sort();
                        all.dedup();
                        *dst_list = all
                            .iter()
                            .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.clone())))
                            .collect();
                    }
                }
                _ => {}
            }
        }
    }

    let mut changed = 0usize;
    if !dry_run {
        for (f, m) in &parsed {
            if *m != merged {
                if let Ok(out) = serde_json::to_string_pretty(&Value::Object(merged.clone())) {
                    let root = backup_root.join("my-files").join(utc_iso());
                    let _ = fs::create_dir_all(&root);
                    let _ = fs::copy(f, root.join("my-files.json"));
                    if atomic_write(f, &out).is_ok() {
                        changed += 1;
                    }
                }
            }
        }
    } else {
        changed = parsed.iter().filter(|(_, m)| *m != merged).count();
    }
    json!({ "files": files.len(), "changed": changed, "keys": merged.len() })
}

// ---------------------------------------------------------------------------
// 总入口
// ---------------------------------------------------------------------------

/// 多账号对齐总入口（L1 备份 + L3 归属 + L4 文件 + L5 合并）。
///
/// dry_run=true 时只统计计划变更；任一开关都未打开时返回 noop。
pub fn align_data(target_uid: &str, source_uid: Option<&str>, opts: &AlignOptions) -> Value {
    let mut report = json!({
        "targetUid": target_uid,
        "dryRun": opts.dry_run,
        "automations": { "updated": 0, "outbox": 0 },
        "settings": {
            "claw": { "changed": 0, "skipped": true },
            "storage": { "copied": 0, "skipped": 0, "deferred": 0, "samples": [] },
            "memory": { "changed": false },
            "myFiles": { "files": 0, "changed": 0, "keys": 0 },
        },
    });
    let any = opts.align_automations || opts.align_files;
    if !any || target_uid.is_empty() {
        report["noop"] = json!(true);
        return report;
    }

    let db = workbuddy_db_path(WbVariant::Cn);
    if !db.is_file() {
        report["error"] = json!("workbuddy.db 不存在");
        return report;
    }

    if !opts.dry_run {
        report["backup"] = backup_all();
        // 备份轮转：每类只保留最近 BACKUP_KEEP 份（含本次），防 backups 无限堆积
        let pruned = prune_backups(BACKUP_KEEP);
        if pruned > 0 {
            report["backup"]["pruned"] = json!(pruned);
        }
    }

    if opts.align_automations {
        if let Ok((a, o)) = automations::align_automations_owner_in_db(&db, target_uid, opts.dry_run) {
            report["automations"] = json!({ "updated": a, "outbox": o });
        }
    }

    if opts.align_files {
        let storage = discover::storage_dir();
        let backups = backup_dir();
        let source = source_uid
            .map(|s| s.to_string())
            .or_else(|| pick_source(target_uid));
        report["settings"]["sourceUid"] = json!(source);
        if let Some(src) = source {
            if src != target_uid {
                report["settings"]["claw"] =
                    align_settings_claw_users(&src, target_uid, opts.dry_run);

                let mut copied = 0usize;
                let mut skipped = 0usize;
                let mut deferred = 0usize;
                let mut samples: Vec<String> = Vec::new();
                for suffix in ["", "-personal"] {
                    let s = storage.join(format!("user-{src}{suffix}"));
                    let d = storage.join(format!("user-{target_uid}{suffix}"));
                    align_storage_dir(
                        &s, &d, &backups, opts.dry_run, &mut copied, &mut skipped, &mut deferred,
                        &mut samples,
                    );
                }
                report["settings"]["storage"] = json!({
                    "copied": copied, "skipped": skipped, "deferred": deferred, "samples": samples,
                });

                report["settings"]["memory"] =
                    align_memory_profile_at(&discover::memory_dir(), &backups, &src, target_uid, opts.dry_run);
            }
        }
        let accounts = discover::discover_accounts();
        report["settings"]["myFiles"] =
            merge_my_files_all_at(&storage, &backups, &accounts, opts.dry_run);
    }

    report
}

/// 会话瘦身，追加进报告（真实执行与预览共用）。
///
/// （原「项目侧栏同步」已于 2026-09-17 弃用，本函数只剩瘦身。）
/// `dry_run` 只从 `opts.dry_run` 读——不再另收一个可能与之矛盾的独立参数。
/// `protected_ids` = 本次手动勾选复制体，瘦身时跳过（不删、也不占保留名额）。
/// `keep_sids` = 统一保留名单（autoLink 计算的 keepTargetSids）；传入时瘦身按
/// 「名单外全删」判定，`slim_keep` 的每项目 keep 逻辑退位（见 projects_anchor）。
fn append_project_and_slim(
    report: &mut Value,
    target_uid: &str,
    opts: &AlignOptions,
    protected_ids: &[String],
    keep_sids: &[String],
) {
    // 会话瘦身（slim_keep）随「清理旧会话」PR 单独发，本版本该字段恒为 0。
    // 参数签名保持不动（调用点零改动），此处显式消化掉以免 unused 警告。
    let _ = (report, target_uid, opts, protected_ids, keep_sids);
}

/// 只读取一批会话的 cwd（预览阶段估算复制体落点用，查不到就忽略）。
fn session_cwds(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(conn) = open_db(&workbuddy_db_path(WbVariant::Cn), true) else {
        return out;
    };
    for id in ids {
        if let Ok(Some(c)) = conn.query_row(
            "SELECT cwd FROM sessions WHERE id = ?1",
            [id.as_str()],
            |r| r.get::<_, Option<String>>(0),
        ) {
            if !c.is_empty() {
                out.push(c);
            }
        }
    }
    out
}

/// 预览专用：量化「本次将复制的会话」对瘦身的抵消。
///
/// 预览不执行复制，拿不到复制体的新 id，无法把它们放进保护名单，所以 `planned`
/// 偏大。这里按 cwd 把复制体对齐到瘦身项目上，给出「将复制几条 / 命中几个瘦身项目」，
/// 替代原来「删除数可能更少」的模糊提示。
fn annotate_copy_impact(report: &mut Value, copy_session_ids: &[String]) {
    if copy_session_ids.is_empty() {
        return;
    }
    let cwds = session_cwds(copy_session_ids);
    let slimmed: BTreeSet<&str> = report["slim"]["groups"]
        .as_array()
        .map(|gs| gs.iter().filter_map(|g| g["cwd"].as_str()).collect())
        .unwrap_or_default();
    let (hit_count, hit_projects) = copy_hits(&slimmed, &cwds);
    report["slim"]["copyPlanned"] = json!({
        "total": copy_session_ids.len(),
        "hitCount": hit_count,
        "hitProjects": hit_projects,
    });
}

/// 纯函数：复制体 cwd 与瘦身项目的交集 → (命中会话条数, 命中项目数)。
fn copy_hits(slimmed: &BTreeSet<&str>, cwds: &[String]) -> (usize, usize) {
    let mut projects: BTreeSet<&str> = BTreeSet::new();
    let mut count = 0usize;
    for c in cwds {
        if slimmed.contains(c.as_str()) {
            count += 1;
            projects.insert(c.as_str());
        }
    }
    (count, projects.len())
}

/// 切号对齐的统一入口：预览（`opts.dry_run=true`）与真实执行（false）共用。
///
/// 两者唯一分歧在主题：真实执行调用 `sync_theme_for_switch`（写 leveldb + 云端，
/// 预览绝不能调），预览只给 `planned` 占位。主题跟随受 `align_files`
/// （「同步设置与文件」）管辖：关闭时不执行、报告也不写 `settings.theme`
/// ——前端按字段缺失隐藏该行，预览与执行天然一致。
///
/// `protected_ids` = 真实执行时本次手动复制出来的会话 id（预览为空）。
/// `copy_session_ids` = 预览时将要复制的源会话 id（真实执行为空，已计入 protected_ids）。
/// `keep_sids` = 统一保留名单（autoLink 计算的 keepTargetSids；预览时为 dry_run 报告的同名字段）。
fn run_switch_sync(
    target_acc: &Value,
    opts: &AlignOptions,
    protected_ids: &[String],
    copy_session_ids: &[String],
    keep_sids: &[String],
) -> Option<Value> {
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty() || !opts.any_enabled() {
        return None;
    }
    let source_uid = crate::modules::session::current_user_uid(WbVariant::Cn);
    let mut report = align_data(&target_uid, source_uid.as_deref(), opts);

    append_project_and_slim(&mut report, &target_uid, opts, protected_ids, keep_sids);

    if opts.dry_run {
        report["dryRun"] = json!(true);
        annotate_copy_impact(&mut report, copy_session_ids);
        if opts.align_files {
            report["settings"]["theme"] = json!({ "planned": true });
        }
        return Some(report);
    }

    // 主题跟随账号（受「同步设置与文件」管辖）：本地继承（leveldb 注入，启动瞬间
    // 生效）+ 云端继承（把 target 的云端外观选择改成 source 的值，根治回跳）。
    if opts.align_files {
        let source_acc = source_uid.as_deref().and_then(|uid| {
            crate::modules::account::load_accounts()
                .into_iter()
                .find(|a| account_uid(a) == uid)
        });
        let source_token = source_acc
            .as_ref()
            .and_then(|a| crate::modules::account::get_str(a, "access_token"));
        let target_token = crate::modules::account::get_str(target_acc, "access_token");
        report["settings"]["theme"] = crate::modules::ui_theme::sync_theme_for_switch(
            source_uid.as_deref(),
            &target_uid,
            source_token.as_deref(),
            target_token.as_deref(),
        );
    }
    Some(report)
}

/// 切号预览（dry_run）：统计「对齐 + 项目侧栏 + 会话瘦身」将发生的变更。
///
/// 与 [`post_close_sync`] 的区别：**不写库、不写快照、不写云端**。
/// `copy_session_ids` = 本次勾选要复制的会话（预览不真复制，只用于估算瘦身抵消）。
/// `keep_sids` = 统一保留名单（预览链路取自 autoLink dry_run 报告）。
pub fn preview_sync(
    target_acc: &Value,
    opts: &AlignOptions,
    copy_session_ids: &[String],
    keep_sids: &[String],
) -> Option<Value> {
    let mut dry_opts = opts.clone();
    dry_opts.dry_run = true;
    run_switch_sync(target_acc, &dry_opts, &[], copy_session_ids, keep_sids)
}

/// 切号「关进程之后」的数据后置同步：归属/文件对齐 + 界面主题跟随。
///
/// 从 `switch.rs` 内联块下沉到这里（本地专属文件），使 switch.rs 的本地改动保持最小。
/// 返回 `align_report`；主题跟随（原独立 theme_report）已并入报告的 `settings.theme`
/// 子项——主题即账号外观设置，归入「设置同步」呈现（2026-09-12 定稿）。
/// `protected_ids` = 本次切号刚手动复制到目标账号的会话 id：瘦身时必须跳过，
/// 否则「复制多条同项目会话 + 瘦身」会让复制体互相挤掉，用户只看到 1 条。
/// `keep_sids` = 统一保留名单（autoLink 计算的 keepTargetSids）。
pub fn post_close_sync(
    target_acc: &Value,
    opts: &AlignOptions,
    protected_ids: &[String],
    keep_sids: &[String],
) -> Option<Value> {
    let mut full_opts = opts.clone();
    full_opts.dry_run = false;
    run_switch_sync(target_acc, &full_opts, protected_ids, &[], keep_sids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 临时目录句柄：Drop 时整体清理。
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("wb-align-{tag}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    /// L4 settings：源补齐目标、secret 键跳过、无变化不写。
    #[test]
    fn align_settings_claw_fills_target_and_skips_secrets() {
        let t = TmpDir::new("claw");
        let f = t.0.join("settings.json");
        fs::write(
            &f,
            r#"{"claw":{"users":{
                "src":{"a":"1","nested":{"b":"2"},"apikey":"SRC-SECRET"},
                "dst":{"a":"OLD","apikey":"DST-SECRET"}
            }}}"#,
        )
        .unwrap();
        let r = align_settings_claw_users_at(&f, "src", "dst", false);
        assert_eq!(r["changed"], 2, "a 覆盖 + nested 补齐；apikey 跳过");
        let out: Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
        let dst = &out["claw"]["users"]["dst"];
        assert_eq!(dst["a"], "1");
        assert_eq!(dst["nested"]["b"], "2");
        assert_eq!(dst["apikey"], "DST-SECRET", "secret 键绝不被源覆盖");
        // 无变化：再跑一次 changed=0，且不重写文件
        let r2 = align_settings_claw_users_at(&f, "src", "dst", false);
        assert_eq!(r2["changed"], 0);
    }

    /// L4 storage：缺失复制、内容不同覆盖（带备份）、.bak 跳过、备份失败不覆盖。
    #[test]
    fn align_storage_dir_copies_with_backup_and_skips_bak() {
        let t = TmpDir::new("storage");
        let src = t.0.join("user-src");
        let dst = t.0.join("user-dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("new.txt"), b"new").unwrap();
        fs::write(src.join("same.txt"), b"same").unwrap();
        fs::write(dst.join("same.txt"), b"same").unwrap();
        fs::write(src.join("old.txt"), b"newer").unwrap();
        fs::write(dst.join("old.txt"), b"older").unwrap();
        fs::write(src.join("x.bak"), b"bak").unwrap();
        fs::write(src.join("credential"), b"secret").unwrap();

        let backups = t.0.join("backups");
        let mut copied = 0;
        let mut skipped = 0;
        let mut deferred = 0;
        let mut samples = Vec::new();
        align_storage_dir(
            &src,
            &dst,
            &backups,
            false,
            &mut copied,
            &mut skipped,
            &mut deferred,
            &mut samples,
        );
        assert_eq!(copied, 2, "new.txt 补齐 + old.txt 覆盖；same.txt 相同不复制");
        assert_eq!(skipped, 2, ".bak + credential 跳过");
        assert_eq!(fs::read(dst.join("new.txt")).unwrap(), b"new");
        assert_eq!(fs::read(dst.join("old.txt")).unwrap(), b"newer");
        // 覆盖前备份生成了 old.txt 的还原点（结构：<backup>/storage/<ts>/<user 目录>/old.txt）
        let ts_dir = backups.join("storage");
        let mut found_backup = false;
        for ts in fs::read_dir(&ts_dir).unwrap().flatten() {
            let p = ts.path().join("user-dst").join("old.txt");
            if p.is_file() && fs::read(&p).unwrap() == b"older" {
                found_backup = true;
            }
        }
        assert!(found_backup, "被覆盖文件必须有还原点");

        // 备份失败（backup_root 指向一个文件路径 → create_dir_all 失败）→ 宁可不覆盖
        let fail_root = t.0.join("not-a-dir");
        fs::write(&fail_root, b"x").unwrap();
        fs::write(dst.join("old.txt"), b"older").unwrap();
        let mut c2 = 0;
        let mut s2 = 0;
        let mut d2 = 0;
        let mut sam2 = Vec::new();
        align_storage_dir(
            &src,
            &dst,
            &fail_root,
            false,
            &mut c2,
            &mut s2,
            &mut d2,
            &mut sam2,
        );
        assert_eq!(fs::read(dst.join("old.txt")).unwrap(), b"older", "备份失败必须放弃覆盖");
        let _ = (c2, s2, d2, sam2);
    }

    /// 画像缓存：源→目标覆盖 + 备份生成；目标缺失视为不同。
    #[test]
    fn align_memory_profile_copies_with_backup() {
        let t = TmpDir::new("memory");
        let mem = t.0.join("memory");
        fs::create_dir_all(&mem).unwrap();
        fs::write(mem.join("src_memory.md"), b"v2").unwrap();
        fs::write(mem.join("dst_memory.md"), b"v1").unwrap();
        let backups = t.0.join("backups");

        let r = align_memory_profile_at(&mem, &backups, "src", "dst", false);
        assert_eq!(r["changed"], true);
        assert_eq!(fs::read(mem.join("dst_memory.md")).unwrap(), b"v2");
        // 还原点含旧值
        let mut found = false;
        for ts in fs::read_dir(backups.join("memory")).unwrap().flatten() {
            let p = ts.path().join("dst_memory.md");
            if p.is_file() && fs::read(&p).unwrap() == b"v1" {
                found = true;
            }
        }
        assert!(found, "画像覆盖前必须有备份");
        // 源缺失 → skipped
        let r2 = align_memory_profile_at(&mem, &backups, "ghost", "dst", false);
        assert_eq!(r2["skipped"], true);
    }

    /// L5 my-files：数组并集去重写回两个账号。
    #[test]
    fn merge_my_files_unions_lists_across_accounts() {
        let t = TmpDir::new("myfiles");
        let a = t.0.join("user-a-personal").join("scoped").join("s1");
        let b = t.0.join("user-b-personal").join("scoped").join("s2");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(
            a.join("my-files.json"),
            r#"{"favorites":["x","y"],"name":"A"}"#,
        )
        .unwrap();
        fs::write(
            b.join("my-files.json"),
            r#"{"favorites":["y","z"],"name":"B"}"#,
        )
        .unwrap();

        let r = merge_my_files_all_at(&t.0, &t.0.join("backups"), &["a".into(), "b".into()], false);
        assert_eq!(r["changed"], 2, "两份都与并集不同");
        for p in [a.join("my-files.json"), b.join("my-files.json")] {
            let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
            assert_eq!(v["favorites"], json!(["x", "y", "z"]), "并集去重");
        }
    }

    /// 轮转：每类只留最近 N 份时间戳目录；非时间戳条目不碰。
    #[test]
    fn prune_backups_keeps_latest_and_skips_non_ts_entries() {        let root = std::env::temp_dir().join(format!("wb-align-prune-{}", uuid::Uuid::new_v4()));
        let db = root.join("db");
        fs::create_dir_all(&db).unwrap();
        // 25 个时间戳目录（字典序 = 时间序，全部唯一）
        for i in 0..25 {
            let dir = db.join(format!("2026-08-{:02}T10-00-{:02}Z", 1 + i / 24, i % 60));
            fs::create_dir_all(&dir).unwrap();
        }
        // 非时间戳条目 + 文件条目：都不能被碰
        let custom = db.join("keep-me");
        fs::create_dir_all(&custom).unwrap();
        fs::write(db.join("a-file.txt"), b"x").unwrap();
        // 第二个类别目录：不足 keep 份，应整体不动
        let settings = root.join("settings");
        fs::create_dir_all(settings.join("2026-09-01T00-00-00Z")).unwrap();

        let removed = prune_backups_at(&root, 20);
        assert_eq!(removed, 5, "25 份删到 20 份");
        assert_eq!(
            fs::read_dir(&db).unwrap().flatten().count(),
            22, // 20 份时间戳 + keep-me + a-file.txt
            "非时间戳目录与文件必须保留"
        );
        assert!(custom.is_dir());
        assert!(settings.join("2026-09-01T00-00-00Z").is_dir());
        fs::remove_dir_all(&root).ok();
    }

    /// 五个开关任一开启即视为需要对齐（全关时整个流程跳过）。
    #[test]
    fn any_enabled_covers_all_five_switches() {
        let mut o = AlignOptions::default();
        assert!(!o.any_enabled(), "全关时应跳过对齐");
        o.align_automations = true;
        assert!(o.any_enabled());
        o = AlignOptions::default();
        o.align_files = true;
        assert!(o.any_enabled());
        o = AlignOptions::default();
        o.slim_keep = 1;
        assert!(o.any_enabled(), "slim_keep>0 也算开启");
        o.slim_keep = 0;
        assert!(!o.any_enabled());
    }

    /// 复制体与瘦身项目的交集：条数按会话计，项目数按 cwd 去重。
    #[test]
    fn copy_hits_counts_sessions_and_distinct_projects() {
        let slimmed: BTreeSet<&str> = ["/p/a", "/p/b"].into_iter().collect();
        let cwds = vec![
            "/p/a".to_string(),
            "/p/a".to_string(),
            "/p/c".to_string(),
        ];
        assert_eq!(copy_hits(&slimmed, &cwds), (2, 1));

        let none: Vec<String> = vec![];
        assert_eq!(copy_hits(&slimmed, &none), (0, 0));

        let both = vec!["/p/a".to_string(), "/p/b".to_string()];
        assert_eq!(copy_hits(&slimmed, &both), (2, 2));
    }

    /// 预览拿到「本次会搬过去的会话」就必须写 `copyPlanned`——共享（autoLink）路径
    /// 之前没把 planned sid 传进来，导致字段缺失、前端退化成没有条数的兜底提示。
    /// 空名单时**不写**字段，让前端据此判断「这次没有搬运」。
    #[test]
    fn annotate_copy_impact_writes_total_and_stays_absent_when_empty() {
        let mut report = json!({ "slim": { "groups": [] } });
        annotate_copy_impact(&mut report, &["s1".to_string(), "s2".to_string()]);
        assert_eq!(report["slim"]["copyPlanned"]["total"], 2);

        let mut none = json!({ "slim": { "groups": [] } });
        annotate_copy_impact(&mut none, &[]);
        assert!(
            none["slim"].get("copyPlanned").is_none(),
            "没有搬运时不该写 copyPlanned"
        );
    }
    #[test]
    fn secret_key_rules_match_multi_sync() {
        assert!(is_secret_key("botToken"));
        assert!(is_secret_key("channel_id"));
        assert!(is_secret_key("ownerId"));
        // multi_sync 原逻辑：输入键名不剥点、规则键才剥点，故 "master.key" 键名匹配不上
        // （文件名场景由 SECRET_FILENAMES 兜底）——保持口径一致
        assert!(!is_secret_key("master.key"));
        assert!(!is_secret_key("nickname"));
        assert!(!is_secret_key("themeColor"));
        assert!(is_secret_file("master.key"));
        assert!(!is_secret_file("expert.json"));
        assert!(is_merge_only("my-files.json"));
    }
}
