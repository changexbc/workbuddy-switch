//! Compact protocol smoke tests use an isolated home and non-network credentials.
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("wb-cli-summary-{}-{name}", std::process::id()));
        std::fs::create_dir_all(path.join(".wb-switch")).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_wb-switch"))
            .args(args)
            .env("WB_SWITCH_HOME", &self.0)
            .env_remove("CODEBUDDY_API_KEY")
            .env_remove("CODEBUDDY_AUTH_TOKEN")
            .env_remove("CODEBUDDY_INTERNET_ENVIRONMENT")
            .output()
            .unwrap()
    }

    fn write_fixture(&self) -> Vec<u8> {
        // An encrypted envelope is refused locally, before token/network calls.
        let bytes = json!([{"id":"summary-id","uid":"metadata-uid","email":"metadata@example.test",
            "nickname":"摘要📷","displayField":"note","note":"摘要📷",
            "access_token":{"$wbEncrypted":1,"envelope":"private-fixture"}}])
        .to_string()
        .into_bytes();
        std::fs::write(self.0.join(".wb-switch/accounts.json"), &bytes).unwrap();
        bytes
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn payload(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn summary_is_opt_in_retains_failed_account_protocol_and_never_changes_store() {
    let sandbox = Sandbox::new("failed-query");
    let original = sandbox.write_fixture();
    for command in ["inspect", "credits"] {
        let full = sandbox.run(&[command, "--quiet"]);
        let compact = sandbox.run(&[command, "--summary", "--quiet"]);
        assert_eq!(full.status.code(), Some(1));
        assert_eq!(compact.status.code(), Some(1));
        let full = payload(&full);
        let value = payload(&compact);
        assert!(full.get("outputMode").is_none());
        assert_eq!(value["outputMode"], "summary");
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["exitCode"], 1);
        assert_eq!(value["summary"]["failedAccounts"], 1);
        assert!(value["queryStartedAt"].is_i64());
        assert!(value["queriedAt"].is_i64());
        let credit = if command == "inspect" {
            assert_eq!(value["accounts"][0]["id"], "summary-id");
            assert_eq!(value["accounts"][0]["name"], "摘要📷");
            assert!(full["accounts"][0].get("email").is_some());
            assert!(value["accounts"][0].get("email").is_none());
            assert_eq!(value["accounts"][0]["availabilityStatus"], "unknown");
            &value["accounts"][0]["credits"]
        } else {
            assert_eq!(value["accounts"][0]["accountId"], "summary-id");
            &value["accounts"][0]
        };
        assert_eq!(credit["ok"], false);
        assert_eq!(credit["balanceType"], "unknown");
        assert!(credit["totalRemainingDisplay"].is_null());
        assert!(credit["error"]["code"].is_string());
        assert!(credit["queriedAt"].is_i64());
        assert!(!compact
            .stdout
            .windows(b"private-fixture".len())
            .any(|window| window == b"private-fixture"));
        assert!(compact.stderr.is_empty());
    }
    assert_eq!(
        std::fs::read(sandbox.0.join(".wb-switch/accounts.json")).unwrap(),
        original
    );
    assert!(!sandbox.0.join(".codebuddy").exists());
}

#[test]
fn summary_supports_ascii_and_text_without_confusing_unknown_as_zero() {
    let sandbox = Sandbox::new("output-modes");
    sandbox.write_fixture();
    let output = sandbox.run(&["inspect", "--summary", "--ascii-json", "--quiet"]);
    assert!(output.stdout.is_ascii());
    assert_eq!(payload(&output)["accounts"][0]["name"], "摘要📷");
    let output = sandbox.run(&["inspect", "--summary", "--text", "--quiet"]);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    assert!(text.contains("摘要📷"));
    assert!(text.contains("ERROR:"));
    let output = sandbox.run(&["limits", "--summary", "--quiet"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(payload(&output)["error"]["code"], "INVALID_ARGUMENT");
}
