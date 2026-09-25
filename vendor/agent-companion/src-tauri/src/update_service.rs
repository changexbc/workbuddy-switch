//! 独立版应用内更新：检查 / 下载 / 安装重启的单一状态机。
//!
//! 设置页与托盘菜单都只读这一份快照（`update-state` 事件 + `update_state` 命令），
//! 避免两处状态分叉。更新包必须通过 Tauri updater 的签名校验后才会暂存到内存，
//! 安装与重启只在用户明确点击后发生；进程重启会丢弃内存中的包，界面回到重新检查
//! 或下载的路径。
//!
//! 代理单独保存在应用配置目录的 `update-config.json`（`{"proxy":""}`），检查与下载
//! 都经同一个 updater builder 使用它。代理地址可能带凭据，因此不写入日志、错误文案
//! 或任何其他地方。

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;

/// 后台首次检查延迟：启动 15 秒后。
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);
/// 后台检查间隔。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 状态事件名，负载为 [`UpdateSnapshot`]。
pub const STATE_EVENT: &str = "update-state";
/// 公开更新源，设置页展示用。
pub const RELEASE_URL: &str = "https://github.com/changexbc/agent-companion/releases/latest";
const CONFIG_FILE: &str = "update-config.json";
const BUSY_MESSAGE: &str = "更新任务正在进行中，请稍候";
const NO_PACKAGE_MESSAGE: &str = "没有可安装的更新包，请先下载更新包";
const MISSING_PUBKEY_MESSAGE: &str = "当前构建未配置更新签名公钥，无法使用应用内更新";
const PROXY_INVALID_MESSAGE: &str = "更新代理地址无效，请填写 HTTP 或 HTTPS 地址，例如 http://127.0.0.1:7897";
const CONFIG_BROKEN_MESSAGE: &str = "更新配置无法读取，请在设置中重新保存更新代理";

/// 更新阶段。设置页与托盘菜单共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    ReadyToRestart,
    Error,
}

/// 更新状态快照：命令返回值、`update-state` 事件负载与托盘文案的唯一来源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSnapshot {
    pub phase: UpdatePhase,
    /// 目标版本号（无 `v` 前缀）；已是最新或失败且无目标版本时为 None。
    pub latest: Option<String>,
    /// 下载进度 0-100；总量未知时为 None。
    pub percent: Option<u8>,
    /// 错误或提示文案；复查失败但已保留目标版本时，失败文案也写在这里。
    pub message: Option<String>,
    /// 最近一次检查完成时刻（Unix 秒）。
    pub checked_at: Option<i64>,
}

impl UpdateSnapshot {
    fn idle() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            latest: None,
            percent: None,
            message: None,
            checked_at: None,
        }
    }
}

/// 设置页要展示的更新配置：代理、当前版本与公开更新源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfig {
    pub proxy: String,
    pub current_version: String,
    pub release_url: String,
}

/// 已下载的更新包。`bytes` 交给 `Update::install`，`update` 提供安装上下文
/// （安装目标路径、安装器参数），无法在外部重建。
struct DownloadedPackage {
    version: String,
    bytes: Vec<u8>,
    update: tauri_plugin_updater::Update,
}

/// 更新服务状态。由独立版宿主管理；插件 crate 不持有它。
pub struct UpdateService {
    snapshot: Mutex<UpdateSnapshot>,
    package: Mutex<Option<DownloadedPackage>>,
    busy: AtomicBool,
}

impl Default for UpdateService {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(UpdateSnapshot::idle()),
            package: Mutex::new(None),
            busy: AtomicBool::new(false),
        }
    }
}

/// 互斥 guard：检查 / 下载 / 安装全过程独占，drop 即释放。
struct BusyGuard {
    service: Arc<UpdateService>,
}

impl BusyGuard {
    fn acquire(service: &Arc<UpdateService>) -> Option<Self> {
        service
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self {
                service: service.clone(),
            })
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.service.busy.store(false, Ordering::Release);
    }
}

fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

impl UpdateService {
    /// 当前快照。
    pub fn current(&self) -> UpdateSnapshot {
        lock(&self.snapshot).clone()
    }

