use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
pub const PROTOCOL: u64 = 1;

/// CodeBuddy CN must be matched before CodeBuddy: `codebuddy` is a prefix of `codebuddycn`.
pub fn codebuddy_edition_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("codebuddycn") || text.contains("CodeBuddy CN") {
        Some("domestic")
    } else if lower.contains("com.tencent.codebuddy")
        || text.contains("/CodeBuddy.app")
        || text.contains("\\CodeBuddy.app")
        || text.contains("Application Support/CodeBuddy/")
        || text.contains("Application Support\\CodeBuddy\\")
    {
        Some("international")
    } else {
        None
    }
}

pub fn codebuddy_edition_from_host() -> Option<&'static str> {
    let mut blob = String::new();
    for key in [
        "__CFBundleIdentifier",
        "XPC_SERVICE_NAME",
        "VSCODE_IPC_HOOK",
        "VSCODE_CODE_CACHE_PATH",
        "VSCODE_NLS_CONFIG",
    ] {
        if let Ok(value) = std::env::var(key) {
            blob.push(' ');
            blob.push_str(&value);
        }
    }
    if let Some(found) = codebuddy_edition_from_text(&blob) {
        return Some(found);
    }
    #[cfg(unix)]
    {
        let mut pid = std::process::id();
        for _ in 0..8 {
            let output = std::process::Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "ppid=,command="])
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&output.stdout);
            blob.push(' ');
            blob.push_str(&text);
            if let Some(found) = codebuddy_edition_from_text(&blob) {
                return Some(found);
            }
            let ppid = text
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())?;
            if ppid <= 1 || ppid == pid {
                break;
            }
            pid = ppid;
        }
    }
    None
}
pub fn arg_value(name: &str) -> Option<String> {
    let args: Vec<_> = std::env::args().collect();
    args.iter()
        .position(|s| s == name)
        .and_then(|i| args.get(i + 1).cloned())
}
pub fn home() -> PathBuf {
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(i) = args.iter().position(|s| s == "--home") {
        if let Some(p) = args.get(i + 1) {
            return PathBuf::from(p);
        }
    }
    std::env::var_os("AGENT_STUDIO_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
pub fn endpoint(home: &Path) -> PathBuf {
    home.join(".agent-studio/runtime-v1.json")
}
pub fn call(home: &Path, command: &str, payload: Value) -> Result<Value, String> {
    let bytes = std::fs::read(endpoint(home)).map_err(|_| "监听服务未启动")?;
    let info: Value = serde_json::from_slice(&bytes).map_err(|_| "监听服务地址无效")?;
    if info["protocol"] != PROTOCOL {
        return Err("监听协议版本不兼容".into());
    }
    let port = info["port"]
        .as_u64()
        .filter(|p| *p > 0 && *p < 65536)
        .ok_or("监听服务端口无效")?;
    let token = info["token"].as_str().ok_or("监听认证信息缺失")?;
    let response: Value = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(if command.starts_with("settings_")
            || command.starts_with("integrations_")
            || command.starts_with("custom_")
        {
            12
        } else {
            3
        }))
        .build()
        .post(&format!("http://127.0.0.1:{port}/rpc"))
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(json!({"command":command,"payload":payload}))
        .map_err(|_| "监听服务连接失败")?
        .into_json()
        .map_err(|_| "监听服务响应无效")?;
    if let Some(e) = response["error"].as_str() {
        Err(e.into())
    } else {
        Ok(response["value"].clone())
    }
}
pub struct Client {
    pub home: PathBuf,
    pub id: String,
}
impl Client {
    pub fn connect(home: PathBuf, binary: &Path) -> Result<Self, String> {
        let client = Self {
            home,
            id: uuid::Uuid::new_v4().to_string(),
        };
        if call(&client.home, "hello", json!({"client":client.id})).is_err() {
            let mut c = std::process::Command::new(binary);
            c.arg("serve")
                .env("AGENT_STUDIO_HOME", &client.home)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x08000000);
            }
            let mut child = c.spawn().map_err(|_| "无法启动原生采集服务")?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            for _ in 0..60 {
                if call(&client.home, "hello", json!({"client":client.id})).is_ok() {
                    return Ok(client);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return Err("原生采集服务启动超时".into());
        }
        Ok(client)
    }
    pub fn poll(&self) -> Result<Value, String> {
        call(&self.home, "poll", json!({"client":self.id}))
    }
    pub fn request(&self, command: &str, payload: Value) -> Result<Value, String> {
        call(&self.home, command, payload)
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = call(&self.home, "leave", json!({"client":self.id}));
    }
}

#[cfg(test)]
mod tests {
    use super::codebuddy_edition_from_text;

    #[test]
    fn integration_rpc_allows_remote_status_latency() {
        let home = std::env::temp_dir().join(format!("integration-rpc-{}", uuid::Uuid::new_v4()));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        agent_studio_core::atomic_json(&super::endpoint(&home), &serde_json::json!({"protocol":super::PROTOCOL,"port":port,"token":"test"})).unwrap();
        let worker = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3200));
            request.respond(tiny_http::Response::from_string(r#"{"value":{"sources":[]}}"#)).unwrap();
        });
        let result = super::call(&home, "integrations_get", serde_json::json!({}));
        worker.join().unwrap();
        std::fs::remove_dir_all(home).unwrap();
        assert_eq!(result.unwrap(), serde_json::json!({"sources":[]}));
    }

    #[test]
    fn codebuddy_host_text_prefers_cn_before_the_international_prefix() {
        assert_eq!(
            codebuddy_edition_from_text("__CFBundleIdentifier=com.tencent.codebuddycn"),
            Some("domestic")
        );
        assert_eq!(
            codebuddy_edition_from_text("/Applications/CodeBuddy CN.app/Contents/MacOS/Electron"),
            Some("domestic")
        );
        assert_eq!(
            codebuddy_edition_from_text("Application Support/CodeBuddy CN/1.10-main.sock"),
            Some("domestic")
        );
        assert_eq!(
            codebuddy_edition_from_text("__CFBundleIdentifier=com.tencent.codebuddy"),
            Some("international")
        );
        assert_eq!(
            codebuddy_edition_from_text("/Applications/CodeBuddy.app/Contents/MacOS/Electron"),
            Some("international")
        );
        assert_eq!(
            codebuddy_edition_from_text("Application Support/CodeBuddy/1.10-main.sock"),
            Some("international")
        );
        assert_eq!(codebuddy_edition_from_text("unrelated"), None);
    }
}
