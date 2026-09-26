use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
fn path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_config_dir().map_err(|e| e.to_string())?.join("rail-settings.json"))
}

/// Login-time launch target. `args` follow `program`; `env_path` carries PATH
/// for the macOS dev fallback, which has no Windows/Linux equivalent.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Launch { program: PathBuf, args: Vec<String>, env_path: Option<String> }

/// The configured product name. The Windows Run value is named after it, so the
/// NSIS uninstall (which deletes `${PRODUCTNAME}`) and `/UPDATE` (which keeps
/// the value) stay in sync; the desktop entry shows it as `Name`.
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
fn product_name(app: &tauri::AppHandle) -> String {
    app.config().product_name.clone().unwrap_or_else(|| app.config().identifier.clone())
}

/// Quoted absolute program path for the Run value. SetupAPI caps the whole
/// command line at 260 characters.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn run_value_data(program: &Path) -> Result<String, String> {
    let data = format!("\"{}\"", plain_path(&program.to_string_lossy()));
    if data.chars().count() > 260 { return Err("开机自启路径超过 260 字符，无法写入注册表".into()); }
    Ok(data)
}

/// `std::fs::canonicalize` spells Windows paths in the verbatim `\\?\` form.
/// Explorer, Task Manager and security software all expect the ordinary
/// spelling in a Run value, so the prefix is stripped before writing and before
/// comparing. `\\?\UNC\server\share` becomes `\\server\share`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn plain_path(value: &str) -> String {
    match value.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => value.strip_prefix(r"\\?\").unwrap_or(value).to_owned(),
    }
}

/// `REG_SZ` data is UTF-16LE, and its terminator counts towards `cbData`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn reg_sz_bytes(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Canonical form of a stored Run value, so a respelled path (short names, for
/// instance) compares equal to the freshly resolved target. An unresolvable
/// path is compared as-is. The verbatim `\\?\` prefix is removed here too, or
/// every read would look different from the plain value `run_value_data` writes.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn canonical_run_value(value: &str) -> String {
    match std::fs::canonicalize(value.trim().trim_matches('"')) {
        Ok(path) => format!("\"{}\"", plain_path(&path.to_string_lossy())),
        Err(_) => value.to_owned(),
    }
}

/// `\\?\` is the same file after canonicalization, but Explorer does not want
/// that spelling in a Run value. Quoted short names are not rejected: those
/// already compare equal and must not be rewritten on every read.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn stored_run_spelling_rejected(value: &str) -> bool {
    value.trim().trim_matches('"').starts_with(r"\\?\")
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn run_value_needs_rewrite(current: &str, expected: &str) -> bool {
    stored_run_spelling_rejected(current) || canonical_run_value(current) != expected
}

/// `Exec` escaping per the Desktop Entry Spec. The quoting layer escapes `\`,
/// `"`, backtick and `$`; the file's string layer then doubles every backslash
/// again, so a literal backslash is written as `\\\\`. `%` is a field code and
/// is always doubled. Arguments with reserved characters are double-quoted.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn xdg_exec_quote(arg: &str) -> String {
    let escaped = arg
        .replace('\\', r"\\\\")
        .replace('"', r#"\\""#)
        .replace('`', r"\\`")
        .replace('$', r"\\$")
        .replace('%', "%%");
    if arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '"' | '\'' | '\\' | '>' | '<' | '~' | '|' | '&' | ';' | '$' | '*' | '?' | '#' | '(' | ')' | '`')) {
        format!("\"{escaped}\"")
    } else { escaped }
}

/// XDG autostart entry. `Type` and `Name` are required by the spec; the file
/// never sets `Hidden`, so the entry stays active.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry(name: &str, comment: &str, program: &Path) -> Result<String, String> {
    let program = program.to_str().ok_or("程序路径不是有效的 UTF-8")?;
    // The spec limits Exec's string value to ASCII, and a file some desktops
    // cannot parse is worse than a clear error at the switch.
    if !program.is_ascii() { return Err("程序路径包含非 ASCII 字符，无法写入自启配置".into()); }
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName={name}\nComment={comment}\nExec={}\nTerminal=false\nStartupNotify=false\nX-GNOME-Autostart-enabled=true\n",
        xdg_exec_quote(program)
    ))
}

