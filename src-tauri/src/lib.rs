// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod commands;

use std::time::Duration;
use wb_switch_core::modules;

#[cfg(desktop)]
use tauri::{
    menu::{MenuBuilder, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

#[cfg(desktop)]
fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
fn set_icon_pixel(pixels: &mut [u8], size: i32, x: i32, y: i32) {
    if x < 0 || y < 0 || x >= size || y >= size {
        return;
    }
    let offset = ((y * size + x) * 4) as usize;
    pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
}

#[cfg(desktop)]
fn draw_disc(pixels: &mut [u8], size: i32, cx: i32, cy: i32, radius: i32) {
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= radius * radius {
                set_icon_pixel(pixels, size, x, y);
            }
        }
    }
}

#[cfg(desktop)]
fn draw_segment(
    pixels: &mut [u8],
    size: i32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    width: i32,
) {
    let steps = (x2 - x1).abs().max((y2 - y1).abs()) * 2;
    let radius = width / 2;
    for step in 0..=steps {
        let progress = step as f32 / steps.max(1) as f32;
        let x = (x1 as f32 + (x2 - x1) as f32 * progress).round() as i32;
        let y = (y1 as f32 + (y2 - y1) as f32 * progress).round() as i32;
        draw_disc(pixels, size, x, y, radius);
    }
}

/// 简洁的单色双向切换图标，作为 macOS 菜单栏模板图标使用。
#[cfg(desktop)]
fn menu_bar_icon() -> tauri::image::Image<'static> {
    let size = 36;
    let mut pixels = vec![0; (size * size * 4) as usize];

    draw_segment(&mut pixels, size, 8, 10, 25, 10, 4);
    draw_segment(&mut pixels, size, 25, 10, 20, 5, 4);
    draw_segment(&mut pixels, size, 25, 10, 20, 15, 4);
    draw_segment(&mut pixels, size, 28, 26, 11, 26, 4);
    draw_segment(&mut pixels, size, 11, 26, 16, 21, 4);
    draw_segment(&mut pixels, size, 11, 26, 16, 31, 4);

    tauri::image::Image::new_owned(pixels, size as u32, size as u32)
}

#[cfg(desktop)]
fn setup_menu_bar(app: &mut tauri::App) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open-main-window", "打开主窗口", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit-app", "退出应用", true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .item(&open_item)
        .separator()
        .item(&quit_item)
        .build()?;

    TrayIconBuilder::with_id("main-menu-bar")
        .icon(menu_bar_icon())
        .icon_as_template(true)
        .tooltip("workbuddy-switch")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open-main-window" => show_main_window(app),
            "quit-app" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    let app_handle = app.handle().clone();
    if let Some(window) = app.get_webview_window("main") {
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
        });
    }

    Ok(())
}

/// 后台循环：30 秒轮询自动签到；每天一次保活刷新（对照 server.py `_background_loops`）。
fn spawn_background_loops() {
    tauri::async_runtime::spawn(async move {
        let mut last_keepalive_day = String::new();
        loop {
            let cfg = modules::config::load_checkin_config();
            if cfg.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                let _ = modules::checkin::run_checkin_cycle().await;
            }
            let today = modules::checkin::date_str(None);
            if today != last_keepalive_day {
                last_keepalive_day = today;
                let _ = modules::refresh::run_keepalive_cycle().await;
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            #[cfg(desktop)]
            setup_menu_bar(app)?;
            spawn_background_loops();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_accounts,
            commands::delete_account,
            commands::oauth_start,
            commands::oauth_status,
            commands::import_local,
            commands::manual_add,
            commands::switch_account,
            commands::list_sessions,
            commands::copy_sessions,
            commands::open_permission_settings,
            commands::check_auth_permission,
            commands::reveal_app_in_finder,
            commands::get_checkin_status,
            commands::checkin,
            commands::checkin_all,
            commands::get_auto_checkin_config,
            commands::save_auto_checkin_config,
            commands::get_checkin_logs,
            commands::refresh_account_token,
            commands::get_github_config,
            commands::save_github_config,
            commands::check_update,
            commands::relaunch_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
