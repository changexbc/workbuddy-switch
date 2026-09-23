//! Desktop tray: menu-bar icon, dock visibility, lightweight mode, and check-in.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, Runtime, WebviewWindowBuilder, Window, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use wb_switch_core::modules::{checkin, update};

use crate::update_service::{UpdatePhase, UpdateSnapshot};

const TRAY_ID: &str = "main-menu-bar";
const MAIN_WINDOW_LABEL: &str = "main";
const DEFAULT_TOOLTIP: &str = "workbuddy-switch";
const CHECKIN_TOOLTIP_RESTORE_SECS: u64 = 8;

/// 系统自启注册的启动参数：仅携带该精确参数的启动进入静默托盘模式。
pub const SILENT_STARTUP_ARG: &str = "--hidden";

static LIGHTWEIGHT_MODE: AtomicBool = AtomicBool::new(false);
static CHECKIN_BUSY: AtomicBool = AtomicBool::new(false);
static TOOLTIP_GENERATION: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "macos")]
static DOCK_ICON_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon())
        // 模板图标（系统按明暗自适应着色）是 macOS 独有的语义；
        // Windows/Linux 会忽略该标记并直接贴原始像素，用单色剪影会显示成纯白方块。
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(DEFAULT_TOOLTIP)
        .menu(&menu)
        // 左键弹菜单只保留在 macOS：菜单栏图标的惯例本就是左键展开菜单。
        // Windows 的惯例相反——左键唤起主界面、右键出菜单，见 on_tray_icon_event。
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            {
                if should_wake_main_window(button, button_state) {
                    show_main_window(tray.app_handle());
                }
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open-main-window" => show_main_window(app),
            "open-github" => open_github(app),
            "checkin-all" => start_checkin_all(app),
            "check-update" => start_update_check(app),
            "update-now" => start_update_download(app),
            "update-restart" => start_update_restart(app),
            "lightweight-mode" => toggle_lightweight(app),
            "quit-app" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    #[cfg(windows)]
    watch_taskbar_theme(app.handle().clone());

    Ok(())
}

pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != MAIN_WINDOW_LABEL {
        return;
    }
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
        apply_dock_visible(window.app_handle(), false);
        emit_main_window_visible(window.app_handle(), false);
    }
}

/// 判断本次启动是否携带精确的 `--hidden` 参数（系统自启触发）。
///
/// 必须整参相等，禁止子串匹配，避免 `--hidden-x`、`x--hidden` 等误入静默模式。
pub fn is_silent_startup(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter()
        .any(|arg| arg.as_ref() == SILENT_STARTUP_ARG)
}

/// 第二次启动是否需要把既有实例唤醒到前台。
///
/// 静默启动（精确 `--hidden`，自启重复触发）不打扰用户：不显示窗口、不改 Dock 状态。
pub fn should_activate_on_second_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    !is_silent_startup(args)
}

/// 单实例插件回调：已有实例时后启动进程已退出，这里处理既有实例的反应。
///
/// 参数由插件回传，**含 argv[0]**；精确 `--hidden` 判定与自启路径一致。
/// 回调运行在 tokio worker 线程，而 `show_main_window` 会经 `apply_dock_visible`
/// 直接操作 AppKit（`MainThreadMarker::new_unchecked`），必须跳回主线程执行。
pub fn on_second_instance<R: Runtime>(app: &AppHandle<R>, args: Vec<String>) {
    if !should_activate_on_second_launch(&args) {
        return;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_main_window(&handle));
}

/// macOS `RunEvent::Reopen`（点击 Dock / Finder 激活已运行应用）时显示主窗口。
///
/// 该事件在主线程派发，可直接走 `show_main_window`；窗口策略仍集中在 tray 模块。
#[cfg(target_os = "macos")]
pub(crate) fn show_main_window_on_reopen<R: Runtime>(app: &AppHandle<R>) {
    show_main_window(app);
}

/// 在事件循环呈现应用前决定首次启动的主窗口可见性。
///
/// `main` 窗口由 `tauri.conf.json` 配置创建为不可见，此处做出第一次
/// show / hide 决策，静默启动不会先闪出主窗口：
///
/// - 普通启动（无 `--hidden`）：走既有 `show_main_window` 路径，
///   先恢复 Regular / Dock，再 show / unminimize / focus。
/// - 静默启动（`--hidden`）：窗口保持隐藏，隐藏 Dock / 任务栏入口，
///   只保留托盘；不设置 `LIGHTWEIGHT_MODE`（WebView 仍然存在）。
///
/// 之后从托盘「打开主界面」仍走 `show_main_window`，与隐藏窗口完全一致。
pub fn setup_startup_visibility<R: Runtime>(app: &AppHandle<R>, silent: bool) {
    if silent {
        apply_dock_visible(app, false);
        emit_main_window_visible(app, false);
    } else {
        show_main_window(app);
    }
}

fn emit_main_window_visible<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    let _ = app.emit("main-window-visible", visible);
}

/// Keep the tray process alive when the last window is destroyed (lightweight mode).
///
/// `code == None` is Tauri's runtime exit after zero windows remain.
/// `code == Some(_)` is an explicit `app.exit()` / restart — let those through.
pub fn on_run_event(event: RunEvent) {
    if let RunEvent::ExitRequested { api, code, .. } = event {
        if should_keep_tray_alive(code) {
            api.prevent_exit();
        }
    }
}