/// `Hidden=true` tells the desktop environment to ignore the file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry_hidden(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line.split_once('=').is_some_and(|(key, value)| key.trim() == "Hidden" && value.trim() == "true")
    })
}

/// `Exec` value from the main `[Desktop Entry]` group, not from actions.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry_exec(body: &str) -> Option<&str> {
    let mut in_group = false;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Some(header) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            in_group = header == "Desktop Entry";
            continue;
        }
        if !in_group { continue; }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "Exec" { return Some(value.trim()); }
        }
    }
    None
}

/// True only when an active entry points somewhere else. Other keys
/// (`X-GNOME-Autostart-enabled`, comments) are not the launch target.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn autostart_target_differs(body: &str, expected: &str) -> bool {
    !desktop_entry_hidden(body) && desktop_entry_exec(body) != desktop_entry_exec(expected)
}

/// `$XDG_CONFIG_HOME/autostart`, which Tauri's `config_dir` already resolves.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn autostart_dir(config_dir: &Path) -> PathBuf { config_dir.join("autostart") }

#[cfg(target_os = "linux")]
const AUTOSTART_COMMENT: &str = "登录后自动显示会话悬浮窗";

#[cfg(target_os = "macos")]
fn login_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().home_dir().map_err(|e| e.to_string())?.join("Library/LaunchAgents").join(format!("{}.rail-login.plist", app.config().identifier)))
}
#[cfg(target_os = "macos")]
fn bundle() -> Option<PathBuf> {
    std::env::current_exe().ok()?.ancestors().find(|p| p.extension().is_some_and(|s| s == "app")).map(|p| p.to_owned())
}
#[cfg(target_os = "macos")]
fn launch_plist(label: &str, args: &[String], path: Option<&str>) -> String {
    let arguments = args.iter().map(|a| format!("<string>{}</string>", xml(a))).collect::<String>();
    let environment = path.map(|p| format!("<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>", xml(p))).unwrap_or_default();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{arguments}</array>{environment}<key>RunAtLoad</key><true/></dict></plist>", xml(label))
}
#[cfg(target_os = "macos")]
fn xml(value: &str) -> String { value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;") }

/// Windows login item: one value under the current user's `Run` key. The
/// predefined root handle is used directly — `RegSetKeyValueW` creates a
/// missing subkey and `RegDeleteKeyValueW` deletes a missing value — so no key
/// is opened (or closed) and no `Win32_Security` feature is needed.
#[cfg(target_os = "windows")]
mod win_login {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};

    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    /// Well above the 260-character command line the Run key allows.
    const MAX_BYTES: usize = 4096;

    fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(std::iter::once(0)).collect() }

    pub fn write(name: &str, data: &str) -> Result<(), String> {
        let subkey = wide(RUN_KEY);
        let name = wide(name);
        let bytes = super::reg_sz_bytes(data);
        // SAFETY: the buffers outlive the call and predefined keys are never closed.
        let status = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr(), REG_SZ, bytes.as_ptr().cast::<c_void>(), (bytes.len() * 2) as u32) };
        if status != ERROR_SUCCESS { return Err(format!("无法写入开机自启项（注册表错误 {status}），可能被安全软件或系统策略阻止")); }
        Ok(())
    }

    pub fn read(name: &str) -> Result<Option<String>, String> {
        let subkey = wide(RUN_KEY);
        let name = wide(name);
        let mut kind = 0;
        let mut bytes = MAX_BYTES as u32;
        let mut data = vec![0u16; MAX_BYTES / 2];
        // SAFETY: the buffers outlive the call; `RRF_RT_REG_SZ` restricts the
        // data to a string and makes Windows count the terminator.
        let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, &mut kind, data.as_mut_ptr().cast::<c_void>(), &mut bytes) };
        if status == ERROR_FILE_NOT_FOUND { return Ok(None); }
        if status != ERROR_SUCCESS { return Err(format!("无法读取开机自启项（注册表错误 {status}）")); }
        data.truncate(bytes as usize / 2);
        while data.last() == Some(&0) { data.pop(); }
        Ok(Some(String::from_utf16_lossy(&data)))
    }

    pub fn remove(name: &str) -> Result<(), String> {
        let subkey = wide(RUN_KEY);
        let name = wide(name);
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr()) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND { return Err(format!("无法删除开机自启项（注册表错误 {status}）")); }
        Ok(())
    }
}