    fn edit(&self, change: impl FnOnce(&mut UpdateSnapshot)) -> UpdateSnapshot {
        let mut snapshot = lock(&self.snapshot);
        change(&mut snapshot);
        snapshot.clone()
    }

    /// 进入检查阶段。下载中或待重启不被后台检查改写，否则用户正要点的入口会闪掉。
    fn begin_check(&self) -> UpdateSnapshot {
        self.edit(|snapshot| {
            if matches!(
                snapshot.phase,
                UpdatePhase::Downloading | UpdatePhase::ReadyToRestart
            ) {
                return;
            }
            snapshot.phase = UpdatePhase::Checking;
            snapshot.percent = None;
            snapshot.message = None;
        })
    }

    /// 写入检查结果：`latest` 为 Some 表示有新版本。
    fn apply_check(&self, latest: Option<String>) -> UpdateSnapshot {
        self.edit(|snapshot| {
            if matches!(
                snapshot.phase,
                UpdatePhase::Downloading | UpdatePhase::ReadyToRestart
            ) {
                return;
            }
            snapshot.percent = None;
            snapshot.message = None;
            snapshot.checked_at = Some(now_secs());
            snapshot.phase = if latest.is_some() {
                UpdatePhase::Available
            } else {
                UpdatePhase::UpToDate
            };
            snapshot.latest = latest;
        })
    }

    /// 检查失败。已有目标版本时保留 Available 与下载入口，但失败文案必须留在快照里，
    /// 让设置页同时显示「有新版本可下载」和「本次检查失败」。
    fn apply_failure(&self, message: &str) -> UpdateSnapshot {
        self.edit(|snapshot| {
            if matches!(
                snapshot.phase,
                UpdatePhase::Downloading | UpdatePhase::ReadyToRestart
            ) {
                return;
            }
            snapshot.percent = None;
            if snapshot.latest.is_some()
                && matches!(snapshot.phase, UpdatePhase::Checking | UpdatePhase::Available)
            {
                snapshot.phase = UpdatePhase::Available;
                snapshot.message = Some(message.to_string());
                return;
            }
            snapshot.phase = UpdatePhase::Error;
            snapshot.latest = None;
            snapshot.message = Some(message.to_string());
        })
    }

    /// 进入下载阶段；整数百分比由分包回调填入。
    fn begin_download(&self, latest: &str) -> UpdateSnapshot {
        self.edit(|snapshot| {
            snapshot.phase = UpdatePhase::Downloading;
            snapshot.latest = Some(latest.to_string());
            snapshot.percent = None;
            snapshot.message = None;
        })
    }

    /// 记录一次进度回调：整数百分比变化才写快照（未变化返回 None）。
    fn advance_progress(&self, downloaded: u64, total: Option<u64>) -> Option<u8> {
        let percent = percent_of(downloaded, total)?;
        let mut changed = false;
        self.edit(|snapshot| {
            if snapshot.percent == Some(percent) {
                return;
            }
            snapshot.percent = Some(percent);
            changed = true;
        });
        changed.then_some(percent)
    }

    /// 下载完成：包已暂存，等待用户确认后安装。
    fn mark_ready_to_restart(&self, latest: &str) -> UpdateSnapshot {
        self.edit(|snapshot| {
            snapshot.phase = UpdatePhase::ReadyToRestart;
            snapshot.latest = Some(latest.to_string());
            snapshot.percent = Some(100);
            snapshot.message = None;
        })
    }

    /// 失败：置 Error 并保留 `latest`，界面据此区分「检查失败」与「下载失败」。
    fn fail(&self, message: impl Into<String>) -> UpdateSnapshot {
        self.edit(|snapshot| {
            snapshot.phase = UpdatePhase::Error;
            snapshot.percent = None;
            snapshot.message = Some(message.into());
        })
    }

    fn store_package(&self, package: DownloadedPackage) {
        *lock(&self.package) = Some(package);
    }

    fn take_package(&self) -> Option<DownloadedPackage> {
        lock(&self.package).take()
    }
}

/// 下载进度换算，总量未知或为 0 时返回 None，超出总量按 100 截断。
fn percent_of(downloaded: u64, total: Option<u64>) -> Option<u8> {
    let total = total.filter(|total| *total > 0)?;
    Some((downloaded.saturating_mul(100) / total).min(100) as u8)
}

