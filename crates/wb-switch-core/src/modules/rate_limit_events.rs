//! hook 信号消费：把 `~/.wb-switch/hook-events.jsonl` 里的 `Stop` / `FinalStop` payload
//! 变成限额台账条目。
//!
//! 数据流：hook 脚本 append payload（行级、O_APPEND）→ 本模块按**已读 offset** 逐行消费
//! （只有完整行才处理，写到一半的行留到下一轮）→ 识别限额文案（`QUOTA_MARKERS`）并取官方
//! 恢复时刻（`limits::parse_reset_at`）→ 归因（CLI：会话进程启动时刻不早于 `state.json`
//! 写入时刻即当前账号；WorkBuddy：登录态文件 / 会话表）→ 入账（内存 + 落盘）→ 由宿主 emit
//! `rate-limits-updated` 通知前端。
//!
//! 关键决定（实施时定的开放点）：
//! - **消费方式**：记 offset + 到 1 MB 轮转（`rename` 后再读旧 inode 的增量），不裁剪写入方
//!   正在追加的文件；轮转失败不动文件，宁可继续增长也不丢事件。
//! - **CLI 归因**：key 是**进程级快照**（切换只对新进程生效）。本轮起「活进程 key ==
//!   当前账号」是显式不变式（INV，见 `.trellis/spec/wb-switch-core/backend/model-rate-limits.md`）：
//!   手动切换先关进程再写 `state.json`，自动轮换只在没有存活会话时才切。因此归因只需
//!   「读当前账号 + 一个陈旧守卫」：取「该会话进程的启动时刻」（`sessions/<pid>.json` 的
//!   `startedAt`，兜底 transcript 首行），`startedAt ≥ state.json mtime` → 当前账号；
//!   早于 mtime ⇒ INV 被破坏（有进程持旧 key）⇒ 丢弃 + 告警；时刻取不到也丢弃。
//!   宁可少显示，不可显示错账号。
//! - **WorkBuddy 归因**：① 客户端登录态文件里的 uid → 账号库；② 兜底用会话 id 查 `sessions`
//!   表（与日志扫描同源）；都拿不到就丢弃（宁可少显示，不可显示错账号）。
//! - **轮询节奏**：1 秒一次 `stat`（未变则不做任何读取），hook 事件要求秒级可见；
//!   不引入 `notify` 依赖（本 crate 不新增依赖）。
//!
//! 事件 payload 不含账号 uid：CLI 的 key 归属由进程启动时刻决定，WorkBuddy 由触发当轮的
//! 登录态决定，因此必须在事件到达时归因，不能延后。

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::modules::account;
use crate::modules::codebuddy_cli;
use crate::modules::config::{atomic_write, home_dir, now_ms, store_dir};
use crate::modules::limits::{self, Resolved};
use crate::modules::rate_limit_hook;
use crate::modules::variant::WbVariant;

/// 后端状态文件名（offset + 已归因的限额条目）。
const STATE_FILE_NAME: &str = "rate_limit_state.json";

/// 消费到该字节数后轮转事件文件，避免无限增长（一次事件一行，常规使用远低于此）。
const COMPACT_AFTER_BYTES: u64 = 1024 * 1024;

/// watcher 轮询间隔：一次 `stat`，hook 事件要求秒级可见（AC1）。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// `transcript_model` 时间截断的容差：hook 写入时刻略晚于事件当轮最后的 transcript 行，
/// 给少量余量防写入顺序边界；切模型后的人工操作（看到 429 → 切模型 → 发送）远超该值。
const STALE_PAYLOAD_TOLERANCE_MS: i64 = 2_000;

/// 模型字段的未知哨兵值（与 IDE 解析同一套）：不得作为模型名展示。
const MODEL_SENTINELS: [&str; 3] = ["auto", "undefined", "null"];

/// transcript 首行上限：`session-meta` 很小；防止无换行的坏文件把数十 MB 读进内存。
const TRANSCRIPT_HEAD_LIMIT: u64 = 64 * 1024;

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 一条已归因的限额事件（落盘形态）。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEntry {
    account_id: String,
    model: Option<String>,
    reset_at: i64,
    first_seen_at: i64,
    hit_count: u32,
}

/// 后端持有的状态：事件文件已读偏移 + 当前有效的限额条目。
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct State {
    offset: u64,
    /// 最近一次入账的 hook 限额事件时刻（诊断用）。
    last_event_at: Option<i64>,
    events: Vec<StoredEntry>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn state_path() -> PathBuf {
    store_dir().join(STATE_FILE_NAME)
}

fn load_state(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<State>(&text).ok())
        .unwrap_or_default()
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(
        path,
        &serde_json::to_string_pretty(state).unwrap_or_default(),
    )
}

// ---------------------------------------------------------------------------
// payload 解析
// ---------------------------------------------------------------------------

/// hook payload 里本模块用到的字段（其余字段与版本差异一律忽略）。
#[derive(Deserialize)]
struct HookPayload {
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    last_assistant_message: Option<String>,
    /// hook 脚本写入事件行的时刻（毫秒，见 `rate_limit_hook::script_body`）。
    ///
    /// 事件可能被延迟消费（App 重启 / watcher 未跑），消费时 transcript 往往已包含
    /// 429 之后的新轮次；没有写入时刻就无法把「切模型后的模型行」排除在归因之外
    /// （2026-09-19 实证：429 在 15:46 被 deepseek 请求触发，16:07 才消费，此时主人
    /// 已切 glm 继续同一会话 ⇒ transcript 最后一条模型行是 glm ⇒ 假 chip）。
    /// 旧版脚本没有该字段（None）；0 是脚本的无效回退值，同样视为 None。
    #[serde(default, rename = "_hookTs")]
    hook_ts: Option<i64>,
}

/// 事件的客户端来源（由 `transcript_path` 前缀判定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookSource {
    /// CodeBuddy CLI（`~/.codebuddy/projects/…`）。
    Cli,
    /// WorkBuddy 客户端（`~/.workbuddy` / `~/.workbuddy-ai`）。
    WorkBuddy(WbVariant),
}

/// 一行 payload 里识别出的限额事件（来源判定需要环境，见 `source_of`）。
struct QuotaEvent {
    transcript_path: String,
    session_id: Option<String>,
    model: Option<String>,
    reset_at: i64,
    /// 事件行写入时刻（毫秒）；旧版脚本 / 无效值 → None。
    hook_ts: Option<i64>,
}

/// 一行 payload → 限额事件；非限额行、解析失败、缺恢复时刻统一返回 None（静默忽略）。
fn parse_quota_line(line: &str) -> Option<QuotaEvent> {
    let payload: HookPayload = serde_json::from_str(line.trim()).ok()?;
    // 限额文案在 `last_assistant_message` 里；`FinalStop` 等没有该字段的事件行直接忽略。
    let message = payload.last_assistant_message.as_deref()?;
    if !limits::QUOTA_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
    {
        return None;
    }
    // 取不到官方恢复时刻就不是可入账的限额事件（不猜时间）。
    let reset_at = limits::parse_reset_at(message)?;
    let model = payload
        .model
        .filter(|model| !MODEL_SENTINELS.contains(&model.as_str()))
        .filter(|model| !model.trim().is_empty());
    Some(QuotaEvent {
        transcript_path: payload.transcript_path?,
        session_id: payload.session_id,
        model,
        reset_at,
        // 0 是 hook 脚本取不到毫秒时的回退值，与缺失等价。
        hook_ts: payload.hook_ts.filter(|ts| *ts > 0),
    })
}