/// Linux login item: an XDG autostart desktop entry, replaced atomically.
#[cfg(target_os = "linux")]
mod xdg_login {
    use std::path::{Path, PathBuf};

    /// A fixed slug: renaming the product must not leave orphan entries behind.
    const FILE: &str = "agent-companion.desktop";

    pub fn path(config_dir: &Path) -> PathBuf { super::autostart_dir(config_dir).join(FILE) }

    pub fn read(file: &Path) -> Result<Option<String>, String> {
        match std::fs::read_to_string(file) {
            Ok(body) => Ok(Some(body)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("无法读取自启配置（{}）：{e}", file.display())),
        }
    }

    pub fn write(file: &Path, body: &str) -> Result<(), String> {
        std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| format!("无法写入自启配置（{}）：{e}", file.display()))?;
        let temporary = file.with_extension("tmp");
        std::fs::write(&temporary, body).map_err(|e| format!("无法写入自启配置（{}）：{e}", file.display()))?;
        std::fs::rename(&temporary, file).map_err(|e| format!("无法写入自启配置（{}）：{e}", file.display()))
    }

    pub fn remove(file: &Path) -> Result<(), String> {
        std::fs::remove_file(file).map_err(|e| format!("无法删除自启配置（{}）：{e}", file.display()))
    }
}

#[cfg(target_os = "linux")]
fn autostart_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(xdg_login::path(&app.path().config_dir().map_err(|e| e.to_string())?))
}

/// What to start after login. macOS launches the bundle through `open`;
/// Windows and Linux register the packaged executable itself. A dev build is
/// unsupported there: it needs the Vite server, and neither the Run value nor
/// the desktop entry can inject the toolchain PATH, so registering one would
/// only produce a login item that never works.
fn launch_command(app: &tauri::AppHandle) -> Result<Launch, String> {
    // Only Linux resolves its target through the app handle.
    #[cfg(not(target_os = "linux"))]
    let _ = app;
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = bundle() {
            return Ok(Launch { program: "/usr/bin/open".into(), args: vec!["-gj".into(), bundle.to_string_lossy().into_owned()], env_path: None });
        }
        // A dev executable needs its Vite server. Start the existing dev script,
        // retaining the Node toolchain PATH instead of registering a broken binary.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().map_err(|e| e.to_string())?;
        let path = std::env::var("PATH").unwrap_or_default();
        let npm = std::env::split_paths(&path).map(|p| p.join("npm")).find(|p| p.is_file()).ok_or("找不到 npm，无法配置开发版自启")?;
        if !root.join("package.json").is_file() { return Err("找不到桌面启动项目".into()); }
        return Ok(Launch { program: npm, args: vec!["--prefix".into(), root.to_string_lossy().into_owned(), "run".into(), "desktop:dev".into()], env_path: Some(path) });
    }
    #[cfg(target_os = "windows")]
    {
        if cfg!(debug_assertions) { return Err("开发版不注册开机自启，请使用打包后的应用".into()); }
        let program = std::env::current_exe().map_err(|e| e.to_string())?;
        return Ok(Launch { program: std::fs::canonicalize(program).map_err(|e| e.to_string())?, args: Vec::new(), env_path: None });
    }
    #[cfg(target_os = "linux")]
    {
        if cfg!(debug_assertions) { return Err("开发版不注册开机自启，请使用打包后的应用".into()); }
        let program = tauri::process::current_binary(&app.env()).map_err(|e| e.to_string())?;
        return Ok(Launch { program, args: Vec::new(), env_path: None });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("当前运行环境不支持开机自启".into())
    }
}