/// 托盘菜单项文案与可用状态。与设置页读同一份快照。
pub fn tray_label(snapshot: &UpdateSnapshot) -> (String, bool) {
    let version = snapshot.latest.as_deref().unwrap_or("").to_string();
    match snapshot.phase {
        UpdatePhase::Idle => ("检查更新".to_string(), true),
        UpdatePhase::Checking => ("正在检查更新…".to_string(), false),
        UpdatePhase::UpToDate => ("已是最新版本".to_string(), true),
        UpdatePhase::Available => (format!("下载更新 v{version}"), true),
        UpdatePhase::Downloading => (
            match snapshot.percent {
                Some(percent) => format!("正在下载更新 {percent}%"),
                None => "正在下载更新…".to_string(),
            },
            false,
        ),
        UpdatePhase::ReadyToRestart => (format!("重启并安装 v{version}"), true),
        UpdatePhase::Error if !version.is_empty() => (format!("重试下载 v{version}"), true),
        UpdatePhase::Error => ("检查更新失败，点击重试".to_string(), true),
    }
}

// ---------------------------------------------------------------------------
// 代理配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(default)]
    proxy: String,
}

/// 校验并规范化代理地址：空白串表示清空；仅接受带主机名的 HTTP/HTTPS 地址。
/// 错误文案不回显输入值，避免把带凭据的代理写进错误信息。
fn normalize_proxy(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    parse_proxy(trimmed)?;
    Ok(trimmed.to_string())
}

fn parse_proxy(value: &str) -> Result<tauri::Url, String> {
    let parsed = tauri::Url::parse(value).map_err(|_| PROXY_INVALID_MESSAGE.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(PROXY_INVALID_MESSAGE.to_string());
    }
    Ok(parsed)
}

fn read_config(path: &Path) -> Result<StoredConfig, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| CONFIG_BROKEN_MESSAGE.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoredConfig::default()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_config(path: &Path, config: &StoredConfig) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "更新配置路径无效".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(config).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    // Windows `rename` 不会覆盖已存在的目标文件，第二次保存会失败。
    replace_file(&temporary, path)
}

fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(to).map_err(|err| err.to_string())?;
            std::fs::rename(from, to).map_err(|err| err.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .join(CONFIG_FILE))
}

fn configured_proxy(app: &AppHandle) -> Result<Option<String>, String> {
    let stored = read_config(&config_path(app)?)?;
    let normalized = normalize_proxy(&stored.proxy)?;
    Ok((!normalized.is_empty()).then_some(normalized))
}

// ---------------------------------------------------------------------------
// 宿主出口
// ---------------------------------------------------------------------------

fn emit(app: &AppHandle, snapshot: UpdateSnapshot) -> UpdateSnapshot {
    let _ = app.emit(STATE_EVENT, &snapshot);
    snapshot
}

/// 构建更新器。缺少签名公钥或代理无效时提前失败，避免把问题留到签名校验阶段。
fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    if signature_public_key(app).is_none() {
        return Err(MISSING_PUBKEY_MESSAGE.to_string());
    }
    let mut builder = app.updater_builder();
    if let Some(proxy) = configured_proxy(app)? {
        builder = builder.proxy(parse_proxy(&proxy)?);
    }
    builder
        .build()
        .map_err(|error| format!("初始化更新器失败：{error}"))
}

/// 构建时写入的更新签名公钥；空值表示这个构建不代表发布信任根。
fn signature_public_key(app: &AppHandle) -> Option<String> {
    app.config()
        .plugins
        .0
        .get("updater")?
        .get("pubkey")?
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

fn service_state(app: &AppHandle) -> Result<Arc<UpdateService>, String> {
    app.try_state::<Arc<UpdateService>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| "更新服务尚未初始化".to_string())
}

