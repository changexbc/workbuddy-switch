//! Exercise the actual binary protocol with child-process home isolation.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "wb-agent-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".wb-switch")).unwrap();
        Self(root)
    }

    fn write(&self, path: &str, value: &Value) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value.to_string()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wb-switch"))
            .args(args)
            .env("WB_SWITCH_HOME", &self.0)
            .env_remove("CODEBUDDY_API_KEY")
            .env_remove("CODEBUDDY_AUTH_TOKEN")
            .env_remove("CODEBUDDY_INTERNET_ENVIRONMENT")
            .output()
            .unwrap()
    }

    fn accounts(&self) {
        self.write(".wb-switch/accounts.json", &json!([
            {"id":"cn-id", "uid":"cn-uid", "email":"cn@example.test", "access_token":"secret-cn", "refresh_token":"secret-refresh"},
            {"id":"ai-id", "uid":"ai-uid", "email":"ai@example.test", "variant":"ai", "access_token":"secret-ai"}
        ]));
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn payload(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "Invalid JSON: {e}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn account_listing_filters_and_never_prints_credentials() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    let output = sandbox.run(&["accounts", "list", "--json", "--variant", "ai"]);
    assert!(output.status.success());
    let value = payload(&output);
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(value["accounts"][0]["id"], "ai-id");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert!(output.stderr.is_empty());
    assert!(!sandbox.0.join(".codebuddy").exists());
}

#[test]
fn utf8_and_ascii_json_round_trip_multilingual_metadata_and_progress() {
    let sandbox = Sandbox::new();
    let name = "中文琴琴 · café · 好事📷 · 𠮷 · العربية";
    let note = "quotes: \" backslash: \\ literal: \\u4e2d\nnew line\ttab";
    sandbox.write(".wb-switch/accounts.json", &json!([
        {"id":"unicode-id", "nickname":name, "note":note, "access_token":{"$wbEncrypted":1,"envelope":"fixture"}}
    ]));
    for ascii in [false, true] {
        let flag = if ascii { "--ascii-json" } else { "--json" };
        let output = sandbox.run(&["accounts", flag]);
        assert!(output.status.success());
        let text = std::str::from_utf8(&output.stdout).unwrap();
        assert!(!output.stdout.starts_with(&[0xef, 0xbb, 0xbf]));
        assert!(!text.contains('\u{fffd}'));
        assert_eq!(text.is_ascii(), ascii);
        assert_eq!(payload(&output)["accounts"][0]["nickname"], name);
        assert_eq!(payload(&output)["accounts"][0]["note"], note);
        let output = sandbox.run(&["credits", flag]);
        assert_eq!(output.status.code(), Some(1));
        let stderr = std::str::from_utf8(&output.stderr).unwrap();
        assert_eq!(stderr.is_ascii(), ascii);
        assert!(!output.stderr.starts_with(&[0xef, 0xbb, 0xbf]));
        let events: Vec<Value> = stderr
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events
            .iter()
            .any(|event| event["event"] == "account_completed"
                && event["details"]["accountName"] == name));
        assert_eq!(payload(&output)["accounts"][0]["accountName"], name);
    }
    let output = sandbox.run(&["accounts", "--text"]);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    assert!(text.contains(name));
    let output = sandbox.run(&["switch", "不存在📷", "--ascii-json", "--quiet"]);
    assert!(output.stdout.is_ascii());
    assert_eq!(payload(&output)["error"]["code"], "ACCOUNT_NOT_FOUND");
}