fn should_keep_tray_alive(code: Option<i32>) -> bool {
    code.is_none()
}

/// 托盘左键单击（抬起）是否应唤起主窗口。
///
/// - macOS：`show_menu_on_left_click` 保持 true，左键展开菜单；但 mouseUp 仍会派发
///   `Click`，所以这里必须显式返回 false，否则左键会「既弹菜单又唤窗」。
/// - Linux：Tauri 不派发 `TrayIconEvent`（仅 Windows / macOS 支持），该分支不会触发，
///   托盘点击行为仍由 libappindicator 决定（左右键都会出菜单）。
fn should_wake_main_window(button: MouseButton, button_state: MouseButtonState) -> bool {
    !cfg!(target_os = "macos")
        && matches!(
            (button, button_state),
            (MouseButton::Left, MouseButtonState::Up)
        )
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    // Recreate if the WebView is gone even when the flag is already false
    // (e.g. destroy() completed after a failed lightweight toggle).
    if LIGHTWEIGHT_MODE.load(Ordering::Acquire)
        || app.get_webview_window(MAIN_WINDOW_LABEL).is_none()
    {
        exit_lightweight(app);
        return;
    }
    apply_dock_visible(app, true);
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    emit_main_window_visible(app, true);
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(unused_variables)
)]
fn apply_dock_visible<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use tauri::ActivationPolicy;
        let policy = if visible {
            ActivationPolicy::Regular
        } else {
            ActivationPolicy::Accessory
        };
        let _ = app.set_dock_visibility(visible);
        let _ = app.set_activation_policy(policy);
        if visible {
            restore_macos_dock_icon(app);
        } else {
            DOCK_ICON_GENERATION.fetch_add(1, Ordering::AcqRel);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
            let _ = window.set_skip_taskbar(!visible);
        }
    }
}

/// Re-apply the Dock icon after returning to Regular.
///
/// `TransformProcessType` rebuilds the Dock tile from the running executable.
/// Packaged `.app` binaries have no icon of their own, and `tauri dev` is named
/// `exec`, so both need an explicit restore. Tauri only sets
/// `setApplicationIconImage` once on `RunEvent::Ready` (dev only).
/// `TransformProcessType` is asynchronous, so we also re-apply after a short delay.
///
/// Do **not** feed the raw `icon.png` here: it is a full-bleed opaque square.
/// `setApplicationIconImage` then bypasses the system squircle, which is why the
/// Dock icon lost rounded corners and changed size after “打开主窗口”.
#[cfg(target_os = "macos")]
fn restore_macos_dock_icon<R: Runtime>(app: &AppHandle<R>) {
    let generation = DOCK_ICON_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    apply_macos_app_icon();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if DOCK_ICON_GENERATION.load(Ordering::Acquire) != generation {
            return;
        }
        let _ = app.run_on_main_thread(move || {
            if DOCK_ICON_GENERATION.load(Ordering::Acquire) == generation {
                apply_macos_app_icon();
            }
        });
    });
}

#[cfg(target_os = "macos")]
fn apply_macos_app_icon() {
    use objc2::AllocAnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    // SAFETY: tray / window events and run_on_main_thread all run on the AppKit main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);

    if let Some(icon) = macos_bundle_dock_icon() {
        unsafe { app.setApplicationIconImage(Some(&icon)) };
        return;
    }

    // `tauri dev` has no `.app` bundle; keep the PNG fallback so Dock is not "exec".
    const APP_ICON_PNG: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/icon.png"));
    let data = NSData::with_bytes(APP_ICON_PNG);
    let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    unsafe { app.setApplicationIconImage(Some(&icon)) };
}

/// Finder-composited app icon (system squircle already applied).
#[cfg(target_os = "macos")]
fn macos_bundle_dock_icon() -> Option<objc2::rc::Retained<objc2_app_kit::NSImage>> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;

    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let bundle = app_bundle_path_from_exe(&exe)?;
    let path = NSString::from_str(&bundle.to_string_lossy());
    Some(NSWorkspace::sharedWorkspace().iconForFile(&path))
}

/// `Foo.app/Contents/MacOS/binary` → `Foo.app`.
#[cfg(any(target_os = "macos", test))]
fn app_bundle_path_from_exe(exe: &std::path::Path) -> Option<&std::path::Path> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?;
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    (bundle.extension()?.to_str()? == "app").then_some(bundle)
}

fn toggle_lightweight<R: Runtime>(app: &AppHandle<R>) {
    if LIGHTWEIGHT_MODE.load(Ordering::Acquire) {
        exit_lightweight(app);
    } else {
        enter_lightweight(app);
    }
}

fn enter_lightweight<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        if window.destroy().is_err() {
            // CheckMenuItem may have already toggled visually; restore it.
            refresh_tray_menu(app);
            return;
        }
    }
    apply_dock_visible(app, false);
    LIGHTWEIGHT_MODE.store(true, Ordering::Release);
    refresh_tray_menu(app);
}