fn managed(app: &tauri::AppHandle) -> bool {
    app.state::<std::sync::Arc<super::Service>>().config.manage_autostart
}
fn supported(app: &tauri::AppHandle) -> bool {
    cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux")) && managed(app) && launch_command(app).is_ok()
}

/// Whether a login item is registered. As with the plist before it, only the
/// presence of the item is reported; a stale target is repaired by `heal_login`.
fn login_state(app: &tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        return login_path(app).map(|path| path.exists());
    }
    #[cfg(target_os = "windows")]
    {
        return win_login::read(&product_name(app)).map(|value| value.is_some());
    }
    #[cfg(target_os = "linux")]
    {
        let file = autostart_file(app)?;
        return xdg_login::read(&file).map(|body| body.is_some_and(|body| !desktop_entry_hidden(&body)));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = app;
        Ok(false)
    }
}

/// Adds or removes the login item, surfacing write failures such as a blocked
/// registry key or an unwritable autostart directory.
fn set_login(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let login = login_path(app)?;
        if enabled {
            let launch = launch_command(app)?;
            let mut args = vec![launch.program.to_string_lossy().into_owned()];
            args.extend(launch.args);
            let plist = launch_plist(&format!("{}.rail-login", app.config().identifier), &args, launch.env_path.as_deref());
            std::fs::create_dir_all(login.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(&login, plist).map_err(|e| e.to_string())?;
        } else { std::fs::remove_file(&login).map_err(|e| e.to_string())?; }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let name = product_name(app);
        if !enabled { return win_login::remove(&name); }
        return win_login::write(&name, &run_value_data(&launch_command(app)?.program)?);
    }
    #[cfg(target_os = "linux")]
    {
        let file = autostart_file(app)?;
        if !enabled { return xdg_login::remove(&file); }
        let launch = launch_command(app)?;
        return xdg_login::write(&file, &desktop_entry(&product_name(app), AUTOSTART_COMMENT, &launch.program)?);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (app, enabled);
        Err("当前运行环境不支持开机自启".into())
    }
}

