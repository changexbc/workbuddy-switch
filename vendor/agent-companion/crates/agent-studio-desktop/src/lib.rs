mod hit_test;
mod integration_folder;
#[cfg(target_os = "macos")]
mod background_cursor;
mod rail_settings;
mod session;
use agent_studio_runtime::Client;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;
pub const RAIL: &str = "agent-studio-rail";
pub const SETTINGS: &str = "agent-studio-settings";
#[derive(Clone)]
pub struct Config {
    pub assets: String,
    pub runtime: PathBuf,
    /// Whether this host owns login-item registration. Embedded hosts set false.
    pub manage_autostart: bool,
    /// Default on a fresh install. Embedded hosts can opt in only after consent.
    pub default_enabled: bool,
    pub show_rail: bool,
}
struct Service {
    client: Mutex<Option<Client>>,
    snapshot: Mutex<Value>,
    connected: AtomicBool,
    stop: AtomicBool,
    enabled: AtomicBool,
    wake: Condvar,
    config: Config,
}
fn enabled_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("agent-studio-enabled.json"))
}
pub fn is_enabled<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    app.try_state::<Arc<Service>>()
        .map(|s| s.enabled.load(Ordering::Acquire))
        .unwrap_or_else(|| read_enabled(app, false))
}
fn read_enabled<R: tauri::Runtime>(app: &tauri::AppHandle<R>, default_enabled: bool) -> bool {
    enabled_path(app)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<bool>(&b).ok())
        .unwrap_or(default_enabled)
}
/// Called from an async host command, outside Tauri's plugin setup lock.
pub fn set_enabled(app: &tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    let state = app
        .try_state::<Arc<Service>>()
        .ok_or("Agent Companion 正在初始化，请稍后重试")?;
    let mut client = state.client.lock().map_err(|e| e.to_string())?;
    if enabled && state.config.show_rail {
        if let Some(rail) = app.get_webview_window(RAIL) {
            rail.show().map_err(|e| e.to_string())?;
        } else {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let handle = app.clone();
            let config = state.config.clone();
            app.run_on_main_thread(move || {
                let _ = tx.send(create_rail(&handle, &config).map_err(|e| e.to_string()));
            })
            .map_err(|e| e.to_string())?;
            rx.recv().map_err(|e| e.to_string())??;
        }
    }
    let path = enabled_path(app)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, if enabled { "true" } else { "false" }).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    state.enabled.store(enabled, Ordering::Release);
    state.wake.notify_all();
    if !enabled {
        *client = None; // Release this host's lease; other applications retain theirs.
        state.connected.store(false, Ordering::Relaxed);
        *state.snapshot.lock().unwrap() = Value::Null;
        for label in [RAIL, SETTINGS] {
            if let Some(w) = app.get_webview_window(label) {
                let _ = w.hide();
            }
        }
        let _ = app.emit("monitor-connection", "offline");
    }
    let _ = app.emit("agent-studio:enabled", enabled);
    Ok(enabled)
}
#[tauri::command]
fn monitor_state(state: State<'_, Arc<Service>>) -> Value {
    json!({"snapshot":*state.snapshot.lock().unwrap(),"connected":state.connected.load(Ordering::Relaxed)})
}
#[tauri::command]
async fn collector_request(
    state: State<'_, Arc<Service>>,
    command: String,
    payload: Option<Value>,
) -> Result<Value, String> {
    if !matches!(
        command.as_str(),
        "settings_get" | "settings_set" | "settings_check" | "integrations_get" | "integrations_set"
            | "custom_integrations_get" | "custom_integrations_set" | "custom_preview"
            | "session_monitor_close"
    ) {
        return Err("不支持的命令".into());
    }
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state
            .client
            .lock()
            .unwrap()
            .as_ref()
            .ok_or("采集器未连接")?
            .request(&command, payload.unwrap_or(Value::Null))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn set_hit_regions(
    app: tauri::AppHandle,
    regions: Vec<hit_test::Region>,
    state: State<'_, hit_test::Regions>,
) -> Result<(), String> {
    if regions.len() > 512 || regions.iter().any(|r| !r.valid()) {
        return Err("无效的窗口交互区域".into());
    }
    *state.lock().unwrap() = regions;
    #[cfg(target_os = "macos")]
    if let Some(w) = app.get_webview_window(RAIL) {
        let pointer = w.ns_window().map_err(|e| e.to_string())? as usize;
        let regions = state.inner().clone();
        app.run_on_main_thread(move || hit_test::refresh(pointer, &regions))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    hit_test::refresh();
    Ok(())
}
#[tauri::command]
fn close_settings(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(SETTINGS) {
        w.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
async fn open_view(app: tauri::AppHandle, view: String) -> Result<(), String> {
    // Sync plugin handlers run while the plugin-store lock is held. Window
    // construction re-enters that store; defer it until dispatch has returned.
    tauri::async_runtime::spawn_blocking(move || open(&app, &view))
        .await
        .map_err(|e| e.to_string())?
}
pub fn open(app: &tauri::AppHandle, view: &str) -> Result<(), String> {
    let state = app.state::<Arc<Service>>();
    if !state.enabled.load(Ordering::Acquire) {
        return Err("Agent Companion 已关闭".into());
    }
    let (label, file, title, width, height) = match view {
        "settings" => (
            SETTINGS,
            "desktop-settings.html",
            "Agent Companion · 悬浮窗设置",
            480.,
            700.,
        ),
        "rail" => {
            if let Some(w) = app.get_webview_window(RAIL) {
                w.show().map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        _ => return Err("未知视图".into()),
    };
    if let Some(w) = app.get_webview_window(label) {
        return focus_settings(&w);
    }
    let mut window = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("{}{file}", state.config.assets).into()),
    )
    .title(title)
    .inner_size(width, height)
    .min_inner_size(420., 520.)
    .center();
    #[cfg(target_os = "macos")]
    {
        // Overlay chrome matches wb-switch: content draws under the traffic lights.
        window = window
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(16., 18.))
            .allow_link_preview(false);
    }
    let window = window.build().map_err(|e| e.to_string())?;
    focus_settings(&window)
}

fn focus_settings(window: &tauri::WebviewWindow) -> Result<(), String> {
    window.show().map_err(|e| e.to_string())?;
    window.unminimize().map_err(|e| e.to_string())?;
    // On macOS set_focus also activates the application. Window construction
    // alone can leave this accessory app behind the previously active app.
    window.set_focus().map_err(|e| e.to_string())
}
pub fn toggle_rail(app: &tauri::AppHandle) {
    if !is_enabled(app) {
        return;
    }
    if let Some(w) = app.get_webview_window(RAIL) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
        }
    }
}
fn position_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|p| p.join("agent-studio-rail-position.json"))
}
pub fn init(config: Config) -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("agent-studio")
        .on_window_ready(|window| {
            // WKWebView can remain document-visible while its native window is minimized.
            std::thread::spawn(move || {
                let mut previous = None;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let (Ok(visible), Ok(minimized)) = (window.is_visible(), window.is_minimized()) else { break; };
                    let active = visible && !minimized;
                    if previous != Some(active) {
                        let _ = window.emit_to(window.label(), "agent-studio-window-active", serde_json::json!({"label":window.label(),"active":active}));
                        previous = Some(active);
                    }
                }
            });
        })
        .invoke_handler(tauri::generate_handler![
            monitor_state,
            collector_request,
            set_hit_regions,
            rail_settings::rail_settings_get,
            rail_settings::rail_settings_set,
            close_settings,
            open_view,
            session::open_session_url,
            integration_folder::open_integration_folder
        ])
        .setup(move |app, _| {
            // Plugin initialization holds Tauri's plugin store lock. Queue window
            // creation from another thread so preparation runs after it unlocks.
            let app = app.clone();
            std::thread::spawn(move || {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Err(error) = start(&handle, config) {
                        eprintln!("Agent Companion initialization: {error}");
                        let _ = handle.emit("monitor-warning", error.to_string());
                    }
                });
            });
            Ok(())
        })
        .on_event(|app, event| match event {
            // The rail hides on close; settings close normally and release their WebView.
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } if label == RAIL => {
                api.prevent_close();
                if let Some(w) = app.get_webview_window(RAIL) {
                    let _ = w.hide();
                }
            }
            tauri::RunEvent::Exit => {
                if let Some(w) = app.get_webview_window(RAIL) {
                    if let (Ok(p), Ok(scale), Some(file)) =
                        (w.outer_position(), w.scale_factor(), position_path(app))
                    {
                        let p = p.to_logical::<f64>(scale);
                        if let Some(parent) = file.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(file, json!({"x":p.x,"y":p.y}).to_string());
                    }
                }
                if let Some(s) = app.try_state::<Arc<Service>>() {
                    s.stop.store(true, Ordering::Relaxed);
                    s.wake.notify_all();
                    if let Ok(mut c) = s.client.try_lock() {
                        *c = None;
                    }
                }
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                hit_test::remove();
            }
            _ => {}
        })
        .build()
}