fn exit_lightweight<R: Runtime>(app: &AppHandle<R>) {
    // Regular / dock first so a from_config window is not created while Accessory.
    apply_dock_visible(app, true);
    if app.get_webview_window(MAIN_WINDOW_LABEL).is_none() {
        let Some(config) = app
            .config()
            .app
            .windows
            .iter()
            .find(|window| window.label == MAIN_WINDOW_LABEL)
            .cloned()
        else {
            apply_dock_visible(app, false);
            refresh_tray_menu(app);
            return;
        };
        if WebviewWindowBuilder::from_config(app, &config)
            .and_then(|builder| builder.build())
            .is_err()
        {
            apply_dock_visible(app, false);
            refresh_tray_menu(app);
            return;
        }
    }
    // Window exists now: Windows skip_taskbar was a no-op before recreate.
    apply_dock_visible(app, true);
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    emit_main_window_visible(app, true);
    LIGHTWEIGHT_MODE.store(false, Ordering::Release);
    refresh_tray_menu(app);
}

fn open_github<R: Runtime>(app: &AppHandle<R>) {
    let url = format!(
        "https://github.com/{}/{}",
        update::GITHUB_OWNER,
        update::GITHUB_REPO
    );
    let _ = app.opener().open_url(url, None::<&str>);
}

struct CheckinBusyGuard<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> Drop for CheckinBusyGuard<R> {
    fn drop(&mut self) {
        CHECKIN_BUSY.store(false, Ordering::Release);
        refresh_tray_menu(&self.app);
    }
}

fn start_checkin_all<R: Runtime>(app: &AppHandle<R>) {
    if crate::is_screenshot_demo() {
        set_tray_tooltip(app, "README 截图演示模式");
        return;
    }
    if CHECKIN_BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    bump_tooltip_generation();
    refresh_tray_menu(app);
    set_tray_tooltip(app, "正在签到…");

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _busy = CheckinBusyGuard { app: app.clone() };
        let payload = checkin::run_checkin_all(None).await;
        let text = format_checkin_tooltip(&payload);
        if checkin_succeeded(&payload) {
            notify_checkin(&app, &text);
        } else {
            let generation = bump_tooltip_generation();
            set_tray_tooltip(&app, &text);
            restore_tooltip_after(app, generation);
        }
    });
}

/// 托盘「检查更新」：用户主动触发，force=true 绕过 core 的 6 小时缓存。
///
/// 菜单回调和「一键签到」同在主线程，异步检查必须 spawn（见 `start_checkin_all`）。
fn start_update_check<R: Runtime>(app: &AppHandle<R>) {
    if crate::is_screenshot_demo() {
        set_tray_tooltip(app, "README 截图演示模式");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::update_service::check(&app, None, true).await;
    });
}

/// 托盘「升级到 vX.Y.Z」/「更新失败，点击重试」：启动后台下载。
///
/// 互斥与演示模式短路都在更新服务内（下载是长任务，托盘不持有它的生命周期）。
fn start_update_download<R: Runtime>(app: &AppHandle<R>) {
    if crate::is_screenshot_demo() {
        set_tray_tooltip(app, "README 截图演示模式");
        return;
    }
    let _ = crate::update_service::start_download(app);
}

/// 托盘「重启以完成升级」：安装已下载的包并重启。
///
/// 安装会解压整包 / 替换应用（macOS 未授权时弹系统授权框），必须离开主线程执行。
fn start_update_restart<R: Runtime>(app: &AppHandle<R>) {
    if crate::is_screenshot_demo() {
        set_tray_tooltip(app, "README 截图演示模式");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::update_service::restart(&app).await;
    });
}

fn notify_checkin<R: Runtime>(app: &AppHandle<R>, body: &str) {
    let _ = app
        .notification()
        .builder()
        .title("workbuddy-switch")
        .body(body)
        .show();
}

/// 投递 core 组装好的自动轮换推迟提示（`rotate::run_rotate_cycle` 返回体里的 `notify`）。
///
/// 走系统通知而不是托盘 tooltip：轮换是后台行为，用户此时多半没看着窗口。
/// 标题与正文都取自 core（文案唯一构造点在 `rotate`），宿主不自造措辞；
/// 无头 server 不投递，只保留日志与返回字段。
pub fn notify_rotate_deferred<R: Runtime>(app: &AppHandle<R>, notify: &Value) {
    let title = notify
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("workbuddy-switch");
    let Some(body) = notify.get("body").and_then(Value::as_str) else {
        return;
    };
    if body.is_empty() {
        return;
    }
    let _ = app.notification().builder().title(title).body(body).show();
}

/// 是否应弹签到完成通知。
///
/// `inactive`（该档位未开放签到活动，如国际版）不是失败：它既不算成功也不重试，
/// 因此不阻断通知；`error` 仍然算失败。
fn checkin_succeeded(value: &Value) -> bool {
    let Some(accounts) = value.get("accounts").and_then(Value::as_array) else {
        return false;
    };
    !accounts.is_empty()
        && accounts.iter().all(|account| {
            matches!(
                account.get("result").and_then(Value::as_str),
                Some("success" | "already" | "inactive")
            )
        })
}