/// 由 `transcript_path` 前缀判定来源；不匹配（IDE 路径、空路径、未知客户端）返回 None。
fn source_of(transcript_path: &str, roots: &[(PathBuf, HookSource)]) -> Option<HookSource> {
    let path = Path::new(transcript_path);
    roots
        .iter()
        .find(|(root, _)| path.starts_with(root))
        .map(|(_, source)| *source)
}

// ---------------------------------------------------------------------------
// 归因
// ---------------------------------------------------------------------------

/// 归因所需的路径与账号库（显式传入，单测不得触碰真实用户数据）。
struct IngestContext<'a> {
    events: PathBuf,
    state_path: PathBuf,
    accounts: Vec<Value>,
    /// transcript 路径前缀 → 来源。
    source_roots: Vec<(PathBuf, HookSource)>,
    /// CLI 当前生效账号的 `activeAccountId` 状态文件。
    cli_state_path: PathBuf,
    /// CLI 运行中会话注册表目录（`~/.codebuddy/sessions`）：`sessionId → 进程启动时刻`。
    ///
    /// 路径由 `codebuddy_cli::sessions_dir()` 给出（唯一拼接点），这里只做注入以便单测。
    cli_sessions_dir: PathBuf,
    /// 两档位的客户端登录态文件。
    auth_file_paths: Vec<(WbVariant, PathBuf)>,
    /// 兜底：会话 id → 账号 id（默认实现查 `sessions` 表）。
    session_lookup: &'a dyn Fn(WbVariant, &str) -> Option<String>,
}

impl IngestContext<'_> {
    /// 真实运行环境（读真实账号库与真实状态文件）。
    fn real() -> IngestContext<'static> {
        let mut source_roots = vec![(home_dir().join(limits::CLI_DATA_DIR), HookSource::Cli)];
        source_roots.extend(
            WbVariant::ALL
                .into_iter()
                .map(|variant| (variant.data_root(), HookSource::WorkBuddy(variant))),
        );
        IngestContext {
            events: rate_limit_hook::events_path(),
            state_path: state_path(),
            accounts: account::load_accounts(),
            source_roots,
            cli_state_path: home_dir()
                .join(limits::CLI_ROTATE_DIR)
                .join(limits::CLI_STATE_FILE),
            cli_sessions_dir: codebuddy_cli::sessions_dir(),
            auth_file_paths: WbVariant::ALL
                .into_iter()
                .map(|variant| (variant, crate::modules::auth_file::auth_file_path(variant)))
                .collect(),
            session_lookup: &session_account_lookup,
        }
    }
}

/// 默认的会话兜底查询：与日志扫描共用 `limits::account_by_session`。
fn session_account_lookup(variant: WbVariant, session_id: &str) -> Option<String> {
    let sessions = std::collections::BTreeSet::from([session_id.to_string()]);
    limits::account_by_session(variant, &sessions)
        .get(session_id)
        .cloned()
}

/// 账号库里按 uid 找账号 id（uid 在库内唯一，不按档位过滤）。
fn account_id_by_uid(accounts: &[Value], uid: &str) -> Option<String> {
    accounts
        .iter()
        .find(|acc| account::get_str(acc, "uid").as_deref() == Some(uid))
        .and_then(|acc| account::get_str(acc, "id"))
}

/// 账号库里按 id 找账号（确认状态文件里的 id 仍然有效）。
fn account_exists(accounts: &[Value], account_id: &str) -> bool {
    accounts
        .iter()
        .any(|acc| account::get_str(acc, "id").as_deref() == Some(account_id))
}

/// CLI 轮换状态文件里的当前生效账号（账号库 id）。
fn cli_active_account_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: Value = serde_json::from_str(&text).ok()?;
    root.get("activeAccountId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// 客户端登录态文件里的 uid（`account.uid`，兼容根节点 `uid`）。
fn auth_file_uid(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: Value = serde_json::from_str(&text).ok()?;
    root.get("account")
        .and_then(|account| account.get("uid"))
        .and_then(Value::as_str)
        .or_else(|| root.get("uid").and_then(Value::as_str))
        .map(str::trim)
        .filter(|uid| !uid.is_empty())
        .map(str::to_string)
}

/// 事件所属会话「当前进程」的启动时刻（= 该进程取 key 的时刻）。
///
/// 读 `~/.codebuddy/sessions/*.json`，取 `sessionId` 匹配且 `startedAt ≤ now` 的**最大**
/// `startedAt`：跨进程接管后旧注册表文件可能残留，这个判据保证取「事件前最后一个持有者」。
/// 目录缺失 / 无匹配 / 文件坏 / `session_id` 为空 → None（调用方回落 transcript 首行）。
fn session_snapshot_time(dir: &Path, session_id: Option<&str>, now: i64) -> Option<i64> {
    let session_id = session_id.map(str::trim).filter(|id| !id.is_empty())?;
    let mut snapshot: Option<i64> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(root) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if root.get("sessionId").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        let Some(started_at) = root.get("startedAt").and_then(Value::as_i64) else {
            continue;
        };
        if started_at > now {
            continue;
        }
        snapshot = Some(snapshot.map_or(started_at, |current| current.max(started_at)));
    }
    snapshot
}

/// transcript 首行（`session-meta`）的会话创建时刻；只读第一行（文件可达数十 MB）。
///
/// 会话可跨进程复用，所以这是**兜底**：只用于注册表没有该会话时的近似。
fn session_created_at(transcript_path: &str) -> Option<i64> {
    let file = std::fs::File::open(transcript_path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file.take(TRANSCRIPT_HEAD_LIMIT))
        .read_line(&mut line)
        .ok()?;
    let root: Value = serde_json::from_str(&line).ok()?;
    root.get("timestamp").and_then(Value::as_i64)
}

/// CLI 状态文件快照：当前生效账号 id + 文件 mtime（毫秒）。
///
/// mtime 不依赖文件内 `updatedAt` 字段（缺该字段的旧文件也要能用）。
/// 文件缺失 / 非 JSON / 无 `activeAccountId` → None（该事件丢弃）。
fn cli_state_snapshot(path: &Path) -> Option<(String, Option<i64>)> {
    let current_id = cli_active_account_id(path)?;
    let mtime = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64);
    Some((current_id, mtime))
}

