// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod commands;
mod commands_local;
#[cfg(target_os = "macos")]
mod instance_lock;
#[cfg(desktop)]
mod tray;

use std::time::Duration;
use tauri::Emitter;
use wb_switch_core::modules;

const SCREENSHOT_DEMO_ENV: &str = "WB_SWITCH_SCREENSHOT_DEMO";

pub(crate) fn is_screenshot_demo() -> bool {
    std::env::var(SCREENSHOT_DEMO_ENV).as_deref() == Ok("1")
}

/// 轮换推迟提示：桌面端先向前端推 `rotate-deferred`（应用内提示，窗口开着就能看到），
/// 再尽力投递系统通知（应用在托盘/后台时可见）。
///
/// 应用内提示不依赖系统通知权限：插件在开发态会把通知登记到「终端」名下，且投递失败
/// 无法观测（`show()` 恒返回 Ok），所以两者都发、以前者为准。
/// 其它形态由 core 的日志与 `notify` 返回字段承载，宿主不投递。
pub(crate) fn deliver_rotate_notify(app: &tauri::AppHandle, result: &serde_json::Value) {
    #[cfg(desktop)]
    {
        if let Some(notify) = result.get("notify") {
            let _ = app.emit("rotate-deferred", notify.clone());
            tray::notify_rotate_deferred(app, notify);
        }
    }
    #[cfg(not(desktop))]
    {
        let _ = (app, result);
    }
}

/// 后台循环：自动签到启动即核验、每 30 分钟补签；自动轮换每 30 秒检查；每天一次保活；
/// 限额 hook 信号每秒轮询一次（入账即通知前端）；限额 hook 启动时后台默认接入。
fn spawn_background_loops(app: tauri::AppHandle) {
    let rotate_app = app.clone();
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

    // 派猫猫旅行：启动即派发，之后周期性补派（并重试 no-buddy / 瞬时错误）。
    // 档位过滤在 core（`travel_capable_accounts`）：不支持成长中心的档位不会发请求。
    tauri::async_runtime::spawn(async move {
        let _ = modules::travel::run_travel_cycle().await;
        loop {
            tokio::time::sleep(modules::travel::TRAVEL_RETRY_INTERVAL).await;
            let _ = modules::travel::run_travel_cycle().await;
        }
    });

    // 旅行领取：启动立刻查一轮（避免重启后空等 15 分钟漏领），之后按周期检查。
    tauri::async_runtime::spawn(async move {
        let _ = modules::travel::run_travel_claim_cycle().await;
        loop {
            tokio::time::sleep(modules::travel::TRAVEL_CLAIM_INTERVAL).await;
            let _ = modules::travel::run_travel_claim_cycle().await;
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
                    let result = modules::rotate::run_rotate_cycle().await;
                    deliver_rotate_notify(&rotate_app, &result);
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

    // 限额 hook 信号：轮询 `~/.wb-switch/hook-events.jsonl`（CLI / WorkBuddy 的 429 当轮
    // 由客户端 hook 追加），入账后通知前端立即拉取。轻量模式下窗口销毁但进程仍在，
    // 状态由后端持有（见 `rate_limit_events.rs`）。
    modules::rate_limit_events::spawn_watcher(move || {
        let _ = app.emit("rate-limits-updated", serde_json::json!({}));
    });

    // 默认接入：后台线程自动安装 hook（幂等、非阻塞、失败静默）。
    // 前置条件（开关开启 / 用户没卸载过 / 存在客户端 / 未装全）由 core 判定；
    // 装上了就作废扫描缓存——扫描范围从全量收窄到「未注册的来源」。
    std::thread::spawn(|| {
        if modules::rate_limit_hook::auto_install_on_startup() {
            modules::limits::invalidate_scan_cache();
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // 单实例互斥必须最先注册：`Builder::build()` 按注册顺序 initialize_plugins，
    // 插件 setup 命中已有实例会直接 `std::process::exit(0)`，因此第二个进程在
    // 建主窗口 / 建托盘图标 / 起后台循环之前就已退出，不会产生账号侧副作用。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            tray::on_second_instance(app, args);
        }));
    }

    builder = builder
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
                // 插件已在 initialize_plugins 阶段决定 notify-or-exit；此处只兜底
                // 插件漏掉的 macOS 竞态。必须在 tray::setup 之前：拿不到锁的第二
                // 实例不能先建出托盘图标。不得放到 run() 开头，否则会抢在插件
                // notify 之前拦下正常第二实例，丢掉「再点开 → 既有窗口弹出」。
                #[cfg(target_os = "macos")]
                instance_lock::acquire_or_exit(app.handle());
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
                spawn_background_loops(app.handle().clone());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_accounts,
            commands::get_codebuddy_cli_status,
            commands::install_codebuddy_cli_helper,
            commands::switch_codebuddy_cli_account,
            commands::get_codebuddy_cn_ide_status,
            commands::switch_codebuddy_cn_ide_account,
            commands::detect_codebuddy_cn_ide_account,
            commands::get_codebuddy_ide_status,
            commands::switch_codebuddy_ide_account,
            commands::detect_codebuddy_ide_account,
            commands::delete_account,
            commands::oauth_start,
            commands::oauth_status,
            commands::import_local,
            commands::export_accounts,
            commands::export_accounts_to_path,
            commands::preview_import_accounts,
            commands::import_accounts,
            commands_local::align_automations,
            commands::switch_account,
            commands::list_sessions,
            commands::copy_sessions,
            commands::open_permission_settings,
            commands::check_auth_permission,
            commands::reveal_app_in_finder,
            commands::get_checkin_status,
            commands::get_credit_expiry,
            commands::get_credit_statistics,
            commands::get_token_statistics,
            commands::get_rate_limits,
            commands::get_rate_limit_hook_status,
            commands::install_rate_limit_hook,
            commands::uninstall_rate_limit_hook,
            commands::get_rate_limit_config,
            commands::save_rate_limit_config,
            commands::checkin,
            commands::checkin_all,
            commands::get_auto_checkin_config,
            commands::save_auto_checkin_config,
            commands::get_checkin_logs,
            commands::get_travel_status,
            commands::get_auto_travel_config,
            commands::save_auto_travel_config,
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
        {
            // 点击 Dock / Finder 再次激活已运行实例：窗口已隐藏到托盘时显示主窗口。
            // `Reopen` 在主线程派发，可直接调用窗口路径。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = &event
            {
                tray::show_main_window_on_reopen(_app_handle);
            }
            tray::on_run_event(event);
        }
    });
}
