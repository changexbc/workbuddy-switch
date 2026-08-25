// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod commands;
#[cfg(desktop)]
mod tray;

use std::time::Duration;
use wb_switch_core::modules;

const SCREENSHOT_DEMO_ENV: &str = "WB_SWITCH_SCREENSHOT_DEMO";

pub(crate) fn is_screenshot_demo() -> bool {
    std::env::var(SCREENSHOT_DEMO_ENV).as_deref() == Ok("1")
}

/// 后台循环：自动签到启动即核验、每 30 分钟补签；自动轮换每 30 秒检查；每天一次保活。
fn spawn_background_loops() {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = modules::config::compact_checkin_logs() {
            eprintln!("[签到] 历史日志整理失败: {error}");
        }
        let _ =
            modules::checkin::run_checkin_cycle(modules::checkin::CheckinCycleMode::StartupVerify)
                .await;
        loop {
            tokio::time::sleep(modules::checkin::CHECKIN_RECOVERY_INTERVAL).await;
            let _ = modules::checkin::run_checkin_cycle(
                modules::checkin::CheckinCycleMode::PeriodicRecovery,
            )
            .await;
        }
    });

    tauri::async_runtime::spawn(async move {
        let mut last_keepalive_day = String::new();
        let mut last_rotate_at: i64 = 0;
        loop {
            // 自动轮换（CodeBuddy CLI）：按配置间隔执行
            let rotate_cfg = modules::config::load_auto_rotate_config();
            if rotate_cfg.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                let interval_minutes = rotate_cfg
                    .get("check_interval_minutes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(5)
                    .max(1);
                let now = modules::config::now_ms();
                if now - last_rotate_at >= interval_minutes * 60_000 {
                    last_rotate_at = now;
                    let _ = modules::rotate::run_rotate_cycle().await;
                }
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
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init());

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![tray::SILENT_STARTUP_ARG]),
        ));
        builder = builder.on_window_event(tray::on_window_event);
    }

    let app = builder
        .setup(|app| {
            #[cfg(desktop)]
            {
                tray::setup(app)?;
                // 主窗口由配置创建为不可见；在事件循环呈现前决定本次启动是否静默。
                // 仅系统自启（精确 `--hidden` 参数）进入静默托盘，普通启动立即显示主窗口。
                tray::setup_startup_visibility(
                    app.handle(),
                    tray::is_silent_startup(std::env::args()),
                );
            }
            // README 截图模式只渲染前端虚构数据，禁止读取账号后执行签到、轮换或保活。
            if !is_screenshot_demo() {
                spawn_background_loops();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_accounts,
            commands::get_codebuddy_cli_status,
            commands::install_codebuddy_cli_helper,
            commands::switch_codebuddy_cli_account,
            commands::delete_account,
            commands::oauth_start,
            commands::oauth_status,
            commands::import_local,
            commands::export_accounts,
            commands::export_accounts_to_path,
            commands::preview_import_accounts,
            commands::import_accounts,
            commands::switch_account,
            commands::list_sessions,
            commands::copy_sessions,
            commands::open_permission_settings,
            commands::check_auth_permission,
            commands::reveal_app_in_finder,
            commands::get_checkin_status,
            commands::get_credit_expiry,
            commands::get_credit_statistics,
            commands::checkin,
            commands::checkin_all,
            commands::get_auto_checkin_config,
            commands::save_auto_checkin_config,
            commands::get_checkin_logs,
            commands::refresh_account_token,
            commands::get_auto_rotate_config,
            commands::save_auto_rotate_config,
            commands::rotate_status,
            commands::run_rotate,
            commands::get_rotate_logs,
            commands::get_github_config,
            commands::save_github_config,
            commands::check_update,
            commands::relaunch_app,
            commands::get_launch_at_login_enabled,
            commands::set_launch_at_login_enabled,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, event| {
        #[cfg(desktop)]
        tray::on_run_event(event);
    });
}