#[test]
fn one_shot_commands_default_to_json_including_progress_and_failures() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["accounts"],
        vec!["status", "--target=codebuddy-cli"],
        vec!["limits"],
        vec!["inspect"],
        vec!["credits"],
    ] {
        let output = sandbox.run(&args);
        assert!(output.status.success(), "{args:?}");
        assert_eq!(payload(&output)["schemaVersion"], 1);
        for line in std::str::from_utf8(&output.stderr).unwrap().lines() {
            let event: Value = serde_json::from_str(line).unwrap();
            assert_eq!(event["type"], "progress");
        }
    }
    sandbox.accounts();
    let preview = sandbox.run(&["switch", "cn-id", "--dry-run"]);
    assert!(preview.status.success(), "{}", payload(&preview));
    assert_eq!(payload(&preview)["command"], "switch");
    assert!(!sandbox.0.join(".codebuddy").exists());
    for args in [
        vec!["switch"],
        vec!["switch", "missing"],
        vec!["accounts", "--variant=bad"],
        vec!["inspect", "--unknown"],
        vec!["accounts", "--text", "--json"],
        vec!["limits", "--text", "--ascii-json"],
    ] {
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(payload(&output)["ok"], false);
    }
    sandbox.write(
        ".wb-switch/accounts.json",
        &json!([
            {"id":"a","access_token":{"$wbEncrypted":1,"envelope":"fixture"}}
        ]),
    );
    for command in ["credits", "inspect"] {
        let output = sandbox.run(&[command]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(payload(&output)["exitCode"], 1);
        let events: Vec<Value> = std::str::from_utf8(&output.stderr)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events
            .iter()
            .any(|event| event["event"] == "account_completed"));
    }
}

#[test]
fn text_override_and_default_help_remain_readable() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["accounts", "--text"],
        vec!["status", "--target=codebuddy-cli", "--text"],
        vec!["limits", "--text"],
        vec!["inspect", "--text"],
        vec!["credits", "--text"],
        vec!["version", "--text"],
        vec!["help"],
        vec!["inspect", "--help"],
    ] {
        let output = sandbox.run(&args);
        assert!(output.status.success(), "{args:?}");
        assert!(!output.stdout.is_empty());
        assert!(
            serde_json::from_slice::<Value>(&output.stdout).is_err(),
            "{args:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("\"type\":\"progress\""));
    }
    sandbox.accounts();
    let preview = sandbox.run(&["switch", "cn-id", "--dry-run", "--text"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(serde_json::from_slice::<Value>(&preview.stdout).is_err());
    let error = sandbox.run(&["switch", "--text"]);
    assert_eq!(error.status.code(), Some(2));
    assert!(error.stdout.is_empty());
    assert!(String::from_utf8_lossy(&error.stderr).contains("INVALID_ARGUMENT"));
}

#[test]
fn invalid_commands_and_options_exit_without_starting_webui() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["swtich", "--json"],
        vec!["switch", "--json"],
        vec!["credits", "--timeout=0", "--json"],
    ] {
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(payload(&output)["error"]["code"], "INVALID_ARGUMENT");
        assert!(output.stderr.is_empty());
    }
    let output = sandbox.run(&["version", "--json"]);
    assert!(output.status.success());
    assert_eq!(payload(&output)["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn client_target_is_consistent_and_status_only_queries_cli() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    for command in ["accounts", "status", "limits"] {
        let output = sandbox.run(&[command, "--target", "codebuddy-cli", "--json"]);
        assert!(output.status.success());
        let value = payload(&output);
        assert_eq!(value["target"], "codebuddy-cli");
        assert!(value.get("workbuddy").is_none());
        if command == "status" {
            assert!(value["codebuddyCli"].is_object());
        }
        let unsupported = sandbox.run(&[command, "--target", "future-client", "--json"]);
        assert_eq!(unsupported.status.code(), Some(2));
    }
}