/// Rewrites a login item whose target no longer matches this binary — the one
/// case a moved AppImage or a reinstall leaves behind. A Windows value that
/// still carries the verbatim `\\?\` prefix is rewritten even though it names
/// the same file. Failures are ignored so reading settings never fails because
/// of the repair. Embedded hosts do not get their login item edited.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn heal_login(app: &tauri::AppHandle) {
    if !managed(app) { return; }
    let Ok(launch) = launch_command(app) else { return; };
    #[cfg(target_os = "windows")]
    {
        let name = product_name(app);
        let (Ok(expected), Ok(Some(current))) = (run_value_data(&launch.program), win_login::read(&name)) else { return; };
        if run_value_needs_rewrite(&current, &expected) { let _ = win_login::write(&name, &expected); }
    }
    #[cfg(target_os = "linux")]
    {
        let (Ok(file), Ok(expected)) = (autostart_file(app), desktop_entry(&product_name(app), AUTOSTART_COMMENT, &launch.program)) else { return; };
        let Ok(Some(body)) = xdg_login::read(&file) else { return; };
        if autostart_target_differs(&body, &expected) { let _ = xdg_login::write(&file, &expected); }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn heal_login(_app: &tauri::AppHandle) {}

fn response(app: &tauri::AppHandle, preferences: Preferences) -> Result<Value, String> {
    let mut value = serde_json::to_value(preferences).map_err(|e| e.to_string())?;
    value["autostartSupported"] = json!(supported(app));
    // Embedded hosts own their own login item, so the settings page hides the row.
    value["autostartManaged"] = json!(managed(app));
    value["autostart"] = json!(login_state(app)?);
    Ok(value)
}
#[tauri::command]
pub fn rail_settings_get(app: tauri::AppHandle) -> Result<Value, String> {
    heal_login(&app);
    let preferences = match std::fs::read(path(&app)?) {
        Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(e) => return Err(e.to_string()),
    };
    preferences.validate()?;
    response(&app, preferences)
}
#[tauri::command]
pub fn rail_settings_set(app: tauri::AppHandle, preferences: Preferences, autostart: bool) -> Result<Value, String> {
    preferences.validate()?;
    if autostart != login_state(&app)? {
        if !supported(&app) { return Err("当前运行环境不支持开机自启".into()); }
        set_login(&app, autostart)?;
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
    #[cfg(target_os = "macos")]
    #[test]
    fn login_plist_keeps_arguments_separate_and_escapes_paths() {
        let args = vec!["/opt/node tools/npm".into(), "--prefix".into(), "/Users/A & B/project".into(), "run".into(), "desktop:dev".into()];
        let plist = launch_plist("com.test.rail-login", &args, Some("/opt/node tools:/usr/bin"));
        assert!(plist.contains("<string>/Users/A &amp; B/project</string>"));
        assert!(plist.contains("<string>run</string><string>desktop:dev</string>"));
        assert!(plist.contains("<key>PATH</key><string>/opt/node tools:/usr/bin</string>"));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    }
    #[test]
    fn run_value_data_quotes_the_program_and_caps_the_command_line() {
        assert_eq!(
            run_value_data(Path::new(r"C:\Users\a b\AppData\Local\Agent Companion\agent-companion.exe")).unwrap(),
            r#""C:\Users\a b\AppData\Local\Agent Companion\agent-companion.exe""#
        );
        // `canonicalize` hands over the verbatim form on Windows; the Run value
        // must keep the ordinary spelling.
        assert_eq!(
            run_value_data(Path::new(r"\\?\C:\Users\a b\AppData\Local\Agent Companion\agent-companion.exe")).unwrap(),
            r#""C:\Users\a b\AppData\Local\Agent Companion\agent-companion.exe""#
        );
        // 258 characters plus the two quotes fill the 260 allowed. The verbatim
        // prefix is not stored, so it must not consume that budget.
        assert!(run_value_data(&PathBuf::from("x".repeat(258))).is_ok());
        assert!(run_value_data(&PathBuf::from("x".repeat(259))).is_err());
        assert!(run_value_data(&PathBuf::from(format!(r"\\?\{}", "x".repeat(258)))).is_ok());
    }
    #[test]
    fn plain_path_strips_the_verbatim_prefix_only() {
        assert_eq!(plain_path(r"\\?\C:\Users\a b\agent-companion.exe"), r"C:\Users\a b\agent-companion.exe");
        assert_eq!(plain_path(r"\\?\UNC\server\share\agent-companion.exe"), r"\\server\share\agent-companion.exe");
        assert_eq!(plain_path(r"C:\Users\a b\agent-companion.exe"), r"C:\Users\a b\agent-companion.exe");
        assert_eq!(plain_path("/usr/bin/agent-companion"), "/usr/bin/agent-companion");
    }
    #[test]
    fn reg_sz_bytes_are_utf16_with_a_terminator() {
        assert_eq!(reg_sz_bytes("Aé"), vec![0x41, 0xe9, 0x00]);
    }
    #[test]
    fn canonical_run_value_rewrites_known_paths_only() {
        assert_eq!(canonical_run_value("\"Z:\\missing\\agent-companion.exe\""), "\"Z:\\missing\\agent-companion.exe\"");
        let exe = std::env::current_exe().unwrap();
        let rewritten = canonical_run_value(&exe.to_string_lossy());
        assert_eq!(rewritten, format!("\"{}\"", plain_path(&std::fs::canonicalize(&exe).unwrap().to_string_lossy())));
        assert!(!rewritten.contains(r"\\?\"));
    }
    #[test]
    fn verbatim_run_spelling_is_rewritten_even_when_the_target_matches() {
        assert!(stored_run_spelling_rejected(r#""\\?\C:\Agent Companion\agent-companion.exe""#));
        assert!(stored_run_spelling_rejected(r#""\\?\UNC\server\share\agent-companion.exe""#));
        assert!(!stored_run_spelling_rejected(r#""C:\Agent Companion\agent-companion.exe""#));
        assert!(!stored_run_spelling_rejected("\"/usr/bin/agent-companion\""));
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let expected = run_value_data(&exe).unwrap();
        assert!(!expected.contains(r"\\?\"));
        assert!(!run_value_needs_rewrite(&expected, &expected));
        let verbatim = format!("\"{}{}\"", r"\\?\", expected.trim_matches('"'));
        assert!(run_value_needs_rewrite(&verbatim, &expected));
    }
    #[test]
    fn xdg_exec_quote_follows_the_desktop_entry_escaping_table() {
        assert_eq!(xdg_exec_quote("/usr/bin/agent-companion"), "/usr/bin/agent-companion");
        assert_eq!(xdg_exec_quote("/opt/Agent Companion/app"), "\"/opt/Agent Companion/app\"");
        assert_eq!(xdg_exec_quote(r"a\b"), r#""a\\\\b""#);
        assert_eq!(xdg_exec_quote(r#"a"b"#), r#""a\\"b""#);
        assert_eq!(xdg_exec_quote("a`b"), r#""a\\`b""#);
        assert_eq!(xdg_exec_quote("a$b"), r#""a\\$b""#);
        assert_eq!(xdg_exec_quote("100%"), "100%%");
    }
    #[test]
    fn desktop_entry_has_the_required_keys_and_rejects_non_ascii() {
        let body = desktop_entry("Agent Companion", "登录后自动显示会话悬浮窗", Path::new("/opt/Agent Companion/agent-companion")).unwrap();
        assert_eq!(body.lines().next(), Some("[Desktop Entry]"));
        for key in ["Type=Application", "Name=Agent Companion", "Exec=\"/opt/Agent Companion/agent-companion\"", "Terminal=false", "StartupNotify=false", "X-GNOME-Autostart-enabled=true"] {
            assert!(body.contains(key), "missing {key}");
        }
        assert!(desktop_entry("Agent Companion", "c", Path::new("/opt/工具/agent-companion")).is_err());
    }
    #[test]
    fn desktop_entry_hidden_reads_the_key_and_ignores_comments() {
        assert!(desktop_entry_hidden("[Desktop Entry]\nHidden=true\n"));
        assert!(!desktop_entry_hidden("[Desktop Entry]\nHidden=false\n"));
        assert!(!desktop_entry_hidden("[Desktop Entry]\nName=Agent Companion\n"));
        assert!(!desktop_entry_hidden("# Hidden=true\n"));
    }
    #[test]
    fn autostart_heal_compares_the_exec_target_only() {
        let expected = desktop_entry("Agent Companion", "登录后自动显示会话悬浮窗", Path::new("/usr/bin/agent-companion")).unwrap();
        assert_eq!(desktop_entry_exec(&expected), Some("/usr/bin/agent-companion"));
        assert!(!autostart_target_differs(&expected, &expected));
        let gnome_off = expected.replace("X-GNOME-Autostart-enabled=true", "X-GNOME-Autostart-enabled=false");
        assert!(!autostart_target_differs(&gnome_off, &expected));
        let hidden = expected.replace("StartupNotify=false", "Hidden=true");
        assert!(!autostart_target_differs(&hidden, &expected));
        let moved = expected.replace("/usr/bin/agent-companion", "/opt/moved/agent-companion");
        assert!(autostart_target_differs(&moved, &expected));
        let spaced = desktop_entry("Agent Companion", "c", Path::new("/opt/Agent Companion/app")).unwrap();
        assert_eq!(desktop_entry_exec(&spaced), Some("\"/opt/Agent Companion/app\""));
        assert_eq!(desktop_entry_exec("[Desktop Action x]\nExec=/other\n[Desktop Entry]\nExec=/usr/bin/agent-companion\n"), Some("/usr/bin/agent-companion"));
    }
    #[test]
    fn autostart_dir_joins_the_config_directory() {
        assert_eq!(autostart_dir(Path::new("/home/user/.config")), PathBuf::from("/home/user/.config/autostart"));
    }
}