/// CLI 归因：按「该会话进程的启动时刻」（key 快照时刻）判断这把 key 属于谁。
///
/// 不变式（INV）：活着的 CLI 进程持有的 key == `state.json` 的 `activeAccountId`。
/// 由两条写路径共同维持——手动切换先关进程再写 state；自动轮换只在无存活会话时切。
/// 于是归因只需要一个陈旧守卫：
///
/// - `snap_at ≥ state.mtime`：进程在最后一次切换之后启动 ⇒ 当前账号就是它的 key；
/// - `snap_at < state.mtime`：进程在切换之前启动且（按 INV）本该已经被关掉 ⇒
///   INV 被破坏（第三方改写 state / 关闭失败 / 时钟异常）⇒ **丢弃 + 告警**，
///   绝不猜"也许是切换前那个账号"；
/// - `snap_at` 或 `mtime` 取不到 ⇒ 丢弃（宁可少显示，不可显示错账号）。
fn attribute_cli(event: &QuotaEvent, ctx: &IngestContext, now: i64) -> Option<String> {
    let snap_at = session_snapshot_time(&ctx.cli_sessions_dir, event.session_id.as_deref(), now)
        .or_else(|| session_created_at(&event.transcript_path))?;
    if snap_at > now {
        // 时钟异常（transcript 首行时间在未来）时不猜。
        return None;
    }
    let (current_id, mtime) = cli_state_snapshot(&ctx.cli_state_path)?;
    let Some(mtime) = mtime else {
        eprintln!(
            "[cli-attribution] 丢弃限额事件：无法读取 state.json 的写入时刻，无法判断归因是否成立"
        );
        return None;
    };
    if snap_at >= mtime {
        return account_exists(&ctx.accounts, &current_id).then_some(current_id);
    }
    eprintln!(
        "[cli-attribution] 丢弃限额事件：会话进程启动于 {snap_at}，早于 state.json 的写入时刻 {mtime}；\
         活进程持有的 key 与当前账号不一致（不变式被破坏）"
    );
    None
}

/// 归因：payload 不含账号 uid，只能靠客户端侧「谁在持 key」。
///
/// - CLI：会话进程启动时刻不早于 `state.json` 写入时刻 → 当前账号（见 `attribute_cli`）；
/// - WorkBuddy：① 登录态文件 uid → 账号库；② 会话 id 查 `sessions` 表兜底；
/// - 都拿不到 → 丢弃（宁可少显示，不可显示错账号）。
fn attribute(
    event: &QuotaEvent,
    source: HookSource,
    ctx: &IngestContext,
    now: i64,
) -> Option<String> {
    match source {
        HookSource::Cli => attribute_cli(event, ctx, now),
        HookSource::WorkBuddy(variant) => {
            let uid = ctx
                .auth_file_paths
                .iter()
                .find(|(candidate, _)| *candidate == variant)
                .and_then(|(_, path)| auth_file_uid(path));
            if let Some(account_id) = uid.and_then(|uid| account_id_by_uid(&ctx.accounts, &uid)) {
                return Some(account_id);
            }
            (ctx.session_lookup)(variant, event.session_id.as_deref()?)
        }
    }
}

// ---------------------------------------------------------------------------
// 消费
// ---------------------------------------------------------------------------

/// 事件文件是否还有未消费的字节（一次 `stat`；watcher 与查询路径共用）。
fn has_pending_bytes(events: &Path, offset: u64) -> bool {
    std::fs::metadata(events)
        .map(|metadata| metadata.len() != offset)
        .unwrap_or(false)
}

/// 只取到最后一个换行为止的完整行；返回（行集合，已消费字节数）。
///
/// 写到一半的行必须留给下一轮：客户端可能在任意时刻 append，半行 JSON 解析不出任何东西，
/// 直接消费会把这一行吃掉。
fn complete_lines(bytes: &[u8]) -> (Vec<String>, u64) {
    let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return (Vec::new(), 0);
    };
    let consumed = (last_newline + 1) as u64;
    let mut text = String::from_utf8_lossy(&bytes[..last_newline]).into_owned();
    // Windows PowerShell 5.1 的 `Add-Content -Encoding UTF8` **创建文件时**会写 BOM：
    // 首行带 `\u{feff}` 前缀，serde_json 解析必失败 ⇒ hook 装好后的**第一个**限额事件
    // 会被静默丢弃（2026-09-18 本机实证）。只在文本开头剥一次，开销可忽略。
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    let lines = text
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    (lines, consumed)
}