#[test]
fn existing_status_output_is_preserved() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["status"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("WorkBuddy 运行中:"));
    assert!(text.contains("账号数: 0"));
    let output = sandbox.run(&["status", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(payload(&output)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn existing_version_output_is_preserved() {
    let sandbox = Sandbox::new();
    for args in [vec!["version"], vec!["--version"], vec!["-V"]] {
        let output = sandbox.run(&args);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("workbuddy-switch {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
    assert_eq!(
        payload(&sandbox.run(&["version", "--json"]))["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn cli_logs_are_scanned_without_consuming_hook_events() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    sandbox.write(
        ".wb-switch/rate_limit_config.json",
        &json!({"enabled":true}),
    );
    let now = chrono::Utc::now();
    let timestamp = now
        .with_timezone(&chrono::Local)
        .format("%Y/%m/%d %H:%M:%S%.3f")
        .to_string();
    let reset = (now + chrono::Duration::hours(1))
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    let request = "34fccb8a1ed74da39dea5481eba85b46";
    let session = "387a486d-b5c9-473d-b823-121db59f0084";
    let logs = sandbox
        .0
        .join(".codebuddy/logs")
        .join(now.format("%Y-%m-%d").to_string());
    std::fs::create_dir_all(&logs).unwrap();
    let log = format!(
        "[{timestamp}] [Info] [pid=1] [AuthDoInitProbe] stage=fastPathBeforeEmit uid=cn-uid hasAccessToken=true\n\
         [{timestamp}] [Info] [pid=1] [ModelProvider] Sending request: agent=cli, model=glm-test, requestId={request}, stream=true\n\
         [{timestamp}] [Error] [pid=1] 429 您的使用量已超出频率限制，将在 {} UTC+8 重置，您也可以切换其他模型继续使用。 ({request}/{session})\n", reset.format("%Y-%m-%d %H:%M:%S"));
    std::fs::write(logs.join("business.log"), log).unwrap();
    let output = sandbox.run(&["limits", "cn-id", "--target=codebuddy-cli", "--json"]);
    assert!(output.status.success());
    let value = payload(&output);
    assert_eq!(value["accounts"][0]["limited"][0]["model"], "glm-test");
    assert_eq!(value["accounts"][0]["hasObservedLimits"], true);
    assert!(!sandbox.0.join(".wb-switch/rate_limit_state.json").exists());
    assert!(!sandbox.0.join(".wb-switch/hook-events.jsonl").exists());
}

#[test]
fn missing_store_is_empty_but_corruption_is_an_error() {
    let sandbox = Sandbox::new();
    assert_eq!(
        payload(&sandbox.run(&["accounts", "--json"]))["accounts"],
        json!([])
    );
    std::fs::write(
        sandbox.0.join(".wb-switch/accounts.json"),
        "{corrupt secret",
    )
    .unwrap();
    let output = sandbox.run(&["accounts", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(payload(&output)["error"]["code"], "ACCOUNT_STORE_INVALID");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
}

#[test]
fn compact_commands_preserve_protocol_failures_encoding_and_account_store() {
    let sandbox = Sandbox::new();
    sandbox.write(
        ".wb-switch/accounts.json",
        &json!([
        {"id":"compact-id","nickname":"中文账号📷","displayField":"note",
            "email":"private-contact@example.test","note":"中文账号📷","phoneNumber":"private-phone",
                "access_token":{"$wbEncrypted":1,"envelope":"fixture"}}
        ]),
    );
    let before = std::fs::read(sandbox.0.join(".wb-switch/accounts.json")).unwrap();
    for command in ["inspect", "credits"] {
        let output = sandbox.run(&[command, "--summary", "--ascii-json"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_ascii());
        let value = payload(&output);
        assert_eq!(value["outputMode"], "summary");
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["exitCode"], 1);
        assert_eq!(value["summary"]["complete"], false);
        let credit = if command == "inspect" {
            assert_eq!(value["accounts"][0]["id"], "compact-id");
            assert_eq!(value["accounts"][0]["name"], "中文账号📷");
            &value["accounts"][0]["credits"]
        } else {
            assert_eq!(value["accounts"][0]["accountId"], "compact-id");
            &value["accounts"][0]
        };
        assert_eq!(credit["balanceType"], "unknown");
        assert!(credit["totalRemainingDisplay"].is_null());
        assert!(credit["error"]["code"].is_string());
        assert!(credit.get("resources").is_none());
        assert!(credit["queryStartedAt"].is_i64());
        let text = std::str::from_utf8(&output.stdout).unwrap();
        assert!(!text.contains("private-contact"));
        assert!(!text.contains("private-phone"));
        assert!(!text.contains("envelope"));
        for line in std::str::from_utf8(&output.stderr).unwrap().lines() {
            assert_eq!(
                serde_json::from_str::<Value>(line).unwrap()["type"],
                "progress"
            );
        }
        assert_eq!(
            before,
            std::fs::read(sandbox.0.join(".wb-switch/accounts.json")).unwrap()
        );
        let text_output = sandbox.run(&[command, "--summary", "--text", "--quiet"]);
        assert_eq!(text_output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&text_output.stdout).contains("中文账号📷"));
        let full = payload(&sandbox.run(&[command, "--quiet"]));
        assert!(full.get("outputMode").is_none());
    }
    for command in ["accounts", "limits", "status", "switch", "serve"] {
        let output = sandbox.run(&[command, "--summary", "--json"]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(payload(&output)["error"]["code"], "INVALID_ARGUMENT");
    }
}

#[test]
fn unassigned_limits_remain_unknown_in_model_filtered_decisions() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    sandbox.write(
        ".wb-switch/rate_limit_config.json",
        &json!({"enabled":true}),
    );
    let now = chrono::Utc::now().timestamp_millis();
    sandbox.write(".wb-switch/rate_limit_state.json", &json!({"events":[
        {"accountId":"cn-id","model":null,"resetAt":now+3_600_000,"firstSeenAt":now,"hitCount":10},
        {"accountId":"cn-id","model":"hy3","resetAt":now-1,"firstSeenAt":now-100,"hitCount":1}
    ]}));
    let value = payload(&sandbox.run(&["limits", "cn-id"]));
    assert_eq!(value["accounts"][0]["limited"][0]["scope"], "unknown");
    assert_eq!(
        value["accounts"][0]["limited"][0]["reason"],
        "MODEL_ATTRIBUTION_UNKNOWN"
    );
    assert_eq!(value["accounts"][0]["modelStates"], json!([]));
    let filtered = payload(&sandbox.run(&["limits", "cn-id", "--model=hy3"]));
    assert_eq!(filtered["accounts"][0]["unattributedLimitCount"], 1);
    assert_eq!(filtered["accounts"][0]["availabilityStatus"], "unknown");
    assert_eq!(
        filtered["accounts"][0]["modelStates"][0]["status"],
        "unknown"
    );
    assert_eq!(filtered["accounts"][0]["limited"], json!([]));
}

#[test]
fn switch_dry_run_resolves_id_without_writing_configuration() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    let before = std::fs::read(sandbox.0.join(".wb-switch/accounts.json")).unwrap();
    let output = sandbox.run(&["switch", "ai-id", "--dry-run", "--json"]);
    assert!(output.status.success(), "{}", payload(&output));
    let value = payload(&output);
    assert_eq!(value["account"]["id"], "ai-id");
    assert_eq!(value["dryRun"], true);
    assert_eq!(value["willCloseRunningCli"], true);
    assert_eq!(value["autoReopened"], false);
    assert_eq!(value["impact"]["snapshotOnly"], true);
    assert_eq!(value["impact"]["sessionState"], "unknown");
    assert_eq!(value["impact"]["sessionActivityKnown"], false);
    assert_eq!(
        value["impact"]["closesProcessTree"],
        cfg!(target_os = "windows")
    );
    let processes = value["impact"]["processes"].as_array().unwrap();
    assert_eq!(value["impact"]["processCount"], processes.len());
    for process in processes {
        assert!(process["pid"].is_u64());
        assert_eq!(process["sessionState"], "unknown");
        assert_eq!(process["sessionActivityKnown"], false);
        assert!(process.get("commandLine").is_none());
    }
    assert!(!sandbox.0.join(".codebuddy").exists());
    assert!(!sandbox.0.join(".codebuddy-rotate").exists());
    assert_eq!(
        before,
        std::fs::read(sandbox.0.join(".wb-switch/accounts.json")).unwrap()
    );
    let unknown = sandbox.run(&["switch", "nonexistent", "--json"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(payload(&unknown)["error"]["code"], "ACCOUNT_NOT_FOUND");
}

#[test]
fn authentication_lock_rejects_switch_before_touching_client_configuration() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    let guard = wb_switch_core::modules::state_lock::StateLock::try_at(
        &sandbox.0.join(".wb-switch/auth-state.lock"),
    )
    .unwrap();
    let output = sandbox.run(&["switch", "cn-id", "--json", "--wait=0", "--quiet"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(payload(&output)["error"]["code"], "AUTH_STATE_BUSY");
    assert!(!sandbox.0.join(".codebuddy").exists());
    assert!(!sandbox.0.join(".codebuddy-rotate").exists());
    let output = sandbox.run(&["status", "--target=codebuddy-cli", "--json", "--wait=0"]);
    assert_eq!(payload(&output)["error"]["code"], "AUTH_STATE_BUSY");
    drop(guard);
    assert!(sandbox
        .run(&["status", "--target=codebuddy-cli", "--json"])
        .status
        .success());
}

#[test]
fn limits_snapshot_filters_expired_entries_and_preserves_shared_state() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    sandbox.write(
        ".wb-switch/rate_limit_config.json",
        &json!({"enabled":true, "scanIdeLogs":false}),
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    sandbox.write(".wb-switch/rate_limit_state.json", &json!({"offset":0,"events":[
        {"accountId":"cn-id", "model":"claude-test", "resetAt":now + 3600000, "firstSeenAt":now, "hitCount":1},
        {"accountId":"cn-id", "model":"expired", "resetAt":now - 1000, "firstSeenAt":now - 2000, "hitCount":1}
    ]}));
    std::fs::write(
        sandbox.0.join(".wb-switch/hook-events.jsonl"),
        "{\"pending\":true}\n",
    )
    .unwrap();
    let state = std::fs::read(sandbox.0.join(".wb-switch/rate_limit_state.json")).unwrap();
    let queue = std::fs::read(sandbox.0.join(".wb-switch/hook-events.jsonl")).unwrap();
    let output = sandbox.run(&["limits", "--json"]);
    assert!(output.status.success());
    let value = payload(&output);
    assert_eq!(value["availabilityKnown"], false);
    assert_eq!(value["accounts"][0]["limited"].as_array().unwrap().len(), 1);
    assert_eq!(value["accounts"][0]["limited"][0]["model"], "claude-test");
    let limit = &value["accounts"][0]["limited"][0];
    assert_eq!(limit["scope"], "model");
    assert_eq!(limit["status"], "limited");
    assert_eq!(limit["observedAt"], now);
    assert_eq!(limit["source"], "client-hook");
    assert_eq!(limit["sourceClients"], json!([]));
    assert_eq!(value["accounts"][0]["modelStates"][0]["status"], "limited");
    assert_eq!(value["accounts"][0]["availabilityStatus"], "unknown");
    assert!(
        value["accounts"][0]["limited"][0]["remainingSeconds"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(value["accounts"][0]["limited"][0]["resetAtUtc"]
        .as_str()
        .is_some());
    assert_eq!(value["accounts"][1]["hasObservedLimits"], false);
    assert_eq!(
        state,
        std::fs::read(sandbox.0.join(".wb-switch/rate_limit_state.json")).unwrap()
    );
    assert_eq!(
        queue,
        std::fs::read(sandbox.0.join(".wb-switch/hook-events.jsonl")).unwrap()
    );
    let output = sandbox.run(&["limits", "cn-id", "--model=other", "--json"]);
    assert_eq!(
        payload(&output)["accounts"][0]["modelStates"][0]["status"],
        "unknown"
    );
    assert_eq!(
        payload(&output)["accounts"][0]["modelStates"][0]["model"],
        "other"
    );
    assert!(output.status.success());
    assert_eq!(payload(&output)["accounts"][0]["limited"], json!([]));
}

#[test]
fn credit_failures_are_identifiable_and_do_not_count_as_zero_balance() {
    let sandbox = Sandbox::new();
    // Encrypted-envelope credentials short-circuit before any network request.
    sandbox.write(
        ".wb-switch/accounts.json",
        &json!([
        {"id":"cn-id", "uid":"uid", "access_token":{"$wbEncrypted":1, "envelope":"test"}}
        ]),
    );
    let output = sandbox.run(&["credits", "--all", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let value = payload(&output);
    assert_eq!(value["ok"], false);
    assert_eq!(value["summary"]["complete"], false);
    assert_eq!(value["summary"]["failedAccounts"], 1);
    assert_eq!(value["accounts"][0]["accountId"], "cn-id");
    assert_eq!(value["accounts"][0]["variant"], "cn");
    assert_eq!(value["accounts"][0]["ok"], false);
    let progress: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(progress.iter().any(|event| event["event"] == "started"));
    assert!(progress
        .iter()
        .any(|event| event["event"] == "account_completed" && event["details"]["completed"] == 1));
}

#[test]
fn inspect_combines_metadata_balances_status_and_limits_without_credentials() {
    let sandbox = Sandbox::new();
    sandbox.write(".wb-switch/accounts.json", &json!([
        {"id":"a", "nickname":"Alpha", "access_token":{"$wbEncrypted":1,"envelope":"secret-envelope"}},
        {"id":"b", "nickname":"Beta", "access_token":{"$wbEncrypted":1,"envelope":"secret-envelope"}, "variant":"ai"}
    ]));
    sandbox.write(
        ".wb-switch/rate_limit_config.json",
        &json!({"enabled":true}),
    );
    let now = wb_switch_core::modules::config::now_ms();
    sandbox.write(
        ".wb-switch/rate_limit_state.json",
        &json!({"offset":0,"events":[
            {"accountId":"a","model":"model-x","resetAt":now+3600000,"firstSeenAt":now,"hitCount":1}
        ]}),
    );
    let output = sandbox.run(&["inspect", "--json", "--quiet"]);
    let value = payload(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(value["command"], "inspect");
    assert_eq!(value["summary"]["failedAccounts"], 2);
    assert!(value["codebuddyCli"].is_object());
    assert_eq!(value["accounts"][0]["nickname"], "Alpha");
    assert_eq!(value["accounts"][0]["limited"][0]["model"], "model-x");
    assert_eq!(value["accounts"][0]["credits"]["ok"], false);
    assert_eq!(value["accounts"][0]["present"], true);
    assert_eq!(value["availabilityKnown"], false);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-envelope"));
    let output = sandbox.run(&["inspect", "a", "--variant=cn", "--json", "--quiet"]);
    assert_eq!(payload(&output)["accounts"].as_array().unwrap().len(), 1);
}

#[test]
fn waiting_emits_heartbeat_and_completes_without_a_second_invocation() {
    use std::io::{BufRead, Read};
    let sandbox = Sandbox::new();
    let guard = wb_switch_core::modules::state_lock::StateLock::try_at(
        &sandbox.0.join(".wb-switch/auth-state.lock"),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_wb-switch"))
        .args(["status", "--target=codebuddy-cli", "--json", "--wait=5"])
        .env("WB_SWITCH_HOME", &sandbox.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut progress_text = String::new();
    // Begin the hold interval after the child reports waiting, independent of startup load.
    stderr.read_line(&mut progress_text).unwrap();
    assert!(progress_text.contains("waiting"));
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(2300));
        drop(guard);
    });
    stderr.read_to_string(&mut progress_text).unwrap();
    let output = child.wait_with_output().unwrap();
    release.join().unwrap();
    assert!(output.status.success());
    assert!(payload(&output)["codebuddyCli"].is_object());
    let events: Vec<Value> = progress_text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event["event"] == "waiting"));
    assert!(events.iter().any(|event| event["event"] == "heartbeat"));
}

#[test]
fn waiting_has_a_deadline_and_reports_busy_without_writing_credentials() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    let _guard = wb_switch_core::modules::state_lock::StateLock::try_at(
        &sandbox.0.join(".wb-switch/auth-state.lock"),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let output = sandbox.run(&["switch", "cn-id", "--json", "--wait=1"]);
    assert_eq!(payload(&output)["error"]["code"], "AUTH_STATE_BUSY");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(!sandbox.0.join(".codebuddy").exists());
}

#[test]
fn disconnected_progress_reader_does_not_abort_the_operation() {
    use std::io::BufRead;
    let sandbox = Sandbox::new();
    let guard = wb_switch_core::modules::state_lock::StateLock::try_at(
        &sandbox.0.join(".wb-switch/auth-state.lock"),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_wb-switch"))
        .args(["status", "--target=codebuddy-cli", "--wait=10"])
        .env("WB_SWITCH_HOME", &sandbox.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.contains("waiting"));
    drop(reader); // Close the progress pipe before the next heartbeat.
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(2300));
        drop(guard);
    });
    let output = child.wait_with_output().unwrap();
    release.join().unwrap();
    assert!(
        output.status.success(),
        "closed stderr must not cause exit 101"
    );
    assert_eq!(payload(&output)["command"], "status");
}

#[cfg(unix)]
#[test]
fn unix_interrupts_end_lock_wait_safely_with_a_final_result() {
    use std::io::{BufRead, Read};
    for signal in ["-INT", "-TERM"] {
        let sandbox = Sandbox::new();
        let _guard = wb_switch_core::modules::state_lock::StateLock::try_at(
            &sandbox.0.join(".wb-switch/auth-state.lock"),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_wb-switch"))
            .args(["status", "--target=codebuddy-cli", "--wait=30"])
            .env("WB_SWITCH_HOME", &sandbox.0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = std::io::BufReader::new(child.stderr.take().unwrap());
        let mut progress = String::new();
        reader.read_line(&mut progress).unwrap();
        assert!(progress.contains("waiting"));
        let started = std::time::Instant::now();
        assert!(Command::new("kill")
            .args([signal, &child.id().to_string()])
            .status()
            .unwrap()
            .success());
        reader.read_to_string(&mut progress).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(output.status.code(), Some(130));
        assert_eq!(payload(&output)["error"]["code"], "INTERRUPTED");
        assert!(progress.contains("interrupted"));
        assert!(!sandbox.0.join(".codebuddy").exists());
    }
}

#[test]
fn response_envelopes_and_account_errors_are_consistent() {
    let sandbox = Sandbox::new();
    sandbox.accounts();
    for command in ["help", "version"] {
        let output = sandbox.run(&[command, "--json"]);
        let value = payload(&output);
        assert_eq!(value["command"], command);
        assert_eq!(value["exitCode"], 0);
        assert!(value["error"].is_null());
        assert!(value["target"].is_null());
        assert!(value["scope"].is_null());
    }
    for (args, command, scope, exit) in [
        (vec!["accounts", "--json"], "accounts", "account", 0),
        (
            vec!["status", "--target=codebuddy-cli", "--json"],
            "status",
            "client",
            0,
        ),
        (vec!["limits", "--json", "--quiet"], "limits", "client", 0),
        (
            vec!["switch", "cn-id", "--dry-run", "--json", "--quiet"],
            "switch",
            "client",
            0,
        ),
        (
            vec!["switch", "missing", "--json", "--quiet"],
            "switch",
            "client",
            2,
        ),
        (
            vec!["--variant=invalid", "accounts", "--json"],
            "accounts",
            "account",
            2,
        ),
    ] {
        let output = sandbox.run(&args);
        let value = payload(&output);
        assert_eq!(output.status.code(), Some(exit));
        assert_eq!(value["command"], command);
        assert_eq!(value["scope"], scope);
        assert_eq!(value["exitCode"], exit);
        assert_eq!(value["target"], "codebuddy-cli");
        assert!(value["elapsedMs"].is_u64());
        if exit == 0 {
            assert!(value["error"].is_null());
        } else {
            assert!(value["error"]["code"].is_string());
            assert!(value["error"]["message"].is_string());
            assert_eq!(value["error"]["retryable"], false);
            assert!(value["error"].get("hint").is_some());
        }
    }
    sandbox.write(
        ".wb-switch/accounts.json",
        &json!([{"id":"a","access_token":{"$wbEncrypted":1,"envelope":"fixture"}}]),
    );
    for command in ["credits", "inspect"] {
        let output = sandbox.run(&[command, "--json", "--quiet"]);
        let value = payload(&output);
        assert_eq!(value["exitCode"], 1);
        let nested = if command == "credits" {
            &value["accounts"][0]["error"]
        } else {
            &value["accounts"][0]["credits"]["error"]
        };
        for error in [&value["error"], nested] {
            assert!(error["code"].is_string());
            assert!(error["message"].is_string());
            assert!(error.get("retryable").is_some());
            assert!(error.get("hint").is_some());
        }
    }
}