fn restore_tooltip_after<R: Runtime>(app: AppHandle<R>, generation: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(CHECKIN_TOOLTIP_RESTORE_SECS)).await;
        if TOOLTIP_GENERATION.load(Ordering::Acquire) == generation {
            set_tray_tooltip(&app, DEFAULT_TOOLTIP);
        }
    });
}

fn bump_tooltip_generation() -> u64 {
    TOOLTIP_GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

fn set_tray_tooltip<R: Runtime>(app: &AppHandle<R>, text: &str) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(text));
    }
}

/// 更新服务写入 tooltip（下载进度）。
///
/// 先 bump generation：任何迟到的签到 tooltip 恢复计时器都会因代际不符而放弃，
/// 不会把下载进度覆盖回默认文案。
pub(crate) fn set_update_tooltip<R: Runtime>(app: &AppHandle<R>, text: &str) {
    bump_tooltip_generation();
    set_tray_tooltip(app, text);
}

/// 更新流程结束（完成 / 失败）后复位 tooltip。
pub(crate) fn reset_tray_tooltip<R: Runtime>(app: &AppHandle<R>) {
    bump_tooltip_generation();
    set_tray_tooltip(app, DEFAULT_TOOLTIP);
}

/// 重建托盘菜单（更新服务在阶段切换 / 下载跨 10% 时调用）。
///
/// 注意：不要在指针进入 / 点击回调里重建——菜单正在展示时 `set_menu` 会让它闪掉。
pub(crate) fn refresh_tray_menu<R: Runtime>(app: &AppHandle<R>) {
    let Ok(menu) = build_tray_menu(app) else {
        return;
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// 托盘更新入口的三种动作（菜单项 id 即对外契约）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UpdateMenuAction {
    /// 检查更新。
    Check,
    /// 下载 / 重试下载。
    Download,
    /// 安装并重启。
    Restart,
}

impl UpdateMenuAction {
    fn id(self) -> &'static str {
        match self {
            Self::Check => "check-update",
            Self::Download => "update-now",
            Self::Restart => "update-restart",
        }
    }
}

/// 更新菜单项的阶段映射：`(动作, 文案, 是否可点)`。
///
/// 菜单与前端弹窗读同一份快照，因此同一时刻两处显示的是同一阶段。
/// `Error` 按是否已知目标版本区分两种重试文案：已知版本 → 重试下载；未知 → 重试检查。
fn update_menu_spec(snapshot: &UpdateSnapshot) -> (UpdateMenuAction, String, bool) {
    match snapshot.phase {
        UpdatePhase::Idle | UpdatePhase::UpToDate => {
            (UpdateMenuAction::Check, "检查更新".to_string(), true)
        }
        UpdatePhase::Checking => (UpdateMenuAction::Check, "正在检查…".to_string(), false),
        UpdatePhase::Available => (
            UpdateMenuAction::Download,
            match snapshot.latest.as_deref() {
                Some(latest) => format!("升级到 v{latest}"),
                None => "升级到新版本".to_string(),
            },
            true,
        ),
        UpdatePhase::Downloading => (
            UpdateMenuAction::Download,
            match snapshot.percent {
                Some(percent) => format!("正在下载更新 {percent}%"),
                None => "正在下载更新…".to_string(),
            },
            false,
        ),
        UpdatePhase::ReadyToRestart => {
            (UpdateMenuAction::Restart, "重启以完成升级".to_string(), true)
        }
        UpdatePhase::Error => {
            if snapshot.latest.is_some() {
                (
                    UpdateMenuAction::Download,
                    "更新失败，点击重试".to_string(),
                    true,
                )
            } else {
                (
                    UpdateMenuAction::Check,
                    "检查更新失败，点击重试".to_string(),
                    true,
                )
            }
        }
    }
}

fn build_tray_menu<R: Runtime, M: Manager<R>>(app: &M) -> tauri::Result<Menu<R>> {
    let open_item = MenuItem::with_id(app, "open-main-window", "打开主界面", true, None::<&str>)?;
    let github_item = MenuItem::with_id(app, "open-github", "打开 GitHub", true, None::<&str>)?;
    // 档位区分在 core 判定：无签到活动的档位（国际版）不参与「待签到」集合，
    // 否则这些账号永远不会产生签到日志，托盘会一直显示「可签到」。
    let checked_in = checkin::all_accounts_checked_in_today();
    let (checkin_label, checkin_enabled) = if CHECKIN_BUSY.load(Ordering::Acquire) {
        ("一键签到", false)
    } else if checked_in {
        ("已签到", false)
    } else {
        ("一键签到", true)
    };
    let checkin_item = MenuItem::with_id(
        app,
        "checkin-all",
        checkin_label,
        checkin_enabled,
        None::<&str>,
    )?;
    let (update_action, update_label, update_enabled) =
        update_menu_spec(&crate::update_service::snapshot());
    let update_item = MenuItem::with_id(
        app,
        update_action.id(),
        update_label,
        update_enabled,
        None::<&str>,
    )?;
    let lightweight_item = CheckMenuItem::with_id(
        app,
        "lightweight-mode",
        "轻量模式",
        true,
        LIGHTWEIGHT_MODE.load(Ordering::Acquire),
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit-app", "退出应用", true, None::<&str>)?;

    MenuBuilder::new(app)
        .item(&open_item)
        .item(&github_item)
        .item(&checkin_item)
        .separator()
        .item(&update_item)
        .separator()
        .item(&lightweight_item)
        .separator()
        .item(&quit_item)
        .build()
}

/// macOS 托盘图标：单色模板素材（系统按明暗主题自动着色）。
///
/// 注意：函数名 `tray_icon` 为跨平台统称；macOS 侧历史名为 `menu_bar_icon`。
#[cfg(target_os = "macos")]
fn tray_icon() -> tauri::image::Image<'static> {
    const ICON: &[u8; 36 * 36 * 4] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/icons/tray-icon-template.rgba"
    ));
    tauri::image::Image::new(ICON, 36, 36)
}

