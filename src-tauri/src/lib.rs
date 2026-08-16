// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod commands;

use std::time::Duration;
use wb_switch_core::modules;

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
        .setup(|_app| {
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