fn start(app: &tauri::AppHandle, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let enabled = read_enabled(app, config.default_enabled);
    let regions: hit_test::Regions = Arc::new(Mutex::new(vec![]));
    app.manage(regions);
    let state = Arc::new(Service {
        client: Mutex::new(None),
        snapshot: Mutex::new(Value::Null),
        connected: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        enabled: AtomicBool::new(enabled),
        wake: Condvar::new(),
        config: config.clone(),
    });
    app.manage(state.clone());
    if enabled {
        create_rail(app, &config)?;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut previous_settings = Value::Null;
        while !state.stop.load(Ordering::Relaxed) {
            let mut guard = state.client.lock().unwrap();
            guard = state
                .wake
                .wait_while(guard, |_| {
                    !state.enabled.load(Ordering::Acquire) && !state.stop.load(Ordering::Relaxed)
                })
                .unwrap();
            if state.stop.load(Ordering::Relaxed) {
                break;
            }
            if guard.is_none() {
                *guard = Client::connect(agent_studio_runtime::home(), &state.config.runtime).ok();
            }
            let result = guard
                .as_ref()
                .ok_or("连接中".to_string())
                .and_then(Client::poll);
            match result {
                Ok(v) => {
                    state
                        .connected
                        .store(v["snapshot"]["ready"] == true, Ordering::Relaxed);
                    *state.snapshot.lock().unwrap() = v["snapshot"].clone();
                    let _ = app.emit("monitor-state", &v["snapshot"]);
                    if v["settings"] != previous_settings {
                        previous_settings = v["settings"].clone();
                        let _ = app.emit("monitor-settings", &previous_settings);
                    }
                    for a in v["notifications"].as_array().into_iter().flatten() {
                        // 再单独发给 rail：桌面端的声音以前只有系统通知那一条（Windows 上还不生效），
                        // rail 侧据此播自定义提示音；`sound` 开关由 runtime 随告警一起发下来。
                        let _ = app.emit("monitor-alert", a.clone());
                        let kind = match a["kind"].as_str() {
                            Some("wait") => "需要确认",
                            Some("error") => "任务失败",
                            // 客户端不发「需要确认」信号时的兜底推断：工具长时间没有反馈。
                            Some("prolonged") => "长时间无响应",
                            _ => "任务完成",
                        };
                        let mut n = app
                            .notification()
                            .builder()
                            .title(format!("Agent Companion · {kind}"))
                            .body(a["title"].as_str().unwrap_or("会话状态已更新"));
                        if a["sound"] == true {
                            // builder 的声音在 Windows 上不生效，所以自己再播一声系统音。
                            n = n.sound("default");
                            snd::play(sound_alias(a["kind"].as_str().unwrap_or("")));
                        }
                        let _ = n.show();
                    }
                }
                Err(_) => {
                    state.connected.store(false, Ordering::Relaxed);
                    let _ = app.emit("monitor-connection", "offline");
                    *guard = None;
                }
            }
            drop(guard);
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    Ok(())
}

/// 按告警类型挑提示音（Windows 的 `PlaySound` 认系统音色别名，也认 wav 文件路径）。
///
/// **三种都用具体 wav**：系统别名在不同的声音方案/主题下会指向同一个文件（本机实测
/// `SystemAsterisk` 与 `SystemExclamation` 听起来一样，用户据此反馈「需要确认和完成音一样」），
/// 只有指定文件才能保证三者彼此不同。文件缺失时 `snd::play` 会退回系统信息音，不会变哑。
fn sound_alias(raw_kind: &str) -> &'static str {
    match raw_kind {
        // 用户听感：通知音偏小、感叹音更响更明显 ⇒ **需要确认与完成都用感叹音**（要抓得住
        // 注意力；用户明确要求两者一致）。失败仍用错误音，保持可区分。
        "error" => r"C:\Windows\Media\Windows Error.wav",
        _ => r"C:\Windows\Media\Windows Exclamation.wav",
    }
}

/// 自己播一声提示音。
///
/// Windows 上 `tauri::notification().sound("default")` 实际不生效（toast 的声音由应用的
/// toast XML / 系统设置决定，builder 的 sound 基本是 macOS/iOS 侧能力），所以这里自己播：
/// Windows 走 `PlaySoundW` 播系统音色别名（零新依赖、不需要音频文件），macOS 用 `afplay`。
/// 失败一律静默——提示音绝不能影响主流程。
#[cfg(target_os = "windows")]
mod snd {
    #[link(name = "winmm")]
    extern "system" {
        fn PlaySoundW(pszSound: *const u16, hmod: isize, fdwSound: u32) -> i32;
    }

    const SND_ALIAS: u32 = 0x0001_0000;
    const SND_FILENAME: u32 = 0x0002_0000;
    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;

    /// `spec` 以 `.wav` 结尾就按**文件**播（缺文件时退回系统信息音），否则当系统音色别名播。
    ///
    /// 需要具体音色时用文件：系统别名在多数主题里会指向同一个声音（本机实测 `SystemAsterisk`
    /// 与 `SystemExclamation` 听起来一样），只有指定 wav 才能保证「需要确认」和「完成」不同。
    pub(super) fn play(spec: &str) {
        let is_wav = spec.to_ascii_lowercase().ends_with(".wav");
        let probe = if is_wav && !std::path::Path::new(spec).is_file() {
            "SystemAsterisk"
        } else {
            spec
        };
        let flags = if probe.to_ascii_lowercase().ends_with(".wav") {
            SND_FILENAME
        } else {
            SND_ALIAS
        };
        let name: Vec<u16> = probe.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            PlaySoundW(name.as_ptr(), 0, flags | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

/// 非 Windows：交给系统播放器（同样是 best-effort）。
#[cfg(not(target_os = "windows"))]
mod snd {
    use std::process::Command;

    pub(super) fn play(_alias: &str) {
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut c = Command::new("afplay");
            c.arg("/System/Library/Sounds/Glass.aiff");
            c
        };
        #[cfg(not(target_os = "macos"))]
        let mut command = {
            let mut c = Command::new("paplay");
            c.arg("/usr/share/sounds/freedesktop/stereo/complete.oga");
            c
        };
        let _ = command.spawn();
    }
}

fn create_rail(app: &tauri::AppHandle, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let rail = WebviewWindowBuilder::new(
        app,
        RAIL,
        WebviewUrl::App(format!("{}desktop.html", config.assets).into()),
    )
    .title("Agent Companion · 会话栏")
    .inner_size(368., 600.)
    .transparent(true)
    .decorations(false)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false)
    .focusable(false)
    .accept_first_mouse(true)
    .visible_on_all_workspaces(true)
    .visible(config.show_rail)
    .build()?;
    if let Some(m) = rail.primary_monitor()? {
        let scale = m.scale_factor();
        let size = m.size().to_logical::<f64>(scale);
        let origin = m.position().to_logical::<f64>(scale);
        let saved: Value = position_path(app)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let x = saved["x"]
            .as_f64()
            .unwrap_or(origin.x + size.width - 380.)
            .clamp(origin.x - 240., origin.x + size.width - 70.);
        let y = saved["y"]
            .as_f64()
            .unwrap_or(origin.y + 80.)
            .clamp(origin.y + 30., origin.y + size.height - 130.);
        rail.set_position(tauri::LogicalPosition::new(x, y))?;
    }
    #[cfg(target_os = "macos")]
    hit_test::install(
        rail.ns_window()? as usize,
        app.state::<hit_test::Regions>().inner().clone(),
        {
            let window = rail.clone();
            move |point| {
                let payload = point.map(|(x, y)| json!({ "x": x, "y": y }));
                let _ = window.emit("agent-studio-pointer", payload);
            }
        },
    );
    #[cfg(target_os = "windows")]
    hit_test::install(
        rail.clone(),
        app.state::<hit_test::Regions>().inner().clone(),
    );
    Ok(())
}
