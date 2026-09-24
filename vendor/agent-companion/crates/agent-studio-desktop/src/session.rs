use std::path::{Path, PathBuf};
use std::process::Command;

const VSCODE_APP: &str = "Visual Studio Code.app";
const VSCODE_BUNDLE: &str = "com.microsoft.VSCode";

#[derive(Debug)]
enum SessionTarget {
    Link(url::Url),
    App(&'static str),
    /// A CodeBuddy VS Code plugin session. The plugin has no URI handler, so the
    /// only addressable thing is the window's folder, which is resolved from the
    /// plugin's own history; `cwd` is a candidate, never the answer by itself.
    VSCode { session: String, cwd: Option<PathBuf> },
}
fn codebuddy_international(edition: Option<&str>) -> bool {
    !matches!(edition, Some("domestic" | "codebuddycn"))
}
fn codebuddy_bundle(edition: Option<&str>) -> &'static str {
    if codebuddy_international(edition) {
        "com.tencent.codebuddy"
    } else {
        "com.tencent.codebuddycn"
    }
}
fn app_for_scheme(scheme: &str) -> Option<(&'static str, &'static str)> {
    Some(match scheme {
        "workbuddy-ai" => ("WorkBuddy AI.app", "com.workbuddy.workbuddy-ai"),
        "workbuddy" => ("WorkBuddy.app", "com.tencent.workbuddy.mac"),
        "codebuddy" => ("CodeBuddy.app", "com.tencent.codebuddy"),
        "codebuddycn" => ("CodeBuddy CN.app", "com.tencent.codebuddycn"),
        _ => return None,
    })
}
/// The app to activate for a bare `scheme://file` link, when the scheme has one.
/// VS Code is not here: `vscode://file/...` is handled by VS Code's URL handler,
/// which always targets the last active window, so the plugin's sessions open
/// the folder by path instead (see `resolve_vscode_folder`).
fn bare_file_app(scheme: &str) -> Option<&'static str> {
    Some(match scheme {
        "codebuddy" => "com.tencent.codebuddy",
        "codebuddycn" => "com.tencent.codebuddycn",
        _ => return None,
    })
}
fn session_target(url: &str) -> Result<SessionTarget, String> {
    if url.starts_with("/api/open-session?") {
        let parsed =
            url::Url::parse(&format!("http://localhost{url}")).map_err(|e| e.to_string())?;
        let sources: Vec<_> = parsed.query_pairs().filter(|(key, _)| key == "source").map(|(_, value)| value.into_owned()).collect();
        let edition = parsed
            .query_pairs()
            .find(|(key, _)| key == "edition")
            .map(|(_, value)| value.into_owned());
        let host = parsed
            .query_pairs()
            .find(|(key, _)| key == "host")
            .map(|(_, value)| value.into_owned());
        let session = parsed
            .query_pairs()
            .find(|(key, _)| key == "session")
            .map(|(_, value)| value.into_owned());
        let cwd = parsed
            .query_pairs()
            .find(|(key, _)| key == "cwd")
            .map(|(_, value)| PathBuf::from(value.into_owned()));
        return match sources.as_slice() {
            [source] if source == "codeg" => Ok(SessionTarget::App("app.codeg")),
            // Only the plugin's own target consults `host`; an unknown value
            // keeps the edition behaviour, and so does every other source.
            [source] if source == "codebuddy-ide" => {
                if host.as_deref() != Some("vscode") {
                    return Ok(SessionTarget::App(codebuddy_bundle(edition.as_deref())));
                }
                Ok(match session.as_deref().map(str::trim).filter(|id| usable_session_id(id)) {
                    Some(session) => SessionTarget::VSCode { session: session.to_string(), cwd },
                    // Without a session id there is nothing to resolve a folder
                    // from, so only the application itself can be activated.
                    None => SessionTarget::App(VSCODE_BUNDLE),
                })
            }
            _ => Err("不支持的应用".into()),
        };
    }
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    let supported = matches!(
        (parsed.scheme(), parsed.host_str()),
        ("codex", Some("threads"))
            | ("workbuddy", Some("chat"))
            | ("workbuddy-ai", Some("chat"))
            | ("codebuddy", Some("file"))
            | ("codebuddycn", Some("file"))
            | ("codeg", Some("session"))
    );
    // Older frontends emitted a bare file URL when a Hook reported cwd="/".
    // Activate the app rather than opening the filesystem root as a project.
    if parsed.host_str() == Some("file")
        && matches!(parsed.path(), "" | "/")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
    {
        if let Some(app) = bare_file_app(parsed.scheme()) {
            return Ok(SessionTarget::App(app));
        }
    }
    if !supported
        || parsed.path().len() < 2
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("不支持的会话链接".into());
    }
    Ok(SessionTarget::Link(parsed))
}
/// A plugin conversation id is a plain token. Anything with a path separator or
/// a parent-directory segment would let the read-only history lookup walk out of
/// its root, so such an id is never used.
fn usable_session_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains('/') && !id.contains('\\')
}

