use super::{ensure_hook_binary, hook_command};
use agent_studio_core::{
    adapters::{
        codebuddy_settings_files, workbuddy_edition, workbuddy_settings_files, Collector,
        CODEBUDDY_IDE_HOOK_EVENTS, WORKBUDDY_HOOK_EVENTS,
    },
    atomic_json,
    settings::SOURCES,
    text,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
const CODEX_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SessionEnd",
];
fn events(source: &str) -> &[&str] {
    match source {
        "codex" => &CODEX_EVENTS,
        "workbuddy" => &WORKBUDDY_HOOK_EVENTS,
        _ => &CODEBUDDY_IDE_HOOK_EVENTS,
    }
}
fn files(c: &Collector, source: &str) -> Vec<PathBuf> {
    let custom = text(&c.settings["sources"][source]["path"]);
    match source {
        "codex" => vec![c.paths(source)[0].join("hooks.json")],
        "workbuddy" => workbuddy_settings_files(&c.home, &custom),
        _ => codebuddy_settings_files(&c.home, &custom),
    }
}
fn binary(c: &Collector) -> PathBuf {
    c.home.join(if cfg!(windows) {
        ".agent-studio/bin/agent-studio-runtime-v1.exe"
    } else {
        ".agent-studio/bin/agent-studio-runtime-v1"
    })
}
fn command(c: &Collector, source: &str, file: &Path) -> String {
    hook_command(
        &binary(c),
        &c.home,
        source,
        if source == "workbuddy" {
            Some(workbuddy_edition(file))
        } else {
            None
        },
    )
}
// Match complete commands at known application-owned locations, never substring names.
fn owned(c: &Collector, source: &str, file: &Path, value: &Value) -> bool {
    let cmd = text(&value["command"]);
    if cmd == command(c, source, file) {
        return true;
    }
    if source == "workbuddy" && cmd == hook_command(&binary(c), &c.home, source, None) {
        return true;
    }
    let name = match source {
        "codex" => "astra-office-status.py",
        "workbuddy" => "astra-office-workbuddy.py",
        _ => "astra-office-codebuddy-ide.py",
    };
    let script = file.parent().unwrap().join("hooks").join(name);
    cmd == format!("/usr/bin/python3 {}", script.display())
        || cmd == format!("/usr/bin/python3 {}", super::quote_path(&script))
}
fn read(file: &Path) -> Result<Value, String> {
    let v: Value = match std::fs::read(file) {
        Ok(b) => serde_json::from_slice(&b)
            .map_err(|_| format!("{} 配置不是有效 JSON，未修改", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    if !v.is_object() || (!v["hooks"].is_null() && !v["hooks"].is_object()) {
        return Err(format!("{} Hooks 配置无效，未修改", file.display()));
    }
    if let Some(map) = v["hooks"].as_object() {
        for groups in map.values() {
            let groups = groups.as_array().ok_or("Hook 事件配置无效，未修改")?;
            for group in groups {
                if !group.is_object()
                    || (group.get("hooks").is_some() && !group["hooks"].is_array())
                {
                    return Err("Hook 分组配置无效，未修改".into());
                }
            }
        }
    }
    Ok(v)
}
fn contains(c: &Collector, source: &str, file: &Path, groups: &Value) -> bool {
    groups.as_array().into_iter().flatten().any(|g| {
        owned(c, source, file, g)
            || g["hooks"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|h| owned(c, source, file, h))
    })
}
fn edit(c: &Collector, source: &str, install: bool) -> Result<(), String> {
    let targets = files(c, source);
    if install
        && (targets.is_empty()
            || targets
                .iter()
                .any(|p| !p.parent().is_some_and(Path::is_dir)))
    {
        return Err("未找到应用配置目录，请先安装应用或设置路径".into());
    }
    // Validate every target before writing either edition.
    let documents: Vec<_> = targets
        .iter()
        .map(|p| read(p).map(|v| (p, v)))
        .collect::<Result<_, _>>()?;
    if install {
        ensure_hook_binary(&c.home)?;
    }
    for (file, before) in documents {
        let mut doc = before.clone();
        if install && doc["hooks"].is_null() {
            doc["hooks"] = json!({});
        }
        if let Some(map) = doc["hooks"].as_object_mut() {
            for groups in map.values_mut() {
                let groups = groups.as_array_mut().unwrap();
                groups.retain_mut(|g| {
                    if owned(c, source, file, g) {
                        return false;
                    }
                    if let Some(hooks) = g["hooks"].as_array_mut() {
                        let count = hooks.len();
                        hooks.retain(|h| !owned(c, source, file, h));
                        return count == hooks.len() || !hooks.is_empty();
                    }
                    true
                });
            }
        }
        if install {
            for event in events(source) {
                if doc["hooks"][event].is_null() {
                    doc["hooks"][event] = json!([]);
                }
                let mut h = json!({"type":"command","command":command(c,source,file),"timeout":3});
                if source != "codebuddy-ide" {
                    h["statusMessage"] = json!("Agent Studio");
                    if *event != "SessionEnd" {
                        h["async"] = json!(true);
                    }
                }
                let group = if source == "codex" {
                    json!({"hooks":[h]})
                } else {
                    json!({"matcher":"","hooks":[h]})
                };
                doc["hooks"][event].as_array_mut().unwrap().push(group);
            }
        }
        if doc != before {
            let backup = file.parent().unwrap().join(match source {
                "codex" => "hooks.agent-studio-before-rust.json",
                "workbuddy" => "settings.agent-studio-before-hooks.json",
                _ => "settings.agent-studio-before-ide-hooks.json",
            });
            if file.exists() && !backup.exists() {
                atomic_json(&backup, &before)?;
            }
            atomic_json(file, &doc)?;
        }
    }
    Ok(())
}
pub fn install_hooks(c: &Collector) -> Result<(), String> {
    let mut errors = vec![];
    for source in ["codex", "workbuddy", "codebuddy-ide"] {
        if c.settings["sources"][source]["enabled"] == true && c.integration_automatic(source) {
            // First launch before the agent itself is installed remains harmless.
            if files(c, source)
                .iter()
                .any(|p| p.parent().is_some_and(Path::is_dir))
            {
                if let Err(e) = edit(c, source, true) {
                    errors.push(e);
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}
pub fn get(c: &Collector) -> Value {
    let sources: Vec<_> = SOURCES.iter().map(|source| {
        let locations = if *source == "codeg" { c.paths(source) } else { files(c,source) };
        let result = if *source == "codeg" { c.inspect_codeg_webhook() } else {
            (|| {
                if locations.is_empty() || locations.iter().all(|p| !p.parent().is_some_and(Path::is_dir)) { return Ok(("unavailable", "未找到应用配置目录".into())); }
                let mut total = 0; let mut present = 0; let mut any_owned = false;
                for file in &locations {
                    let doc = read(file)?;
                    any_owned |= doc["hooks"].as_object().is_some_and(|m|m.values().any(|g|contains(c,source,file,g)));
                    for event in events(source) { total += 1; if contains(c,source,file,&doc["hooks"][event]) { present += 1; } }
                }
                if present == total && total > 0 && binary(c).is_file() { Ok(("installed", "Hooks 已安装".into())) }
                else if any_owned { Ok(("partial", "Hooks 不完整或运行文件缺失，请修复".into())) }
                else { Ok(("not_installed", "Hooks 未安装".into())) }
            })()
        };
        let (status,message) = result.unwrap_or_else(|e|("error",e));
        json!({"source":source,"kind":if *source=="codeg"{"webhook"}else{"hooks"},"status":status,"message":message,"locations":locations,"automatic":c.integration_automatic(source),"lastEventAt":c.last_hook_at.get(*source)})
    }).collect();
    json!({"sources":sources})
}
pub fn set(c: &mut Collector, p: &Value) -> Result<Value, String> {
    let source = p["source"]
        .as_str()
        .filter(|s| SOURCES.contains(s))
        .ok_or("未知接入来源")?;
    let install = match p["action"].as_str() {
        Some("install") => true,
        Some("uninstall") => false,
        _ => return Err("未知接入操作".into()),
    };
    if source == "codeg" && install && c.settings["sources"][source]["enabled"] != true {
        return Err("请先开启 Codeg 监听，再注册 Webhook".into());
    }
    c.set_integration_automatic(source, install)?;
    if source == "codeg" {
        c.codeg.registered = false;
        c.codeg.next_attempt = None;
        // Preserve pending ownership on offline cleanup; the status shows pending.
        if let Err(e) = c.maintain_codeg_webhook() {
            if install {
                return Err(e);
            }
        }
    } else {
        edit(c, source, install)?;
    }
    Ok(get(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_preserves_mixed_groups_and_opt_out_survives_restart() {
        let home = std::env::temp_dir().join(format!("integrations-{}", uuid::Uuid::new_v4()));
        for dir in [".codex", ".workbuddy-ai", ".codebuddy"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        let mut c = Collector::new(home.clone()).unwrap();
        for source in ["codex", "workbuddy", "codebuddy-ide"] {
            let file = files(&c, source)[0].clone();
            let mine = command(&c, source, &file);
            let other = json!({"type":"command","command":format!("echo {mine}"),"custom":42});
            atomic_json(&file,&json!({"custom":true,"hooks":{"Stop":[{"matcher":"custom","metadata":"keep","hooks":[{"command":mine},other.clone()]},{"hooks":[]}]}})).unwrap();
            set(&mut c, &json!({"source":source,"action":"uninstall"})).unwrap();
            let doc = read(&file).unwrap();
            assert_eq!(doc["hooks"]["Stop"][0]["hooks"], json!([other]));
            assert_eq!(doc["hooks"]["Stop"][0]["metadata"], "keep");
            assert_eq!(doc["hooks"]["Stop"].as_array().unwrap().len(), 2);
            assert_eq!(doc["custom"], true);
        }
        let mut restarted = Collector::new(home.clone()).unwrap();
        restarted
            .request("settings_set", &restarted.settings.clone())
            .unwrap();
        install_hooks(&restarted).unwrap();
        assert!(get(&restarted)["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["source"] != "codeg")
            .all(|r| r["status"] == "not_installed" && r["automatic"] == false));
        set(
            &mut restarted,
            &json!({"source":"codex","action":"install"}),
        )
        .unwrap();
        assert_eq!(get(&restarted)["sources"][0]["status"], "installed");
        std::fs::write(home.join(".codex/hooks.json"), "malformed").unwrap();
        assert_eq!(get(&restarted)["sources"][0]["status"], "error");
        assert!(set(
            &mut restarted,
            &json!({"source":"codex","action":"uninstall"})
        )
        .is_err());
        assert!(!Collector::new(home.clone())
            .unwrap()
            .integration_automatic("codex"));
        assert_eq!(
            std::fs::read_to_string(home.join(".codex/hooks.json")).unwrap(),
            "malformed"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
