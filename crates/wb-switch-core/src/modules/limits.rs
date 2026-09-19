//! 模型限额台账：从**本机日志**与**客户端 hook 信号**还原「哪个账号的哪个模型被限流、
//! 官方给出的恢复时刻」。
//!
//! 两条通路合并（`get_rate_limits()`）：
//! - **hook 通路**（`rate_limit_events.rs`）：CLI 与 WorkBuddy 两档位的 `Stop` / `FinalStop`
//!   payload 里直接带限额文案与模型，实时归因、秒级可见；这些来源已接 hook 时不扫日志。
//! - **日志扫描**（本模块）：两个 CodeBuddy IDE 只走这条（受 `scanIdeLogs` 开关约束）；CLI /
//!   WorkBuddy 只在**该处没接上 hook**（未安装 / 条目被删 / 配置写坏）时按来源逐个回退。
//!   扫描按 5 分钟节流并缓存，`scannedAt` 是最近一次真实扫描的时刻。
//!
//! 扫描范围逐来源判定：**客户端数据根不存在**（本机没装）的来源既不安装 hook 也不扫日志。
//!
//! 不新增任何网络请求，也不解析客户端 UI 文案。日志窗口固定为最近 2 天（WorkBuddy 两档位
//! 与 CLI 是最近 2 个日期目录，两个 IDE 按文件 mtime 收窗）；重置时刻直接采用日志原文 / payload
//! 原文给出的官方值，不自建限流窗口模型。
//!
//! 五个来源各扫一遍（PRD D1：插件宿主共享根 `CodeBuddyExtension/Logs/CodeBuddyIDE/` 与 CN
//! IDE 真身重复记录同一次 429，**不扫**）：
//!
//! | 来源 | 日志根 | 格式 | 枚举 | 归因 |
//! |---|---|---|---|---|
//! | WorkBuddy 国内版 / 国际版 | `~/.workbuddy*/logs` | `WorkBuddy` | 日期目录 | `sessions` 表 |
//! | CodeBuddy CLI | `~/.codebuddy/logs` | `WorkBuddy` | 日期目录 | 日志内鉴权 uid → 轮换状态文件 |
//! | CodeBuddy IDE | `<data_dir>/logs` | `Ide` | 会话目录 + mtime | 日志内鉴权 uid → IDE 状态文件 |
//! | CodeBuddy CN IDE | `<data_dir>/logs` | `Ide` | 会话目录 + mtime | 同上 |
//!
//! 一次返回全部账号的当前受限状态——扫描本身就是全局的，按账号调用会把同一份日志扫 N 遍。
//!
//! 数据流：目录枚举 → 字节级粗筛 → 命中才逐行解码 → 事件与模型解析 → 两步去重 →
//! 账号归因 → 按 (账号, 模型) 聚合 → 过滤掉已过官方重置时刻的条目。

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use chrono::{FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::{json, Value};

use crate::modules::account;
use crate::modules::config::{home_dir, now_ms, store_dir};
use crate::modules::rate_limit_hook::{hook_marker, needs_log_scan, SETTINGS_FILE_NAME};
use crate::modules::session::{open_db, table_exists, workbuddy_db_path};
use crate::modules::variant::WbVariant;
use crate::modules::vscode_cn_inject::{codebuddy_ide_data_dir, CodeBuddyIdeFlavor};

/// 日志根目录名（档位/IDE 数据根下）。
const LOG_DIR_NAME: &str = "logs";

/// 扫描窗口：五个来源一致，取文件 mtime 在最近 2 天内的日志（固定，不做可配置）。
///
/// 判据用 mtime 而非目录名/会话名：日志文件当天创建后会写到**次日凌晨**（本机实测
/// `2026-09-15/` 的两个文件写到 9/16 07:59），按「最近 N 个目录」会漏掉跨天那批记录。
///
/// 本机实测（release）五个来源全量扫描约 115 ms：CLI 最重（候选 43MB / 7 文件，约 66 ms，
/// 因为 CLI 的业务日志是完整会话记录）、CN IDE 约 12 ms、WorkBuddy 两档位合计约 7 ms。
/// 仍远快于前端 60s 轮询间隔，因此**不建**增量索引或本地缓存层。
const WINDOW_DAYS: usize = 2;

/// 一天的毫秒数（mtime 收窗用）。
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// CodeBuddy CLI 数据根目录名（三平台同构）。
pub(crate) const CLI_DATA_DIR: &str = ".codebuddy";

/// CodeBuddy CLI 轮换状态目录名（`rotate.rs` / `codebuddy_cli.rs` 的写入方）。
pub(crate) const CLI_ROTATE_DIR: &str = ".codebuddy-rotate";

/// 状态文件名（`rotate.rs` / `codebuddy_ide.rs` / `codebuddy_cn_ide.rs` 的写入方）。
///
/// 与写入方保持同名：这里只读回落的 `activeAccountId`（账号库的 `id`，不是 uid）。
pub(crate) const CLI_STATE_FILE: &str = "state.json";
const IDE_STATE_FILE: &str = "codebuddy_ide.json";
const CN_IDE_STATE_FILE: &str = "codebuddy_cn_ide.json";

/// IDE 插件在 `exthost/` 下的日志目录名（两个 IDE 一致）。
///
/// 这是**唯一稳定**的过滤条件：文件名不固定（CN 侧 `腾讯云代码助手.log`，含 `.1.log` 轮转；
/// 国际版侧还出现过 `Tencent Cloud CodeBuddy.log`），不加该过滤会把候选集从 18MB 涨到 43MB。
const IDE_LOG_DIR_NAME: &str = "Tencent-Cloud.coding-copilot";

/// CLI 启动鉴权行标记：`[FirstScreen] [AuthDoInitProbe] stage=… uid=<uuid>`。
const CLI_AUTH_MARKER: &str = "[AuthDoInitProbe]";

/// IDE 会话鉴权行标记：`[PulseServiceLifecycle] Auth session changed: …, uid=<uuid>`。
const IDE_AUTH_MARKER: &str = "[PulseServiceLifecycle] Auth session changed:";

/// IDE 会话级模型标记：`[ModelSelection] conversationId=<conv32>, mode=…, modelId=<m>`
/// 与 `[AcpAgent:<conv32>] … modelId=<m>` 共用同一个字段名（见 `ide_model_id`）。
const IDE_MODEL_FIELD: &str = "modelId=";

/// IDE 模型字段上的未知哨兵值：`auto` 是「会话尚未选定模型」（`[AcpAgent:…] Model cache
/// synced … source=new-session` 会出现），`undefined` / `null` 是 JS 侧字段缺失的写法。
/// 都按未知处理——卡片绝不能显示 `auto` / `undefined`。
const IDE_MODEL_UNKNOWN: [&str; 3] = ["auto", "undefined", "null"];

/// IDE 模型兜底行标记：`[handleAuthError] modelId=<m>, …`（只在调用失败时出现）。
const IDE_AUTH_ERROR_MARKER: &str = "[handleAuthError]";

/// IDE 会话 id 的方括号 tag：`[AcpAgent:<conv32>]` / `[AcpConnection:<conv32>]`。
const IDE_ACP_AGENT_TAG: &str = "AcpAgent:";
const IDE_ACP_CONNECTION_TAG: &str = "AcpConnection:";

/// IDE 行首时间格式（本地时间，定长 23 字节）：`2026-09-17 10:28:26.730`。
const IDE_TS_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";
const IDE_TS_LEN: usize = 23;

/// 超长行的字段嗅探窗口（字节）。
///
/// IDE 的 `Agent execution failed` 行含完整请求体（本机实测 117 KB，研究报告另有 350 KB
/// 样本）：只对行首/行尾各一个定长窗口做字段嗅探，不对整行做全量正则或 JSON 反序列化。
const LONG_LINE_SNIFF_BYTES: usize = 4 * 1024;

/// 同一事件的重置时刻相同时，发生时刻相差 ≤ 该值即视为同一次事件。
///
/// 一次 429 会在业务日志写 5–6 行、SDK 日志再写一份；本机实测 14 行只是 2 次事件，
/// 不做去重的话卡片会把 2 次事件显示成 14 条。
const MERGE_WINDOW_MS: i64 = 15_000;

/// 限额文案标记（粗筛与逐行解析共用）：中文（国内版）与英文（国际版）。
///
/// 同时是 hook 通路（`rate_limit_events.rs`）识别限额 payload 的唯一判据。
pub(crate) const QUOTA_MARKERS: [&str; 2] = ["超出频率限制", "usage exceeds frequency limit"];

/// 官方重置时刻的句式标记：`将在 <时间> UTC+8 重置` / `will reset at <时间> UTC+8`。
const RESET_MARKERS: [&str; 2] = ["将在 ", "will reset at "];

/// 更可靠的分类行标记：`[ACP Agent] refusal classified: …, category=quota`。
const CLASSIFIER_MARKER: &str = "refusal classified";

/// 分类行的配额判据（比 `httpStatus=429` 更贴近「限额」语义）。
const CLASSIFIER_CATEGORY: &str = "category=quota";
/// 分类器兜底的时间窗：合法场景是分类器行后毫秒级紧随的裸文案行（上游实测 1ms）；
/// 回显行与分类器行相隔分钟级，窗口一卡就出局。
const CLASSIFIER_FALLBACK_MS: i64 = 2_000;

/// 会话当前模型标记：`[ModelConfig] sessionId=…, resolved model=…`。
const RESOLVED_MODEL_MARKER: &str = "resolved model=";

/// SDK 会话日志里的「发起请求」标记：`method:sendPrompt {…"modelId":"<模型>"}`。
///
/// 这类行**不带会话 id**（只有 `instanceId`），会话身份只能来自文件名 —— 而它正是模型归因的盲区：
/// 上游只认业务日志的 `requestId → model` 与 `resolved model=`，SDK 侧两样都没有 ⇒ 事件被标「未知模型」。
/// 本机实测（2026-09-18 15:40:22）：那条「未知模型」实为 `hy4-preview-f`。
const SDK_SEND_PROMPT_MARKER: &str = "method:sendPrompt";

/// 业务日志行首时间格式（本地时间）：`9/17/2026, 12:20:31 AM.232`。
const BUSINESS_TS_FORMAT: &str = "%m/%d/%Y, %I:%M:%S %p%.3f";
/// 业务日志的另一种行首时间格式（24 小时制，本机实测）：`2026/9/18 15:46:30.242`。
/// 只认上面那一种时，这类行会被 `line_timestamp` 判为「无时间戳」而整行跳过 —— 表现为台账为空。
const BUSINESS_TS_ALT_FORMAT: &str = "%Y/%m/%d %H:%M:%S%.3f";

// ---------------------------------------------------------------------------
// 扫描
// ---------------------------------------------------------------------------

/// 日志格式族：决定「行首时间戳 / 事件 id / 模型归因」三处适配。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    /// `[9/15/2026, 6:19:48 PM.755] … (requestId/sessionId)`：WorkBuddy 两档位与 CodeBuddy CLI。
    WorkBuddy,
    /// `2026-09-17 10:28:26.730 [info] …`：两个 CodeBuddy IDE。
    Ide,
}

/// 鉴权行标记：顺序扫描时据此维护「事件前最近一次 uid」，是新来源唯一的时态可靠账号线索。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthMarker {
    /// WorkBuddy 两档位：日志里没有账号 uid，归因走 `sessions` 表。
    None,
    /// CodeBuddy CLI：`[AuthDoInitProbe] stage=… uid=<uuid>`。
    AuthDoInitProbe,
    /// 两个 CodeBuddy IDE：`[PulseServiceLifecycle] Auth session changed: … uid=<uuid>`。
    AuthSessionChanged,
}

/// 去重前的原始命中行。
struct Hit {
    /// 事件 id：WorkBuddy/CLI 是 `sessionId`，IDE 是 `conversationId`。
    session_id: Option<String>,
    model: Option<String>,
    reset_at: i64,
    occurred_at: i64,
    /// 该事件前最近一次鉴权行的账号 uid（WorkBuddy 两档位不采集）。
    uid: Option<String>,
}

/// 去重后的一次限额事件。
struct Event {
    session_id: Option<String>,
    model: Option<String>,
    reset_at: i64,
    first_seen_at: i64,
    hit_count: u32,
    uid: Option<String>,
}

impl Event {
    /// 合并同一次事件的重复书写：模型/会话 id/uid 保留任一有值的，首次出现时刻取最早，
    /// 命中行数累加。
    fn merge(&mut self, other: Event) {
        if self.model.is_none() {
            self.model = other.model;
        }
        if self.session_id.is_none() {
            self.session_id = other.session_id;
        }
        if self.uid.is_none() {
            self.uid = other.uid;
        }
        self.first_seen_at = self.first_seen_at.min(other.first_seen_at);
        self.hit_count += other.hit_count;
    }
}

/// 账号归因后的条目：`(账号, 模型)` 聚合的输入；hook 通路（`rate_limit_events.rs`）也产出它。
#[derive(Clone)]
pub(crate) struct Resolved {
    pub(crate) account_id: String,
    pub(crate) model: Option<String>,
    pub(crate) reset_at: i64,
    pub(crate) first_seen_at: i64,
    pub(crate) hit_count: u32,
}

/// 字节级粗筛：命中限额文案的文件才解码（整份文件逐行解码是本模块的主要开销）。
///
/// 用 Boyer–Moore–Horspool 的坏字符跳表：一次比较失败即可跳过多个字节，
/// 而不是退回到「每个偏移都比一次」。2 天窗口（47MB / 30 文件）实测把粗筛
/// 从 ~0.9s 降到 ~0.1s（debug 构建），是「扫描 < 1s」的主要保障。
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    let Some(&last) = needle.last() else {
        return false;
    };
    if haystack.len() < needle.len() {
        return false;
    }
    // 坏字符跳表：needle 中每个字节最后一次出现处到末尾的距离。
    let mut skip = [needle.len(); 256];
    for (index, &byte) in needle[..needle.len() - 1].iter().enumerate() {
        skip[byte as usize] = needle.len() - 1 - index;
    }
    let mut offset = 0;
    while offset + needle.len() <= haystack.len() {
        if haystack[offset + needle.len() - 1] == last
            && &haystack[offset..offset + needle.len()] == needle
        {
            return true;
        }
        offset += skip[haystack[offset + needle.len() - 1] as usize];
    }
    false
}

/// 窗口内的 WorkBuddy 格式日志文件（两档位与 CLI 共用；日期目录下可能还有
/// `sdk/conversations/` 一层）。
///
/// 两层过滤：
/// - 只认日期形态的目录名：`logs/` 下还有 `sdk`、`migration`、`Crash-Log`、`memwatch`
///   等非日期目录，整体忽略；
/// - 目录内**没有任何** mtime 在窗口内的 `.log` 时整目录跳过。
///
/// 判据是文件 mtime 而非目录名：日志文件当天创建后会写到**次日凌晨**（本机实测
/// `2026-09-15/` 的两个文件写到 9/16 07:59），按「最近 N 个目录」会漏掉跨天那批记录。
/// 目录名只用于排除非日志目录，不参与窗口判定。
fn windowed_log_files(logs_root: &Path, cutoff_ms: i64) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(logs_root) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let is_dated = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| NaiveDate::parse_from_str(name, "%Y-%m-%d").is_ok());
        if !is_dated {
            continue;
        }
        let mut candidates = Vec::new();
        log_files(&path, &mut candidates);
        files.extend(candidates.into_iter().filter(|file| {
            std::fs::metadata(file)
                .ok()
                .and_then(|metadata| modified_ms(&metadata))
                .is_some_and(|modified| modified >= cutoff_ms)
        }));
    }
    files
}

/// 递归收集候选日志文件（日期目录下可能还有 `sdk/conversations/` 一层）。
fn log_files(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            log_files(&path, output);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("log") {
            output.push(path);
        }
    }
}

/// 逐行解析限额事件（文件已由粗筛确认命中）。
///
/// 顺序扫描是关键：命中限额行时，`session_models` / `conversation_models` 里恰好是「该会话
/// 在被限**之前**最近一次选用的模型」，`current_uid` 同理是「事件前最近一次鉴权账号」，
/// 因此不会归因成被限之后才切换到的模型或账号。
fn scan_text(text: &str, format: LogFormat, auth: AuthMarker) -> Vec<Hit> {
    scan_text_scoped(text, format, auth, None)
}