/// 检查更新。手动点击与后台定时检查共用；失败时返回值带错误文案，快照仍是唯一状态。
pub async fn check(app: &AppHandle) -> Result<UpdateSnapshot, String> {
    let service = service_state(app)?;
    // 下载中或待重启时直接返回当前快照：这两个阶段本就忽略检查结果，若继续持有互斥锁
    // 做完整个网络请求，用户点「重启并安装」会被误判为「更新任务正在进行中」。
    let current = service.current();
    if matches!(
        current.phase,
        UpdatePhase::Downloading | UpdatePhase::ReadyToRestart
    ) {
        return Ok(current);
    }
    let Some(_busy) = BusyGuard::acquire(&service) else {
        return Err(BUSY_MESSAGE.to_string());
    };
    emit(app, service.begin_check());
    let result = match updater(app) {
        Ok(updater) => updater.check().await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let snapshot = match result {
        Ok(update) => service.apply_check(update.map(|update| update.version)),
        Err(error) => service.apply_failure(&format!("检查更新失败：{error}")),
    };
    emit(app, snapshot.clone());
    match snapshot.phase {
        UpdatePhase::Error => Err(snapshot.message.unwrap_or_default()),
        _ => Ok(snapshot),
    }
}

/// 启动下载：立即返回，进度经 `update-state` 事件上报。重复调用由互斥拒绝。
pub fn start_download(app: &AppHandle) -> Result<(), String> {
    let service = service_state(app)?;
    let Some(busy) = BusyGuard::acquire(&service) else {
        return Err(BUSY_MESSAGE.to_string());
    };
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _busy = busy;
        match download_package(&handle).await {
            Ok(Some(package)) => {
                let version = package.version.clone();
                service.store_package(package);
                emit(&handle, service.mark_ready_to_restart(&version));
            }
            // 更新源已没有可用包：版本已追平。
            Ok(None) => {
                emit(&handle, service.apply_check(None));
            }
            Err(message) => {
                emit(&handle, service.fail(message));
            }
        }
    });
    Ok(())
}

/// 安装已下载的更新包并重启应用（用户确认后调用）。无包时返回可读错误。
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let service = service_state(app)?;
    // 先抢互斥锁再取内存包：安装进行中的第二次点击会先撞上互斥锁得到「正在进行」，
    // 而不是「没有可安装的更新包」（包已被第一次安装取走）。
    let Some(_busy) = BusyGuard::acquire(&service) else {
        return Err(BUSY_MESSAGE.to_string());
    };
    let package = service
        .take_package()
        .ok_or_else(|| NO_PACKAGE_MESSAGE.to_string())?;
    let DownloadedPackage {
        version,
        bytes,
        update,
    } = package;
    emit(
        app,
        service.edit(|snapshot| {
            snapshot.message = Some(format!("正在安装 v{version} 并重启…"));
        }),
    );
    // 安装会解压整包并替换应用，放 blocking 线程，避免占住 async worker。
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await;
    let error = match installed {
        Ok(Ok(())) => {
            // macOS / Linux：包已就位，由本进程拉起新版本。Windows 的安装器会先让
            // 本进程退出，走不到这里。
            app.restart()
        }
        Ok(Err(error)) => format!("安装 v{version} 失败：{error}"),
        Err(error) => format!("安装 v{version} 失败：{error}"),
    };
    Err(emit(app, service.fail(error)).message.unwrap_or_default())
}

/// 后台定时检查：启动 15 秒后首次，此后每 30 分钟一次。
pub fn spawn_periodic_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let _ = check(&app).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// 托盘项点击：按当前快照决定检查、下载还是安装重启。
pub fn tray_activate(app: &AppHandle) {
    let snapshot = match service_state(app) {
        Ok(service) => service.current(),
        Err(_) => return,
    };
    match snapshot.phase {
        UpdatePhase::Available => {
            let _ = start_download(app);
        }
        UpdatePhase::Error if snapshot.latest.is_some() => {
            let _ = start_download(app);
        }
        UpdatePhase::ReadyToRestart => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = install(&handle).await;
            });
        }
        UpdatePhase::Checking | UpdatePhase::Downloading => {}
        _ => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = check(&handle).await;
            });
        }
    }
}