/// transcript 里 429 事件当轮的 `providerData.model`（实际服务模型）。
///
/// hook payload 的 `model` 是**会话 UI 选定模型**，与真正被限的服务模型可能不同
/// （2026-09-18 实证：UI 选 glm-5.3-flash，实际被限 hy4-preview-f，台账出现双 chip）。
/// 与日志通路「触发前同会话最近一次实际请求模型」同一语义：流式读 transcript，
/// 取最后一条带 `providerData.model` 的行；文件不可读 / 无模型行 → None（回落 payload）。
///
/// ⚠️ 事件可能被延迟消费（App 重启 / watcher 未跑，2026-09-19 实测延迟 21 分钟），
/// 届时 transcript 已包含 429 之后的新轮次 —— 主人切模型继续会话后，「最后一条模型行」
/// 就是切过去的模型，不再是被限模型。因此有 `hook_ts`（事件写入时刻）时做时间截断：
/// 只采信 `timestamp ≤ hook_ts + 容差` 的模型行，晚于事件的行与无法判时的行一律不采信
/// （宁可回落 payload.model —— 429 当轮 UI 模型与被限模型同轮，是安全兜底）。
/// 无 `hook_ts`（旧版脚本）保持旧行为：取最后一条，风险与历史一致。
fn transcript_model(transcript_path: &str, hook_ts: Option<i64>) -> Option<String> {
    let cutoff = hook_ts.map(|ts| ts + STALE_PAYLOAD_TOLERANCE_MS);
    let file = std::fs::File::open(transcript_path).ok()?;
    let mut last = None;
    for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
        let Ok(line) = line else { continue };
        // 子串预筛：transcript 可达数十 MB，避免逐行做完整 JSON 解析。
        if !line.contains("\"providerData\"") || !line.contains("\"model\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(model) = value
            .get("providerData")
            .and_then(|provider| provider.get("model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        if let Some(cutoff) = cutoff {
            match timestamp_ms(&value) {
                Some(ts) if ts <= cutoff => {}
                // 晚于事件（429 之后的新轮次）或无法判时的行都不可信。
                _ => continue,
            }
        }
        last = Some(model.to_string());
    }
    last
}

/// transcript 行的时间戳（毫秒）：数值优先，其次数字字符串。
fn timestamp_ms(value: &Value) -> Option<i64> {
    let ts = value.get("timestamp")?;
    ts.as_i64().or_else(|| ts.as_str().and_then(|s| s.parse().ok()))
}

/// 把一批行入账到状态；返回是否新增了条目。
fn ingest_lines(ctx: &IngestContext, state: &mut State, lines: &[String], now: i64) -> bool {
    let mut added = false;
    for line in lines {
        let Some(event) = parse_quota_line(line) else {
            continue;
        };
        let Some(source) = source_of(&event.transcript_path, &ctx.source_roots) else {
            continue;
        };
        let Some(account_id) = attribute(&event, source, ctx, now) else {
            continue;
        };
        // 模型归因：transcript 的实际服务模型优先（时间截断到事件当轮），payload 的
        // UI 选定模型只做兜底。
        let model = transcript_model(&event.transcript_path, event.hook_ts).or(event.model.clone());
        merge_entry(
            &mut state.events,
            StoredEntry {
                account_id,
                model,
                reset_at: event.reset_at,
                first_seen_at: now,
                hit_count: 1,
            },
        );
        added = true;
    }
    if added {
        state.last_event_at = Some(now);
    }
    added
}

/// 同一次事件（同账号 + 同模型 + 同恢复时刻）只保留一条，命中行数累加。
fn merge_entry(events: &mut Vec<StoredEntry>, entry: StoredEntry) {
    if let Some(existing) = events.iter_mut().find(|existing| {
        existing.account_id == entry.account_id
            && existing.model == entry.model
            && existing.reset_at == entry.reset_at
    }) {
        existing.hit_count += entry.hit_count;
        existing.first_seen_at = existing.first_seen_at.min(entry.first_seen_at);
        return;
    }
    events.push(entry);
}

/// 清掉已过官方恢复时刻的条目，并转成台账条目。
fn live_entries(state: &mut State, now: i64) -> Vec<Resolved> {
    state.events.retain(|entry| entry.reset_at > now);
    state
        .events
        .iter()
        .map(|entry| Resolved {
            account_id: entry.account_id.clone(),
            model: entry.model.clone(),
            reset_at: entry.reset_at,
            first_seen_at: entry.first_seen_at,
            hit_count: entry.hit_count,
        })
        .collect()
}

/// 消费新字节；返回 `None` 表示「没有新字节」，`Some(changed)` 表示已消费（changed = 有新条目）。
///
/// 偏移与条目一并持久化：进程重启后不重复入账，也不重放已消费的事件。
fn consume_with(ctx: &IngestContext, state: &mut State, now: i64) -> Option<bool> {
    let bytes = std::fs::read(&ctx.events).ok()?;
    // 文件被外部截断/替换（编辑器保存、用户手删）时从头再来一次：重复条目会被 merge 吞掉。
    let truncated = (bytes.len() as u64) < state.offset;
    if truncated {
        state.offset = 0;
        let _ = save_state(&ctx.state_path, state);
    }
    if (bytes.len() as u64) == state.offset {
        return None;
    }
    let (lines, consumed) = complete_lines(&bytes[state.offset as usize..]);
    let mut changed = false;
    if consumed > 0 {
        changed = ingest_lines(ctx, state, &lines, now);
        state.offset += consumed;
    }
    let rotated = compact_if_needed(ctx, state, now);
    state.events.retain(|entry| entry.reset_at > now);
    let _ = save_state(&ctx.state_path, state);
    Some(changed || rotated)
}

/// 消费到轮转阈值时轮转事件文件：改名后再读旧 inode 的尾部增量，最后删除。
///
/// 不裁剪写入方正在追加的文件：`rename` 是原子的，改名**之前**的 append 都落在旧 inode 里，
/// 会被这里重新读到；改名之后的 append 落在新文件里，由下一轮消费。失败就放弃本次轮转
/// （文件继续增长，不丢事件）。
fn compact_if_needed(ctx: &IngestContext, state: &mut State, now: i64) -> bool {
    if state.offset < COMPACT_AFTER_BYTES {
        return false;
    }
    // 还有未消费字节（例如末尾是写到一半的行）时不轮转，等下一轮读全。
    if has_pending_bytes(&ctx.events, state.offset) {
        return false;
    }
    let rotated = ctx.events.with_extension("jsonl.1");
    if std::fs::rename(&ctx.events, &rotated).is_err() {
        return false;
    }
    let Ok(bytes) = std::fs::read(&rotated) else {
        // 读不到就退回原名，保持「文件仍是事件源」的形态。
        let _ = std::fs::rename(&rotated, &ctx.events);
        return false;
    };
    let mut added = false;
    if (bytes.len() as u64) > state.offset {
        let (lines, _) = complete_lines(&bytes[state.offset as usize..]);
        added = ingest_lines(ctx, state, &lines, now);
    }
    let _ = std::fs::remove_file(&rotated);
    state.offset = 0;
    added
}

// ---------------------------------------------------------------------------
// 对外入口
// ---------------------------------------------------------------------------

/// 消费事件文件里的新事件；返回是否入账了新条目。
pub(crate) fn consume_pending() -> bool {
    let events = rate_limit_hook::events_path();
    let state_file = state_path();
    let mut guard = STATE.lock().unwrap();
    let state = guard.get_or_insert_with(|| load_state(&state_file));
    // 快速路径：一次 stat，没有新字节就直接返回（watcher 每秒走这里）。
    if !has_pending_bytes(&events, state.offset) {
        return false;
    }
    consume_with(&IngestContext::real(), state, now_ms()).unwrap_or(false)
}

/// 当前有效的 hook 条目（已归因；过期条目在这里清掉）。
///
/// 顺带消费一次待处理事件：即使 watcher 线程没跑（例如 webui 侧未启用），
/// 查询路径也能拿到最新状态。
pub(crate) fn hook_entries(now: i64) -> Vec<Resolved> {
    let _ = consume_pending();
    let mut guard = STATE.lock().unwrap();
    let state = guard.get_or_insert_with(|| load_state(&state_path()));
    live_entries(state, now)
}

/// 最近一次入账的 hook 限额事件时刻（诊断用）。
pub fn last_event_at() -> Option<i64> {
    {
        let guard = STATE.lock().unwrap();
        if let Some(state) = guard.as_ref() {
            return state.last_event_at;
        }
    }
    load_state(&state_path()).last_event_at
}

/// 启动 watcher：轮询事件文件，有新条目时回调（宿主据此 emit `rate-limits-updated`）。
///
/// 开关（`rate_limit_config.json`）只影响「是否通知前端」：信号照常入账，关闭期间
/// 发生的限额在重新打开开关后仍在（后端持有）。
pub fn spawn_watcher(emit: impl Fn() + Send + 'static) {
    std::thread::spawn(move || loop {
        if consume_pending() && rate_limit_enabled() {
            emit();
        }
        std::thread::sleep(POLL_INTERVAL);
    });
}

/// 限额监听开关（默认开启）。
pub(crate) fn rate_limit_enabled() -> bool {
    crate::modules::config::load_rate_limit_config()
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const UID_A: &str = "f0ae9eeb-8476-4ef5-9a3e-1d6339d545da";

    /// 本机实测的 WorkBuddy payload 骨架（`Stop` 行，字段已替换）。
    fn stop_payload(session: &str, model: &str, transcript: &Path) -> String {
        json!({
            "session_id": session,
            "transcript_path": transcript.to_string_lossy(),
            "hook_event_name": "Stop",
            "model": model,
            "last_assistant_message": "429 您的使用量已超出频率限制，将在 2026-09-17 17:59:27 UTC+8 重置，您也可以切换其他模型继续使用。",
        })
        .to_string()
    }

    /// 非限额轮次（正常回答）的 payload。
    fn ok_payload(session: &str, transcript: &Path) -> String {
        json!({
            "session_id": session,
            "transcript_path": transcript.to_string_lossy(),
            "hook_event_name": "Stop",
            "model": "deepseek-v4.1-flash",
            "last_assistant_message": "已完成。",
        })
        .to_string()
    }

    /// 带 `_hookTs`（hook 脚本写入时刻）的限额 payload（新版脚本形态）。
    fn stop_payload_at(session: &str, model: &str, transcript: &Path, hook_ts: i64) -> String {
        json!({
            "session_id": session,
            "transcript_path": transcript.to_string_lossy(),
            "hook_event_name": "Stop",
            "model": model,
            "_hookTs": hook_ts,
            "last_assistant_message": "429 您的使用量已超出频率限制，将在 2026-09-17 17:59:27 UTC+8 重置，您也可以切换其他模型继续使用。",
        })
        .to_string()
    }

    fn absent_lookup(_variant: WbVariant, _session: &str) -> Option<String> {
        None
    }

    /// 临时环境：所有路径都在 tempdir 下，绝不触碰真实用户配置。
    struct Fixture {
        root: PathBuf,
    }

    /// 测试结束自动清理 tempdir：不清理的话每轮 cargo test 留 20+ 个残留目录
    /// （2026-09-18 实证 127 个，且目录名与事件文件形态酷似「沙箱写入」，误导过排查方向）。
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "wb-switch-events-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).expect("临时目录");
            Self { root }
        }

        fn events(&self) -> PathBuf {
            self.root.join("hook-events.jsonl")
        }

        fn state_file(&self) -> PathBuf {
            self.root.join("rate_limit_state.json")
        }

        fn cli_state(&self) -> PathBuf {
            self.root.join("codebuddy-rotate.json")
        }

        fn cli_sessions_dir(&self) -> PathBuf {
            self.root.join("codebuddy-sessions")
        }

        fn auth_file(&self) -> PathBuf {
            self.root.join("workbuddy-desktop.info")
        }

        /// CLI transcript 路径（前缀与真实形态一致，只是根在 tempdir）。
        fn cli_transcript(&self, session: &str) -> PathBuf {
            self.root
                .join(".codebuddy/projects")
                .join(format!("{session}.jsonl"))
        }

        fn workbuddy_transcript(&self, session: &str) -> PathBuf {
            self.root
                .join(".workbuddy/projects")
                .join(format!("{session}.jsonl"))
        }

        fn context<'a>(
            &self,
            accounts: Vec<Value>,
            lookup: &'a dyn Fn(WbVariant, &str) -> Option<String>,
        ) -> IngestContext<'a> {
            let mut source_roots = vec![(self.root.join(".codebuddy"), HookSource::Cli)];
            source_roots.push((
                self.root.join(".workbuddy"),
                HookSource::WorkBuddy(WbVariant::Cn),
            ));
            source_roots.push((
                self.root.join(".workbuddy-ai"),
                HookSource::WorkBuddy(WbVariant::Ai),
            ));
            IngestContext {
                events: self.events(),
                state_path: self.state_file(),
                accounts,
                source_roots,
                cli_state_path: self.cli_state(),
                cli_sessions_dir: self.cli_sessions_dir(),
                auth_file_paths: vec![
                    (WbVariant::Cn, self.auth_file()),
                    (WbVariant::Ai, self.root.join("workbuddy-desktop-ai.info")),
                ],
                session_lookup: lookup,
            }
        }

        fn append(&self, lines: &[String]) {
            let mut text = std::fs::read_to_string(self.events()).unwrap_or_default();
            for line in lines {
                text.push_str(line);
                text.push('\n');
            }
            std::fs::write(self.events(), text).expect("追加事件");
        }

        fn state(&self) -> State {
            load_state(&self.state_file())
        }

        /// 预置一份 CLI 会话注册表（`sessionId → 进程启动时刻`）。
        fn write_session_registry(&self, pid: u32, session: &str, started_at: i64) {
            let dir = self.cli_sessions_dir();
            std::fs::create_dir_all(&dir).expect("会话注册表目录");
            std::fs::write(
                dir.join(format!("{pid}.json")),
                json!({ "pid": pid, "sessionId": session, "startedAt": started_at }).to_string(),
            )
            .expect("会话注册表");
        }

        /// 写 `state.json`（当前账号 + 其后由 `set_state_mtime` 钉住的写入时刻）。
        fn write_cli_state(&self, active_account_id: &str) {
            std::fs::write(
                self.cli_state(),
                json!({ "activeAccountId": active_account_id }).to_string(),
            )
            .expect("状态文件");
        }
    }

    /// 一条 CLI 限额事件（只用于直接调 `attribute` 的用例）。
    fn cli_quota_event(fixture: &Fixture, session: &str) -> QuotaEvent {
        QuotaEvent {
            transcript_path: fixture
                .cli_transcript(session)
                .to_string_lossy()
                .to_string(),
            session_id: Some(session.to_string()),
            model: Some("deepseek-v4.1-flash".to_string()),
            reset_at: 1_789_670_683_000,
            hook_ts: None,
        }
    }

    fn cli_account() -> Value {
        json!({ "id": "acc-cli", "uid": UID_A, "variant": "cn" })
    }

    #[test]
    fn parses_quota_lines_and_ignores_everything_else() {
        let transcript = Path::new("/x/.workbuddy/projects/s.jsonl");
        assert!(
            parse_quota_line(&ok_payload("s-1", transcript)).is_none(),
            "非限额轮次忽略"
        );
        assert!(parse_quota_line("{ not json").is_none(), "损坏行忽略");
        assert!(
            parse_quota_line(
                &json!({
                    "transcript_path": "/x/.workbuddy/projects/s.jsonl",
                    "last_assistant_message": "429 您的使用量已超出频率限制"
                })
                .to_string()
            )
            .is_none(),
            "只有文案没有恢复时刻 → 不入账"
        );
        assert!(
            parse_quota_line(
                &json!({ "last_assistant_message": "429 您的使用量已超出频率限制" }).to_string()
            )
            .is_none(),
            "缺 transcript_path → 不入账"
        );

        // 模型哨兵值不得作为模型名。
        let event = parse_quota_line(
            &json!({
                "transcript_path": "/x/.workbuddy/projects/s.jsonl",
                "session_id": "s-1",
                "model": "auto",
                "last_assistant_message": "429 您的使用量已超出频率限制，将在 2026-09-17 17:59:27 UTC+8 重置"
            })
            .to_string(),
        )
        .expect("限额行");
        assert_eq!(event.model, None);
        assert_eq!(event.session_id.as_deref(), Some("s-1"));
        assert_eq!(event.reset_at, 1_789_639_167_000);
    }

    #[test]
    fn source_is_decided_by_transcript_prefix() {
        let roots = vec![
            (PathBuf::from("/home/u/.codebuddy"), HookSource::Cli),
            (
                PathBuf::from("/home/u/.workbuddy"),
                HookSource::WorkBuddy(WbVariant::Cn),
            ),
            (
                PathBuf::from("/home/u/.workbuddy-ai"),
                HookSource::WorkBuddy(WbVariant::Ai),
            ),
        ];
        assert_eq!(
            source_of("/home/u/.codebuddy/projects/p/s.jsonl", &roots),
            Some(HookSource::Cli)
        );
        assert_eq!(
            source_of("/home/u/.workbuddy/projects/p/s.jsonl", &roots),
            Some(HookSource::WorkBuddy(WbVariant::Cn))
        );
        assert_eq!(
            source_of("/home/u/.workbuddy-ai/projects/p/s.jsonl", &roots),
            Some(HookSource::WorkBuddy(WbVariant::Ai))
        );
        // IDE 的 transcript 在 Application Support 下，不属于任一来源。
        assert_eq!(
            source_of(
                "/Users/u/Library/Application Support/CodeBuddyIDE/x.json",
                &roots
            ),
            None
        );
        assert_eq!(source_of("", &roots), None);
    }

    #[test]
    fn only_complete_lines_are_consumed() {
        let (lines, consumed) = complete_lines(b"{\"a\":1}\n{\"b\"");
        assert_eq!(lines, vec!["{\"a\":1}"]);
        assert_eq!(consumed, 8);
        // 一行都没有完整时不得消费任何字节。
        let (lines, consumed) = complete_lines(b"{\"a\"");
        assert!(lines.is_empty());
        assert_eq!(consumed, 0);
        // 空行忽略，但换行本身要计入已消费长度。
        let (lines, consumed) = complete_lines(b"\n\n{\"a\":1}\n");
        assert_eq!(lines, vec!["{\"a\":1}"]);
        assert_eq!(consumed, 10);
    }

    /// 事件文件首行带 UTF-8 BOM（Windows PowerShell 5.1 `Add-Content -Encoding UTF8`
    /// 建文件时的实测行为）不得吞掉第一个限额事件。
    #[test]
    fn utf8_bom_on_the_first_line_does_not_kill_the_first_event() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-bom";
        fixture.write_session_registry(4242, session, 500);
        std::fs::write(
            fixture.events(),
            format!("\u{feff}{}\n", stop_payload(session, "hy3", &fixture.cli_transcript(session))),
        )
        .expect("带 BOM 的事件文件");

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events.len(), 1, "BOM 前缀不得让首行解析失败");
    }

    /// 模型归因：transcript 的实际服务模型（`providerData.model`）优先于 payload 的
    /// UI 选定模型（2026-09-18 实证：UI 选 glm-5.3-flash，实际被限 hy4-preview-f）。
    #[test]
    fn transcript_serving_model_wins_over_the_payload_ui_model() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-model";
        let transcript = fixture.cli_transcript(session);
        std::fs::create_dir_all(transcript.parent().expect("父目录")).expect("transcript 目录");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n{}\n",
                json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }),
                json!({ "type": "reasoning", "providerData": { "model": "glm-5.3-flash", "reasoning": "x" } }),
                // 最后一条实际服务模型是 hy4-preview-f（中途换过模型也要取最新的）。
                json!({ "type": "function_call", "providerData": { "model": "hy4-preview-f" } })
            ),
        )
        .expect("写 transcript");
        fixture.write_session_registry(4242, session, 500);
        std::fs::write(fixture.events(), format!("{}\n", stop_payload(session, "glm-5.3-flash", &transcript)))
            .expect("事件文件");

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events.len(), 1);
        assert_eq!(
            state.events[0].model.as_deref(),
            Some("hy4-preview-f"),
            "必须取 transcript 的实际服务模型，不是 payload 的 UI 模型"
        );
    }

    /// transcript 不可读 / 无模型行 → 回落 payload 的模型，不入成「未知」。
    #[test]
    fn payload_model_is_the_fallback_when_transcript_has_no_model_rows() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-fallback";
        // transcript 存在但没有 providerData.model 行。
        let transcript = fixture.cli_transcript(session);
        std::fs::create_dir_all(transcript.parent().expect("父目录")).expect("transcript 目录");
        std::fs::write(&transcript, json!({ "type": "message", "role": "user" }).to_string())
            .expect("写 transcript");
        fixture.write_session_registry(4242, session, 500);
        std::fs::write(fixture.events(), format!("{}\n", stop_payload(session, "hy3", &transcript))).expect("事件文件");

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].model.as_deref(), Some("hy3"));
    }

    /// 2026-09-19 glm 假 chip 实证回归：事件被延迟消费（429 在 15:46、消费在 16:07），
    /// 期间主人切模型继续同一会话 ⇒ transcript 已有「事件之后」的模型行。带 `_hookTs`
    /// 的事件必须按写入时刻截断，归因到事件当轮的模型（切之前的），不是切之后的。
    #[test]
    fn stale_transcript_rows_after_the_hook_timestamp_are_ignored() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-stale";
        let transcript = fixture.cli_transcript(session);
        std::fs::create_dir_all(transcript.parent().expect("父目录")).expect("transcript 目录");
        let hook_ts = 1_000_000_i64;
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n{}\n",
                // 事件当轮：实际服务模型 deepseek（429 请求本身无响应行，最后一条是它）。
                json!({ "type": "reasoning", "timestamp": hook_ts - 1_000, "providerData": { "model": "deepseek-v4.1-flash" } }),
                // 事件之后主人切 glm 继续会话（延迟消费时已在文件里）。
                json!({ "type": "message", "timestamp": hook_ts + 60_000, "providerData": { "model": "glm-5.3-flash" } }),
                // 事件之后无时间戳的行同样不可信。
                json!({ "type": "function_call", "providerData": { "model": "kimi-k3-1" } })
            ),
        )
        .expect("写 transcript");
        fixture.write_session_registry(4242, session, 500);
        std::fs::write(
            fixture.events(),
            format!(
                "{}\n",
                stop_payload_at(session, "deepseek-v4.1-flash", &transcript, hook_ts)
            ),
        )
        .expect("事件文件");

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, hook_ts + 120_000), Some(true));
        assert_eq!(state.events.len(), 1);
        assert_eq!(
            state.events[0].model.as_deref(),
            Some("deepseek-v4.1-flash"),
            "事件之后的新轮次模型行必须被时间截断排除"
        );
    }

    /// 同一场景但 `_hookTs` 为 0（脚本取不到毫秒的回退值）：视为无时间锚，保持旧行为。
    #[test]
    fn zero_hook_timestamp_falls_back_to_the_last_model_row() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-zero-ts";
        let transcript = fixture.cli_transcript(session);
        std::fs::create_dir_all(transcript.parent().expect("父目录")).expect("transcript 目录");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({ "type": "reasoning", "providerData": { "model": "deepseek-v4.1-flash" } }),
                json!({ "type": "message", "providerData": { "model": "glm-5.3-flash" } })
            ),
        )
        .expect("写 transcript");
        fixture.write_session_registry(4242, session, 500);
        std::fs::write(
            fixture.events(),
            format!(
                "{}\n",
                stop_payload_at(session, "glm-5.3-flash", &transcript, 0)
            ),
        )
        .expect("事件文件");

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 2_000), Some(true));
        assert_eq!(state.events.len(), 1);
        assert_eq!(
            state.events[0].model.as_deref(),
            Some("glm-5.3-flash"),
            "无有效时间锚时保持旧行为：取最后一条模型行"
        );
    }

    #[test]
    fn cli_events_are_attributed_to_the_active_account_and_persisted() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        // 写入时刻钉在会话启动之前 ⇒ 该进程取到的 key 就是当前账号。
        set_state_mtime(&fixture, 1);
        let session = "01a0ad53-0c95-7767-bd6a-d43356edb644";
        fixture.write_session_registry(4242, session, 500);
        fixture.append(&[stop_payload(
            session,
            "deepseek-v4.1-flash",
            &fixture.cli_transcript(session),
        )]);

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].account_id, "acc-cli");
        assert_eq!(
            state.events[0].model.as_deref(),
            Some("deepseek-v4.1-flash")
        );
        assert_eq!(state.last_event_at, Some(1_000));
        assert!(state.offset > 0);
        assert_eq!(live_entries(&mut state, 1_100).len(), 1);

        // 重复消费同一文件：偏移已推进 → 不重复入账。
        let mut reloaded = fixture.state();
        assert_eq!(consume_with(&ctx, &mut reloaded, 2_000), None);
        assert_eq!(reloaded.events.len(), 1);

        // 同一次事件的重复书写（重试轮）：hitCount 累加、不新增条目。
        fixture.append(&[stop_payload(
            session,
            "deepseek-v4.1-flash",
            &fixture.cli_transcript(session),
        )]);
        let mut again = fixture.state();
        assert_eq!(consume_with(&ctx, &mut again, 3_000), Some(true));
        assert_eq!(again.events.len(), 1);
        assert_eq!(again.events[0].hit_count, 2);
    }

    /// 本案时间线（research/2026-09-17-misattribution-evidence.md，本机实测，CST）。
    const CLI_SESSION: &str = "01a0aeb3-1f70-7d85-b356-0faa85650c1f";
    /// 17:29:29 旧进程 pid=9312 启动（此后取到张佳的 key）。
    const OLD_PROCESS_STARTED_AT: i64 = 1_789_637_369_000;
    /// 18:44:02 新进程 pid=60705 启动（取到 wkbdtest 的 key）。
    const NEW_PROCESS_STARTED_AT: i64 = 1_789_641_842_256;
    /// 18:00:30 state.json 切到 wkbdtest。
    const SWITCHED_TO_WKBDTEST_AT: i64 = 1_789_639_230_009;
    /// 18:43:56 旧进程触发 429。
    const EVENT_AT: i64 = 1_789_639_436_000;

    /// `state.json` 的最后写入时刻（毫秒）：构造「快照早于 / 等于 state 写入」用。
    fn state_mtime_ms(fixture: &Fixture) -> i64 {
        std::fs::metadata(fixture.cli_state())
            .and_then(|metadata| metadata.modified())
            .expect("状态文件 mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("mtime 晚于 UNIX_EPOCH")
            .as_millis() as i64
    }

    /// 把 state.json 的 mtime 钉到指定毫秒，避免用例依赖墙钟。
    fn set_state_mtime(fixture: &Fixture, ms: i64) {
        let file = std::fs::File::options()
            .write(true)
            .open(fixture.cli_state())
            .expect("打开状态文件以设置 mtime");
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64))
            .expect("设置状态文件 mtime");
    }

    /// 同一会话先后被两个进程持有：取「事件前最后一个持有者」的启动时刻。
    #[test]
    fn session_snapshot_prefers_the_latest_holder_before_the_event() {
        let fixture = Fixture::new();
        fixture.write_session_registry(9312, CLI_SESSION, OLD_PROCESS_STARTED_AT);
        fixture.write_session_registry(60705, CLI_SESSION, NEW_PROCESS_STARTED_AT);
        let dir = fixture.cli_sessions_dir();
        assert_eq!(
            session_snapshot_time(&dir, Some(CLI_SESSION), EVENT_AT),
            Some(OLD_PROCESS_STARTED_AT),
            "18:43:56 的事件：60705 尚未启动，取 9312"
        );
        assert_eq!(
            session_snapshot_time(&dir, Some(CLI_SESSION), NEW_PROCESS_STARTED_AT + 1_000),
            Some(NEW_PROCESS_STARTED_AT),
            "接管后的事件取 60705"
        );
        // 不匹配 / 缺 sessionId / 目录缺失 → 无快照。
        assert_eq!(session_snapshot_time(&dir, Some("other"), EVENT_AT), None);
        assert_eq!(session_snapshot_time(&dir, None, EVENT_AT), None);
        assert_eq!(
            session_snapshot_time(&fixture.root.join("missing"), Some(CLI_SESSION), EVENT_AT),
            None
        );
        // 坏文件跳过，不影响其它候选。
        std::fs::write(dir.join("broken.json"), "not-json").expect("坏注册表");
        assert_eq!(
            session_snapshot_time(&dir, Some(CLI_SESSION), EVENT_AT),
            Some(OLD_PROCESS_STARTED_AT)
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 注册表缺失（或坏文件被跳过）→ 兜底 transcript 首行的会话创建时刻。
    #[test]
    fn session_snapshot_falls_back_to_the_transcript_first_line() {
        let fixture = Fixture::new();
        let transcript = fixture.cli_transcript(CLI_SESSION);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session-meta",
                    "sessionId": CLI_SESSION,
                    "timestamp": OLD_PROCESS_STARTED_AT + 769
                }),
                json!({ "type": "user" })
            ),
        )
        .unwrap();
        fixture.write_cli_state("acc-cli");
        // 注册表目录在但文件坏：跳过，仍走 transcript。
        std::fs::create_dir_all(fixture.cli_sessions_dir()).expect("会话注册表目录");
        std::fs::write(fixture.cli_sessions_dir().join("9312.json"), "{").expect("坏注册表");
        // state 的写入时刻早于会话创建时刻 ⇒ 这个进程取到的就是当前账号。
        set_state_mtime(&fixture, OLD_PROCESS_STARTED_AT);
        let ctx = fixture.context(vec![cli_account()], &absent_lookup);
        assert_eq!(
            session_snapshot_time(&fixture.cli_sessions_dir(), Some(CLI_SESSION), EVENT_AT),
            None,
            "坏注册表不提供快照时刻"
        );
        assert_eq!(
            session_created_at(&transcript.to_string_lossy()),
            Some(OLD_PROCESS_STARTED_AT + 769)
        );
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                EVENT_AT
            )
            .as_deref(),
            Some("acc-cli")
        );
        // transcript 也不可读 → 丢弃。
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, "missing-transcript"),
                HookSource::Cli,
                &ctx,
                EVENT_AT
            ),
            None
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 切换之后仍有旧进程触发限额：按不变式这种进程本该已经被关闭 ⇒ 丢弃（不猜账号）。
    #[test]
    fn old_process_event_after_switch_is_dropped() {
        let fixture = Fixture::new();
        fixture.write_session_registry(9312, CLI_SESSION, OLD_PROCESS_STARTED_AT);
        fixture.write_cli_state("acc-wkbdtest");
        // state 在旧进程启动之后才被改写：它持有的是切换前的 key。
        set_state_mtime(&fixture, SWITCHED_TO_WKBDTEST_AT);
        let ctx = fixture.context(vec![json!({ "id": "acc-wkbdtest" })], &absent_lookup);
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                EVENT_AT
            ),
            None,
            "不变式被破坏时应丢弃，不得猜切换前的账号"
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 快照时刻恰等于 state 写入时刻 ⇒ 进程在最后一次切换之后启动，归当前账号。
    #[test]
    fn new_process_event_after_the_last_switch_uses_the_state_account() {
        let fixture = Fixture::new();
        fixture.write_cli_state("acc-wkbdtest");
        let mtime = state_mtime_ms(&fixture);
        fixture.write_session_registry(60705, CLI_SESSION, mtime);
        let ctx = fixture.context(vec![json!({ "id": "acc-wkbdtest" })], &absent_lookup);
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                mtime + 1_000
            )
            .as_deref(),
            Some("acc-wkbdtest")
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 快照时刻比 state 写入时刻早 1ms ⇒ 归因边界收紧到「丢弃」。
    #[test]
    fn snapshot_one_millisecond_before_the_state_write_is_dropped() {
        let fixture = Fixture::new();
        fixture.write_cli_state("acc-wkbdtest");
        let mtime = state_mtime_ms(&fixture);
        fixture.write_session_registry(9312, CLI_SESSION, mtime - 1);
        let ctx = fixture.context(vec![json!({ "id": "acc-wkbdtest" })], &absent_lookup);
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                mtime + 1_000
            ),
            None,
            "无法区分旧进程/新进程接管时不得猜账号"
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 快照时刻不可得（注册表与 transcript 都不可用）→ 丢弃，即使 state.json 可读。
    #[test]
    fn missing_snapshot_evidence_is_dropped() {
        let fixture = Fixture::new();
        fixture.write_cli_state("acc-cli");
        let ctx = fixture.context(vec![cli_account()], &absent_lookup);
        // 注册表目录不存在 + transcript 不存在 + session_id 缺失。
        let mut event = cli_quota_event(&fixture, CLI_SESSION);
        event.session_id = None;
        assert_eq!(attribute(&event, HookSource::Cli, &ctx, 10_000), None);

        // state.json 缺失时同样丢弃（即使快照时刻可得）。
        let fixture = Fixture::new();
        fixture.write_session_registry(9312, CLI_SESSION, 500);
        let ctx = fixture.context(vec![cli_account()], &absent_lookup);
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                10_000
            ),
            None
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 当前账号已被删除 → 丢弃（不得改判到任何别的账号）。
    #[test]
    fn deleted_current_account_is_dropped() {
        let fixture = Fixture::new();
        fixture.write_session_registry(60705, CLI_SESSION, NEW_PROCESS_STARTED_AT);
        fixture.write_cli_state("acc-deleted");
        set_state_mtime(&fixture, SWITCHED_TO_WKBDTEST_AT);
        let ctx = fixture.context(vec![json!({ "id": "acc-zhangjia" })], &absent_lookup);
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                NEW_PROCESS_STARTED_AT + 1_000
            ),
            None,
            "当前账号不在账号库时丢弃，不得改判到账号库里仍存在的旧账号"
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 时钟异常（快照时刻晚于事件时刻）时不猜。
    #[test]
    fn future_snapshot_time_is_dropped() {
        let fixture = Fixture::new();
        let transcript = fixture.cli_transcript(CLI_SESSION);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, format!("{}\n", json!({ "timestamp": 20_000 }))).unwrap();
        fixture.write_cli_state("acc-cli");
        let ctx = fixture.context(vec![cli_account()], &absent_lookup);
        assert_eq!(
            attribute(
                &cli_quota_event(&fixture, CLI_SESSION),
                HookSource::Cli,
                &ctx,
                10_000
            ),
            None
        );
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    /// 归因到的账号不在账号库（用户删了账号）→ 丢弃，不误归。
    #[test]
    fn events_are_dropped_when_attribution_fails() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![json!({ "id": "acc-other" })], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-1";
        fixture.write_session_registry(4242, session, 500);
        fixture.append(&[stop_payload(
            session,
            "hy3",
            &fixture.cli_transcript(session),
        )]);

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(false));
        assert!(state.events.is_empty());
        assert_eq!(state.last_event_at, None, "没有入账就不算事件时刻");
        assert!(state.offset > 0, "已消费的行不得反复重读");
    }

    #[test]
    fn workbuddy_events_use_the_auth_file_uid_then_the_session_fallback() {
        let fixture = Fixture::new();
        // ① 登录态文件给了 uid → 直接命中账号库。
        std::fs::write(
            fixture.auth_file(),
            json!({ "account": { "uid": UID_A }, "auth": { "accessToken": "t" } }).to_string(),
        )
        .expect("登录态文件");
        let accounts = vec![
            json!({ "id": "acc-wb", "uid": UID_A, "variant": "cn" }),
            json!({ "id": "acc-session", "uid": "other-uid", "variant": "cn" }),
        ];
        let lookup = absent_lookup;
        let ctx = fixture.context(accounts.clone(), &lookup);
        fixture.append(&[stop_payload(
            "s-wb",
            "hy3",
            &fixture.workbuddy_transcript("s-wb"),
        )]);
        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events[0].account_id, "acc-wb");

        // ② 登录态文件缺失 → 会话兜底；③ 兜底也拿不到 → 丢弃。
        std::fs::remove_file(fixture.auth_file()).expect("移除登录态文件");
        let fallback = |variant: WbVariant, session: &str| {
            (variant == WbVariant::Cn && session == "s-fallback").then(|| "acc-session".to_string())
        };
        let ctx = fixture.context(accounts, &fallback);
        fixture.append(&[
            stop_payload(
                "s-fallback",
                "hy3",
                &fixture.workbuddy_transcript("s-fallback"),
            ),
            stop_payload(
                "s-unknown",
                "hy3",
                &fixture.workbuddy_transcript("s-unknown"),
            ),
        ]);
        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events.len(), 1, "只有兜底命中的那条入账");
        assert_eq!(state.events[0].account_id, "acc-session");
    }

    #[test]
    fn entries_expire_at_their_reset_time() {
        let mut state = State {
            offset: 0,
            last_event_at: None,
            events: vec![StoredEntry {
                account_id: "a".to_string(),
                model: Some("hy3".to_string()),
                reset_at: 1_500,
                first_seen_at: 1_000,
                hit_count: 1,
            }],
        };
        assert_eq!(
            live_entries(&mut state, 1_400).len(),
            1,
            "未到恢复时刻仍保留"
        );
        assert!(
            live_entries(&mut state, 1_500).is_empty(),
            "到恢复时刻即清掉"
        );
    }

    #[test]
    fn truncated_events_file_resets_the_offset_without_duplicating_entries() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-1";
        fixture.write_session_registry(4242, session, 500);
        fixture.append(&[stop_payload(
            session,
            "hy3",
            &fixture.cli_transcript(session),
        )]);
        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert!(state.offset > 0);

        // 用户/编辑器把文件截短（或轮转掉）→ 偏移必须回退到可读范围，且不产生重复条目。
        std::fs::write(fixture.events(), "").expect("清空事件文件");
        assert_eq!(consume_with(&ctx, &mut state, 2_000), None);
        assert_eq!(state.offset, 0);
        assert_eq!(state.events.len(), 1, "同一事件不得重复入账");
    }

    #[test]
    fn large_event_files_are_rotated_without_losing_the_tail() {
        let fixture = Fixture::new();
        let lookup = absent_lookup;
        let ctx = fixture.context(vec![cli_account()], &lookup);
        fixture.write_cli_state("acc-cli");
        set_state_mtime(&fixture, 1);
        let session = "s-1";
        fixture.write_session_registry(4242, session, 500);
        let payload = stop_payload(session, "hy3", &fixture.cli_transcript(session));
        // 先把文件撑到轮转阈值，再在末尾追一条事件。
        let filler = "x".repeat(COMPACT_AFTER_BYTES as usize);
        std::fs::write(fixture.events(), format!("{filler}\n{payload}\n")).expect("大事件文件");

        let mut state = State::default();
        assert_eq!(consume_with(&ctx, &mut state, 1_000), Some(true));
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.offset, 0, "轮转后偏移归零");
        assert!(!fixture.events().exists(), "旧文件已轮转删除");
        assert!(
            !ctx.events.with_extension("jsonl.1").exists(),
            "临时轮转文件已清理"
        );
    }

    #[test]
    fn watcher_state_path_lives_in_the_store_directory() {
        assert!(state_path().ends_with(STATE_FILE_NAME));
    }
}
