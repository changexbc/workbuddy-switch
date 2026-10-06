use serde_json::{json, Value};
use std::process::{Command, Output};

struct Fixture(std::path::PathBuf);
static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "wb-cli-test-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wb-switch"))
            .args(args)
            .env("WB_SWITCH_HOME", &self.0)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn import_list_and_reject_invalid_switch_without_modifying_accounts() {
    let f = Fixture::new();
    let file = f.0.join("import.json");
    std::fs::write(&file, json!([
        {"id":"test-cn","uid":"uid-cn","nickname":"CN","access_token":"SECRET_CN","refresh_token":"SECRET_REFRESH"},
        {"id":"test-ai","uid":"uid-ai","nickname":"AI","domain":"www.codebuddy.ai","access_token":"SECRET_AI"}
    ]).to_string()).unwrap();
    let added = f.run(&["accounts", "add", "--file", file.to_str().unwrap()]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&added.stdout).unwrap()["imported"],
        2
    );
    let listed = f.run(&["accounts", "list"]);
    assert!(listed.status.success());
    let accounts: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(accounts.as_array().unwrap().len(), 2);
    assert_eq!(accounts[0]["index"], 1);
    assert_eq!(accounts[1]["index"], 2);
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("SECRET"));
    let before = std::fs::read(f.0.join(".wb-switch/accounts.json")).unwrap();
    for args in [
        vec!["switch", "codebuddy-cli", "test-cn", "copy", "true"],
        vec!["switch", "workbuddy", "test-cn", "syn", "yes"],
        vec!["switch", "vscode", "test-cn", "overwrite", "true"],
        vec!["switch", "missing", "test-cn"],
        vec!["switch", "workbuddy", "does-not-exist"],
        vec!["switch", "cli", "2", "copy", "true"],
        vec!["switch", "ide", "2", "share", "true"],
        vec!["switch", "0"],
        vec!["switch", "3"],
        vec!["switch", "2", "syn", "true", "restart", "false"],
    ] {
        let output = f.run(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
    assert_eq!(
        std::fs::read(f.0.join(".wb-switch/accounts.json")).unwrap(),
        before
    );
}

#[test]
fn credits_commands_and_lite_surface_failed_queries_without_network() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.0.join(".wb-switch")).unwrap();
    std::fs::write(f.0.join(".wb-switch/accounts.json"), json!([
        {"id":"a","uid":"u-a","nickname":"中文昵称","access_token":{"$wbEncrypted":true,"envelope":"SECRET"}},
        {"id":"b","uid":"u-b","nickname":"B","access_token":{"$wbEncrypted":true,"envelope":"SECRET"}}
    ]).to_string()).unwrap();
    let single = f.run(&["credits", "2"]);
    assert_eq!(single.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&single.stdout).unwrap();
    assert_eq!(result[0]["index"], 2);
    assert_eq!(result[0]["id"], "b");
    assert_eq!(result[0]["credits"]["ok"], false);
    let all = f.run(&["credits", "all"]);
    assert_eq!(all.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&all.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for args in [
        vec!["accounts", "list", "--lite"],
        vec!["credits", "all", "--lite"],
    ] {
        let output = f.run(&args);
        assert_eq!(output.status.code(), Some(2));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("index")
                && text.contains("ac id")
                && text.contains("积分最近过期时间")
                && text.contains("needrelogin")
        );
        assert!(!text.contains("SECRET") && !text.contains("access_token"));
        assert!(!text.trim_start().starts_with('['));
    }
    for strategy in ["soonest", "richest"] {
        let failed = f.run(&["switch", "cli", strategy]);
        assert_eq!(failed.status.code(), Some(1));
        assert!(failed.stdout.is_empty());
    }
}

#[test]
fn help_and_unknown_command_do_not_start_server() {
    let f = Fixture::new();
    let help = f.run(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("accounts add"));
    assert_eq!(f.run(&["typo"]).status.code(), Some(1));
    assert!(!f.0.join(".wb-switch").exists());
    let empty = f.run(&["--lite", "accounts", "list"]);
    assert!(empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("积分最近过期时间"));
}
