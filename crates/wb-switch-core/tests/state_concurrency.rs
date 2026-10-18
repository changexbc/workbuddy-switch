//! Real child processes share an isolated home; no real clients/network involved.
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wb_switch_core::modules::{
    account, credit_usage, refresh, state_lock::StateLock, variant::WbVariant,
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("wb-concurrency-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".wb-switch")).unwrap();
        Self(root)
    }
    fn child(&self, mode: &str, index: usize) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "worker", "--nocapture"])
            .env("WB_SWITCH_HOME", &self.0)
            .env("WB_TEST_WORKER", mode)
            .env("WB_TEST_INDEX", index.to_string())
            .env_remove("CODEBUDDY_AUTH_TOKEN")
            .env_remove("CODEBUDDY_INTERNET_ENVIRONMENT")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }
    fn read(&self, file: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.0.join(file)).unwrap()).unwrap()
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "Worker did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn worker() {
    let Ok(mode) = std::env::var("WB_TEST_WORKER") else {
        return;
    };
    let root = wb_switch_core::modules::config::home_dir();
    let index: usize = std::env::var("WB_TEST_INDEX").unwrap().parse().unwrap();
    match mode.as_str() {
        "hold" => {
            let _guard = StateLock::try_at(&root.join(".wb-switch/auth-state.lock")).unwrap();
            std::fs::write(root.join("ready"), "ready").unwrap();
            // Parent kills this process; kernel must release the lock without cleanup.
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "accounts" => {
            for i in 0..15 {
                let value = json!({"id":format!("account-{index}-{i}"), "uid":format!("uid-{index}-{i}"), "note":"kept"});
                let start = Instant::now();
                loop {
                    match account::save_collected_account(value.clone()) {
                        Ok(_) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(start.elapsed() < Duration::from_secs(10));
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("{error}"),
                    }
                }
                assert!(credit_usage::record_snapshot(
                    &format!("account-{index}-{i}"),
                    "test",
                    100.0,
                    90.0,
                    WbVariant::Cn
                ));
            }
        }
        "refresh" => {
            let original = json!({"id":"account", "uid":"uid", "access_token":"old", "refresh_token":"old-rt"});
            std::fs::write(root.join("ready"), "ready").unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(refresh::refresh_account_token(original));
            std::fs::write(root.join("result.json"), result.to_string()).unwrap();
        }
        _ => panic!("Unknown worker mode"),
    }
}

#[test]
fn lock_is_released_when_a_process_is_killed() {
    let sandbox = Sandbox::new();
    let mut child = sandbox.child("hold", 0);
    wait_for(&sandbox.0.join("ready"));
    let path = sandbox.0.join(".wb-switch/auth-state.lock");
    assert_eq!(
        StateLock::try_at(&path).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let _guard = StateLock::try_at(&path).unwrap();
}

#[test]
fn simultaneous_account_and_snapshot_writers_do_not_lose_records() {
    let sandbox = Sandbox::new();
    let mut children = (0..4)
        .map(|i| sandbox.child("accounts", i))
        .collect::<Vec<_>>();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    let accounts = sandbox.read(".wb-switch/accounts.json");
    assert_eq!(accounts.as_array().unwrap().len(), 60);
    let snapshots = sandbox.read(".wb-switch/credit_usage_snapshots.json");
    assert_eq!(snapshots.as_array().unwrap().len(), 60);
    assert!(sandbox.0.join(".wb-switch/auth-state-revision").exists());
}

#[test]
fn waiting_refresh_uses_new_credentials_without_refreshing_old_token() {
    let sandbox = Sandbox::new();
    let path = sandbox.0.join(".wb-switch/auth-state.lock");
    let guard = StateLock::try_at(&path).unwrap();
    let mut child = sandbox.child("refresh", 0);
    wait_for(&sandbox.0.join("ready"));
    let fresh = json!({"id":"account", "uid":"uid", "access_token":"new", "refresh_token":"new-rt", "note":"concurrent edit"});
    std::fs::write(
        sandbox.0.join(".wb-switch/accounts.json"),
        json!([fresh.clone()]).to_string(),
    )
    .unwrap();
    drop(guard);
    assert!(child.wait().unwrap().success());
    assert_eq!(sandbox.read("result.json"), fresh);
    assert_eq!(sandbox.read(".wb-switch/accounts.json"), json!([fresh]));
    assert!(!sandbox.0.join(".codebuddy/settings.json").exists());
}