/// 带「文件级会话 id」的解析入口。
///
/// SDK 会话日志（`logs/<日期>/sdk/conversations/<会话UUID>.log`）的 `method:sendPrompt`
/// 行不带会话 id，只能靠**文件名**确定会话；顺序扫描保证该模型是「本事件之前最近一次」。
fn scan_text_scoped(
    text: &str,
    format: LogFormat,
    auth: AuthMarker,
    file_session: Option<&str>,
) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut session_models: HashMap<String, String> = HashMap::new();
    let mut request_models: HashMap<String, String> = HashMap::new();
    let mut classifier_session: Option<(String, Option<i64>)> = None;
    let mut conversation_models: HashMap<String, String> = HashMap::new();
    let mut auth_error_model: Option<String> = None;
    let mut current_uid: Option<String> = None;
    for line in text.lines() {
        // 传输行（工具输出 / 命令回显的载体）对上下文采集与限额检测都一律跳过。
        if is_transport_line(line) {
            continue;
        }
        if let Some(uid) = auth_uid(line, auth) {
            current_uid = Some(uid.to_string());
        }
        match format {
            LogFormat::WorkBuddy => {
                if let Some(model) = model_after(line, RESOLVED_MODEL_MARKER) {
                    if let Some(session) = field_after(line, "sessionId=") {
                        session_models.insert(session.to_string(), model.to_string());
                    }
                }
                if let Some(request) = field_after(line, "requestId=") {
                    if let Some(model) = field_model(line, "model=") {
                        request_models.insert(request.to_string(), model.to_string());
                    }
                }
                // SDK 会话日志的模型线索（行内无会话 id ⇒ 用文件名）；顺序扫描 ⇒ 取到事件前的最近一次。
                if let Some(session) = file_session {
                    if line.contains(SDK_SEND_PROMPT_MARKER) {
                        if let Some(model) = json_field(line, "modelId") {
                            session_models.insert(session.to_string(), model.to_string());
                        }
                    }
                }
                // 分类器行自带 `sessionId=`；裸文案行的兜底只在紧随其后（时间窗内）有效，
                // 相隔分钟级的回显行不得借道（2026-09-18 glm 假 chip 实证）。
                if line.contains(CLASSIFIER_MARKER) && line.contains(CLASSIFIER_CATEGORY) {
                    if let Some(session) = field_after(line, "sessionId=") {
                        classifier_session =
                            Some((session.to_string(), line_timestamp(line, format)));
                    }
                }
            }
            LogFormat::Ide => {
                // 会话级模型映射：`[ModelSelection] conversationId=…, modelId=…` 与
                // `[AcpAgent:<conv>] … modelId=…`。`modelId=auto` 视为未知（不覆盖已有值）。
                if line.contains(IDE_MODEL_FIELD) {
                    if let Some(model) = ide_model_id(line) {
                        if let Some(conversation) = ide_session_id(line) {
                            conversation_models.insert(conversation.to_string(), model.to_string());
                        }
                    }
                }
                // 文件级兜底：`[handleAuthError]` 只在调用失败时出现，因此该值不会跨会话陈旧。
                if line.contains(IDE_AUTH_ERROR_MARKER) {
                    if let Some(model) = ide_model_id(line) {
                        auth_error_model = Some(model.to_string());
                    }
                }
            }
        }
        if !QUOTA_MARKERS.iter().any(|marker| line.contains(marker)) {
            continue;
        }
        // `Agent execution failed` 的响应头里有本次请求的账号 uid，是鉴权行缺失时的同级
        // 补充。只对用 uid 归因的来源有意义（WorkBuddy 两档位走 `sessions` 表，不采集 uid）；
        // 该行可能含完整请求体，因此只做定长窗口嗅探。
        if auth != AuthMarker::None && current_uid.is_none() {
            current_uid = json_field(line, "x-user-id").map(str::to_string);
        }
        let Some(reset_at) = parse_reset_at(line) else {
            continue;
        };
        // 没有发生时刻就无法参与去重：宁可丢这一行，也不猜一个时间。
        let Some(occurred_at) = line_timestamp(line, format) else {
            continue;
        };
        let (session_id, model) = match format {
            LogFormat::WorkBuddy => {
                // 分类器兜底只认「紧随其后」的裸文案行：quota 行发生在分类器行之后
                // 且在时间窗内才允许借道（上游实测 1ms；回显行相隔分钟级 ⇒ 出局）。
                let classifier = classifier_session.as_ref().and_then(|(session, ts)| {
                    let ts = (*ts)?;
                    (occurred_at >= ts && occurred_at - ts <= CLASSIFIER_FALLBACK_MS)
                        .then_some(session.as_str())
                });
                workbuddy_attribution(line, &request_models, &session_models, classifier)
            }
            LogFormat::Ide => {
                ide_attribution(line, &conversation_models, auth_error_model.as_deref())
            }
        };
        // WorkBuddy 限额行必须有事件身份（行尾 `(requestId/sessionId)` 或 `sessionId=`）：
        // 会话日志会把任意文本原样回显进文件（诊断命令、工具输出），无身份的行只能落到
        // 分类器兜底上 —— 账号/模型都会错归（2026-09-18 glm 假 chip 实证）。宁可少显示。
        if format == LogFormat::WorkBuddy && session_id.is_none() {
            continue;
        }
        hits.push(Hit {
            session_id,
            model,
            reset_at,
            occurred_at,
            uid: current_uid.clone(),
        });
    }
    hits
}

/// 会话日志里的「传输行」：WorkBuddy 会把工具输出 / 命令回显整段塞进
/// `[SandboxShell] ProcessOutput … content=`（及 Sandbox 系列的 stdout/stderr 摘要），
/// 其中可能带着**别处日志的原文**——包括 429 行与其 requestId。按原样解析会把回显
/// 当成真事件（截断行还会错落归因到回显会话的模型上，2026-09-18 实证 glm 假 chip、
/// hitCount 虚高）。真实限额行（`[Interruption]` / `[Error: 429` 等）不含这些载体标记。
fn is_transport_line(line: &str) -> bool {
    line.contains("ProcessOutput")
        || line.contains(" | content=")
        || line.contains("stdout(")
        || line.contains("stderr(")
        || line.contains("sandbox attempt output")
}

/// WorkBuddy 格式（含 CLI）的事件 id 与模型归因。
///
/// 事件 id 取行尾 `(requestId/sessionId)`，回退行内 `sessionId=`，再回退
/// **紧随其后**（`CLASSIFIER_FALLBACK_MS` 内）的分类器行；模型走
/// `requestId → model`（`[ModelProvider] Sending request`）→ 该会话最近一次
/// `resolved model=` → 未知（不猜）。
///
/// ⚠️ 分类器兜底必须带时间窗：会话日志会把任意文本原样回显进文件（诊断命令、
/// 工具输出），无时间窗时无身份的回显行会错归到「最近一次分类器会话」上
/// （2026-09-18 glm 假 chip 实证）。
fn workbuddy_attribution(
    line: &str,
    request_models: &HashMap<String, String>,
    session_models: &HashMap<String, String>,
    classifier_session: Option<&str>,
) -> (Option<String>, Option<String>) {
    let (request_id, session_id) = match request_pair(line) {
        Some((request, session)) => (Some(request), Some(session)),
        None => (None, None),
    };
    let session_id = session_id
        .or_else(|| field_after(line, "sessionId=").map(str::to_string))
        .or_else(|| classifier_session.map(str::to_string));
    let model = request_id
        .as_deref()
        .and_then(|request| request_models.get(request))
        .or_else(|| {
            session_id
                .as_deref()
                .and_then(|session| session_models.get(session))
        })
        .cloned();
    (session_id, model)
}

/// IDE 格式的事件 id 与模型归因。
///
/// 事件 id 是 `conversationId`（三级提取）；模型走 `conversationId → modelId`（会话级，
/// 实测限额行前 814 ms 命中）→ 本行 `modelId=` → 本行 JSON `"model"`（报错行内，
/// 是本次请求自己的模型）→ 文件级 `[handleAuthError] modelId=` → 未知（不猜）。
fn ide_attribution(
    line: &str,
    conversation_models: &HashMap<String, String>,
    auth_error_model: Option<&str>,
) -> (Option<String>, Option<String>) {
    let session_id = ide_conversation_id(line).map(str::to_string);
    let model = session_id
        .as_deref()
        .and_then(|conversation| conversation_models.get(conversation))
        .cloned()
        .or_else(|| ide_model_id(line).map(str::to_string))
        .or_else(|| ide_json_model(line))
        .or_else(|| auth_error_model.map(str::to_string));
    (session_id, model)
}

/// `sdk/conversations/<会话UUID>.log` 的文件名就是会话 id；业务日志是
/// `<工作区>__<hash>.log`，**不得**当会话 id 用（否则会把工作区名写进会话模型映射）。
fn session_from_file_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let looks_like_uuid = stem.len() == 36
        && stem.matches('-').count() == 4
        && stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    looks_like_uuid.then(|| stem.to_string())
}

fn scan_file(path: &Path, format: LogFormat, auth: AuthMarker, hits: &mut Vec<Hit>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    if !QUOTA_MARKERS
        .iter()
        .any(|marker| contains(&bytes, marker.as_bytes()))
    {
        return;
    }
    let text = String::from_utf8_lossy(&bytes);
    match session_from_file_name(path) {
        Some(session) => hits.extend(scan_text_scoped(&text, format, auth, Some(&session))),
        None => hits.extend(scan_text(&text, format, auth)),
    }
}

/// 枚举 → 粗筛 → 解析 → 去重，返回某个来源的限额事件。
fn collect_events(root: &Path, format: LogFormat, auth: AuthMarker) -> Vec<Event> {
    let cutoff_ms = now_ms() - WINDOW_DAYS as i64 * DAY_MS;
    let files = match format {
        // 日期目录形态校验 + 文件 mtime 收窗（目录名不参与窗口判定）。
        LogFormat::WorkBuddy => windowed_log_files(root, cutoff_ms),
        // IDE 会话目录下按插件目录过滤 + 文件 mtime 收窗。
        LogFormat::Ide => ide_log_files(root, cutoff_ms),
    };
    let mut hits = Vec::new();
    for file in files {
        scan_file(&file, format, auth, &mut hits);
    }
    dedupe(hits)
}

/// 两个 IDE 的候选日志文件：
/// `<root>/<YYYYMMDDTHHMMSS>/window<N>/exthost/Tencent-Cloud.coding-copilot/` 下的**所有
/// 常规文件**（不限文件名/扩展名），只保留 `mtime ≥ cutoff_ms` 的。
///
/// 两个关键决策（研究报告 §B1）：
/// - 会话目录名是**会话启动时间**，不是内容日期，且旧会话目录仍会被追加写入（本机那次 429
///   就落在第 3 新的会话目录里）。按目录名收窗会漏事件，必须按文件 mtime。
/// - 文件名不写死：CN 侧 `腾讯云代码助手.log`（含 `.1.log` 轮转），国际版侧还出现过
///   `Tencent Cloud CodeBuddy.log`；父目录名才是稳定的过滤条件。
fn ide_log_files(root: &Path, cutoff_ms: i64) -> Vec<PathBuf> {
    let mut files = Vec::new();
    // 日志根缺失（该 IDE 未安装/未使用）返回空集，不是错误。
    let Ok(sessions) = std::fs::read_dir(root) else {
        return files;
    };
    for session in sessions.flatten() {
        let session_dir = session.path();
        if !session_dir.is_dir() {
            continue;
        }
        let Ok(windows) = std::fs::read_dir(&session_dir) else {
            continue;
        };
        for window in windows.flatten() {
            let directory = window.path().join("exthost").join(IDE_LOG_DIR_NAME);
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                if modified_ms(&metadata).is_some_and(|modified| modified >= cutoff_ms) {
                    files.push(entry.path());
                }
            }
        }
    }
    files
}

/// 文件最后修改时刻（毫秒）；取不到即视为不在窗口内（宁可漏，不报错）。
fn modified_ms(metadata: &std::fs::Metadata) -> Option<i64> {
    let since_epoch = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since_epoch.as_millis()).ok()
}

