#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod icons;
#[cfg(all(target_os = "macos", feature = "diagnostics"))]
mod native_qa;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
fn main() {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let runtime = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("agent-studio-runtime{suffix}"));
    let runtime = if runtime.is_file() {
        runtime
    } else {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!(
                "agent-studio-runtime-{}{suffix}",
                env!("DESKTOP_TARGET")
            ))
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            let _ = agent_studio_desktop::open(app, "rail");
        }))
        .plugin(agent_studio_desktop::init(agent_studio_desktop::Config {
            assets: String::new(),
            runtime,
            manage_autostart: true,
            default_enabled: true,
            show_rail: true,
        }))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                let _ = app.set_dock_visibility(false);
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            let rail = MenuItem::with_id(app, "rail", "显示 / 隐藏会话栏", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出 Agent Companion", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&rail, &settings, &quit])?;
            TrayIconBuilder::with_id("agent-companion")
                .icon(icons::tray_icon())
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Agent Companion")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "rail" => agent_studio_desktop::toggle_rail(app),
                    "settings" => {
                        let _ = agent_studio_desktop::open(app, "settings");
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            #[cfg(all(target_os = "macos", feature = "diagnostics"))]
            native_qa::schedule(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Unable to start Agent Companion")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested {
                api, code: None, ..
            } => api.prevent_exit(),
            tauri::RunEvent::Reopen { .. } => {
                let _ = agent_studio_desktop::open(app, "rail");
            }
            _ => {}
        });
}
