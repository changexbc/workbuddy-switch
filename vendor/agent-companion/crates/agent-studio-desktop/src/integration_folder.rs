use super::Service;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use tauri::State;

fn listed_location(status: &Value, source: &str, location: &str) -> bool {
    status["sources"].as_array().is_some_and(|sources| {
        sources.iter().any(|item| {
            item["source"] == source
                && item["locations"].as_array().is_some_and(|locations| {
                    locations.iter().any(|path| path.as_str() == Some(location))
                })
        })
    })
}

fn containing_folder(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("配置位置不是绝对路径".into());
    }
    let folder = if path.is_dir() {
        path
    } else {
        path.parent().ok_or("无法确定配置文件夹")?
    };
    if !folder.is_dir() {
        return Err("配置文件夹不存在".into());
    }
    Ok(folder.to_path_buf())
}

fn open_folder(folder: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = Command::new("/usr/bin/open").arg(folder).status();
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer.exe").arg(folder).status();
    #[cfg(target_os = "linux")]
    let result = Command::new("xdg-open").arg(folder).status();
    let status = result.map_err(|e| format!("无法打开配置文件夹：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("无法打开配置文件夹".into())
    }
}

#[tauri::command]
pub async fn open_integration_folder(
    state: State<'_, Arc<Service>>,
    source: String,
    location: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let status = state
            .client
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
            .ok_or("采集器未连接")?
            .request("integrations_get", Value::Null)?;
        if !listed_location(&status, &source, &location) {
            return Err("配置位置已变化，请刷新状态后重试".into());
        }
        let folder = containing_folder(Path::new(&location))?;
        open_folder(&folder)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_current_source_locations_can_be_opened() {
        let status = json!({"sources": [
            {"source": "codex", "locations": ["/tmp/a/hooks.json", "/tmp/b/hooks.json"]},
            {"source": "workbuddy", "locations": ["/tmp/c/settings.json"]}
        ]});
        assert!(listed_location(&status, "codex", "/tmp/a/hooks.json"));
        assert!(listed_location(&status, "codex", "/tmp/b/hooks.json"));
        assert!(!listed_location(&status, "codex", "/tmp/c/settings.json"));
        assert!(!listed_location(&status, "codex", "/tmp/a/../b/hooks.json"));
    }

    #[test]
    fn file_location_opens_its_existing_parent() {
        let folder = std::env::temp_dir();
        assert_eq!(
            containing_folder(&folder.join("hooks.json")).unwrap(),
            folder
        );
        assert_eq!(containing_folder(&folder).unwrap(), folder);
        assert!(containing_folder(Path::new("relative/hooks.json")).is_err());
    }
}