/// 两步去重：① `(事件 id, 重置时刻)` 相同即同一次事件；② 重置时刻相同且发生时刻相差
/// ≤ `MERGE_WINDOW_MS` 的记录再合并一次（业务日志与 SDK 日志各写一份，IDE 侧一次事件写
/// 8 行且只有部分行带事件 id，且两边的 id 未必都取得到）。
fn dedupe(hits: Vec<Hit>) -> Vec<Event> {
    let mut groups: BTreeMap<(Option<String>, i64), Event> = BTreeMap::new();
    for hit in hits {
        let key = (hit.session_id.clone(), hit.reset_at);
        let event = Event {
            session_id: hit.session_id,
            model: hit.model,
            reset_at: hit.reset_at,
            first_seen_at: hit.occurred_at,
            hit_count: 1,
            uid: hit.uid,
        };
        match groups.entry(key) {
            Entry::Occupied(mut entry) => entry.get_mut().merge(event),
            Entry::Vacant(entry) => {
                entry.insert(event);
            }
        }
    }

    let mut sorted: Vec<Event> = groups.into_values().collect();
    sorted.sort_by_key(|event| (event.reset_at, event.first_seen_at));
    let mut merged: Vec<Event> = Vec::new();
    for event in sorted {
        let same_event = merged.last().is_some_and(|last| {
            last.reset_at == event.reset_at
                && event.first_seen_at - last.first_seen_at <= MERGE_WINDOW_MS
        });
        if same_event {
            if let Some(last) = merged.last_mut() {
                last.merge(event);
            }
        } else {
            merged.push(event);
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// 行解析
// ---------------------------------------------------------------------------

/// 取行首时间戳（毫秒）。
///
/// WorkBuddy 格式：业务日志是本地时间，SDK 日志是 UTC（`2026-09-16T16:20:31.523Z`），
/// 两者都折算到同一绝对时间轴后再比较。IDE 格式：行首 `2026-09-17 10:28:26.730` 定长
/// 23 字节，本地时间、无时区标记。
fn line_timestamp(line: &str, format: LogFormat) -> Option<i64> {
    let trimmed = line.trim_start();
    let naive = match format {
        LogFormat::WorkBuddy => {
            let head = trimmed.split_whitespace().next().unwrap_or_default();
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(head) {
                return Some(parsed.timestamp_millis());
            }
            let rest = trimmed.strip_prefix('[')?;
            let end = rest.find(']')?;
            let stamp = rest[..end].trim();
            NaiveDateTime::parse_from_str(stamp, BUSINESS_TS_FORMAT)
                .or_else(|_| NaiveDateTime::parse_from_str(stamp, BUSINESS_TS_ALT_FORMAT))
                .ok()?
        }
        LogFormat::Ide => {
            let head = std::str::from_utf8(trimmed.as_bytes().get(..IDE_TS_LEN)?).ok()?;
            NaiveDateTime::parse_from_str(head, IDE_TS_FORMAT).ok()?
        }
    };
    // DST 回拨的那一小时是歧义的：取较早的一次，而不是丢掉这一行。
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|date| date.timestamp_millis())
}

/// 官方给出的重置时刻（毫秒）。原文形如 `将在 2026-09-17 17:59:27 UTC+8 重置` /
/// `will reset at 2026-09-14 10:59:21 UTC+8,`。
///
/// 直接采用原文值（含原文声明的时区偏移），不自建窗口模型。hook payload 的
/// `last_assistant_message` 走同一个入口，保证两条通路的时刻口径一致。
pub(crate) fn parse_reset_at(line: &str) -> Option<i64> {
    for marker in RESET_MARKERS {
        let Some(index) = line.find(marker) else {
            continue;
        };
        let rest = &line[index + marker.len()..];
        let Some(naive) = parse_datetime_prefix(rest) else {
            continue;
        };
        let Some(offset) = parse_utc_offset(rest) else {
            continue;
        };
        return offset
            .from_local_datetime(&naive)
            .single()
            .map(|date| date.timestamp_millis());
    }
    None
}

/// 取前缀里的 `YYYY-MM-DD HH:MM:SS`。
fn parse_datetime_prefix(text: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(text.get(..19)?, "%Y-%m-%d %H:%M:%S").ok()
}

/// 解析文案里的 `UTC±H[:MM]` 偏移。
fn parse_utc_offset(text: &str) -> Option<FixedOffset> {
    let index = text.find("UTC")?;
    let rest = &text[index + 3..];
    let sign = match rest.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = rest[1..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let (hours, minutes) = match digits.split_once(':') {
        Some((hours, minutes)) => (hours.parse::<i32>().ok()?, minutes.parse::<i32>().ok()?),
        None => match digits.len() {
            1 | 2 => (digits.parse::<i32>().ok()?, 0),
            4 => (
                digits[..2].parse::<i32>().ok()?,
                digits[2..].parse::<i32>().ok()?,
            ),
            _ => return None,
        },
    };
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

/// 取行尾 `(requestId/sessionId)`：32 位 hex 请求 id + 带连字符的 36 位会话 uuid。
///
/// 括号里那串 hex 是 **requestId，不是账号**；账号归因只能走会话 id。
fn request_pair(line: &str) -> Option<(String, String)> {
    let mut found = None;
    let mut search = 0;
    while let Some(offset) = line[search..].find('(') {
        let start = search + offset + 1;
        let Some(end) = line[start..].find(')') else {
            break;
        };
        if let Some((request, session)) = line[start..start + end].split_once('/') {
            if is_hex_id(request, 32) && is_hex_id(session, 36) {
                found = Some((request.to_string(), session.to_string()));
            }
        }
        search = start + end;
    }
    found
}

fn is_hex_id(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

/// 取 `<key>` 字段值（到 `,` / 空格 / 引号 / 右括号为止）。
///
/// 要求 key 前是字段边界，避免 `parentSessionId=` 之类的相似字段误命中。
fn field_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(offset) = line[search..].find(key) {
        let index = search + offset;
        let boundary = index == 0 || !is_field_char(line.as_bytes()[index - 1]);
        if boundary {
            let value = line[index + key.len()..]
                .split([',', ' ', '"', '}', ')'])
                .next()
                .unwrap_or_default()
                .trim();
            if !value.is_empty() {
                return Some(value);
            }
        }
        search = index + key.len();
    }
    None
}

fn is_field_char(byte: u8) -> bool {
    // 点号也算字段字符：`requestOptions.model=` 不是 `model=` 字段。
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

/// 取 `<marker>` 之后的模型名。
fn model_after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let start = line.find(marker)? + marker.len();
    let value = line[start..].split([',', ' ', ';']).next()?.trim();
    (!value.is_empty()).then_some(value)
}

/// 取字段边界上的 `<key>` 字段值（`model=` 与 IDE 的 `modelId=` 共用）。
///
/// 只认字段边界上的 key：`[ModelProvider] Sending request: agent=cli, model=<model>,
/// requestId=<id>` 命中，而 `requestOptions.model=` 不命中。
fn field_model<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(offset) = line[search..].find(key) {
        let index = search + offset;
        if index == 0 || !is_field_char(line.as_bytes()[index - 1]) {
            return model_after(&line[index..], key);
        }
        search = index + key.len();
    }
    None
}

/// 取 IDE 行的 `modelId=` 字段（哨兵值见 `IDE_MODEL_UNKNOWN`）。
fn ide_model_id(line: &str) -> Option<&str> {
    let value = field_model(line, IDE_MODEL_FIELD)?;
    (!IDE_MODEL_UNKNOWN.contains(&value)).then_some(value)
}

/// 鉴权行给出的账号 uid（顺序扫描时维护「事件前最近一次」）。
///
/// `uid=none` 是「无会话 / 未登录」的哨兵值，必须忽略。
fn auth_uid(line: &str, auth: AuthMarker) -> Option<&str> {
    let marker = match auth {
        AuthMarker::None => return None,
        AuthMarker::AuthDoInitProbe => CLI_AUTH_MARKER,
        AuthMarker::AuthSessionChanged => IDE_AUTH_MARKER,
    };
    if !line.contains(marker) {
        return None;
    }
    let uid = field_after(line, "uid=")?;
    (!uid.eq_ignore_ascii_case("none")).then_some(uid)
}

/// 取行首定长窗口（对齐到字符边界，不切碎多字节字符）。
fn head_window(line: &str) -> &str {
    let mut end = line.len().min(LONG_LINE_SNIFF_BYTES);
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

/// 取行尾定长窗口（对齐到字符边界）。
fn tail_window(line: &str) -> &str {
    let mut start = line.len().saturating_sub(LONG_LINE_SNIFF_BYTES);
    while start < line.len() && !line.is_char_boundary(start) {
        start += 1;
    }
    &line[start..]
}

/// 取 JSON 形态的字符串字段 `"<key>":"<value>"`。
///
/// IDE 的 `Agent execution failed` 行可能含完整请求体（本机实测 117 KB，研究报告另有
/// 350 KB 样本），因此短行整行嗅探、超长行只看行首与行尾两个定长窗口
/// （`requestBodyValues` 在行首、`responseHeaders` / `responseBody` 在行尾），
/// 不对整行做全量正则或 JSON 反序列化。
fn json_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    if line.len() <= LONG_LINE_SNIFF_BYTES {
        return json_field_in(line, key);
    }
    json_field_in(head_window(line), key).or_else(|| json_field_in(tail_window(line), key))
}

/// 在给定文本里取 `"<key>":"<value>"`（两侧引号即字段边界，故 `"modelId"` 不会误命中 `model`）。
///
/// 兼容两种书写：未转义的 `"<key>":"<value>"` 与嵌在字符串里的转义形式
/// `\"<key>\":\"<value>\"`（IDE 的 `responseBody: "…"` 这类行），转义形式只是引号前多一个
/// 反斜杠。
///
/// 值必须以**闭合引号**结束才采纳：超长行只嗅探定长窗口，窗口边界可能正好落在值的中间，
/// 掐头去尾的半个模型名／半个 uid 一个都不能用（取不到就按「未知」处理，绝不猜）。
fn json_field_in<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let bytes = text.as_bytes();
    let mut search = 0;
    while let Some(offset) = text[search..].find(key) {
        let index = search + offset;
        search = index + key.len();
        // 键的左侧必须是引号（转义形式下引号前还有一个反斜杠，与本次判断无关）。
        if index == 0 || bytes[index - 1] != b'"' {
            continue;
        }
        // 依次吃掉：键的右引号 → 冒号 → 值的左引号（两处都可能带转义反斜杠）。
        let mut cursor = index + key.len();
        if bytes.get(cursor) == Some(&b'\\') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'"') || bytes.get(cursor + 1) != Some(&b':') {
            continue;
        }
        cursor += 2;
        if bytes.get(cursor) == Some(&b'\\') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'"') {
            continue;
        }
        let Some((value, _)) = text[cursor + 1..].split_once('"') else {
            continue;
        };
        // 转义形式下值的闭合引号写作 `\"`：那个反斜杠是转义记号，不属于值本身。
        let value = value.strip_suffix('\\').unwrap_or(value).trim();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// 取方括号 tag 里的会话 id：`[AcpAgent:<conv32>]` / `[AcpConnection:<conv32>]`。
///
/// 只认会话 id 形态：`[BaseAgent:craft]` 之类的 agent tag 必须忽略。
fn bracket_tag_after<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(offset) = line[search..].find(tag) {
        let index = search + offset;
        let value = line[index + tag.len()..].split(']').next()?.trim();
        if is_ide_session_id(value) {
            return Some(value);
        }
        search = index + tag.len();
    }
    None
}

/// IDE 会话 id 形态：32 位 hex（`conversationId`）或 36 位带连字符 uuid（兼容旧形态）。
fn is_ide_session_id(value: &str) -> bool {
    is_hex_id(value, 32) || is_hex_id(value, 36)
}

/// IDE 的会话 id（不含 JSON 形式）：`conversationId=<conv32>` → `[AcpAgent:<conv32>]`
/// → `[AcpConnection:<conv32>]`。
///
/// 用于「顺序扫描时把 `modelId=` 记到哪个会话」——`[handleAuthError]` 这类行没有会话 id，
/// 只作为文件级兜底。
fn ide_session_id(line: &str) -> Option<&str> {
    field_after(line, "conversationId=")
        .filter(|value| is_ide_session_id(value))
        .or_else(|| bracket_tag_after(line, IDE_ACP_AGENT_TAG))
        .or_else(|| bracket_tag_after(line, IDE_ACP_CONNECTION_TAG))
}

/// IDE 的事件 id：会话 id，再回退报错行里的 JSON 形式 `"conversationId":"<conv32>"`。
fn ide_conversation_id(line: &str) -> Option<&str> {
    ide_session_id(line)
        .or_else(|| json_field(line, "conversationId").filter(|value| is_ide_session_id(value)))
}

/// 报错行内的 JSON 模型字段 `"model":"<value>"`（哨兵值同 `IDE_MODEL_UNKNOWN`）。
fn ide_json_model(line: &str) -> Option<String> {
    let value = json_field(line, "model")?;
    (!IDE_MODEL_UNKNOWN.contains(&value)).then(|| value.to_string())
}

// ---------------------------------------------------------------------------
// 汇总
// ---------------------------------------------------------------------------

/// sessionId → 账号 id。
///
/// 归因失败（`sessions` 表缺失、会话不在库、uid 未收录）时返回空映射，调用方据此丢弃
/// 该事件——宁可少显示，不可显示错账号。hook 通路（`rate_limit_events.rs`）的
/// WorkBuddy 兜底归因复用本函数。
pub(crate) fn account_by_session(
    variant: WbVariant,
    session_ids: &BTreeSet<String>,
) -> HashMap<String, String> {
    let mut mapping = HashMap::new();
    if session_ids.is_empty() {
        return mapping;
    }
    let uid_to_account: HashMap<String, String> = account::load_accounts()
        .iter()
        .filter(|acc| account::variant_of(acc) == variant)
        .filter_map(|acc| Some((account::get_str(acc, "uid")?, account::get_str(acc, "id")?)))
        .collect();
    if uid_to_account.is_empty() {
        return mapping;
    }
    let db = workbuddy_db_path(variant);
    if !db.is_file() {
        return mapping;
    }
    let Some(conn) = open_db(&db, true) else {
        return mapping;
    };
    if !table_exists(&conn, "sessions") {
        return mapping;
    }
    // 批量查询：逐条查库在事件多时会成为主要开销。
    let placeholders = vec!["?"; session_ids.len()].join(",");
    let sql = format!("SELECT id, user_id FROM sessions WHERE id IN ({placeholders})");
    let Ok(mut statement) = conn.prepare(&sql) else {
        return mapping;
    };
    let Ok(rows) = statement.query_map(rusqlite::params_from_iter(session_ids.iter()), |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        ))
    }) else {
        return mapping;
    };
    for (session_id, user_id) in rows.flatten() {
        let (Some(session_id), Some(user_id)) = (session_id, user_id) else {
            continue;
        };
        if let Some(account_id) = uid_to_account.get(&user_id) {
            mapping.insert(session_id, account_id.clone());
        }
    }
    mapping
}

/// 账号归因：归因不到账号的事件直接丢弃（不错归给任何账号）。
fn resolve(variant: WbVariant, events: &[Event]) -> Vec<Resolved> {
    let session_ids: BTreeSet<String> = events
        .iter()
        .filter_map(|event| event.session_id.clone())
        .collect();
    let mapping = account_by_session(variant, &session_ids);
    events
        .iter()
        .filter_map(|event| {
            let account_id = event
                .session_id
                .as_deref()
                .and_then(|session| mapping.get(session))?;
            Some(Resolved {
                account_id: account_id.clone(),
                model: event.model.clone(),
                reset_at: event.reset_at,
                first_seen_at: event.first_seen_at,
                hit_count: event.hit_count,
            })
        })
        .collect()
}

/// 来源状态文件里的「当前生效账号」。
///
/// `activeAccountId` 是**账号库的 `id`，不是 uid**（三个状态文件均如此）。
struct ActiveAccount {
    account_id: Option<String>,
    updated_at: Option<i64>,
}

impl ActiveAccount {
    /// 读该来源的状态文件；文件缺失/损坏时回落不可用（返回空，不是错误）。
    fn load(path: &Path) -> Self {
        let state = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or_else(|| json!({}));
        Self {
            account_id: state
                .get("activeAccountId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            updated_at: state.get("updatedAt").and_then(Value::as_i64),
        }
    }

    /// 该状态文件能否作为这一事件的归因依据。
    ///
    /// 必须 `updatedAt ≤ 事件时刻`：IDE 的 `detect_current_account()` 匹配成功时也会回写状态
    /// 文件（`codebuddy_ide.rs` / `codebuddy_cn_ide.rs`），使 `updatedAt` 晚于事件却并非账号
    /// 切换。缺 `updatedAt` 时无从证实「当时生效的是谁」，同样不用。
    fn account_for(&self, first_seen_at: i64) -> Option<&str> {
        let account_id = self.account_id.as_deref()?;
        (self.updated_at? <= first_seen_at).then_some(account_id)
    }
}

/// 账号库 `uid → 账号 id` 映射（CLI 与两个 IDE 的日志内鉴权 uid 归因用）。
///
/// 不按档位过滤：CLI 跨两档位共用同一套账号库，IDE 侧写入的账号也未必与日志来源档位一致；
/// uid 在账号库内唯一（导入即按 uid 去重），过滤只会漏、不会错。
fn account_id_by_uid() -> HashMap<String, String> {
    account::load_accounts()
        .iter()
        .filter_map(|acc| Some((account::get_str(acc, "uid")?, account::get_str(acc, "id")?)))
        .collect()
}

/// 新来源（CLI / 两个 IDE）的账号归因。
///
/// ① 事件 uid → 账号库 `uid → id`（日志内鉴权行，时态正确）；② 回落该来源状态文件的
/// `activeAccountId`（带 `updatedAt ≤ 事件时刻` 门控）；③ 都不满足 → 丢弃该事件，
/// 不错归给任何账号。
fn resolve_by_uid(
    events: &[Event],
    uid_to_account: &HashMap<String, String>,
    fallback: &ActiveAccount,
) -> Vec<Resolved> {
    events
        .iter()
        .filter_map(|event| {
            let account_id = event
                .uid
                .as_deref()
                .and_then(|uid| uid_to_account.get(uid))
                .map(String::as_str)
                .or_else(|| fallback.account_for(event.first_seen_at))?;
            Some(Resolved {
                account_id: account_id.to_string(),
                model: event.model.clone(),
                reset_at: event.reset_at,
                first_seen_at: event.first_seen_at,
                hit_count: event.hit_count,
            })
        })
        .collect()
}

/// 聚合与过滤：per (账号, 模型) 取 `resetAt` 最大的一条（同模型多次限流以最新一次为准），
/// 只保留官方重置时刻尚未到达的条目。
///
/// 无受限模型的账号**不出现在结果里**，前端据此决定是否渲染图标。
fn build_payload(resolved: Vec<Resolved>, scanned_at: i64) -> Value {
    let mut latest: BTreeMap<(String, Option<String>), Resolved> = BTreeMap::new();
    for item in resolved {
        if item.reset_at <= scanned_at {
            continue;
        }
        match latest.entry((item.account_id.clone(), item.model.clone())) {
            Entry::Occupied(mut entry) => {
                if item.reset_at > entry.get().reset_at {
                    entry.insert(item);
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(item);
            }
        }
    }

    let mut by_account: BTreeMap<String, Vec<Resolved>> = BTreeMap::new();
    for item in latest.into_values() {
        by_account
            .entry(item.account_id.clone())
            .or_default()
            .push(item);
    }
    let accounts: Vec<Value> = by_account
        .into_iter()
        .map(|(account_id, mut items)| {
            // 按恢复时间升序：最早解锁的排在最前（最有行动价值）。
            items.sort_by_key(|item| item.reset_at);
            let limited: Vec<Value> = items
                .into_iter()
                .map(|item| {
                    json!({
                        "model": item.model,
                        "resetAt": item.reset_at,
                        "firstSeenAt": item.first_seen_at,
                        "hitCount": item.hit_count,
                    })
                })
                .collect();
            json!({ "accountId": account_id, "limited": limited })
        })
        .collect();
    json!({
        "scannedAt": scanned_at,
        "windowDays": WINDOW_DAYS,
        "accounts": accounts,
    })
}

/// CodeBuddy CLI 日志根（`~/.codebuddy/logs`；三平台同构，`home_dir()` 已跨平台）。
fn cli_logs_root() -> PathBuf {
    home_dir().join(CLI_DATA_DIR).join(LOG_DIR_NAME)
}

/// CodeBuddy CLI 轮换状态文件（`~/.codebuddy-rotate/state.json`）。
fn cli_state_path() -> PathBuf {
    home_dir().join(CLI_ROTATE_DIR).join(CLI_STATE_FILE)
}

// ---------------------------------------------------------------------------
// 扫描缓存与范围（hook 通路接入后：逐来源判定）
// ---------------------------------------------------------------------------

/// 日志扫描的最小间隔：与前端节流口径一致（hook 已安装时 IDE 日志仍按 5 分钟扫一次）。
///
/// 前端在「距上次扫描 ≥ 5 分钟」时才发起请求，后端这一层是不依赖前端行为的兜底：
/// 事件驱动的拉取、页面反复开关都不会触发额外的全量扫描。
const SCAN_MIN_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// 一个日志来源（扫描范围的最小单位）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScanSource {
    /// WorkBuddy 客户端（两档位各算一个来源）。
    WorkBuddy(WbVariant),
    /// CodeBuddy CLI（与两个 IDE **共用** `~/.codebuddy/settings.json`）。
    Cli,
    /// CodeBuddy IDE 的一个形态（429 不触发任何事件；`scanIdeLogs` 开启且数据根存在才扫）。
    Ide(CodeBuddyIdeFlavor),
}

impl ScanSource {
    /// 位图里的位序（与 `ScanScope::ALL` 对应）。
    fn bit(self) -> u8 {
        match self {
            Self::WorkBuddy(WbVariant::Cn) => 0,
            Self::WorkBuddy(WbVariant::Ai) => 1,
            Self::Cli => 2,
            Self::Ide(CodeBuddyIdeFlavor::Intl) => 3,
            Self::Ide(CodeBuddyIdeFlavor::Cn) => 4,
        }
    }
}

/// 一次请求要扫的来源集合（位图）。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
struct ScanScope(u8);

impl ScanScope {
    /// 一个来源都不扫。
    const NONE: Self = Self(0);
    /// 全量五源。装 / 卸 hook 的强制全量在 `scanIdeLogs` 开启时用这个；关闭时改走 `HOOK_SOURCES`。
    const ALL: Self = Self(0b1_1111);
    /// 只含「hook 通路来源」（CLI + WorkBuddy 两档位）：`scanIdeLogs` 关闭时强制全量的上限
    /// —— 两个 IDE 是独立开关，任何路径下都不该被带回来。
    const HOOK_SOURCES: Self = Self(0b0_0111);