async fn download_package(app: &AppHandle) -> Result<Option<DownloadedPackage>, String> {
    let update = updater(app)?
        .check()
        .await
        .map_err(|error| format!("检查更新包失败：{error}"))?;
    let Some(update) = update else {
        return Ok(None);
    };
    let version = update.version.clone();
    let service = service_state(app)?;
    emit(app, service.begin_download(&version));
    let progress_app = app.clone();
    let progress_service = service.clone();
    let mut downloaded: u64 = 0;
    let bytes = update
        .download(
            move |chunk_length, total| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                // 只在整数百分比变化时写快照并广播，避免每个分包都触发一次前端渲染。
                if progress_service.advance_progress(downloaded, total).is_some() {
                    let _ = progress_app.emit(STATE_EVENT, progress_service.current());
                }
            },
            || {},
        )
        .await
        .map_err(|error| format!("下载更新包失败：{error}"))?;
    Ok(Some(DownloadedPackage {
        version,
        bytes,
        update,
    }))
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn update_config_get(app: AppHandle) -> Result<UpdateConfig, String> {
    let stored = read_config(&config_path(&app)?)?;
    Ok(UpdateConfig {
        proxy: normalize_proxy(&stored.proxy)?,
        current_version: app.package_info().version.to_string(),
        release_url: RELEASE_URL.to_string(),
    })
}

#[tauri::command]
pub fn update_config_set(app: AppHandle, proxy: String) -> Result<UpdateConfig, String> {
    let proxy = normalize_proxy(&proxy)?;
    write_config(&config_path(&app)?, &StoredConfig { proxy: proxy.clone() })?;
    Ok(UpdateConfig {
        proxy,
        current_version: app.package_info().version.to_string(),
        release_url: RELEASE_URL.to_string(),
    })
}

#[tauri::command]
pub fn update_state(service: State<'_, Arc<UpdateService>>) -> UpdateSnapshot {
    service.current()
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<UpdateSnapshot, String> {
    check(&app).await
}

#[tauri::command]
pub fn update_download(app: AppHandle) -> Result<(), String> {
    start_download(&app)
}

#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    install(&app).await
}

/// 打开公开更新源。地址固定、不接受参数，因此不能当作任意 URL 打开器使用。
#[tauri::command]
pub async fn update_open_release() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(open_release_page)
        .await
        .map_err(|error| error.to_string())?
}