/// Windows 托盘图标：按「Windows 模式」在黑白猫之间选择，读不到主题就用彩色素材兜底。
#[cfg(windows)]
fn tray_icon() -> tauri::image::Image<'static> {
    match tray_icon_variant(taskbar_uses_light_theme()) {
        TrayIconVariant::MonoBlack => mono_black_icon(),
        TrayIconVariant::MonoWhite => mono_white_icon(),
        TrayIconVariant::Color => color_icon(),
    }
}

/// Linux 托盘图标：彩色应用图标。
///
/// Linux 没有模板图标语义，也无法可靠判断任务栏底色（面板主题与发行版相关），
/// 只能用一份在深浅底色下都还看得清的彩色素材。
#[cfg(all(not(target_os = "macos"), not(windows)))]
fn tray_icon() -> tauri::image::Image<'static> {
    color_icon()
}

/// 彩色应用图标（32×32，透明底）。Windows 读不到主题时的兜底，也是 Linux 的唯一素材。
#[cfg(not(target_os = "macos"))]
fn color_icon() -> tauri::image::Image<'static> {
    const ICON: &[u8; 32 * 32 * 4] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/icons/tray-icon-color.rgba"
    ));
    tauri::image::Image::new(ICON, 32, 32)
}

/// 深色单色猫：浅色任务栏下使用（与 macOS 模板同一份猫形，由生成脚本重着色）。
#[cfg(windows)]
fn mono_black_icon() -> tauri::image::Image<'static> {
    const ICON: &[u8; 32 * 32 * 4] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/icons/tray-icon-mono-black.rgba"
    ));
    tauri::image::Image::new(ICON, 32, 32)
}

/// 浅色单色猫：深色任务栏下使用。
#[cfg(windows)]
fn mono_white_icon() -> tauri::image::Image<'static> {
    const ICON: &[u8; 32 * 32 * 4] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/icons/tray-icon-mono-white.rgba"
    ));
    tauri::image::Image::new(ICON, 32, 32)
}

/// Windows 托盘图标形状。
///
/// Windows 不会给第三方托盘图标自动配色（没有 macOS 的模板语义），
/// 只能按任务栏底色自己挑素材。
#[cfg(any(windows, test))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrayIconVariant {
    /// 深色猫：用于浅色任务栏。
    MonoBlack,
    /// 浅色猫：用于深色任务栏。
    MonoWhite,
    /// 彩色应用图标：读不到任务栏主题时的兜底。
    Color,
}

/// 由「Windows 模式」是否为浅色决定用哪套素材。
///
/// `None`（读不到设置）时退回彩色素材，而不是盲猜黑白：猜错的那一版会在对应底色上
/// 彻底看不见，而彩色素材在两种底色下都能辨认。
#[cfg(any(windows, test))]
fn tray_icon_variant(light_taskbar: Option<bool>) -> TrayIconVariant {
    match light_taskbar {
        Some(true) => TrayIconVariant::MonoBlack,
        Some(false) => TrayIconVariant::MonoWhite,
        None => TrayIconVariant::Color,
    }
}

/// 「Windows 模式」是否为浅色（任务栏与开始菜单跟随它，而**不是**应用模式）。
///
/// 必须读 `SystemUsesLightTheme`：`AppsUseLightTheme` 是应用模式（窗口、对话框），
/// 两者可以不一致（「应用浅色 + 系统深色」是常见组合）。Tauri 的
/// `WindowEvent::ThemeChanged` 走的正是 `AppsUseLightTheme`（tao 的 `should_use_dark_mode`），
/// 所以它不能用来判断任务栏底色——跟着它切图标恰好会在浅色任务栏上贴出白猫。
///
/// 返回 `None` 表示读不到（键不存在、权限异常等），由调用方兜底。
#[cfg(windows)]
fn taskbar_uses_light_theme() -> Option<bool> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let subkey = wide(PERSONALIZE_KEY);
    let value = wide("SystemUsesLightTheme");
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: 两个字符串均以 NUL 结尾且在本调用期间存活；`data` / `size` 指向本函数栈上的
    // 有效内存，缓冲区类型（DWORD）与 RRF_RT_REG_DWORD 一致。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(data != 0)
}

