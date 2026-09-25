use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences { pub avatar_style: String, pub visible_count: u8, pub animation: bool, #[serde(default = "default_size")] pub size: String }
fn default_size() -> String { "standard".into() }
impl Default for Preferences {
    fn default() -> Self { Self { avatar_style: "animal".into(), visible_count: 8, animation: true, size: default_size() } }
}
impl Preferences {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.avatar_style.as_str(), "animal" | "bot") || !(3..=16).contains(&self.visible_count) || !matches!(self.size.as_str(), "small" | "medium" | "standard") { return Err("悬浮窗设置无效".into()); }
        Ok(())
    }
}
fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().app_config_dir().map_err(|e| e.to_string())?.join("rail-settings.json"))
}
fn login_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().home_dir().map_err(|e| e.to_string())?.join("Library/LaunchAgents").join(format!("{}.rail-login.plist", app.config().identifier)))
}
fn bundle() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()?.ancestors().find(|p| p.extension().is_some_and(|s| s == "app")).map(|p| p.to_owned())
}
fn launch_command() -> Result<(Vec<String>, Option<String>), String> {
    if let Some(bundle) = bundle() {
        return Ok((vec!["/usr/bin/open".into(), "-gj".into(), bundle.to_string_lossy().into_owned()], None));
    }
    // A dev executable needs its Vite server. Start the existing dev script,
    // retaining the Node toolchain PATH instead of registering a broken binary.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().map_err(|e| e.to_string())?;
    let path = std::env::var("PATH").unwrap_or_default();
    let npm = std::env::split_paths(&path).map(|p| p.join("npm")).find(|p| p.is_file()).ok_or("找不到 npm，无法配置开发版自启")?;
    if !root.join("package.json").is_file() { return Err("找不到桌面启动项目".into()); }
    Ok((vec![npm.to_string_lossy().into_owned(), "--prefix".into(), root.to_string_lossy().into_owned(), "run".into(), "desktop:dev".into()], Some(path)))
}
fn managed(app: &tauri::AppHandle) -> bool {
    app.state::<std::sync::Arc<super::Service>>().config.manage_autostart
}
fn supported(app: &tauri::AppHandle) -> bool {
    cfg!(target_os = "macos") && managed(app) && launch_command().is_ok()
}
fn launch_plist(label: &str, args: &[String], path: Option<&str>) -> String {
    let arguments = args.iter().map(|a| format!("<string>{}</string>", xml(a))).collect::<String>();
    let environment = path.map(|p| format!("<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>", xml(p))).unwrap_or_default();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{arguments}</array>{environment}<key>RunAtLoad</key><true/></dict></plist>", xml(label))
}
fn response(app: &tauri::AppHandle, preferences: Preferences) -> Result<Value, String> {
    let mut value = serde_json::to_value(preferences).map_err(|e| e.to_string())?;
    value["autostartSupported"] = json!(supported(app));
    // Embedded hosts own their own login item, so the settings page hides the row.
    value["autostartManaged"] = json!(managed(app));
    value["autostart"] = json!(login_path(app)?.exists());
    Ok(value)
}
#[tauri::command]
pub fn rail_settings_get(app: tauri::AppHandle) -> Result<Value, String> {
    let preferences = match std::fs::read(path(&app)?) {
        Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(e) => return Err(e.to_string()),
    };
    preferences.validate()?;
    response(&app, preferences)
}
fn xml(value: &str) -> String { value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;") }
#[tauri::command]
pub fn rail_settings_set(app: tauri::AppHandle, preferences: Preferences, autostart: bool) -> Result<Value, String> {
    preferences.validate()?;
    let login = login_path(&app)?;
    if autostart != login.exists() {
        if !supported(&app) { return Err("当前运行环境不支持开机自启".into()); }
        if autostart {
            let (args, environment) = launch_command()?;
            let plist = launch_plist(&format!("{}.rail-login", app.config().identifier), &args, environment.as_deref());
            std::fs::create_dir_all(login.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(&login, plist).map_err(|e| e.to_string())?;
        } else { std::fs::remove_file(&login).map_err(|e| e.to_string())?; }
    }
    let file = path(&app)?;
    std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    let temporary = file.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&preferences).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, file).map_err(|e| e.to_string())?;
    let value = response(&app, preferences)?;
    app.emit("agent-studio-rail-settings", &value).map_err(|e| e.to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_preferences_keep_the_existing_size() {
        let preferences: Preferences = serde_json::from_value(json!({"avatarStyle":"animal","visibleCount":8,"animation":true})).unwrap();
        assert_eq!(preferences.size, "standard");
        assert!(preferences.validate().is_ok());
    }
    #[test]
    fn login_plist_keeps_arguments_separate_and_escapes_paths() {
        let args = vec!["/opt/node tools/npm".into(), "--prefix".into(), "/Users/A & B/project".into(), "run".into(), "desktop:dev".into()];
        let plist = launch_plist("com.test.rail-login", &args, Some("/opt/node tools:/usr/bin"));
        assert!(plist.contains("<string>/Users/A &amp; B/project</string>"));
        assert!(plist.contains("<string>run</string><string>desktop:dev</string>"));
        assert!(plist.contains("<key>PATH</key><string>/opt/node tools:/usr/bin</string>"));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    }
}