/// A folder the resolver may name: absolute, and never the filesystem root (a
/// window without a folder is not addressable at the OS level).
fn usable_folder(path: &Path) -> bool {
    path.is_absolute() && path != Path::new("/")
}

/// Where the VS Code installs archive the plugin's conversations.
#[cfg(target_os = "macos")]
fn vscode_history_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let base = PathBuf::from(home).join("Library/Application Support");
    ["Code", "Code - Insiders"]
        .into_iter()
        .map(|name| {
            base.join(name)
                .join("User/globalStorage/tencent-cloud.coding-copilot/genie-history")
        })
        .collect()
}

/// The bucket whose `conversations/<session>` entry exists, or `None` when the
/// session is not archived under this root. Read-only, and silent on any error.
fn bucket_for_session(root: &Path, session: &str) -> Option<String> {
    for entry in std::fs::read_dir(root).ok()? {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        if !entry.path().join("conversations").join(session).is_dir() {
            continue;
        }
        if let Some(bucket) = entry.file_name().to_str() {
            return Some(bucket.to_string());
        }
    }
    None
}

/// What a bucket name resolves to.
#[derive(Debug, PartialEq, Eq)]
enum BucketFolder {
    /// A folder that exists on disk.
    Folder(PathBuf),
    /// `/`: the session belongs to a window with no folder.
    RootlessWindow,
    /// Undecodable, truncated past recovery, or naming no existing directory.
    Unknown,
}

/// `sanitizeWorkspaceId(p) = base64(p).replace(/[/+=]/g,'_').substring(0,64)`,
/// mirrored from the plugin — this is how a window folder is spelled as a
/// bucket name, and how a `cwd` can be checked against one.
fn sanitize_workspace_id(path: &str) -> String {
    base64_encode(path.as_bytes())
        .chars()
        .map(|byte| if matches!(byte, '/' | '+' | '=') { '_' } else { byte })
        .take(64)
        .collect()
}

/// Decodes a bucket name back to its window folder. The name is base64 with
/// `/`, `+` and `=` flattened to `_` and cut at 64 characters, so every variant
/// is tried, and only a decode that re-encodes to the same bucket may win.
fn decode_bucket_folder(bucket: &str) -> BucketFolder {
    if !bucket.is_ascii() {
        return BucketFolder::Unknown;
    }
    for candidate in bucket_candidates(bucket) {
        let Ok(text) = std::str::from_utf8(&candidate) else { continue };
        // A guess that does not reproduce the bucket would name an unrelated
        // path, so it is never accepted — even when such a path exists.
        if !bucket_matches_folder(bucket, text) {
            continue;
        }
        let path = Path::new(text);
        if path == Path::new("/") {
            return BucketFolder::RootlessWindow;
        }
        if path.is_absolute() && path.is_dir() {
            return BucketFolder::Folder(path.to_path_buf());
        }
    }
    BucketFolder::Unknown
}

/// A bucket is `sanitize_workspace_id(folder)` cut at 64 characters; re-encoding
/// a decoded candidate must therefore reproduce the whole bucket (or its first
/// 64 characters, for a folder long enough to have been cut).
fn bucket_matches_folder(bucket: &str, folder: &str) -> bool {
    let encoded = sanitize_workspace_id(folder);
    if bucket.len() == 64 {
        encoded.starts_with(bucket)
    } else {
        encoded == bucket
    }
}