/// 跟随「Windows 模式」切换托盘图标。
///
/// 主题变化没有任何可用的 Tauri 事件（见 `taskbar_uses_light_theme`），因此在后台线程用
/// `RegNotifyChangeKeyValue` 阻塞等待注册表键被改动，醒来后重读并换图标：用户切主题时
/// 任务栏会立刻重绘，图标晚一步就会在对应底色上消失。
///
/// 开键失败、通知失败、或托盘已被销毁时线程直接退出：图标停留在启动时选定的那一版，
/// 不会影响其他功能。线程随进程退出而结束。
#[cfg(windows)]
fn watch_taskbar_theme<R: Runtime>(app: AppHandle<R>) {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
        REG_NOTIFY_CHANGE_LAST_SET,
    };

    std::thread::spawn(move || {
        let subkey = wide(PERSONALIZE_KEY);
        let mut key: HKEY = 0;
        // SAFETY: subkey 以 NUL 结尾且存活到调用结束；`key` 是本函数栈上的有效输出参数。
        let opened =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_NOTIFY, &mut key) };
        if opened != ERROR_SUCCESS {
            return;
        }
        loop {
            // SAFETY: `key` 由上面的 RegOpenKeyExW 打开且尚未关闭；事件句柄传 0 且
            // fAsynchronous = 0，表示同步等待——本调用会阻塞到该键被改动。
            let notified =
                unsafe { RegNotifyChangeKeyValue(key, 1, REG_NOTIFY_CHANGE_LAST_SET, 0, 0) };
            if notified != ERROR_SUCCESS {
                break;
            }
            match app.tray_by_id(TRAY_ID) {
                Some(tray) => {
                    let _ = tray.set_icon(Some(tray_icon()));
                }
                None => break,
            }
        }
        // SAFETY: `key` 由 RegOpenKeyExW 打开，且每条路径上只在这里关闭一次。
        unsafe { RegCloseKey(key) };
    });
}