    fn insert(&mut self, source: ScanSource) {
        self.0 |= 1 << source.bit();
    }

    fn contains(self, source: ScanSource) -> bool {
        self.0 & (1 << source.bit()) != 0
    }

    /// `self` 是否覆盖 `other`（缓存命中判据：缓存范围必须是请求范围的超集）。
    fn covers(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// 扫描范围判定的输入路径（显式传入：单测注入 tempdir，不触碰真实配置）。
struct ScanRoots {
    /// 需要按「数据根存在 且 该处未注册 hook」判定的来源：CLI 与 WorkBuddy 两档位。
    hook_sources: Vec<(ScanSource, PathBuf, PathBuf)>,
    /// 只判「数据根存在」的来源：两个 IDE。
    ide_sources: Vec<(ScanSource, PathBuf)>,
    /// 本工具 hook 脚本的绝对路径（配置里的 marker）。
    marker: String,
}

impl ScanRoots {
    fn real() -> Self {
        let mut hook_sources = Vec::new();
        for variant in WbVariant::ALL {
            let root = variant.data_root();
            hook_sources.push((
                ScanSource::WorkBuddy(variant),
                root.clone(),
                root.join(SETTINGS_FILE_NAME),
            ));
        }
        let cli_root = home_dir().join(CLI_DATA_DIR);
        hook_sources.push((
            ScanSource::Cli,
            cli_root.clone(),
            cli_root.join(SETTINGS_FILE_NAME),
        ));
        let ide_sources = [CodeBuddyIdeFlavor::Intl, CodeBuddyIdeFlavor::Cn]
            .into_iter()
            .filter_map(|flavor| {
                codebuddy_ide_data_dir(flavor).map(|dir| (ScanSource::Ide(flavor), dir))
            })
            .collect();
        Self {
            hook_sources,
            ide_sources,
            marker: hook_marker(),
        }
    }
}

/// 逐来源扫描范围：
///
/// - CLI / WorkBuddy(x)：数据根存在 **且** 该处配置未注册本工具 hook —— 某处注册失败
///   （非法 JSON / 用户手删条目）时只有那一处回退日志扫描，其余来源继续走事件通路；
/// - IDE(x)：`scan_ide_logs` 开启 **且** 数据根存在才扫（IDE 的 429 不触发任何事件，
///   日志是它唯一的数据源；开关关闭时两个 IDE 一律不入范围）。
fn scan_scope(roots: &ScanRoots, scan_ide_logs: bool) -> ScanScope {
    let mut scope = ScanScope::NONE;
    for (source, data_root, settings) in &roots.hook_sources {
        if needs_log_scan(data_root, settings, &roots.marker) {
            scope.insert(*source);
        }
    }
    if scan_ide_logs {
        for (source, data_root) in &roots.ide_sources {
            if data_root.is_dir() {
                scope.insert(*source);
            }
        }
    }
    scope
}

/// 配置里的 `scanIdeLogs`（IDE 日志扫描开关）：缺失 / 类型不符按默认开启。
fn scan_ide_logs_of(cfg: &Value) -> bool {
    cfg.get("scanIdeLogs")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// 当前配置是否扫描两个 CodeBuddy IDE 的日志。
fn scan_ide_logs_enabled() -> bool {
    scan_ide_logs_of(&crate::modules::config::load_rate_limit_config())
}

struct ScanCache {
    at: i64,
    /// 本次缓存覆盖的来源范围。
    scope: ScanScope,
    entries: Vec<Resolved>,
}

impl ScanCache {
    /// 缓存能否满足请求：范围是请求的超集（全量缓存满足子范围请求）。
    fn covers(&self, requested: ScanScope, now: i64) -> bool {
        now - self.at < SCAN_MIN_INTERVAL_MS && self.scope.covers(requested)
    }
}

static SCAN_CACHE: Mutex<Option<ScanCache>> = Mutex::new(None);

/// 置位后下一次扫描按**全量**走一遍（只由装 / 卸 hook 置位）。
static FORCE_FULL_SCAN: AtomicBool = AtomicBool::new(false);

/// 补扫基线（进程级常驻）：对「hook 通路来源」补扫一次日志，结果保留整个进程生命周期。
///
/// 背景：装 hook **之前**发生的限额只存在于日志里（2026-09-18 实测：15:46 的 429 在 17:00 装 hook 后消失），
/// 而 `scan_scope` 会把「已注册 hook」的来源排除，交给事件通路 —— 事件通路无法回溯历史。
///
/// ⚠️ 必须**常驻**：`cached_scan` 的缓存只有 5 分钟（`SCAN_MIN_INTERVAL_MS`），过期后会按当时范围重扫；
/// 若把补扫结果只留在缓存里，5 分钟后就会被「不含 hook 来源」的空扫描结果覆盖 ⇒ 卡片上的限额又消失。
static BACKFILL_BASE: std::sync::Mutex<Option<Vec<Resolved>>> = std::sync::Mutex::new(None);

/// 只清缓存，不置「强制全量」：**开关类**范围变更（`scanIdeLogs`）用。
///
/// 下一次 `get_rate_limits()` 按当前 scope 重扫——关掉 IDE 扫描后，缓存里的 IDE 旧条目随之消失，
/// 也不会再用旧缓存把两个 IDE 扫一遍。
fn clear_scan_cache() {
    *SCAN_CACHE.lock().unwrap() = None;
}

/// 装 / 卸 hook 后调用：扫描范围变了，缓存作废，下一次扫描强制全量走一遍。
///
/// 全量一次是为了装 hook 的瞬间不丢已经显示出来的 CLI / WorkBuddy 限额（之后按 hook 通路实时更新）。
/// `scanIdeLogs = false` 时全量上限是 `HOOK_SOURCES`，不会把两个 IDE 带回来。
/// 与 `clear_scan_cache` 的分工：这里额外置 `FORCE_FULL_SCAN`，开关类变更不得走这条
/// （否则刚关掉的 IDE 又会按旧范围扫一次）。
pub fn invalidate_scan_cache() {
    FORCE_FULL_SCAN.store(true, Ordering::SeqCst);
    clear_scan_cache();
}

/// 取一次扫描结果（命中缓存则不重扫）。
///
/// 命中要求缓存范围 ⊇ 请求范围；`allow_scan = false`（限额监听被关闭）时不重扫，
/// 只用已有缓存——关闭开关后不得再读日志。
fn cached_scan(requested: ScanScope, now: i64, allow_scan: bool) -> (i64, Vec<Resolved>) {
    let mut cache = SCAN_CACHE.lock().unwrap();
    if let Some(cached) = cache.as_ref() {
        if cached.covers(requested, now) || !allow_scan {
            return (cached.at, cached.entries.clone());
        }
    }
    if !allow_scan {
        return (0, Vec::new());
    }
    let entries = scan_sources(requested);
    *cache = Some(ScanCache {
        at: now,
        scope: requested,
        entries: entries.clone(),
    });
    (now, entries)
}

/// 按范围扫描日志：只扫 `scope` 里的来源（已注册 hook 的来源由事件通路负责）。
fn scan_sources(scope: ScanScope) -> Vec<Resolved> {
    let mut resolved = Vec::new();
    // ① WorkBuddy 两档位：日期目录枚举 + `sessions` 表归因（现有行为不变）。
    for variant in WbVariant::ALL {
        if !scope.contains(ScanSource::WorkBuddy(variant)) {
            continue;
        }
        let events = collect_events(
            &variant.data_root().join(LOG_DIR_NAME),
            LogFormat::WorkBuddy,
            AuthMarker::None,
        );
        resolved.extend(resolve(variant, &events));
    }
    // ② CodeBuddy CLI：日志与 WorkBuddy 同格式（时间戳/事件 id/模型归因全部兼容），
    // 但会话不在 WorkBuddy 的 `sessions` 表里，归因走日志内鉴权 uid + 轮换状态文件回落。
    if scope.contains(ScanSource::Cli) {
        let uid_to_account = account_id_by_uid();
        let cli = collect_events(
            &cli_logs_root(),
            LogFormat::WorkBuddy,
            AuthMarker::AuthDoInitProbe,
        );
        resolved.extend(resolve_by_uid(
            &cli,
            &uid_to_account,
            &ActiveAccount::load(&cli_state_path()),
        ));
    }
    // ③ 两个 CodeBuddy IDE：同一份 Ide 格式与同一套归因，只有日志根与状态文件不同。
    //
    // **不扫** `CodeBuddyExtension/Logs/CodeBuddyIDE/`：它是插件宿主的跨 App 共享日志根，
    // 与 CN IDE 真身重复记录同一次 429（同一 requestId、行时间差 3 ms），扫它会重复展示
    // 且档位归属不清（PRD D1）。
    let mut ide_uid_to_account: Option<HashMap<String, String>> = None;
    for (flavor, state_file) in [
        (CodeBuddyIdeFlavor::Intl, IDE_STATE_FILE),
        (CodeBuddyIdeFlavor::Cn, CN_IDE_STATE_FILE),
    ] {
        if !scope.contains(ScanSource::Ide(flavor)) {
            continue;
        }
        // 该 IDE 的数据目录不可定位（平台不支持）时跳过，不是错误。
        let Some(data_dir) = codebuddy_ide_data_dir(flavor) else {
            continue;
        };
        let events = collect_events(
            &data_dir.join(LOG_DIR_NAME),
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged,
        );
        let uid_to_account = ide_uid_to_account.get_or_insert_with(account_id_by_uid);
        resolved.extend(resolve_by_uid(
            &events,
            uid_to_account,
            &ActiveAccount::load(&store_dir().join(state_file)),
        ));
    }
    resolved
}

/// 全部账号当前的模型限额状态。
///
/// 两条通路合并：
/// - **hook 通路**（CLI / WorkBuddy 两档位）：事件驱动、实时归因，由 `rate_limit_events` 持有；
/// - **日志扫描**（两个 IDE 只走这条，受 `scanIdeLogs` 开关约束；CLI / WorkBuddy 只在「该处未注册 hook」时回退扫描）：
///   按 `SCAN_MIN_INTERVAL_MS` 节流，合并后的 `scannedAt` 是**最近一次真实扫描**的时刻
///   （前端据此节流）。
///
/// 不接收档位参数：扫描本身就是全局的，按档位调用会把同一份日志扫 N 遍。
pub fn get_rate_limits() -> Value {
    let now = now_ms();
    // 限额监听关闭时不再扫日志（hook 信号照常入账，由后端持有）。
    let enabled = crate::modules::rate_limit_events::rate_limit_enabled();
    // 「扫描 CodeBuddy IDE 日志」是独立开关：关闭后两个 IDE 一律不入 scope，
    // CLI / WorkBuddy 的逐来源判定（hook 未接上则回退日志）不受影响。
    let scan_ide_logs = scan_ide_logs_enabled();
    // 逐来源判定：不存在的客户端不参与扫描；已注册 hook 的来源交给事件通路。
    // 装 / 卸 hook 的瞬间强制全量一次。关闭开关时不得把「强制全量」标志消费掉：
    // 否则重新开启后会直接按当前范围扫，装 hook 前已经显示的 CLI / WorkBuddy 限额会丢。
    let full_scan_once = if enabled {
        FORCE_FULL_SCAN.swap(false, Ordering::SeqCst)
    } else {
        FORCE_FULL_SCAN.load(Ordering::SeqCst)
    };
    let scope = if full_scan_once {
        // 强制全量也不能把已被开关关掉的 IDE 带回来。
        if scan_ide_logs {
            ScanScope::ALL
        } else {
            ScanScope::HOOK_SOURCES
        }
    } else {
        scan_scope(&ScanRoots::real(), scan_ide_logs)
    };
    let (scanned_at, mut resolved) = cached_scan(scope, now, enabled);
    // 「hook 通路来源」补扫一次并常驻（历史限额只在日志里）；之后的增量交给事件通路。
    if enabled {
        let mut slot = BACKFILL_BASE.lock().unwrap();
        if slot.is_none() {
            *slot = Some(scan_sources(ScanScope(ScanScope::HOOK_SOURCES.0)));
        }
        if let Some(base) = slot.as_ref() {
            resolved.extend(base.iter().cloned());
        }
    }
    resolved.extend(crate::modules::rate_limit_events::hook_entries(now));
    build_payload(resolved, if scanned_at > 0 { scanned_at } else { now })
}

/// 保存限额监听配置，并同步日志扫描范围。
///
/// `scanIdeLogs` 变化 → **只清缓存**（`clear_scan_cache`，不置 `FORCE_FULL_SCAN`）：
/// 下一次 `get_rate_limits()` 按新 scope 重扫，关掉后不会再用旧缓存扫一次 IDE。
/// 装 / 卸 hook 仍走 `invalidate_scan_cache()`（清缓存 + 强制全量），语义不同不能混用。
pub fn save_rate_limit_config(cfg: &Value) -> std::io::Result<()> {
    save_rate_limit_config_at(&crate::modules::config::rate_limit_config_file(), cfg)
}

/// [`save_rate_limit_config`] 的可注入路径版本（单测不得触碰真实 `~/.wb-switch`）。
pub(crate) fn save_rate_limit_config_at(path: &Path, cfg: &Value) -> std::io::Result<()> {
    let before = crate::modules::config::load_rate_limit_config_at(path);
    crate::modules::config::save_rate_limit_config_at(path, cfg)?;
    let after = crate::modules::config::load_rate_limit_config_at(path);
    if scan_ide_logs_of(&before) != scan_ide_logs_of(&after) {
        clear_scan_cache();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机实测的重置时刻：`2026-09-17 17:59:27 UTC+8`。
    const CN_RESET_AT: i64 = 1_789_639_167_000;
    /// 国际版文案样例的重置时刻：`2026-09-14 10:59:21 UTC+8`。
    const EN_RESET_AT: i64 = 1_789_354_761_000;
    /// 本机实测的 SDK 行时间戳：`2026-09-16T16:20:31.523Z`。
    const SDK_OCCURRED_AT: i64 = 1_789_575_631_523;

    const CN_SESSION: &str = "387a486d-b5c9-473d-b823-121db59f0084";
    const CN_REQUEST: &str = "34fccb8a1ed74da39dea5481eba85b46";
    const EN_SESSION: &str = "9b9130df-3665-4fdc-9ae7-65c23c8f8edd";
    const EN_REQUEST: &str = "4d83788eb9de41d08424eb2b64c08bd3";

    fn business_line(timestamp: &str, body: &str) -> String {
        format!("[{timestamp}] [Error] [pid=1] {body}")
    }

    /// 业务日志有两种行首时间戳：12 小时制（`9/17/2026, 12:20:31 AM.232`）与
    /// 24 小时制（`2026/9/18 15:46:30.242`，2026-09-18 本机实测）。只认前者时，
    /// 后者会被整行跳过 —— 现象是台账始终为空。
    /// SDK 会话日志：模型线索来自 `method:sendPrompt`，会话 id 来自文件名。
    ///
    /// 回归：修复前这类事件被标成「未知模型」（归因只认业务日志的 requestId / resolved model）。
    #[test]
    fn sdk_send_prompt_supplies_the_model_via_file_session() {
        let session = "b3df1149-8a3f-4d73-b7a4-c71c46e15762";
        let quota = cn_quota_with(session, CN_REQUEST, "2026-09-19 13:49:50");
        let text = [
            "2026-09-18T07:37:17.657Z method:sendPrompt {\"instanceId\":\"ci-3\",\"modelId\":\"hy4-preview-f\"}"
                .to_string(),
            format!("2026-09-18T07:40:22.997Z runtime.applyStopReason {{\"preview\":\"{quota}\"}}"),
        ]
        .join("\n");
        let events = dedupe(scan_text_scoped(
            &text,
            LogFormat::WorkBuddy,
            AuthMarker::None,
            Some(session),
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("hy4-preview-f"));
        // 不给文件级会话 id 时保持上游行为（未知），避免把别的会话的模型误归因过来。
        let without = dedupe(scan_text(&text, LogFormat::WorkBuddy, AuthMarker::None));
        assert_eq!(without[0].model, None);
    }

    #[test]
    fn accepts_both_business_timestamp_styles() {
        for ts in ["9/17/2026, 12:20:31 AM.232", "2026/9/18 15:46:30.242"] {
            let events = dedupe(workbuddy_hits(&business_line(ts, &cn_quota())));
            assert_eq!(events.len(), 1, "时间戳 {ts} 未被解析");
        }
    }

    /// WorkBuddy 限额行没有事件身份（无 `(requestId/sessionId)`、无 `sessionId=`）时
    /// 必须丢弃 —— 哪怕前面有分类器行把「当前会话」指向了别处，也不能让回显文案
    /// 借道分类器兜底入账（2026-09-18 glm 假 chip 实证）。
    #[test]
    fn workbuddy_quota_lines_without_event_identity_are_dropped() {
        let text = [
            // 分类器行把「当前会话」指向 glm 会话（修复前回显行借道它入账）。
            "[2026/9/18 20:09:00.000] [Info] [pid=9999] [AgentClassifier] refusal classified category=quota sessionId=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            &config_line("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee", "glm-5.3-flash"),
            // 无身份的限额文案行（BashTool 命令回显的截断形态）。
            "[2026/9/18 20:10:00.000] [Info] [pid=9999] [BashTool] execute start | command=\"grep 429 您的使用量已超出频率限制，将在 2026-09-19 13:49:50 UTC+8 重置 logs/\"",
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&text));
        assert!(events.is_empty(), "无身份的限额文案行不得入账");
    }

    /// 传输行（Sandbox 回显 / 工具输出载体）里即使嵌着 429 原文，也不得当成真事件，
    /// 更不得污染 requestId → model 的归因映射（2026-09-18 glm 假 chip 实证）。
    #[test]
    fn transport_lines_are_neither_events_nor_model_context() {
        let other_session = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let text = [
            // 真事件：requestId → hy4-preview-f。
            provider_line(CN_REQUEST, "hy4-preview-f"),
            business_line("2026/9/18 15:46:30.242", &cn_quota()),
            // 回显 A：截断的 429 行（requestId 不完整 → 会错落归因到回显会话的模型）。
            // 回显会话此前「刚用过」glm —— 修复前会多出一条 glm 事件。
            config_line(other_session, "glm-5.3-flash"),
            format!(
                "[2026/9/18 20:10:00.000] [Info] [pid=9999] [SandboxShell] ProcessOutput | processId=pipe-1 | stream=stdout | dataLen=1980 | content= 2026-09-19 13:49:50 UTC+8 重置，您也可以切换其他模型继续使用。 ({}/{}",
                &CN_REQUEST[..12], other_session
            ),
            // 回显 B：完整 429 原文整行嵌在 content= 里（会被二次计数）。
            format!(
                "[2026/9/18 20:10:01.000] [Info] [pid=9999] [SandboxShell] ProcessOutput | processId=pipe-2 | stream=stdout | dataLen=900 | content=\"{}\"",
                business_line("2026/9/18 15:46:30.242", &cn_quota())
            ),
            // 回显 C：模型上下文行也被回显 —— 不得写进 requestId → model 映射。
            format!(
                "[2026/9/18 20:10:02.000] [Info] [pid=9999] [SandboxShell] ProcessOutput | processId=pipe-3 | stream=stdout | dataLen=220 | content=[ModelProvider] Sending request: agent=cli, model=glm-5.3-flash, requestId=zzz{}, stream=true",
                &CN_REQUEST[3..]
            ),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&text));
        assert_eq!(events.len(), 1, "回显行不得产生事件");
        assert_eq!(
            events[0].model.as_deref(),
            Some("hy4-preview-f"),
            "归因必须来自真实 Sending request 行，而不是回显"
        );
    }

    /// 中文限额文案（`session` / `request` / `reset` 可替换，便于构造多条事件）。
    fn cn_quota_with(session: &str, request: &str, reset: &str) -> String {
        format!(
            "429 您的使用量已超出频率限制，将在 {reset} UTC+8 重置，您也可以切换其他模型继续使用。 ({request}/{session})"
        )
    }

    fn cn_quota() -> String {
        cn_quota_with(CN_SESSION, CN_REQUEST, "2026-09-17 17:59:27")
    }

    fn en_quota() -> String {
        format!(
            "429 usage exceeds frequency limit, please try later, will reset at 2026-09-14 10:59:21 UTC+8, switch to another model. ({EN_REQUEST}/{EN_SESSION})"
        )
    }

    fn config_line(session: &str, model: &str) -> String {
        format!("[ModelConfig] sessionId={session}, resolved model={model} for agent=cli, requestOptions.model={model}")
    }

    fn provider_line(request: &str, model: &str) -> String {
        format!(
            "[ModelProvider] Sending request: agent=cli, model={model}, requestId={request}, stream=true"
        )
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb-switch-limits-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// 把文件 mtime 设为 N 天前（收窗测试用；`File::set_modified` 需要可写句柄）。
    fn set_days_old_mtime(path: &Path, days: u64) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 3600);
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("打开文件以设置 mtime")
            .set_modified(old)
            .expect("设置文件 mtime");
    }

    /// 现有两档位（WorkBuddy 格式、无鉴权行）的解析入口。
    fn workbuddy_hits(text: &str) -> Vec<Hit> {
        scan_text(text, LogFormat::WorkBuddy, AuthMarker::None)
    }

    // -----------------------------------------------------------------------
    // CodeBuddy CLI 与两个 IDE 的样本（行骨架取自本机真实日志，id/uid 已替换）
    // -----------------------------------------------------------------------

    /// IDE 会话 id（`conversationId`，32 位 hex）。
    const IDE_CONV: &str = "b3cca2525ba544debd0abc63ca69f8b9";
    /// 另一次会话（验证模型归因按会话隔离）。
    const IDE_OTHER_CONV: &str = "a12075d16ada4fbba38047b0d879db2b";
    const IDE_REQUEST: &str = "0ffa4f2d001e4aee92e1cd6ce6a709c2";
    const IDE_TRACE: &str = "8e090575f6032a82163c94dc3d65e9ba";
    const IDE_MODEL: &str = "deepseek-v4.1-flash";
    const UID_A: &str = "f0ae9eeb-8476-4ef5-9a3e-1d6339d545da";
    const UID_B: &str = "2e649415-05c1-4c8a-8736-6b8e3aac0f9a";

    /// IDE 限额文案（与研究样本逐字一致）。
    const IDE_QUOTA: &str = "您的使用量已超出频率限制，将在 2026-09-17 17:59:27 UTC+8 重置，您也可以切换其他模型继续使用。";

    /// IDE 行首时间格式（本地时间，无方括号）。
    fn ide_line(timestamp: &str, body: &str) -> String {
        format!("{timestamp} [error] {body}")
    }

    fn ide_auth_line(uid: &str) -> String {
        format!(
            "2026-09-17 10:26:01.772 [info] [PulseServiceLifecycle] Auth session changed: hasSession=true, initialized=true, uid={uid}"
        )
    }

    fn cli_auth_line(timestamp: &str, uid: &str) -> String {
        business_line(
            timestamp,
            &format!(
                "[AuthenticationManager]  [FirstScreen] [AuthDoInitProbe] stage=fastPathBeforeEmit totalMs=591 sinceLastMs=0 uid={uid} hasAccessToken=true"
            ),
        )
    }

    /// 本机真实事件的行集合：一次 429 写 8 行带文案 + 1 行 `[handleAuthError]`。
    fn ide_event_lines(conversation: &str, uid: &str) -> String {
        let quota = IDE_QUOTA;
        let notify_step_error = ide_line(
            "2026-09-17 10:28:26.730",
            &format!(
                "[BaseAgent:craft] [{IDE_REQUEST}]  notifyStepError responseBody: \"{{\\\"code\\\":6004,\\\"msg\\\":\\\"{quota}\\\",\\\"requestId\\\":\\\"{IDE_REQUEST}\\\"}}\""
            ),
        );
        // 超长行：请求体已截断，但 `"model":`（行首）与 `"x-user-id"`（行尾）保留原位置。
        let agent_execution_failed = ide_line(
            "2026-09-17 10:28:26.741",
            &format!(
                "[AgentReporter] [{IDE_TRACE}]  Agent execution failed: {{\"name\":\"AI_APICallError\",\"url\":\"https://copilot.tencent.com/v2/chat/completions\",\"requestBodyValues\":{{\"model\":\"{IDE_MODEL}\"}},\"statusCode\":429,\"responseHeaders\":{{\"x-request-id\":\"{IDE_REQUEST}\",\"x-user-id\":\"{uid}\"}},\"responseBody\":\"{{\\\"code\\\":6004,\\\"msg\\\":\\\"{quota}\\\"}}\",\"message\":\"{quota}\",\"code\":6004}}"
            ),
        );
        let lines = [
            notify_step_error,
            agent_execution_failed,
            ide_line(
                "2026-09-17 10:28:26.746",
                &format!("[CraftInvokableAgent] [{IDE_TRACE}]  Execution failed: {quota}"),
            ),
            ide_line(
                "2026-09-17 10:28:26.746",
                &format!("[CraftInvokableAgent] [{IDE_TRACE}]  Agent call failed: {quota}"),
            ),
            ide_line(
                "2026-09-17 10:28:26.748",
                &format!("[handleAuthError] modelId={IDE_MODEL}, baseUrl=undefined, officialEndpoint=https://copilot.tencent.com, isCustomAuthFailure=false"),
            ),
            ide_line(
                "2026-09-17 10:28:26.749",
                &format!(
                    "[ResultHandler.handleError]  errorMessage {{\"requestId\":\"acp-{conversation}-1789612105908\",\"complete\":true,\"error\":{{\"code\":6004,\"message\":\"{quota}\",\"traceId\":\"{IDE_TRACE}\",\"model\":\"{IDE_MODEL}\"}},\"isEnd\":true,\"conversationId\":\"{conversation}\"}}"
                ),
            ),
            ide_line(
                "2026-09-17 10:28:26.751",
                &format!("[AcpAgent:{conversation}] Session error: {quota}"),
            ),
            ide_line(
                "2026-09-17 10:28:26.751",
                &format!(
                    "[AgentSessionManager] executeAsync FAILED: conversationId={conversation}, errorCode=6004, traceId={IDE_TRACE}, message={quota}"
                ),
            ),
            ide_line(
                "2026-09-17 10:28:26.751",
                &format!(
                    "[[acp-conn]] [AgentCraftError: {quota}]  [AcpConnection:{conversation}] Error handling request:"
                ),
            ),
        ];
        lines.join("\n")
    }

    fn ide_model_selection_line(conversation: &str, model: &str) -> String {
        ide_line(
            "2026-09-17 10:28:25.919",
            &format!(
                "[ModelSelection] conversationId={conversation}, mode=craft, modelId={model}, source=user-selected"
            ),
        )
    }

    /// IDE 限额文案行（只有文案与事件 id，不含模型线索）。
    fn ide_quota_line(timestamp: &str, conversation: &str) -> String {
        ide_line(
            timestamp,
            &format!("[AcpAgent:{conversation}] Session error: {IDE_QUOTA}"),
        )
    }

    #[test]
    fn byte_filter_matches_multibyte_and_ascii_markers() {
        assert!(contains(cn_quota().as_bytes(), QUOTA_MARKERS[0].as_bytes()));
        assert!(contains(
            b"x usage exceeds frequency limit y",
            QUOTA_MARKERS[1].as_bytes()
        ));
        assert!(!contains(b"nothing to see", QUOTA_MARKERS[1].as_bytes()));
        assert!(!contains(b"", QUOTA_MARKERS[0].as_bytes()));
        assert!(!contains(b"", b""));
        // Horspool 跳表的边界：needle 比 haystack 长、needle 内含重复字节、needle 在末尾命中。
        assert!(!contains(b"aaa", b"aaaa"));
        assert!(contains(b"aaaa", b"aaaa"));
        assert!(contains(b"abcabcabd", b"abcabd"));
        assert!(contains(b"xxababacyy", b"ababac"));
        assert!(!contains(b"xxabababyy", b"ababac"));
    }

    #[test]
    fn parses_chinese_and_english_reset_times() {
        assert_eq!(parse_reset_at(&cn_quota()), Some(CN_RESET_AT));
        assert_eq!(parse_reset_at(&en_quota()), Some(EN_RESET_AT));
        // 缺时区偏移的文案不猜时间。
        assert_eq!(
            parse_reset_at("429 您的使用量已超出频率限制，将在 2026-09-17 17:59:27 重置"),
            None
        );
    }

    #[test]
    fn parses_utc_offsets_the_official_text_may_use() {
        let at = |offset: &str| {
            parse_reset_at(&format!(
                "将在 2026-09-17 17:59:27 UTC{offset} 重置，您也可以切换其他模型继续使用。"
            ))
        };
        assert_eq!(at("+8"), Some(CN_RESET_AT));
        assert_eq!(at("+08"), Some(CN_RESET_AT));
        assert_eq!(at("+08:00"), Some(CN_RESET_AT));
        assert_eq!(at("+0800"), Some(CN_RESET_AT));
        assert_eq!(at("+7"), Some(CN_RESET_AT + 3_600_000));
        assert_eq!(at(""), None);
    }

    #[test]
    fn business_log_timestamp_is_local_while_sdk_log_timestamp_is_utc() {
        let local = Local
            .with_ymd_and_hms(2026, 9, 17, 0, 20, 31)
            .earliest()
            .expect("测试时间必须存在");
        assert_eq!(
            line_timestamp(
                &business_line("9/17/2026, 12:20:31 AM.232", "[Info] x"),
                LogFormat::WorkBuddy,
            ),
            Some(local.timestamp_millis() + 232)
        );
        assert_eq!(
            line_timestamp(
                "2026-09-16T16:20:31.523Z runtime.applyStopReason {}",
                LogFormat::WorkBuddy,
            ),
            Some(SDK_OCCURRED_AT)
        );
        assert_eq!(
            line_timestamp("no timestamp here", LogFormat::WorkBuddy),
            None
        );
    }

    #[test]
    fn parses_the_trailing_request_id_pair() {
        assert_eq!(
            request_pair(&cn_quota()),
            Some((CN_REQUEST.to_string(), CN_SESSION.to_string()))
        );
        assert_eq!(
            request_pair(&format!(
                "x ({CN_REQUEST}/{CN_SESSION}), lastPendingTool=(none)"
            )),
            Some((CN_REQUEST.to_string(), CN_SESSION.to_string())),
            "取行尾括号，忽略 `(none)` 这种非时间戳括号"
        );
        assert_eq!(request_pair("no pair here"), None);
        assert_eq!(
            request_pair(&format!("({CN_REQUEST}/{CN_REQUEST})")),
            None,
            "会话 id 不是 36 位 uuid 时不认"
        );
    }

    #[test]
    fn model_attribution_falls_back_request_id_then_session_then_unknown() {
        // ① 限额行给出 requestId，同文件有 `model=` + `requestId=` 的行。
        let with_request = [
            business_line(
                "9/17/2026, 12:20:03 AM.349",
                &config_line(CN_SESSION, "hy3"),
            ),
            business_line(
                "9/17/2026, 12:20:30 AM.838",
                &provider_line(CN_REQUEST, "deepseek-v4.1-flash"),
            ),
            business_line("9/17/2026, 12:20:31 AM.232", &cn_quota()),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&with_request));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("deepseek-v4.1-flash"));

        // ② 没有 requestId → model 行，回退到该会话最近一次 `resolved model=`。
        let with_session = [
            business_line(
                "9/17/2026, 12:20:03 AM.349",
                &config_line(CN_SESSION, "hy3"),
            ),
            business_line("9/17/2026, 12:20:31 AM.232", &cn_quota()),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&with_session));
        assert_eq!(events[0].model.as_deref(), Some("hy3"));