/// The base64 texts a bucket name may stand for: the trailing `_` as padding
/// (1 or 2), then a 64-character name read as a truncated base64, then the same
/// name with one `_` read as `+` — the plugin flattens `/`, `+` and `=` alike,
/// and a `+` appears whenever a `~`, `>` or DEL byte lands on a group boundary.
fn bucket_candidates(bucket: &str) -> Vec<Vec<u8>> {
    let mut candidates = Vec::new();
    for padding in 0..=2 {
        if padding > 0 && !bucket.ends_with(&"_".repeat(padding)) {
            break;
        }
        let mut text = desanitize(&bucket[..bucket.len() - padding]);
        text.push_str(&"=".repeat(padding));
        candidates.extend(base64_decode(&text));
    }
    for trimmed in 1..=3 {
        if bucket.len() <= trimmed {
            break;
        }
        candidates.extend(base64_decode(&desanitize(&bucket[..bucket.len() - trimmed])));
    }
    for flip in 0..bucket.len() {
        if bucket.as_bytes()[flip] != b'_' {
            continue;
        }
        let text: String = bucket
            .char_indices()
            .map(|(index, byte)| match byte {
                '_' if index == flip => '+',
                '_' => '/',
                other => other,
            })
            .collect();
        candidates.extend(base64_decode(&text));
    }
    candidates
}

/// `_` → `/`: the plugin flattened three base64 characters into one, and away
/// from the padding only `/` can have been there.
fn desanitize(text: &str) -> String {
    text.chars().map(|byte| if byte == '_' { '/' } else { byte }).collect()
}

