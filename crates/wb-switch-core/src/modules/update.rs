//! 自动更新：检查公开 GitHub Releases 版本 + 更新源配置。
//!
//! 对照 server.py `load_github_config` / `save_github_config` /
//! `compare_versions` / `update_check`。下载安装走 tauri-plugin-updater（整包更新）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::modules::config::{atomic_write, http_request, now_secs, store_dir};

/// 应用当前版本（来自 Cargo.toml package.version）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GITHUB_OWNER: &str = "changexbc";
pub const GITHUB_REPO: &str = "workbuddy-switch";

pub fn github_config_file() -> PathBuf {
    store_dir().join("github_config.json")
}

/// 读取更新源配置（兼容旧配置文件，但永不返回 token）。
pub fn load_github_config() -> Value {
    let mut owner = GITHUB_OWNER.to_string();
    let mut repo = GITHUB_REPO.to_string();
    let f = github_config_file();
    let mut should_normalize = false;
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(value) = v.get("owner").and_then(|v| v.as_str()) {
                    if !value.trim().is_empty() {
                        owner = value.to_string();
                    }
                }
                if let Some(value) = v.get("repo").and_then(|v| v.as_str()) {
                    if !value.trim().is_empty() {
                        repo = value.to_string();
                    }
                }
                should_normalize = v.get("token").is_some();
            }
        }
    }
    // 旧版本截图/配置曾使用 changexbc/wb-switch；迁移到实际公开仓库。
    if owner == "changexbc" && repo == "wb-switch" {
        repo = GITHUB_REPO.to_string();
        should_normalize = true;
    }
    let normalized = json!({"owner": owner, "repo": repo});
    if should_normalize {
        let _ = atomic_write(
            &f,
            &serde_json::to_string_pretty(&normalized).unwrap_or_default(),
        );
    }
    normalized
}

/// 保存更新源配置；公开仓库不需要也不保存 GitHub token。
pub fn save_github_config(cfg: &Value) -> std::io::Result<()> {
    let owner = cfg
        .get("owner")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(GITHUB_OWNER);
    let repo = cfg
        .get("repo")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(GITHUB_REPO);
    let clean = json!({"owner": owner, "repo": repo});
    std::fs::create_dir_all(store_dir())?;
    atomic_write(
        &github_config_file(),
        &serde_json::to_string_pretty(&clean).unwrap_or_default(),
    )
}

fn version_tuple(v: &str) -> Vec<i64> {
    v.trim_start_matches('v')
        .split('.')
        .filter_map(|x| x.parse::<i64>().ok())
        .collect()
}

/// 版本比较：a > b 返回 1，a < b 返回 -1，相等返回 0。
pub fn compare_versions(a: &str, b: &str) -> i64 {
    let ta = version_tuple(a);
    let tb = version_tuple(b);
    for i in 0..ta.len().max(tb.len()) {
        let x = ta.get(i).copied().unwrap_or(0);
        let y = tb.get(i).copied().unwrap_or(0);
        if x != y {
            return if x > y { 1 } else { -1 };
        }
    }
    0
}

/// 查询最新 Release，与本地版本对比。对照 server.py `update_check`。
pub async fn update_check() -> Value {
    let cfg = load_github_config();
    let owner = cfg.get("owner").and_then(|v| v.as_str()).unwrap_or("");
    let repo = cfg.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let release_url = format!("https://github.com/{owner}/{repo}/releases/latest");

    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let mut headers = HashMap::new();
    headers.insert("Accept".to_string(), "application/vnd.github+json".to_string());
    headers.insert("User-Agent".to_string(), "wb-switch".to_string());
    let resp = http_request(&url, "GET", None, Some(&headers)).await;

    let tag = resp.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
    if tag.is_empty() {
        let msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("查询失败");
        let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        return json!({
            "ok": false,
            "error": msg,
            "message": format!("{msg}（code={code}）"),
            "releaseUrl": release_url,
        });
    }

    let latest = tag.strip_prefix('v').unwrap_or(tag).to_string();
    let current = APP_VERSION.to_string();
    let assets: Vec<Value> = resp
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|a| {
                    // Rust 版整包更新产物：.tar.gz 签名包 + .sig + 版本清单
                    a.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| {
                            s.ends_with(".tar.gz") || s.ends_with(".sig") || s.starts_with("latest-")
                        })
                        .unwrap_or(false)
                })
                .map(|a| {
                    json!({
                        "id": a.get("id"),
                        "name": a.get("name"),
                        "size": a.get("size"),
                        "url": a.get("browser_download_url"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "ok": true,
        "current": current,
        "latest": latest,
        "latestTag": tag,
        "hasUpdate": compare_versions(&latest, &current) > 0,
        "assets": assets,
        "releaseName": resp.get("name").and_then(|v| v.as_str()).unwrap_or(tag).to_string(),
        "releaseUrl": resp.get("html_url").and_then(|v| v.as_str()).unwrap_or(&release_url),
        "publishedAt": resp.get("published_at").and_then(|v| v.as_str()).map(|s| s.to_string()),
        "checkedAt": now_secs(),
    })
}