fn open_release_page() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("/usr/bin/open")
        .arg(RELEASE_URL)
        .status();
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", RELEASE_URL])
        .status();
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("xdg-open")
        .arg(RELEASE_URL)
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err("打开更新页面失败".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> UpdateService {
        UpdateService::default()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-companion-update-{name}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn phases_flow_from_check_to_ready_to_restart() {
        let service = service();
        assert_eq!(service.current().phase, UpdatePhase::Idle);

        assert_eq!(service.begin_check().phase, UpdatePhase::Checking);
        let available = service.apply_check(Some("9.9.9".into()));
        assert_eq!(available.phase, UpdatePhase::Available);
        assert_eq!(available.latest.as_deref(), Some("9.9.9"));
        assert!(available.checked_at.is_some());

        let downloading = service.begin_download("9.9.9");
        assert_eq!(downloading.phase, UpdatePhase::Downloading);
        assert_eq!(downloading.percent, None);
        assert_eq!(service.advance_progress(30, Some(100)), Some(30));

        let ready = service.mark_ready_to_restart("9.9.9");
        assert_eq!(ready.phase, UpdatePhase::ReadyToRestart);
        assert_eq!(ready.percent, Some(100));
    }

    #[test]
    fn check_without_update_lands_on_up_to_date() {
        let service = service();
        let snapshot = service.apply_check(None);
        assert_eq!(snapshot.phase, UpdatePhase::UpToDate);
        assert_eq!(snapshot.latest, None);
        assert!(snapshot.checked_at.is_some());
    }

    #[test]
    fn failure_from_idle_lands_on_error_with_message() {
        let service = service();
        service.begin_check();
        let snapshot = service.apply_failure("检查更新失败：网络请求失败");
        assert_eq!(snapshot.phase, UpdatePhase::Error);
        assert_eq!(snapshot.latest, None);
        assert_eq!(
            snapshot.message.as_deref(),
            Some("检查更新失败：网络请求失败")
        );
    }

    /// 已有目标版本时，复查失败必须保留 Available 与目标版本，且失败文案留在快照里，
    /// 让设置页能同时看到可下载的版本和本次检查失败；后续成功检查再清掉文案。
    #[test]
    fn failed_recheck_keeps_available_target_and_surfaces_failure() {
        let service = service();
        service.apply_check(Some("9.9.9".into()));
        assert_eq!(service.begin_check().phase, UpdatePhase::Checking);
        let snapshot = service.apply_failure("检查更新失败：网络请求失败");
        assert_eq!(snapshot.phase, UpdatePhase::Available);
        assert_eq!(snapshot.latest.as_deref(), Some("9.9.9"));
        assert_eq!(
            snapshot.message.as_deref(),
            Some("检查更新失败：网络请求失败")
        );

        let recovered = service.apply_check(Some("9.9.9".into()));
        assert_eq!(recovered.phase, UpdatePhase::Available);
        assert_eq!(recovered.message, None);
    }

    #[test]
    fn check_does_not_downgrade_downloading_or_ready_to_restart() {
        let service = service();
        service.begin_download("9.9.9");
        assert_eq!(service.begin_check().phase, UpdatePhase::Downloading);
        assert_eq!(
            service.apply_check(Some("9.9.10".into())).phase,
            UpdatePhase::Downloading
        );
        service.mark_ready_to_restart("9.9.9");
        assert_eq!(service.begin_check().phase, UpdatePhase::ReadyToRestart);
        assert_eq!(
            service.apply_failure("检查更新失败").phase,
            UpdatePhase::ReadyToRestart
        );
    }

    #[test]
    fn failed_download_keeps_target_for_retry() {
        let service = service();
        service.apply_check(Some("9.9.9".into()));
        service.begin_download("9.9.9");
        let snapshot = service.fail("下载更新包失败：连接超时");
        assert_eq!(snapshot.phase, UpdatePhase::Error);
        assert_eq!(snapshot.latest.as_deref(), Some("9.9.9"));
        assert_eq!(snapshot.percent, None);
    }

    #[test]
    fn busy_guard_is_exclusive_until_released() {
        let service = Arc::new(service());
        let first = BusyGuard::acquire(&service);
        assert!(first.is_some());
        assert!(BusyGuard::acquire(&service).is_none());
        drop(first);
        assert!(BusyGuard::acquire(&service).is_some());
    }

    #[test]
    fn restart_without_package_returns_error() {
        let service = service();
        assert!(service.take_package().is_none());
        assert!(service.take_package().is_none());
    }

    #[test]
    fn percent_conversion_handles_unknown_total_and_clamps() {
        assert_eq!(percent_of(0, Some(100)), Some(0));
        assert_eq!(percent_of(1, Some(3)), Some(33));
        assert_eq!(percent_of(150, Some(100)), Some(100));
        assert_eq!(percent_of(10, None), None);
        assert_eq!(percent_of(10, Some(0)), None);
    }

    #[test]
    fn progress_written_only_on_integer_change() {
        let service = service();
        service.begin_download("9.9.9");
        assert_eq!(service.advance_progress(1, Some(1000)), Some(0));
        assert_eq!(service.advance_progress(2, Some(1000)), None);
        assert_eq!(service.current().percent, Some(0));
        assert_eq!(service.advance_progress(10, Some(1000)), Some(1));
        assert_eq!(service.advance_progress(10, None), None);
    }

    #[test]
    fn proxy_validation_accepts_http_and_https_only() {
        assert_eq!(normalize_proxy("").unwrap(), "");
        assert_eq!(normalize_proxy("   ").unwrap(), "");
        assert_eq!(
            normalize_proxy("  http://127.0.0.1:7897 ").unwrap(),
            "http://127.0.0.1:7897"
        );
        assert_eq!(
            normalize_proxy("https://proxy.example.com:8080").unwrap(),
            "https://proxy.example.com:8080"
        );
        // 带凭据的代理可用，但错误文案不回显它。
        assert_eq!(
            normalize_proxy("http://user:secret@127.0.0.1:7897").unwrap(),
            "http://user:secret@127.0.0.1:7897"
        );
        for invalid in [
            "127.0.0.1:1080",
            "socks5://proxy.internal:1080",
            "ftp://proxy.internal",
            "http://",
            "not a url",
        ] {
            assert_eq!(normalize_proxy(invalid).unwrap_err(), PROXY_INVALID_MESSAGE);
        }
        // 错误文案不回显输入值，带凭据的地址也不会泄漏到错误信息里。
        let secret = "socks5://user:supersecret@proxy.internal:1080";
        let error = normalize_proxy(secret).unwrap_err();
        assert_eq!(error, PROXY_INVALID_MESSAGE);
        assert!(!error.contains("supersecret"), "错误文案不得包含代理凭据");
    }

    #[test]
    fn config_round_trips_through_atomic_write() {
        let dir = temp_dir("config");
        let path = dir.join("update-config.json");

        assert_eq!(read_config(&path).unwrap().proxy, "");
        write_config(
            &path,
            &StoredConfig {
                proxy: "http://127.0.0.1:7897".into(),
            },
        )
        .unwrap();
        assert_eq!(read_config(&path).unwrap().proxy, "http://127.0.0.1:7897");
        write_config(&path, &StoredConfig { proxy: String::new() }).unwrap();
        assert_eq!(read_config(&path).unwrap().proxy, "");

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(read_config(&path).unwrap_err(), CONFIG_BROKEN_MESSAGE);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 事件 / 命令负载的字段契约（前端 `UpdateSnapshot` 按此解析）。
    #[test]
    fn snapshot_serializes_camel_case_contract() {
        let value = serde_json::to_value(UpdateSnapshot::idle()).unwrap();
        assert_eq!(value["phase"], serde_json::json!("idle"));
        assert_eq!(value["latest"], serde_json::json!(null));
        assert_eq!(value["percent"], serde_json::json!(null));
        assert_eq!(value["message"], serde_json::json!(null));
        assert_eq!(value["checkedAt"], serde_json::json!(null));

        let ready = UpdateSnapshot {
            phase: UpdatePhase::ReadyToRestart,
            latest: Some("1.2.3".into()),
            percent: Some(100),
            message: None,
            checked_at: Some(1_700_000_000),
        };
        let value = serde_json::to_value(&ready).unwrap();
        assert_eq!(value["phase"], serde_json::json!("readyToRestart"));
        assert_eq!(value["latest"], serde_json::json!("1.2.3"));
        assert_eq!(value["percent"], serde_json::json!(100));
        assert_eq!(value["checkedAt"], serde_json::json!(1_700_000_000));

        let config = serde_json::to_value(UpdateConfig {
            proxy: String::new(),
            current_version: "0.1.0".into(),
            release_url: RELEASE_URL.into(),
        })
        .unwrap();
        assert_eq!(config["proxy"], serde_json::json!(""));
        assert_eq!(config["currentVersion"], serde_json::json!("0.1.0"));
        assert_eq!(config["releaseUrl"], serde_json::json!(RELEASE_URL));
    }

    #[test]
    fn tray_labels_follow_snapshot() {
        let mut snapshot = UpdateSnapshot::idle();
        assert_eq!(tray_label(&snapshot).0, "检查更新");
        snapshot.phase = UpdatePhase::Checking;
        assert_eq!(tray_label(&snapshot).1, false);
        snapshot.phase = UpdatePhase::Available;
        snapshot.latest = Some("0.2.0".into());
        assert_eq!(tray_label(&snapshot), ("下载更新 v0.2.0".to_string(), true));
        snapshot.phase = UpdatePhase::Downloading;
        snapshot.percent = Some(42);
        assert_eq!(tray_label(&snapshot), ("正在下载更新 42%".to_string(), false));
        snapshot.percent = None;
        assert_eq!(tray_label(&snapshot).0, "正在下载更新…");
        snapshot.phase = UpdatePhase::ReadyToRestart;
        assert_eq!(tray_label(&snapshot).0, "重启并安装 v0.2.0");
        snapshot.phase = UpdatePhase::Error;
        assert_eq!(tray_label(&snapshot).0, "重试下载 v0.2.0");
        snapshot.latest = None;
        assert_eq!(tray_label(&snapshot).0, "检查更新失败，点击重试");
    }
}