        // ③ 都取不到 → None（前端显示「未知模型」，绝不猜）。
        let without_model = business_line("9/17/2026, 12:20:31 AM.232", &cn_quota());
        let events = dedupe(workbuddy_hits(&without_model));
        assert_eq!(events[0].model, None);
    }

    #[test]
    fn attributes_the_limited_model_not_the_one_switched_to_afterwards() {
        let text = [
            business_line(
                "9/17/2026, 12:20:03 AM.349",
                &config_line(CN_SESSION, "hy3"),
            ),
            business_line(
                "9/17/2026, 12:20:30 AM.838",
                &provider_line(CN_REQUEST, "deepseek-v4.1-flash"),
            ),
            business_line("9/17/2026, 12:20:31 AM.232", &cn_quota()),
            // 被限之后用户切到了 hy3：不得归因成 hy3。
            business_line(
                "9/17/2026, 12:21:54 AM.388",
                &config_line(CN_SESSION, "hy3"),
            ),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&text));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("deepseek-v4.1-flash"));
    }

    #[test]
    fn request_options_model_is_not_mistaken_for_the_request_model() {
        let text = [
            business_line(
                "9/17/2026, 12:20:03 AM.349",
                &config_line("other", "kimi-k3-1"),
            ),
            business_line(
                "9/17/2026, 12:20:30 AM.838",
                &format!("[ModelProvider] requestId={CN_REQUEST}, requestOptions.model=glm-5.2"),
            ),
            business_line("9/17/2026, 12:20:31 AM.232", &cn_quota()),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&text));
        assert_eq!(
            events[0].model, None,
            "带点号的 `requestOptions.model=` 不是请求模型字段"
        );
    }

    #[test]
    fn classifier_line_supplies_the_session_when_the_pair_is_missing() {
        let text = [
            business_line(
                "9/17/2026, 12:20:31 AM.467",
                &format!(
                    "[ACP Agent] refusal classified: sessionId={CN_SESSION}, rpcCode=-32003, httpStatus=429, bizCode=6004, category=quota"
                ),
            ),
            business_line(
                "9/17/2026, 12:20:31 AM.468",
                "429 您的使用量已超出频率限制，将在 2026-09-17 17:59:27 UTC+8 重置，您也可以切换其他模型继续使用。",
            ),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&text));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id.as_deref(), Some(CN_SESSION));
    }

    #[test]
    fn recognizes_english_quota_text() {
        let text = [
            business_line(
                "9/14/2026, 10:59:00 AM.000",
                &config_line(EN_SESSION, "kimi-k3-1"),
            ),
            business_line("9/14/2026, 10:59:21 AM.523", &en_quota()),
        ]
        .join("\n");
        let events = dedupe(workbuddy_hits(&text));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reset_at, EN_RESET_AT);
        assert_eq!(events[0].model.as_deref(), Some("kimi-k3-1"));
        assert_eq!(events[0].session_id.as_deref(), Some(EN_SESSION));
    }

    /// AC4：本机最近 3 天 14 条限额行只是 2 次事件（每次事件业务日志 6 行 + SDK 冗余 1 行）。
    #[test]
    fn fourteen_duplicate_lines_collapse_into_two_events() {
        let root = temp_dir("dedupe");
        let business = root.join("2026-09-16");
        let sdk = root.join("2026-09-17").join("sdk").join("conversations");
        std::fs::create_dir_all(&business).expect("业务日志目录");
        std::fs::create_dir_all(&sdk).expect("SDK 日志目录");

        let event_lines = |session: &str,
                           request: &str,
                           reset: &str,
                           provider_at: &str,
                           quota_at: &str,
                           switched_at: &str| {
            let mut lines = vec![
                business_line(provider_at, &config_line(session, "deepseek-v4.1-flash")),
                business_line(provider_at, &provider_line(request, "deepseek-v4.1-flash")),
            ];
            for ms in ["232", "233", "233", "462", "467", "468"] {
                lines.push(business_line(
                    &format!("{quota_at}.{ms}"),
                    &cn_quota_with(session, request, reset),
                ));
            }
            lines.push(business_line(switched_at, &config_line(session, "hy3")));
            lines
        };

        // 与真机一致：两次事件的**重置时刻相同**，靠发生时刻相差 805s（≫15s 合并窗口）
        // 保持为两次事件——若这里用不同的重置时刻区分，就测不到「同 reset 不得误合并」。
        let first = event_lines(
            CN_SESSION,
            CN_REQUEST,
            "2026-09-17 17:59:27",
            "9/17/2026, 12:20:03 AM.349",
            "9/17/2026, 12:20:31 AM",
            "9/17/2026, 12:21:54 AM.388",
        );
        std::fs::write(business.join("event-one.log"), first.join("\n")).expect("写入业务日志");
        let second = event_lines(
            EN_SESSION,
            EN_REQUEST,
            "2026-09-17 17:59:27",
            "9/17/2026, 12:33:56 AM.520",
            "9/17/2026, 12:33:56 AM",
            "9/17/2026, 1:25:24 AM.523",
        );
        std::fs::write(business.join("event-two.log"), second.join("\n")).expect("写入业务日志");

        // SDK 侧对同一事件再写一份（UTC 时间戳、无模型行），会话 id 仍取得到。
        // 两次书写各随其业务日志的时刻（真机相差约 0.3s），否则两条 SDK 冗余会把
        // 相隔 13 分钟的事件拉进同一个 15s 窗口。
        for (session, quota, sdk_at) in [
            (CN_SESSION, &first[2], "2026-09-16T16:20:31.523Z"),
            (EN_SESSION, &second[2], "2026-09-16T16:33:56.520Z"),
        ] {
            std::fs::write(
                sdk.join(format!("{session}.log")),
                format!(
                    "{sdk_at} runtime.applyStopReason {{\"errorMessageMetaPreview\":\"{quota}\"}}"
                ),
            )
            .expect("写入 SDK 日志");
        }

        let events = collect_events(&root, LogFormat::WorkBuddy, AuthMarker::None);
        assert_eq!(events.len(), 2, "14 条限额行必须聚合为 2 次事件");
        assert_eq!(events[0].hit_count, 7);
        assert_eq!(events[1].hit_count, 7);
        assert_eq!(events[0].model.as_deref(), Some("deepseek-v4.1-flash"));
        assert_eq!(events[1].model.as_deref(), Some("deepseek-v4.1-flash"));
        assert_eq!(events[0].reset_at, CN_RESET_AT);
        assert_eq!(
            events[1].reset_at, CN_RESET_AT,
            "两次事件重置时刻相同，仍必须是两次事件"
        );
        assert_eq!(events[0].session_id.as_deref(), Some(CN_SESSION));
        assert_eq!(events[1].session_id.as_deref(), Some(EN_SESSION));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dedupe_keeps_sessions_apart_and_merges_only_within_fifteen_seconds() {
        let hit = |session: &str, occurred_at: i64, model: Option<&str>| Hit {
            session_id: Some(session.to_string()),
            model: model.map(str::to_string),
            reset_at: CN_RESET_AT,
            occurred_at,
            uid: None,
        };
        let events = dedupe(vec![
            // ① 步：同一会话的重复书写先合并。
            hit("session-a", 1_000_000, None),
            hit("session-a", 1_014_000, None),
            // ② 步：不同会话、重置时刻相同且相差 ≤15s，属同一次事件的另一次书写。
            hit("session-b", 1_003_000, Some("hy3")),
            // 相差 20s（超出合并窗口）的另一次限流不得被吞。
            hit("session-c", 1_020_000, None),
        ]);
        assert_eq!(events.len(), 2, "相差 20s 的第二次限流不得被合并");
        assert_eq!(events[0].hit_count, 3);
        assert_eq!(events[0].session_id.as_deref(), Some("session-a"));
        assert_eq!(
            events[0].model.as_deref(),
            Some("hy3"),
            "合并时保留有归因的那条"
        );
        assert_eq!(events[1].hit_count, 1);
        assert_eq!(events[1].session_id.as_deref(), Some("session-c"));
    }

    /// 收窗判据是文件 mtime 而非目录名：目录名很旧但文件在窗口内（跨天追加）必须纳入，
    /// 目录名很新但文件在窗口外必须排除；非日期目录整体忽略。
    #[test]
    fn window_selects_files_by_mtime_not_directory_name_and_tolerates_missing_roots() {
        let root = temp_dir("window");
        // 目录名很旧、文件刚写：真机「9/15 目录写到 9/16 07:59」的形态，必须纳入。
        let cross_day = root.join("2020-01-01");
        std::fs::create_dir_all(&cross_day).expect("跨天目录");
        std::fs::write(cross_day.join("cross-day.log"), "x").expect("跨天日志");
        // 目录名很新、文件 mtime 在窗口外：必须排除。
        let old_file = root.join("2099-12-31").join("old.log");
        std::fs::create_dir_all(old_file.parent().expect("父目录")).expect("未来目录");
        std::fs::write(&old_file, "x").expect("旧日志");
        set_days_old_mtime(&old_file, 30);
        // 非日期目录整体忽略（即使里面有窗口内的 .log）。
        let plain = root.join("memwatch");
        std::fs::create_dir_all(&plain).expect("非日期目录");
        std::fs::write(plain.join("mem.log"), "x").expect("非日期目录内日志");

        let cutoff = now_ms() - WINDOW_DAYS as i64 * DAY_MS;
        let names: BTreeSet<String> = windowed_log_files(&root, cutoff)
            .iter()
            .filter_map(|path| Some(path.file_name()?.to_string_lossy().to_string()))
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["cross-day.log".to_string()]),
            "旧目录里的新文件要收，新目录里的旧文件要排除，非日期目录整体忽略"
        );
        // 目录名不参与判定：cutoff 推到未来 → 空集；拉到 0 → 两个日期目录下的文件都收。
        assert!(windowed_log_files(&root, now_ms() + DAY_MS).is_empty());
        assert_eq!(windowed_log_files(&root, 0).len(), 2);
        assert!(
            windowed_log_files(&root.join("missing"), 0).is_empty(),
            "档位日志根缺失时返回空集，不是错误"
        );
        assert!(collect_events(
            &root.join("missing"),
            LogFormat::WorkBuddy,
            AuthMarker::None
        )
        .is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn payload_drops_expired_entries_and_sorts_by_reset_time() {
        let now = 1_000_000;
        let resolved = vec![
            Resolved {
                account_id: "a".to_string(),
                model: Some("late".to_string()),
                reset_at: now + 7_200_000,
                first_seen_at: 0,
                hit_count: 1,
            },
            Resolved {
                account_id: "a".to_string(),
                model: Some("soon".to_string()),
                reset_at: now + 600_000,
                first_seen_at: 0,
                hit_count: 3,
            },
            Resolved {
                account_id: "a".to_string(),
                model: Some("expired".to_string()),
                reset_at: now - 1,
                first_seen_at: 0,
                hit_count: 1,
            },
            Resolved {
                account_id: "b".to_string(),
                model: None,
                reset_at: now + 60_000,
                first_seen_at: 0,
                hit_count: 2,
            },
        ];
        let payload = build_payload(resolved, now);
        assert_eq!(payload["windowDays"], 2);
        assert_eq!(payload["scannedAt"], now);
        let accounts = payload["accounts"].as_array().expect("accounts 数组");
        assert_eq!(accounts.len(), 2, "无受限模型的账号不出现在结果里");
        assert_eq!(accounts[0]["accountId"], "a");
        let limited = accounts[0]["limited"].as_array().expect("limited 数组");
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0]["model"], "soon", "按恢复时间升序");
        assert_eq!(limited[1]["model"], "late");
        assert_eq!(accounts[1]["limited"][0]["model"], Value::Null);
    }

    #[test]
    fn payload_keeps_the_latest_reset_time_per_account_and_model() {
        let now = 1_000_000;
        let resolved = vec![
            Resolved {
                account_id: "a".to_string(),
                model: Some("hy3".to_string()),
                reset_at: now + 600_000,
                first_seen_at: 10,
                hit_count: 1,
            },
            Resolved {
                account_id: "a".to_string(),
                model: Some("hy3".to_string()),
                reset_at: now + 3_600_000,
                first_seen_at: 20,
                hit_count: 2,
            },
        ];
        let payload = build_payload(resolved, now);
        let limited = payload["accounts"][0]["limited"]
            .as_array()
            .expect("limited 数组");
        assert_eq!(limited.len(), 1, "同 (账号, 模型) 只保留一条");
        assert_eq!(limited[0]["resetAt"], now + 3_600_000);
        assert_eq!(limited[0]["hitCount"], 2);
    }

    // -----------------------------------------------------------------------
    // CodeBuddy CLI
    // -----------------------------------------------------------------------

    /// CLI 的事件账号取「该事件前最近一次鉴权行」的 uid：切换账号后不串号、也不回溯。
    #[test]
    fn cli_uid_comes_from_the_last_auth_line_before_the_event() {
        let text = [
            cli_auth_line("9/15/2026, 2:07:59 PM.057", UID_A),
            business_line(
                "9/15/2026, 2:08:10 PM.000",
                &cn_quota_with(CN_SESSION, CN_REQUEST, "2026-09-15 15:55:40"),
            ),
            // 切换账号：之后的事件必须归到新账号。
            cli_auth_line("9/15/2026, 2:09:00 PM.000", UID_B),
            business_line(
                "9/15/2026, 2:09:10 PM.000",
                &cn_quota_with(EN_SESSION, EN_REQUEST, "2026-09-15 16:55:40"),
            ),
        ]
        .join("\n");
        let hits = scan_text(&text, LogFormat::WorkBuddy, AuthMarker::AuthDoInitProbe);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].uid.as_deref(), Some(UID_A));
        assert_eq!(hits[1].uid.as_deref(), Some(UID_B));

        // 鉴权行在事件之后：不得回溯使用。
        let later = [
            business_line("9/15/2026, 2:08:10 PM.000", &cn_quota()),
            cli_auth_line("9/15/2026, 2:08:20 PM.000", UID_A),
        ]
        .join("\n");
        let hits = scan_text(&later, LogFormat::WorkBuddy, AuthMarker::AuthDoInitProbe);
        assert_eq!(hits[0].uid, None, "事件后的 uid 行不得回溯归因");

        // WorkBuddy 两档位不采集 uid（归因仍走 sessions 表）。
        let hits = workbuddy_hits(&later);
        assert_eq!(hits[0].uid, None);
    }

    /// `uid=none` 是「无会话 / 未登录」的哨兵值（CLI 与 IDE 都会写），必须忽略。
    #[test]
    fn auth_lines_with_uid_none_are_ignored() {
        let cli = [
            cli_auth_line("9/15/2026, 2:07:59 PM.057", "none"),
            business_line("9/15/2026, 2:08:10 PM.000", &cn_quota()),
        ]
        .join("\n");
        let hits = scan_text(&cli, LogFormat::WorkBuddy, AuthMarker::AuthDoInitProbe);
        assert_eq!(hits[0].uid, None);

        let ide = [
            ide_auth_line("none"),
            ide_quota_line("2026-09-17 10:28:26.751", IDE_CONV),
        ]
        .join("\n");
        let hits = scan_text(&ide, LogFormat::Ide, AuthMarker::AuthSessionChanged);
        assert_eq!(hits[0].uid, None);
    }

    /// CLI 日志目录复用日期目录枚举 + 递归收 `.log`（含 `sdk/conversations/` 一层）。
    #[test]
    fn cli_source_reuses_the_date_directory_enumeration() {
        let root = temp_dir("cli");
        let day = root.join("2026-09-15");
        let sdk = day.join("sdk").join("conversations");
        std::fs::create_dir_all(&sdk).expect("SDK 日志目录");
        std::fs::write(
            day.join("session.log"),
            [
                cli_auth_line("9/15/2026, 2:07:59 PM.057", UID_A),
                business_line("9/15/2026, 2:08:10 AM.000", &cn_quota()),
            ]
            .join("\n"),
        )
        .expect("业务日志");
        // SDK 侧对同一事件再写一份（同一会话 id）。
        std::fs::write(
            sdk.join("session.log"),
            business_line("9/15/2026, 2:08:10 AM.500", &cn_quota()),
        )
        .expect("SDK 日志");

        let events = collect_events(&root, LogFormat::WorkBuddy, AuthMarker::AuthDoInitProbe);
        assert_eq!(events.len(), 1, "业务日志与 SDK 冗余书写聚合为 1 条");
        assert_eq!(events[0].hit_count, 2);
        assert_eq!(events[0].reset_at, CN_RESET_AT);
        assert_eq!(events[0].uid.as_deref(), Some(UID_A));
        std::fs::remove_dir_all(&root).ok();
    }

    /// CLI 的日志根与状态文件都挂在用户主目录下（跨平台由 `home_dir()` 兜住）。
    #[test]
    fn cli_source_paths_live_under_the_home_directory() {
        assert!(cli_logs_root().ends_with(Path::new(CLI_DATA_DIR).join(LOG_DIR_NAME)));
        assert!(cli_state_path().ends_with(Path::new(CLI_ROTATE_DIR).join(CLI_STATE_FILE)));
    }

    // -----------------------------------------------------------------------
    // CodeBuddy IDE / CodeBuddy CN IDE
    // -----------------------------------------------------------------------

    /// IDE 行首时间戳是 23 字节定长前缀（本地时间，保留毫秒）；不是该形态就不猜时间。
    #[test]
    fn ide_timestamp_is_a_23_byte_local_prefix() {
        let at = Local
            .with_ymd_and_hms(2026, 9, 17, 10, 28, 26)
            .earliest()
            .expect("测试时间必须存在");
        let line = ide_quota_line("2026-09-17 10:28:26.730", IDE_CONV);
        assert_eq!(
            line_timestamp(&line, LogFormat::Ide),
            Some(at.timestamp_millis() + 730)
        );
        assert_eq!(
            line_timestamp("2026-09-17 10:28:26", LogFormat::Ide),
            None,
            "不足 23 字节的行首不得猜时间"
        );
    }

    /// 一次 429 的 8 行书写聚合为 1 条，且会话 id / 模型 / 重置时刻 / uid 全部正确。
    #[test]
    fn ide_event_lines_collapse_into_one_event_with_conversation_and_model() {
        let text = [
            ide_auth_line(UID_A),
            ide_model_selection_line(IDE_CONV, IDE_MODEL),
            ide_event_lines(IDE_CONV, UID_A),
        ]
        .join("\n");
        let events = dedupe(scan_text(
            &text,
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged,
        ));
        assert_eq!(events.len(), 1, "8 行带限额文案只是一次事件");
        assert_eq!(events[0].hit_count, 8);
        assert_eq!(events[0].session_id.as_deref(), Some(IDE_CONV));
        assert_eq!(events[0].model.as_deref(), Some(IDE_MODEL));
        assert_eq!(events[0].reset_at, CN_RESET_AT);
        assert_eq!(events[0].uid.as_deref(), Some(UID_A));
        // 发生时刻取最早的限额行（10:28:26.730）。
        let at = Local
            .with_ymd_and_hms(2026, 9, 17, 10, 28, 26)
            .earliest()
            .expect("测试时间必须存在");
        assert_eq!(events[0].first_seen_at, at.timestamp_millis() + 730);
    }

    /// 模型归因按会话隔离，且只取「事件之前」最近一次选用的模型。
    #[test]
    fn ide_model_is_attributed_per_conversation_and_before_the_event() {
        // 别的会话选过模型：不得归给本会话。
        let other = [
            ide_auth_line(UID_A),
            ide_model_selection_line(IDE_OTHER_CONV, "glm-5.2"),
            ide_quota_line("2026-09-17 10:28:26.751", IDE_CONV),
        ]
        .join("\n");
        let events = dedupe(scan_text(
            &other,
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged,
        ));
        assert_eq!(events[0].session_id.as_deref(), Some(IDE_CONV));
        assert_eq!(events[0].model, None, "不得把别的会话的模型归给本会话");

        // 被限之后用户切了模型：不得归因成切换后的模型。
        let switched = [
            ide_model_selection_line(IDE_CONV, IDE_MODEL),
            ide_quota_line("2026-09-17 10:28:26.751", IDE_CONV),
            ide_line(
                "2026-09-17 10:30:00.000",
                &format!(
                    "[AcpAgent:{IDE_CONV}] Model cache synced: mode=craft, modelId=glm-5.2, source=user-selected"
                ),
            ),
        ]
        .join("\n");
        let events = dedupe(scan_text(
            &switched,
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged,
        ));
        assert_eq!(events[0].model.as_deref(), Some(IDE_MODEL));
    }

    /// `modelId=auto` / `undefined` / `null`（会话尚未选定模型或字段缺失）按未知处理。
    #[test]
    fn ide_unknown_model_sentinels_are_treated_as_unknown() {
        for sentinel in IDE_MODEL_UNKNOWN {
            let text = [
                ide_model_selection_line(IDE_CONV, sentinel),
                ide_quota_line("2026-09-17 10:28:26.751", IDE_CONV),
            ]
            .join("\n");
            let events = dedupe(scan_text(
                &text,
                LogFormat::Ide,
                AuthMarker::AuthSessionChanged,
            ));
            assert_eq!(events[0].model, None, "`{sentinel}` 不得作为模型名展示");
        }
        // `[handleAuthError] modelId=undefined` 作为文件级兜底时同样不得展示。
        let text = [
            ide_line(
                "2026-09-17 10:28:26.748",
                "[handleAuthError] modelId=undefined, baseUrl=undefined, officialEndpoint=https://copilot.tencent.com",
            ),
            ide_quota_line("2026-09-17 10:28:26.751", IDE_CONV),
        ]
        .join("\n");
        let events = dedupe(scan_text(
            &text,
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged,
        ));
        assert_eq!(events[0].model, None);
    }

    /// 鉴权行缺失时，由事件行内的 `x-user-id` 补位（同一事件的其余行随之归到该账号）。
    #[test]
    fn x_user_id_fills_the_uid_when_no_auth_line_exists() {
        let text = ide_event_lines(IDE_CONV, UID_A);
        let events = dedupe(scan_text(
            &text,
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged,
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid.as_deref(), Some(UID_A));
    }

    /// 约 350 KB 的 `Agent execution failed` 行（含完整请求体）：只在定长窗口内嗅探字段。
    #[test]
    fn ide_long_error_line_is_sniffed_within_bounded_windows() {
        let filler = "x".repeat(350 * 1024);
        let line = format!(
            "2026-09-17 10:28:26.741 [error] [AgentReporter] [{IDE_TRACE}]  Agent execution failed: {{\"name\":\"AI_APICallError\",\"requestBodyValues\":{{\"model\":\"{IDE_MODEL}\",\"messages\":[{{\"content\":\"{filler}\"}}]}},\"statusCode\":429,\"responseHeaders\":{{\"x-user-id\":\"{UID_A}\"}},\"responseBody\":\"{{\\\"msg\\\":\\\"{IDE_QUOTA}\\\"}}\",\"code\":6004}}"
        );
        assert!(line.len() > 350 * 1024);

        let hits = scan_text(&line, LogFormat::Ide, AuthMarker::AuthSessionChanged);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].uid.as_deref(),
            Some(UID_A),
            "行尾窗口内的 x-user-id"
        );
        assert_eq!(
            hits[0].model.as_deref(),
            Some(IDE_MODEL),
            "行首窗口内的 model"
        );
        assert_eq!(hits[0].reset_at, CN_RESET_AT);
    }

    /// JSON 形态的事件 id 覆盖两种书写：未转义的 `"conversationId":"…"`（本机 `ResultHandler`
    /// 行）与嵌在字符串里的转义形式 `\"conversationId\":\"…\"`（`responseBody: "…"` 这类行）。
    #[test]
    fn ide_conversation_id_covers_quoted_json_in_both_escapes() {
        let plain = ide_line(
            "2026-09-17 10:28:26.749",
            &format!(
                "[ResultHandler.handleError]  errorMessage {{\"isEnd\":true,\"conversationId\":\"{IDE_CONV}\"}}"
            ),
        );
        assert_eq!(ide_conversation_id(&plain), Some(IDE_CONV));

        let escaped = ide_line(
            "2026-09-17 10:28:26.749",
            &format!(
                "[BaseAgent:craft] responseBody: \"{{\\\"code\\\":6004,\\\"conversationId\\\":\\\"{IDE_CONV}\\\"}}\""
            ),
        );
        assert_eq!(ide_conversation_id(&escaped), Some(IDE_CONV));
    }

    /// 载荷被截断（值没有闭合引号）时不得把半个值当成结果：宁可「未知模型」，也不能展示
    /// 掐头去尾的模型名。定长窗口的边界同理——`"modelId"` 之类相似键也不得误命中。
    #[test]
    fn json_sniffing_rejects_values_without_a_closing_quote() {
        assert_eq!(json_field("{\"model\":\"deepseek-v4.1-fl", "model"), None);
        assert_eq!(json_field("{\"modelId\":\"auto\"}", "model"), None);

        // `"model":"` 落在行尾窗口里、值被写出方截断（超长行）。补齐闭合引号后才可用。
        let mut line = String::from("2026-09-17 10:28:26.741 [error] [AgentReporter]  ");
        line.push_str(IDE_QUOTA);
        line.push_str(&"y".repeat(LONG_LINE_SNIFF_BYTES));
        line.push_str("\"model\":\"deepseek-v4.1-fl");
        assert!(line.len() > LONG_LINE_SNIFF_BYTES);
        assert_eq!(json_field(&line, "model"), None);
        assert_eq!(ide_json_model(&line), None);

        let hits = scan_text(&line, LogFormat::Ide, AuthMarker::AuthSessionChanged);
        assert_eq!(hits.len(), 1, "截断的行仍是一次限额命中");
        assert_eq!(hits[0].model, None, "截断的半个模型名不得作为模型展示");
    }

    /// IDE 枚举：只认 `exthost/Tencent-Cloud.coding-copilot/` 下的文件（不限文件名），
    /// 并按**文件 mtime** 收窗——会话目录名是启动时间，按目录名收窗会漏事件。
    #[test]
    fn ide_enumeration_filters_by_plugin_dir_and_file_mtime() {
        let root = temp_dir("ide-enum");
        // 会话目录名是「旧的启动时间」，但文件是刚刚写的：真机那次 429 正是这种情形。
        let window = root.join("20260916T193500").join("window9");
        let copilot = window.join("exthost").join(IDE_LOG_DIR_NAME);
        std::fs::create_dir_all(&copilot).expect("插件日志目录");
        // 三种文件名（含 `.1.log` 轮转与国际版命名）都必须收——文件名不写死。
        let names = [
            "腾讯云代码助手.log",
            "腾讯云代码助手.1.log",
            "Tencent Cloud CodeBuddy.log",
        ];
        for name in names {
            std::fs::write(copilot.join(name), IDE_QUOTA).expect("插件日志");
        }
        // 插件目录之外的同级日志（渲染进程 / exthost 主日志）不得收。
        std::fs::write(window.join("renderer.log"), IDE_QUOTA).expect("渲染进程日志");
        std::fs::write(window.join("exthost").join("exthost.log"), IDE_QUOTA)
            .expect("exthost 主日志");

        let files = ide_log_files(&root, now_ms() - WINDOW_DAYS as i64 * DAY_MS);
        let found: BTreeSet<String> = files
            .iter()
            .filter_map(|path| Some(path.file_name()?.to_string_lossy().to_string()))
            .collect();
        assert_eq!(
            found,
            names.map(str::to_string).into_iter().collect(),
            "只收插件目录下的文件，且不限文件名/扩展名"
        );

        // mtime 收窗：窗口起点在未来 → 一个都不收（证明是按文件 mtime，而不是目录名）。
        assert!(ide_log_files(&root, now_ms() + DAY_MS).is_empty());
        // 日志根缺失（该 IDE 未安装/未使用）不报错。
        assert!(ide_log_files(&root.join("missing"), 0).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    /// IDE 来源端到端（枚举 → 粗筛 → 解析 → 去重），空根/无候选文件都不报错。
    #[test]
    fn ide_source_collects_events_and_tolerates_empty_roots() {
        let root = temp_dir("ide-source");
        let copilot = root
            .join("20260917T102952")
            .join("window3")
            .join("exthost")
            .join(IDE_LOG_DIR_NAME);
        std::fs::create_dir_all(&copilot).expect("插件日志目录");
        std::fs::write(
            copilot.join("腾讯云代码助手.log"),
            [
                ide_auth_line(UID_A),
                ide_model_selection_line(IDE_CONV, IDE_MODEL),
                ide_event_lines(IDE_CONV, UID_A),
            ]
            .join("\n"),
        )
        .expect("插件日志");

        let events = collect_events(&root, LogFormat::Ide, AuthMarker::AuthSessionChanged);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].hit_count, 8);
        assert_eq!(events[0].session_id.as_deref(), Some(IDE_CONV));
        assert!(collect_events(
            &root.join("missing"),
            LogFormat::Ide,
            AuthMarker::AuthSessionChanged
        )
        .is_empty());

        let empty = temp_dir("ide-empty");
        std::fs::create_dir_all(&empty).expect("空目录");
        assert!(collect_events(&empty, LogFormat::Ide, AuthMarker::AuthSessionChanged).is_empty());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    /// 两个 IDE 的日志根各自指向自己的数据目录（平台分支在 `codebuddy_ide_data_dir` 内）。
    #[test]
    fn the_two_ide_sources_use_distinct_data_roots() {
        let (Some(intl), Some(cn)) = (
            codebuddy_ide_data_dir(CodeBuddyIdeFlavor::Intl),
            codebuddy_ide_data_dir(CodeBuddyIdeFlavor::Cn),
        ) else {
            // 该平台定位不到配置目录时不适用（与运行时代码同样跳过）。
            return;
        };
        assert_ne!(intl, cn);
        assert_eq!(
            intl.file_name().and_then(|name| name.to_str()),
            Some("CodeBuddy")
        );
        assert_eq!(
            cn.file_name().and_then(|name| name.to_str()),
            Some("CodeBuddy CN")
        );
    }

    // -----------------------------------------------------------------------
    // 新来源的账号归因（uid 优先，状态文件带 updatedAt 门控回落）
    // -----------------------------------------------------------------------

    fn event_without_uid(first_seen_at: i64) -> Event {
        Event {
            session_id: None,
            model: None,
            reset_at: CN_RESET_AT,
            first_seen_at,
            hit_count: 1,
            uid: None,
        }
    }

    /// 回落门控：只有 `updatedAt ≤ 事件时刻` 才可信；缺失/过晚/损坏一律丢弃（不误归）。
    #[test]
    fn fallback_state_file_only_counts_when_updated_before_the_event() {
        let dir = temp_dir("fallback");
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join(CLI_STATE_FILE);
        let events = [event_without_uid(1_500)];
        let no_uid = HashMap::new();

        // ① uid 不可用 + 状态文件缺失 → 丢弃。
        assert!(resolve_by_uid(&events, &no_uid, &ActiveAccount::load(&path)).is_empty());

        // ② `updatedAt` 晚于事件 → 不可信：IDE 的 detect 会回写状态，但那不是账号切换。
        std::fs::write(
            &path,
            json!({ "activeAccountId": "acc-1", "updatedAt": 2_000 }).to_string(),
        )
        .expect("状态文件");
        assert!(resolve_by_uid(&events, &no_uid, &ActiveAccount::load(&path)).is_empty());

        // ③ `updatedAt` 早于事件 → 该时刻生效的账号可用。
        std::fs::write(
            &path,
            json!({ "activeAccountId": "acc-1", "updatedAt": 1_000 }).to_string(),
        )
        .expect("状态文件");
        let resolved = resolve_by_uid(&events, &no_uid, &ActiveAccount::load(&path));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].account_id, "acc-1");

        // ④ 缺 `updatedAt`（老状态文件）→ 无从证实「当时是谁」，丢弃。
        std::fs::write(&path, json!({ "activeAccountId": "acc-1" }).to_string()).expect("状态文件");
        assert!(resolve_by_uid(&events, &no_uid, &ActiveAccount::load(&path)).is_empty());

        // ⑤ 内容损坏 → 回落不可用，不报错。
        std::fs::write(&path, "{ not json").expect("状态文件");
        assert!(resolve_by_uid(&events, &no_uid, &ActiveAccount::load(&path)).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 日志内 uid 命中账号库时优先于状态文件回落（AC8：不按「当前 activeAccountId」误归）。
    #[test]
    fn uid_match_wins_over_the_state_file_fallback() {
        let dir = temp_dir("uid-wins");
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join(IDE_STATE_FILE);
        std::fs::write(
            &path,
            json!({ "activeAccountId": "acc-stale", "updatedAt": 1 }).to_string(),
        )
        .expect("状态文件");

        let uid_to_account = HashMap::from([(UID_A.to_string(), "acc-uid".to_string())]);
        let mut event = event_without_uid(1_500);
        event.uid = Some(UID_A.to_string());
        let resolved = resolve_by_uid(&[event], &uid_to_account, &ActiveAccount::load(&path));
        assert_eq!(resolved[0].account_id, "acc-uid");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 归因不到的 uid（账号库没有该账号）不得误配给任何账号。
    #[test]
    fn unknown_uid_is_dropped_without_a_fallback() {
        let mut event = event_without_uid(1_500);
        event.uid = Some(UID_A.to_string());
        let resolved = resolve_by_uid(
            &[event],
            &HashMap::from([(UID_B.to_string(), "acc-b".to_string())]),
            &ActiveAccount {
                account_id: None,
                updated_at: None,
            },
        );
        assert!(resolved.is_empty());
    }

    /// 扫描范围是位图：缓存命中要求「缓存范围 ⊇ 请求范围」，且受 5 分钟节流约束。
    #[test]
    fn scan_scope_and_cache_require_a_superset() {
        let mut cli = ScanScope::NONE;
        cli.insert(ScanSource::Cli);
        let mut ides = ScanScope::NONE;
        ides.insert(ScanSource::Ide(CodeBuddyIdeFlavor::Intl));
        ides.insert(ScanSource::Ide(CodeBuddyIdeFlavor::Cn));

        assert!(ScanScope::NONE.covers(ScanScope::NONE));
        assert!(!ScanScope::NONE.covers(cli));
        assert!(ScanScope::ALL.covers(ides));
        assert!(!ides.covers(ScanScope::ALL));
        assert!(!ides.covers(cli));
        // 位图按来源独立：插入 IDE 不影响 CLI。
        assert!(ides.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));
        assert!(!ides.contains(ScanSource::WorkBuddy(WbVariant::Ai)));

        let now = 1_000_000;
        let cache = ScanCache {
            at: now,
            scope: ScanScope::ALL,
            entries: Vec::new(),
        };
        assert!(
            cache.covers(ides, now + SCAN_MIN_INTERVAL_MS - 1),
            "未过期且范围是超集 → 命中（装 hook 后只扫 IDE 的请求由全量缓存满足）"
        );
        assert!(
            !cache.covers(ides, now + SCAN_MIN_INTERVAL_MS),
            "过期缓存不命中"
        );
        let narrow = ScanCache {
            at: now,
            scope: ides,
            entries: Vec::new(),
        };
        assert!(
            !narrow.covers(ScanScope::ALL, now),
            "范围不足（子集）不命中，必须重扫"
        );
        assert!(narrow.covers(ides, now), "同一范围命中");
    }

    /// 测试用扫描范围输入：`dir` 下按固定名字放 CLI / WorkBuddy 两档位 / 两个 IDE 的数据根。
    ///
    /// 只拼路径，不建目录 / 文件：存在性与配置内容由各测试自己决定。
    fn scan_roots_for(dir: &Path, marker: &str) -> ScanRoots {
        let cli_root = dir.join(".codebuddy");
        let wb_cn = dir.join(".workbuddy");
        let wb_ai = dir.join(".workbuddy-ai");
        ScanRoots {
            hook_sources: vec![
                (
                    ScanSource::WorkBuddy(WbVariant::Cn),
                    wb_cn.clone(),
                    wb_cn.join(SETTINGS_FILE_NAME),
                ),
                (
                    ScanSource::WorkBuddy(WbVariant::Ai),
                    wb_ai.clone(),
                    wb_ai.join(SETTINGS_FILE_NAME),
                ),
                (
                    ScanSource::Cli,
                    cli_root.clone(),
                    cli_root.join(SETTINGS_FILE_NAME),
                ),
            ],
            ide_sources: vec![
                (
                    ScanSource::Ide(CodeBuddyIdeFlavor::Intl),
                    dir.join("CodeBuddy"),
                ),
                (
                    ScanSource::Ide(CodeBuddyIdeFlavor::Cn),
                    dir.join("CodeBuddy CN"),
                ),
            ],
            marker: marker.to_string(),
        }
    }

    /// 已注册本工具 hook 的 `settings.json` 内容（marker = 脚本绝对路径）。
    fn registered_settings(marker: &str) -> String {
        json!({
            "hooks": {
                "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": format!("sh '{marker}'") }] }],
                "FinalStop": [{ "matcher": "", "hooks": [{ "type": "command", "command": format!("sh '{marker}'") }] }],
            }
        })
        .to_string()
    }

    /// 逐来源判定扫描范围（注入 tempdir 路径，不触碰真实配置）：
    ///
    /// - 数据根不存在 → 不扫（也不因其缺失而报错）；
    /// - 数据根存在且该处未注册 hook → 扫（某处注册失败只有那一处回退）；
    /// - 数据根存在且已注册 hook → 不扫（交给事件通路）；
    /// - IDE 数据根存在且 `scanIdeLogs` 开启 → 扫（IDE 的 429 不触发任何事件）。
    #[test]
    fn scan_scope_is_decided_per_source() {
        let dir = temp_dir("scan-scope");
        let marker = dir.join(".wb-switch").join("hook.sh");
        let marker = marker.to_string_lossy().to_string();
        let registered = registered_settings(&marker);

        let cli_root = dir.join(".codebuddy");
        let wb_cn = dir.join(".workbuddy");
        let ide_intl = dir.join("CodeBuddy");
        let ide_cn = dir.join("CodeBuddy CN");
        // 已安装：CLI（已注册）、WorkBuddy 国内版（配置里**没有** marker）、两个 IDE。
        std::fs::create_dir_all(&cli_root).expect("CLI 数据根");
        std::fs::write(cli_root.join(SETTINGS_FILE_NAME), &registered).expect("CLI 配置");
        std::fs::create_dir_all(&wb_cn).expect("WorkBuddy 数据根");
        std::fs::write(wb_cn.join(SETTINGS_FILE_NAME), "{}").expect("WorkBuddy 配置");
        std::fs::create_dir_all(&ide_intl).expect("IDE 数据根");
        std::fs::create_dir_all(&ide_cn).expect("CN IDE 数据根");

        let roots = scan_roots_for(&dir, &marker);

        let scope = scan_scope(&roots, true);
        assert!(
            !scope.contains(ScanSource::Cli),
            "已注册 hook 的来源不扫日志"
        );
        assert!(
            scope.contains(ScanSource::WorkBuddy(WbVariant::Cn)),
            "未注册的来源回退日志扫描"
        );
        assert!(
            !scope.contains(ScanSource::WorkBuddy(WbVariant::Ai)),
            "不存在的数据根不参与扫描，也不因其缺失报错"
        );
        assert!(scope.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Intl)));
        assert!(scope.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));

        // 用户手删了 CLI 的条目 → 只有 CLI 回退扫描，其余来源不受影响。
        std::fs::write(cli_root.join(SETTINGS_FILE_NAME), "{}").expect("手删条目");
        let scope = scan_scope(&roots, true);
        assert!(
            scope.contains(ScanSource::Cli),
            "条目被删的来源必须回退扫描"
        );
        assert!(scope.contains(ScanSource::WorkBuddy(WbVariant::Cn)));

        // 某处配置写坏（非法 JSON）按「未注册」处理 → 该来源回退扫描。
        std::fs::write(cli_root.join(SETTINGS_FILE_NAME), "{ not json").expect("写坏配置");
        assert!(scan_scope(&roots, true).contains(ScanSource::Cli));

        // 注册齐全 + 数据根都在 → 只剩两个 IDE。
        std::fs::write(cli_root.join(SETTINGS_FILE_NAME), &registered).expect("重新注册");
        std::fs::write(wb_cn.join(SETTINGS_FILE_NAME), &registered).expect("注册 WorkBuddy");
        let scope = scan_scope(&roots, true);
        assert!(!scope.contains(ScanSource::Cli));
        assert!(!scope.contains(ScanSource::WorkBuddy(WbVariant::Cn)));
        assert!(scope.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Intl)));
        assert!(scope.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `scanIdeLogs = false` **只关两个 IDE**：CLI / WorkBuddy 的逐来源判定完全不变；
    /// 强制全量（装 / 卸 hook）的「全量」上限也不含 IDE。
    #[test]
    fn ide_scan_switch_excludes_only_the_two_ide_sources() {
        let dir = temp_dir("ide-switch");
        let marker = dir.join(".wb-switch").join("hook.sh");
        let marker = marker.to_string_lossy().to_string();
        let registered = registered_settings(&marker);

        let cli_root = dir.join(".codebuddy");
        let wb_cn = dir.join(".workbuddy");
        std::fs::create_dir_all(&cli_root).expect("CLI 数据根");
        std::fs::write(cli_root.join(SETTINGS_FILE_NAME), &registered).expect("CLI 配置");
        std::fs::create_dir_all(&wb_cn).expect("WorkBuddy 数据根");
        std::fs::write(wb_cn.join(SETTINGS_FILE_NAME), "{}").expect("WorkBuddy 配置");
        std::fs::create_dir_all(dir.join("CodeBuddy")).expect("IDE 数据根");
        std::fs::create_dir_all(dir.join("CodeBuddy CN")).expect("CN IDE 数据根");

        let roots = scan_roots_for(&dir, &marker);

        // 关闭：两个 IDE 一个都不扫；CLI（已注册）仍不扫、WorkBuddy（未注册）仍回退扫描。
        let off = scan_scope(&roots, false);
        assert!(!off.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Intl)));
        assert!(!off.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));
        assert!(
            !off.contains(ScanSource::Cli),
            "已注册 hook 的判定不因 IDE 开关改变"
        );
        assert!(
            off.contains(ScanSource::WorkBuddy(WbVariant::Cn)),
            "未接 hook 的档位仍回退日志扫描"
        );

        // 重新开启 → 两个 IDE 回到范围（是关闭时的超集）。
        let on = scan_scope(&roots, true);
        assert!(on.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Intl)));
        assert!(on.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));
        assert!(on.covers(off), "开启是关闭的超集");

        // 强制全量的边界：IDE 关闭时只能扫「hook 通路来源」。
        assert!(ScanScope::ALL.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));
        assert!(ScanScope::HOOK_SOURCES.contains(ScanSource::Cli));
        assert!(ScanScope::HOOK_SOURCES.contains(ScanSource::WorkBuddy(WbVariant::Ai)));
        assert!(!ScanScope::HOOK_SOURCES.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Intl)));
        assert!(!ScanScope::HOOK_SOURCES.contains(ScanSource::Ide(CodeBuddyIdeFlavor::Cn)));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 缓存失效分语义 + 保存配置的范围同步。
    ///
    /// 本测试是唯一操作进程级 `SCAN_CACHE` / `FORCE_FULL_SCAN` 的测试：两者是全局静态，
    /// 拆成多个测试会并行互相干扰。
    #[test]
    fn cache_clearing_semantics_and_config_scope_sync() {
        let dir = temp_dir("cache-sync");
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join("rate_limit_config.json");
        crate::modules::config::save_rate_limit_config_at(&path, &json!({ "scanIdeLogs": true }))
            .expect("写入配置");

        // ① 只清缓存（开关类变更）：缓存清掉，但不置「强制全量」。
        *SCAN_CACHE.lock().unwrap() = Some(ScanCache {
            at: 1,
            scope: ScanScope::ALL,
            entries: Vec::new(),
        });
        FORCE_FULL_SCAN.store(false, Ordering::SeqCst);
        clear_scan_cache();
        assert!(SCAN_CACHE.lock().unwrap().is_none(), "缓存必须清掉");
        assert!(
            !FORCE_FULL_SCAN.load(Ordering::SeqCst),
            "开关类变更不得强制全量扫"
        );

        // ② 装 / 卸 hook 的 invalidate：清缓存 + 强制全量。
        invalidate_scan_cache();
        assert!(SCAN_CACHE.lock().unwrap().is_none());
        assert!(
            FORCE_FULL_SCAN.load(Ordering::SeqCst),
            "装 / 卸 hook 仍强制全量一次"
        );

        // ③ 保存配置：`scanIdeLogs` 变化才清缓存；其它字段变化不动缓存，也不置标志。
        FORCE_FULL_SCAN.store(false, Ordering::SeqCst);
        *SCAN_CACHE.lock().unwrap() = Some(ScanCache {
            at: 1,
            scope: ScanScope::ALL,
            entries: Vec::new(),
        });
        save_rate_limit_config_at(&path, &json!({ "scanIdeLogs": true, "enabled": false }))
            .expect("保存其它字段");
        assert!(
            SCAN_CACHE.lock().unwrap().is_some(),
            "非范围字段变化不清缓存"
        );

        save_rate_limit_config_at(&path, &json!({ "scanIdeLogs": false }))
            .expect("关闭 IDE 日志扫描");
        assert!(
            SCAN_CACHE.lock().unwrap().is_none(),
            "scanIdeLogs 变化必须清缓存"
        );
        assert!(
            !FORCE_FULL_SCAN.load(Ordering::SeqCst),
            "保存配置只清缓存，不强制全量"
        );
        assert_eq!(
            crate::modules::config::load_rate_limit_config_at(&path)
                .get("scanIdeLogs")
                .and_then(Value::as_bool),
            Some(false)
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
