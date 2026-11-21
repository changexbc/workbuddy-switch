// vendor 快照：CI 的 clippy 门禁只针对本仓库自有代码。test.yml 里的 --no-deps
// 不会跳过 path 依赖，所以在这里显式放行快照自身的告警。
#![allow(warnings)]

mod hit_test;
mod integration_folder;
#[cfg(target_os = "macos")]
mod background_cursor;
#[cfg(target_os = "macos")]
mod panel;
mod rail_position;
mod rail_settings;
mod session;
use agent_studio_runtime::Client;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
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
async fn rail_reset_position(app: tauri::AppHandle) -> Result<(), String> {
    // One main-thread read plus a ~200 ms animated move would stall the UI if
    // run inline; hand both to the blocking pool instead.
    tauri::async_runtime::spawn_blocking(move || {
        let rail = app.get_webview_window(RAIL).ok_or("悬浮窗未开启")?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let read_rail = rail.clone();
        app.run_on_main_thread(move || {
            let _ = tx.send(rail_position::reset_plan(&read_rail));
        })
        .map_err(|e| e.to_string())?;
        let target = rx.recv().map_err(|e| e.to_string())??;
        rail_position::glide(&rail, target);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
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
/// Rail move timestamps in milliseconds: `last` is written by
/// `WindowEvent::Moved`, `checked` remembers the value a correction pass has
/// already handled so one settled position is processed once.
struct MoveWatch {
    last: AtomicU64,
    checked: AtomicU64,
}
impl MoveWatch {
    fn new() -> Self {
        Self { last: AtomicU64::new(0), checked: AtomicU64::new(0) }
    }
    fn touch(&self) {
        self.last.store(now_ms(), Ordering::Release);
    }
}
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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
            rail_reset_position,
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
            // Native drags run a modal loop; the worker thread only reads this
            // timestamp and acts once the moves have gone quiet.
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Moved(_),
                ..
            } if label == RAIL => {
                if let Some(watch) = app.try_state::<Arc<MoveWatch>>() {
                    watch.touch();
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
    // Correct a rail that ended up outside every work area (dragged off screen,
    // or its monitor was unplugged). The 500 ms of silence keeps the write out
    // of the platform's modal drag loop, and the swap makes sure each settled
    // position runs through `plan` exactly once.
    {
        let watch = Arc::new(MoveWatch::new());
        app.manage(watch.clone());
        let app = app.clone();
        let state = state.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if state.stop.load(Ordering::Relaxed) {
                break;
            }
            let last = watch.last.load(Ordering::Acquire);
            if last == 0 || now_ms().saturating_sub(last) < 500 {
                continue;
            }
            // A move that landed after the snapshot is not quiet yet. Don't
            // claim the old stamp; the next tick waits out the new one.
            if watch.last.load(Ordering::Acquire) != last {
                continue;
            }
            if watch.checked.swap(last, Ordering::AcqRel) == last {
                continue;
            }
            if let Some(rail) = app.get_webview_window(RAIL) {
                // Snapshot before queueing so a reset that runs first on the
                // main thread invalidates this correction (stale `from`
                // would jump the window back off-screen).
                let generation = rail_position::animation_generation();
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                let read_rail = rail.clone();
                if app
                    .run_on_main_thread(move || {
                        let _ = tx.send(rail_position::plan_at(&read_rail, generation));
                    })
                    .is_err()
                {
                    continue;
                }
                if let Ok(Ok(Some(target))) = rx.recv() {
                    rail_position::glide(&rail, target);
                }
            }
        });
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
                        let kind = match a["kind"].as_str() {
                            Some("wait") => "需要确认",
                            Some("error") => "任务失败",
                            _ => "任务完成",
                        };
                        let mut n = app
                            .notification()
                            .builder()
                            .title(format!("Agent Companion · {kind}"))
                            .body(a["title"].as_str().unwrap_or("会话状态已更新"));
                        if a["sound"] == true {
                            n = n.sound("default");
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
    // Restore through the shared visibility rule; the first launch keeps the
    // original top-right default. Losing the monitor list only skips the move.
    let saved: Value = position_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let saved = match (saved["x"].as_f64(), saved["y"].as_f64()) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
        _ => None,
    };
    if let Err(error) = rail_position::place(&rail, saved) {
        eprintln!("Agent Companion 悬浮窗位置: {error}");
    }
    #[cfg(target_os = "macos")]
    {
        let ns_window = rail.ns_window()? as usize;
        // Clicking the rail must not activate the host application (which
        // would pull every other host window to the front). A failed
        // conversion keeps the plain window, which still works.
        if let Err(error) = panel::convert(ns_window) {
            eprintln!("Agent Companion rail panel conversion: {error}");
        }
        hit_test::install(
            ns_window,
            app.state::<hit_test::Regions>().inner().clone(),
            {
                let window = rail.clone();
                move |point| {
                    let payload = point.map(|(x, y)| json!({ "x": x, "y": y }));
                    let _ = window.emit("agent-studio-pointer", payload);
                }
            },
        );
    }
    #[cfg(target_os = "windows")]
    hit_test::install(
        rail.clone(),
        app.state::<hit_test::Regions>().inner().clone(),
    );
    Ok(())
}