#[cfg(windows)]
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// 转成以 NUL 结尾的 UTF-16，供 Win32 宽字符 API 使用。
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn format_checkin_tooltip(value: &Value) -> String {
    if value.get("status").and_then(Value::as_str) == Some("skipped")
        && value.get("reason").and_then(Value::as_str) == Some("already_running")
    {
        return "签到任务正在进行，请稍后再试".to_string();
    }
    let Some(accounts) = value.get("accounts").and_then(Value::as_array) else {
        return "没有可签到的账号".to_string();
    };
    if accounts.is_empty() {
        return "没有可签到的账号".to_string();
    }

    let mut ok = 0;
    let mut already = 0;
    let mut err = 0;
    let mut inactive = 0;
    for account in accounts {
        match account.get("result").and_then(Value::as_str) {
            Some("success") => ok += 1,
            Some("already") => already += 1,
            Some("error") => err += 1,
            Some("inactive") => inactive += 1,
            _ => {}
        }
    }
    let mut text = format!("签到完成：成功 {ok}，已签 {already}，失败 {err}");
    // 国际版无签到活动：单列「未开放」，避免被误读成失败或漏报。
    if inactive > 0 {
        text.push_str(&format!("，未开放 {inactive}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        format_checkin_tooltip, is_silent_startup, should_activate_on_second_launch,
        should_keep_tray_alive, should_wake_main_window, tray_icon, tray_icon_variant, MouseButton,
        MouseButtonState, TrayIconVariant,
    };
    use serde_json::json;

    #[test]
    fn runtime_exit_with_no_code_keeps_tray() {
        assert!(should_keep_tray_alive(None));
        assert!(!should_keep_tray_alive(Some(0)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn tray_icon_has_transparency_and_antialiasing() {
        let icon = tray_icon();
        assert_eq!((icon.width(), icon.height()), (36, 36));
        assert!(icon.rgba().chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(icon
            .rgba()
            .chunks_exact(4)
            .any(|pixel| (1..=254).contains(&pixel[3])));
    }

    /// Linux：托盘素材必须是**彩色透明底**，避免退化成白块 / 白底方图。
    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn tray_icon_is_colored_on_transparent_background() {
        let icon = tray_icon();
        assert_eq!((icon.width(), icon.height()), (32, 32));
        let px: Vec<&[u8]> = icon.rgba().chunks_exact(4).collect();
        assert!(px.iter().any(|p| p[3] == 255), "应存在不透明像素");
        assert!(
            px.iter().any(|p| p[3] == 0),
            "背景必须透明：满幅不透明方图会在深色任务栏上显示为白底方块"
        );
        let colored = px.iter().filter(|p| p[3] > 200).any(|p| {
            (p[0] as i32 - p[1] as i32).abs() > 12
                || (p[1] as i32 - p[2] as i32).abs() > 12
                || (p[0] as i32 - p[2] as i32).abs() > 12
        });
        assert!(
            colored,
            "托盘图标必须是彩色的（Windows 不支持模板图标语义）"
        );
    }

    /// 单色素材本身的性质：透明底、单一墨色，且黑猫确实比白猫暗。
    ///
    /// 这里直接读文件而不是走 `tray_icon()`，让 Windows 之外也能校验素材。
    #[test]
    fn mono_icons_are_transparent_single_ink() {
        let black: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/icons/tray-icon-mono-black.rgba"
        ));
        let white: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/icons/tray-icon-mono-white.rgba"
        ));
        for (name, bytes) in [("黑猫", black), ("白猫", white)] {
            assert_eq!(bytes.len(), 32 * 32 * 4, "{name}素材尺寸应为 32×32");
            let px: Vec<&[u8]> = bytes.chunks_exact(4).collect();
            assert!(px.iter().any(|p| p[3] == 0), "{name}背景必须透明");
            assert!(px.iter().any(|p| p[3] == 255), "{name}应存在不透明像素");
            let inks: std::collections::HashSet<&[u8]> =
                px.iter().filter(|p| p[3] > 0).map(|p| &p[..3]).collect();
            assert_eq!(
                inks.len(),
                1,
                "{name}必须是单一墨色（模板剪影不该有彩色边缘）"
            );
        }
        let ink = |bytes: &[u8]| {
            let p = bytes.chunks_exact(4).find(|p| p[3] == 255).unwrap();
            p[0] as u32 + p[1] as u32 + p[2] as u32
        };
        assert!(ink(black) < ink(white), "黑猫必须比白猫暗");
    }

    /// 主题 → 素材的映射：浅色任务栏配黑猫，深色配白猫，读不到则退回彩色。
    #[test]
    fn tray_icon_variant_follows_taskbar_theme() {
        assert_eq!(
            tray_icon_variant(Some(true)),
            TrayIconVariant::MonoBlack,
            "浅色任务栏下白猫会隐形，必须用深色猫"
        );
        assert_eq!(
            tray_icon_variant(Some(false)),
            TrayIconVariant::MonoWhite,
            "深色任务栏下黑猫会隐形，必须用浅色猫"
        );
        assert_eq!(
            tray_icon_variant(None),
            TrayIconVariant::Color,
            "读不到主题时退回彩色素材，不能盲猜黑白"
        );
    }

    /// 真机自检：Windows 上必须读得到「Windows 模式」。
    ///
    /// 这条断言同时是「自适应方案在真实机器上到底行不行」的答案：读得到就按主题切换，
    /// 读不到则生产代码会退回彩色素材（不会显示异常，但自适应形同虚设）。
    /// 只在 Windows 上跑。
    #[cfg(windows)]
    #[test]
    fn taskbar_theme_is_readable_on_windows() {
        assert!(
            taskbar_uses_light_theme().is_some(),
            "读不到 SystemUsesLightTheme：自适应不可用，应改为固定素材"
        );
    }

    /// 左键「抬起」才唤窗；右键、按下都不唤窗。
    ///
    /// macOS 例外：左键要留给菜单，唤窗判定必须为 false。
    #[test]
    fn tray_left_click_release_wakes_main_window_only_off_macos() {
        assert_eq!(
            should_wake_main_window(MouseButton::Left, MouseButtonState::Up),
            !cfg!(target_os = "macos")
        );
        assert!(!should_wake_main_window(
            MouseButton::Left,
            MouseButtonState::Down
        ));
        assert!(!should_wake_main_window(
            MouseButton::Right,
            MouseButtonState::Up
        ));
        assert!(!should_wake_main_window(
            MouseButton::Middle,
            MouseButtonState::Up
        ));
    }
    #[test]
    fn silent_startup_matches_exact_hidden_arg() {
        assert!(is_silent_startup(["--hidden"]));
        assert!(is_silent_startup(["wb-switch-rust", "--hidden"]));
        assert!(is_silent_startup(["wb-switch-rust", "--hidden", "--debug"]));
    }

    #[test]
    fn silent_startup_rejects_unrelated_and_substring_args() {
        assert!(!is_silent_startup(Vec::<&str>::new()));
        assert!(!is_silent_startup(["wb-switch-rust"]));
        assert!(!is_silent_startup(["wb-switch-rust", "--debug"]));
        assert!(!is_silent_startup(["wb-switch-rust", "--hidden=true"]));
        assert!(!is_silent_startup(["wb-switch-rust", "-hidden"]));
        assert!(!is_silent_startup(["wb-switch-rust", "--hidden-x"]));
        assert!(!is_silent_startup(["wb-switch-rust", "x--hidden"]));
    }

    #[test]
    fn second_launch_with_exact_hidden_arg_does_not_activate() {
        // 插件回传的 args 含 argv[0]，静默判定必须整参相等。
        assert!(!should_activate_on_second_launch([
            "wb-switch-rust",
            "--hidden"
        ]));
    }

    #[test]
    fn second_launch_activates_unless_exact_hidden_arg() {
        assert!(should_activate_on_second_launch(Vec::<&str>::new()));
        assert!(should_activate_on_second_launch(["wb-switch-rust"]));
        assert!(should_activate_on_second_launch([
            "wb-switch-rust",
            "--debug"
        ]));
        assert!(should_activate_on_second_launch([
            "wb-switch-rust",
            "--hidden-x"
        ]));
        assert!(should_activate_on_second_launch([
            "wb-switch-rust",
            "x--hidden"
        ]));
    }

    #[test]
    fn tooltip_when_no_accounts() {
        assert_eq!(
            format_checkin_tooltip(&json!({"accounts": []})),
            "没有可签到的账号"
        );
    }

    #[test]
    fn tooltip_when_accounts_missing() {
        assert_eq!(format_checkin_tooltip(&json!({})), "没有可签到的账号");
    }

    #[test]
    fn tooltip_reports_overlapping_checkin_as_busy() {
        assert_eq!(
            format_checkin_tooltip(&json!({
                "accounts": [],
                "status": "skipped",
                "reason": "already_running"
            })),
            "签到任务正在进行，请稍后再试"
        );
    }

    #[test]
    fn tooltip_summarizes_success_already_error() {
        let payload = json!({
            "accounts": [
                {"result": "success"},
                {"result": "success"},
                {"result": "already"},
                {"result": "error"},
                {"result": "error"},
                {"result": "error"}
            ]
        });
        assert_eq!(
            format_checkin_tooltip(&payload),
            "签到完成：成功 2，已签 1，失败 3"
        );
    }

    /// 国际版账号签到结果为 inactive：单列「未开放」，不计入失败。
    #[test]
    fn tooltip_separates_inactive_variant_results() {
        let payload = json!({
            "accounts": [
                {"result": "success", "variant": "cn"},
                {"result": "inactive", "inactive": true, "variant": "ai"}
            ]
        });
        assert_eq!(
            format_checkin_tooltip(&payload),
            "签到完成：成功 1，已签 0，失败 0，未开放 1"
        );
    }

    #[test]
    fn checkin_succeeded_requires_all_ok() {
        use super::checkin_succeeded;
        assert!(!checkin_succeeded(&json!({})));
        assert!(!checkin_succeeded(&json!({"accounts": []})));
        assert!(checkin_succeeded(&json!({
            "accounts": [{"result": "success"}, {"result": "already"}]
        })));
        assert!(!checkin_succeeded(&json!({
            "accounts": [{"result": "success"}, {"result": "error"}]
        })));
        // inactive 不是失败：仍弹通知，但绝不伪造成成功。
        assert!(checkin_succeeded(&json!({
            "accounts": [{"result": "success"}, {"result": "inactive"}]
        })));
        assert!(!checkin_succeeded(&json!({
            "accounts": [{"result": "inactive"}, {"result": "error"}]
        })));
    }

    #[test]
    fn app_bundle_path_from_packaged_exe() {
        use std::path::Path;
        let exe = Path::new("/Applications/workbuddy-switch.app/Contents/MacOS/wb-switch-rust");
        assert_eq!(
            super::app_bundle_path_from_exe(exe),
            Some(Path::new("/Applications/workbuddy-switch.app"))
        );
    }

    #[test]
    fn app_bundle_path_none_outside_app_bundle() {
        use std::path::Path;
        assert!(super::app_bundle_path_from_exe(Path::new("/tmp/exec")).is_none());
        assert!(
            super::app_bundle_path_from_exe(Path::new("/Users/x/target/debug/wb-switch-rust"))
                .is_none()
        );
        assert!(super::app_bundle_path_from_exe(Path::new(
            "/Applications/workbuddy-switch.app/Contents/Resources/icon.icns"
        ))
        .is_none());
    }

    #[test]
    fn tooltip_ignores_unknown_results() {
        let payload = json!({
            "accounts": [
                {"result": "success"},
                {"result": "skipped"},
                {"result": null}
            ]
        });
        assert_eq!(
            format_checkin_tooltip(&payload),
            "签到完成：成功 1，已签 0，失败 0"
        );
    }

    /// 托盘更新入口的阶段映射（文案见 design §6，菜单与前端弹窗共用同一快照）。
    #[test]
    fn update_menu_spec_maps_each_phase_to_its_entry() {
        use super::{update_menu_spec, UpdateMenuAction, UpdatePhase, UpdateSnapshot};

        let snapshot = |phase: UpdatePhase, latest: Option<&str>, percent: Option<u8>| {
            UpdateSnapshot {
                phase,
                latest: latest.map(str::to_string),
                percent,
                message: None,
                checked_at: None,
            }
        };

        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Idle, None, None)),
            (UpdateMenuAction::Check, "检查更新".to_string(), true)
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::UpToDate, None, None)),
            (UpdateMenuAction::Check, "检查更新".to_string(), true)
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Checking, None, None)),
            (UpdateMenuAction::Check, "正在检查…".to_string(), false),
            "检查中不可重复点击"
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Available, Some("0.1.48"), None)),
            (
                UpdateMenuAction::Download,
                "升级到 v0.1.48".to_string(),
                true
            )
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Downloading, Some("0.1.48"), Some(42))),
            (
                UpdateMenuAction::Download,
                "正在下载更新 42%".to_string(),
                false
            )
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Downloading, Some("0.1.48"), None)),
            (
                UpdateMenuAction::Download,
                "正在下载更新…".to_string(),
                false
            ),
            "总量未知时只显示进行中，不显示假百分比"
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::ReadyToRestart, Some("0.1.48"), None)),
            (
                UpdateMenuAction::Restart,
                "重启以完成升级".to_string(),
                true
            ),
            "重启时机由用户决定，不自动重启"
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Error, Some("0.1.48"), None)),
            (
                UpdateMenuAction::Download,
                "更新失败，点击重试".to_string(),
                true
            ),
            "已知目标版本 → 重试的是下载"
        );
        assert_eq!(
            update_menu_spec(&snapshot(UpdatePhase::Error, None, None)),
            (
                UpdateMenuAction::Check,
                "检查更新失败，点击重试".to_string(),
                true
            ),
            "未知版本 → 重试的是检查"
        );
    }
}