/// Standard base64 with optional `=` padding, nothing else. A non-canonical
/// tail is rejected so a wrong interpretation cannot pass as data.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let body = text.strip_suffix("==").or_else(|| text.strip_suffix('=')).unwrap_or(text);
    if body.contains('=') || body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in body.bytes() {
        let value = base64_value(byte)? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    (accumulator == 0).then_some(out)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The plugin's own encoder, mirrored for the same-bucket check below.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let group = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(ALPHABET[(group >> 18) as usize & 63] as char);
        out.push(ALPHABET[(group >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(group >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[group as usize & 63] as char } else { '=' });
    }
    out
}

/// The window folder for a plugin session, most trustworthy evidence first: the
/// bucket the plugin archived the conversation in, then the hook's `cwd` — but
/// only when the bucket agrees it is the same window, then the bare `cwd`.
fn resolve_vscode_folder(
    history_roots: &[PathBuf],
    session: &str,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    if !usable_session_id(session) {
        return None;
    }
    for root in history_roots {
        let Some(bucket) = bucket_for_session(root, session) else { continue };
        return match decode_bucket_folder(&bucket) {
            BucketFolder::Folder(folder) => Some(folder),
            // The session ran in a window with no folder.
            BucketFolder::RootlessWindow => None,
            // The bucket says nothing on its own, but a `cwd` archived under the
            // same bucket is still that window's own folder.
            BucketFolder::Unknown => {
                cwd.filter(|path| usable_folder(path))
                    .filter(|path| sanitize_workspace_id(&path.to_string_lossy()) == bucket)
                    .map(Path::to_path_buf)
            }
        };
    }
    // No bucket at all: the plugin is not installed here or changed its layout,
    // so the hook's own cwd is the only candidate left.
    cwd.filter(|path| usable_folder(path)).map(Path::to_path_buf)
}

/// The app bundle location when the app is installed, so `-a` can beat a
/// possibly missing URL scheme registration.
#[cfg(target_os = "macos")]
fn installed_app_path(app: &str) -> Option<PathBuf> {
    [
        Some(PathBuf::from(format!("/Applications/{app}"))),
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications").join(app)),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.join("Contents/Info.plist").is_file())
}

/// Runs an open command and maps its failure to the message the UI shows.
fn run_command(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| format!("无法调用系统打开服务：{e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    eprintln!("Agent session open failed: {}", detail.trim());
    Err(if detail.contains("-10814") || detail.contains("Unable to find application") {
        "系统未找到应用或链接处理程序，请确认应用位置后重试".into()
    } else {
        "未能唤起目标 Agent，请打开应用后重试".into()
    })
}

/// Opens a folder in VS Code, best target first. `open -a <app> <folder>` makes
/// VS Code focus the window that already holds that folder (or open a new one),
/// which is the whole point. The bundled CLI behaves the same but briefly flashes
/// a second Dock icon, so it is only the fallback. `vscode://file/...` is never
/// used: the URL handler always targets the last active window.
#[cfg(target_os = "macos")]
fn open_vscode_folder(folder: &Path) -> Result<(), String> {
    if let Some(app) = installed_app_path(VSCODE_APP) {
        if run_command(Command::new("/usr/bin/open").arg("-a").arg(&app).arg(folder)).is_ok() {
            return Ok(());
        }
        let cli = app.join("Contents/Resources/app/bin/code");
        if cli.is_file() && run_command(Command::new(cli).arg(folder)).is_ok() {
            return Ok(());
        }
    }
    run_command(Command::new("/usr/bin/open").args(["-b", VSCODE_BUNDLE]).arg(folder))
}

#[cfg(target_os = "macos")]
fn open_session_target(target: SessionTarget) -> Result<(), String> {
    match target {
        SessionTarget::Link(target) => {
            let mut command = Command::new("/usr/bin/open");
            if let Some((app, bundle)) = app_for_scheme(target.scheme()) {
                // URL scheme registration can be missing even while the app
                // is installed/running. Deliver the deep link to the app itself.
                if let Some(path) = installed_app_path(app) {
                    command.arg("-a").arg(path);
                } else {
                    command.args(["-b", bundle]);
                }
            }
            command.arg(target.as_str());
            run_command(&mut command)
        }
        SessionTarget::App(bundle) => {
            run_command(Command::new("/usr/bin/open").args(["-b", bundle]))
        }
        SessionTarget::VSCode { session, cwd } => {
            match resolve_vscode_folder(&vscode_history_roots(), &session, cwd.as_deref()) {
                Some(folder) => open_vscode_folder(&folder),
                None => run_command(Command::new("/usr/bin/open").args(["-b", VSCODE_BUNDLE])),
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn open_session_target(target: SessionTarget) -> Result<(), String> {
    let SessionTarget::Link(target) = target else {
        return Err("当前平台尚不支持仅唤起应用".into());
    };
    run_command(Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", target.as_str()]))
}

#[cfg(target_os = "linux")]
fn open_session_target(target: SessionTarget) -> Result<(), String> {
    let SessionTarget::Link(target) = target else {
        return Err("当前平台尚不支持仅唤起应用".into());
    };
    run_command(Command::new("xdg-open").arg(target.as_str()))
}

#[tauri::command]
pub async fn open_session_url(url: String) -> Result<(), String> {
    let target = session_target(&url)?;
    tauri::async_runtime::spawn_blocking(move || open_session_target(target))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn link(value: &str) -> url::Url {
        match session_target(value).unwrap() {
            SessionTarget::Link(url) => url,
            _ => panic!("expected session link"),
        }
    }
    #[test]
    fn codebuddy_missing_workspace_activates_app_without_opening_root() {
        for value in ["/api/open-session?source=codebuddy-ide&edition=domestic", "codebuddycn://file/", "codebuddycn://file"] {
            assert!(matches!(session_target(value).unwrap(), SessionTarget::App("com.tencent.codebuddycn")));
        }
        for value in ["/api/open-session?source=codebuddy-ide", "/api/open-session?source=codebuddy-ide&edition=international", "codebuddy://file/", "codebuddy://file"] {
            assert!(matches!(session_target(value).unwrap(), SessionTarget::App("com.tencent.codebuddy")));
        }
        assert_eq!(link("codebuddycn://file/Users/apple/project").path(), "/Users/apple/project");
        assert_eq!(link("codebuddy://file/Users/apple/project").path(), "/Users/apple/project");
        for value in ["codebuddycn://user@file/", "codebuddy://user@file/", "codebuddycn://other/path", "/api/open-session?source=other", "/api/open-session?source=codeg&source=codebuddy-ide"] {
            assert!(session_target(value).is_err());
        }
    }
    #[test]
    fn vscode_plugin_sessions_open_their_folder_or_the_app() {
        // `host=vscode` with a session id hands the folder resolution to the
        // desktop side; the hook's cwd rides along as a candidate, not an answer.
        match session_target("/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1&cwd=%2Fwork%2Fapp").unwrap() {
            SessionTarget::VSCode { session, cwd } => {
                assert_eq!(session, "conv-1");
                assert_eq!(cwd.as_deref(), Some(Path::new("/work/app")));
            }
            other => panic!("expected a VS Code target, got {other:?}"),
        }
        assert!(matches!(session_target("/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1").unwrap(), SessionTarget::VSCode { .. }));
        // Nothing to resolve a folder from: the application is the whole action.
        for value in [
            "/api/open-session?source=codebuddy-ide&host=vscode",
            "/api/open-session?source=codebuddy-ide&host=vscode&session=",
            "/api/open-session?source=codebuddy-ide&host=vscode&session=%2E%2E%2Fother",
        ] {
            assert!(matches!(session_target(value).unwrap(), SessionTarget::App("com.microsoft.VSCode")));
        }
        // `vscode://` is gone: the URL handler always uses the last active window.
        for value in [
            "vscode://file/Users/apple/project",
            "vscode://file",
            "vscode://file/",
            "vscode://other/x",
            "vscode://user@file/x",
            "vscode://file?query=1",
            "vscode://",
        ] {
            assert!(session_target(value).is_err());
        }
        // An unknown host value keeps the edition behaviour instead of guessing.
        assert!(matches!(session_target("/api/open-session?source=codebuddy-ide&host=cursor").unwrap(), SessionTarget::App("com.tencent.codebuddy")));
        for (value, app) in [
            ("/api/open-session?source=codebuddy-ide&host=vscode&edition=domestic", "com.microsoft.VSCode"),
            ("/api/open-session?source=codebuddy-ide&host=vscode", "com.microsoft.VSCode"),
            ("/api/open-session?source=codebuddy-ide&host=cursor&edition=domestic", "com.tencent.codebuddycn"),
            ("/api/open-session?source=codebuddy-ide&host=codebuddy-ide", "com.tencent.codebuddy"),
        ] {
            assert!(matches!(session_target(value).unwrap(), SessionTarget::App(found) if found == app));
        }
        assert!(session_target("/api/open-session?source=codeg&host=vscode").is_ok());
    }
    /// A scratch root under `/tmp`, kept deliberately short: the truncation
    /// cases below need the 48-byte base64 prefix of a long path to fall inside
    /// a name rather than on a temp-directory boundary. The caller removes it.
    fn scratch(name: &str) -> PathBuf {
        let path = PathBuf::from("/tmp").join(format!("ast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn vscode_bucket_names_decode_back_to_their_window_folder() {
        // Every `/`, `+` and `=` of the base64 becomes `_`: base64("/") is
        // "Lw==", so a window without a folder is archived under "Lw__".
        assert_eq!(sanitize_workspace_id("/"), "Lw__");
        assert_eq!(decode_bucket_folder("Lw__"), BucketFolder::RootlessWindow);

        let base = scratch("bucket");
        // The umlaut keeps a `/` inside the base64, so the bucket carries inner
        // `_` characters; one extra byte when needed forces the `=` padding.
        let mut name = String::from("ÿÿÿÿ");
        while base.join(&name).to_str().unwrap().len() % 3 == 0 {
            name.push('x');
        }
        let folder = base.join(&name);
        std::fs::create_dir_all(&folder).unwrap();
        let raw = base64_encode(folder.to_str().unwrap().as_bytes());
        assert!(raw.contains('/') && raw.ends_with('='));
        let bucket = sanitize_workspace_id(folder.to_str().unwrap());
        assert!(bucket[..bucket.len() - 1].contains('_') && bucket.ends_with('_'));
        assert_eq!(decode_bucket_folder(&bucket), BucketFolder::Folder(folder.clone()));

        // `_` also stands for `+`: a `~` on a base64 group boundary puts a `+`
        // into the name, so the bucket hides a second reading — the round-trip
        // check picks the one that re-encodes to the bucket.
        let mut plus_name = String::from("plus");
        while !base64_encode(base.join(&format!("{plus_name}~")).to_str().unwrap().as_bytes())
            .contains('+')
        {
            plus_name.push('x');
        }
        let plus = base.join(&format!("{plus_name}~"));
        std::fs::create_dir_all(&plus).unwrap();
        let plus_bucket = sanitize_workspace_id(plus.to_str().unwrap());
        assert!(plus_bucket.contains('_'));
        assert_eq!(decode_bucket_folder(&plus_bucket), BucketFolder::Folder(plus.clone()));

        // A path long enough to be cut at 64 characters is no longer decodable:
        // the 48 bytes that survive fall inside a name, not on a directory.
        let long = base.join("y".repeat(60));
        std::fs::create_dir_all(&long).unwrap();
        let long_bucket = sanitize_workspace_id(long.to_str().unwrap());
        assert_eq!(long_bucket.len(), 64);
        assert_eq!(decode_bucket_folder(&long_bucket), BucketFolder::Unknown);

        // Junk names resolve to nothing instead of panicking.
        for junk in ["", "????", "nope", "Lw__extra", "üü"] {
            assert_eq!(decode_bucket_folder(junk), BucketFolder::Unknown);
        }
        std::fs::remove_dir_all(&base).ok();

        // Real buckets observed in a CodeBuddy VS Code install: the mirrored
        // sanitizer has to reproduce them exactly, and the decoder must never
        // turn one into an unrelated path.
        for (bucket, window) in [
            ("L1VzZXJzL2FwcGxlL0Rlc2t0b3A_", "/Users/apple/Desktop"),
            ("L1VzZXJzL2FwcGxlL0RvY3VtZW50cy9BSS1QUk9KRUNUL215LWNvZGUtdGVhbXM_", "/Users/apple/Documents/AI-PROJECT/my-code-teams"),
        ] {
            assert_eq!(sanitize_workspace_id(window), bucket);
            match decode_bucket_folder(bucket) {
                BucketFolder::Folder(path) => assert_eq!(path, Path::new(window)),
                // The folder simply is not on this machine.
                BucketFolder::Unknown => {}
                BucketFolder::RootlessWindow => panic!("{bucket} must not decode to the filesystem root"),
            }
        }
    }
    #[test]
    fn vscode_folder_resolution_follows_the_plugin_history_bucket() {
        let base = scratch("resolve");
        let folder = base.join("Work");
        let other = base.join("Other");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        // A decodable bucket wins, even when the hook's cwd says otherwise.
        let bucket = sanitize_workspace_id(folder.to_str().unwrap());
        assert_eq!(decode_bucket_folder(&bucket), BucketFolder::Folder(folder.clone()));
        std::fs::create_dir_all(base.join(&bucket).join("conversations/conv-1")).unwrap();
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-1", Some(&other)), Some(folder.clone()));
        // A bucket carrying inner `_` characters (base64 `/` and `=`) decodes too.
        let mut umlaut = String::from("ÿÿÿÿ");
        while base.join(&umlaut).to_str().unwrap().len() % 3 == 0 {
            umlaut.push('x');
        }
        let special = base.join(&umlaut);
        std::fs::create_dir_all(&special).unwrap();
        let special_bucket = sanitize_workspace_id(special.to_str().unwrap());
        std::fs::create_dir_all(base.join(&special_bucket).join("conversations/conv-6")).unwrap();
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-6", None), Some(special.clone()));
        // Roots without the session are skipped; the next root is consulted.
        assert_eq!(resolve_vscode_folder(&[base.join("nope"), base.clone()], "conv-1", None), Some(folder.clone()));

        // A `/` bucket is a window without a folder: there is nothing to open.
        std::fs::create_dir_all(base.join("Lw__").join("conversations/conv-2")).unwrap();
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-2", Some(&folder)), None);
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-2", None), None);

        // A bucket cut at 64 characters cannot be decoded, but a cwd that
        // sanitizes to that same bucket is its window's own folder.
        let long = base.join("y".repeat(60));
        std::fs::create_dir_all(&long).unwrap();
        let long_bucket = sanitize_workspace_id(long.to_str().unwrap());
        assert_eq!(decode_bucket_folder(&long_bucket), BucketFolder::Unknown);
        std::fs::create_dir_all(base.join(&long_bucket).join("conversations/conv-3")).unwrap();
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-3", Some(&long)), Some(long.clone()));
        // A cwd from another window is not adopted, and neither is none.
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-3", Some(&folder)), None);
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-3", None), None);

        // No bucket anywhere: the cwd is the only candidate, and only when it
        // could be a folder at all.
        assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-4", Some(&folder)), Some(folder.clone()));
        for cwd in [None, Some(Path::new("/")), Some(Path::new("relative/path"))] {
            assert_eq!(resolve_vscode_folder(&[base.clone()], "conv-4", cwd), None);
        }
        // A session id that could walk out of the history root is never used.
        assert_eq!(resolve_vscode_folder(&[base.clone()], "../conv-1", Some(&folder)), None);
        // A missing root is silent: the cwd fallback still applies.
        assert_eq!(resolve_vscode_folder(&[base.join("nope")], "conv-1", Some(&folder)), Some(folder.clone()));
        std::fs::remove_dir_all(&base).ok();
    }
    #[test]
    fn codeg_links_target_the_requested_session() {
        let target = link("codeg://session/214");
        assert_eq!(target.as_str(), "codeg://session/214");
        let encoded = link("codeg://session/task%2Fa%20%3F%23");
        assert_eq!(encoded.path(), "/task%2Fa%20%3F%23");
        for invalid in ["codeg://session/", "codeg://other/214", "codeg://user@session/214"] {
            assert!(session_target(invalid).is_err());
        }
    }
    #[test]
    fn workbuddy_editions_use_distinct_url_schemes() {
        let domestic = link("workbuddy://chat/abc");
        assert_eq!(domestic.scheme(), "workbuddy");
        let international = link("workbuddy-ai://chat/abc");
        assert_eq!(international.scheme(), "workbuddy-ai");
        assert!(session_target("workbuddy://other/abc").is_err());
    }
}
