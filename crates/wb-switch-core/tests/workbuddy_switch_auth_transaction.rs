//! Fake-only, child-process regression checks for WorkBuddy auth export.
//! restart=false keeps every test away from real client process management.
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use wb_switch_core::modules::{
    account, auth_file, auth_transaction as state_lock, config, refresh, switch, variant::WbVariant,
};

fn fixture(generation: &str) -> Value {
    json!({"id":"fixture", "uid":"fixture-user", "variant":"cn",
        "nickname":"fake account", "access_token":format!("FAKE-AT-{generation}"),
        "refresh_token":format!("FAKE-RT-{generation}"), "expiresAt":4102444800000_i64,
        "refreshExpiresAt":4102531200000_i64})
}

fn child(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "worker", "--nocapture"])
        .env("WB_SWITCH_AUTH_WORKER", mode)
        .env("WB_SWITCH_HOME", root)
        .env("USERPROFILE", root.join("profile"))
        .env("APPDATA", root.join("profile/AppData/Roaming"))
        .env("LOCALAPPDATA", root.join("profile/AppData/Local"))
        .env_remove("CODEBUDDY_AUTH_TOKEN")
        .env_remove("WORKBUDDY_ACCESS_TOKEN")
        .env_remove("WORKBUDDY_REFRESH_TOKEN");
    command
}

#[test]
fn worker() {
    let Ok(mode) = std::env::var("WB_SWITCH_AUTH_WORKER") else {
        return;
    };
    let root = config::home_dir();
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("wb-switch-auth-test-"));
    let auth_path = auth_file::auth_file_path(WbVariant::Cn);
    assert!(auth_path.starts_with(&root));
    assert!(config::accounts_file().starts_with(&root));
    if mode == "contending-writer" {
        match state_lock::try_auth() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::fs::write(root.join("writer-blocked"), b"blocked").unwrap();
            }
            Ok(_guard) => {
                account::upsert_account(&fixture("new")).unwrap();
                std::fs::write(root.join("writer-committed"), b"committed").unwrap();
            }
            Err(error) => panic!("Unexpected writer error: {error}"),
        }
        return;
    }
    account::save_accounts(&[fixture("old")]).unwrap();
    match mode.as_str() {
        "busy" => {
            auth_file::write_account_to_auth_file(&fixture("old"), WbVariant::Cn).unwrap();
            let before = std::fs::read(&auth_path).unwrap();
            let guard = state_lock::try_auth().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let callback: switch::ProgressFn = Box::new(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            });
            let result = switch::switch_account(Some(&callback), "fixture", false, false, &[], &[]);
            assert!(
                matches!(result, Err(ref e) if e.starts_with(state_lock::BUSY_PREFIX)),
                "{result:?}"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "Busy must fail before side effects/progress"
            );
            assert_eq!(std::fs::read(&auth_path).unwrap(), before);
            assert!(!config::backup_dir().exists());
            drop(guard);
        }
        "interleave" => {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let child_root = root.clone();
            let callback: switch::ProgressFn = Box::new(move |message| {
                if message == "正在写入认证文件…" {
                    count.fetch_add(1, Ordering::SeqCst);
                    assert!(child(&child_root, "contending-writer")
                        .status()
                        .unwrap()
                        .success());
                }
                if message == "切换完成" {
                    let guard =
                        state_lock::try_auth().expect("Export retained lock into completion");
                    drop(guard);
                    std::fs::write(child_root.join("lock-released-after-export"), b"released")
                        .unwrap();
                }
            });
            let result =
                switch::switch_account(Some(&callback), "fixture", false, false, &[], &[]).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(
                root.join("writer-blocked").exists(),
                "Writer committed between snapshot and export"
            );
            assert!(!root.join("writer-committed").exists());
            assert!(root.join("lock-released-after-export").exists());
            let exported = auth_file::read_auth_file(WbVariant::Cn).unwrap();
            assert_eq!(
                exported["auth"]["refreshToken"],
                account::load_accounts()[0]["refresh_token"]
            );
            // A completed update before the next switch must be read afresh.
            account::upsert_account(&fixture("new")).unwrap();
            switch::switch_account(None, "fixture", false, false, &[], &[]).unwrap();
            let exported = auth_file::read_auth_file(WbVariant::Cn).unwrap();
            assert_eq!(
                exported["auth"]["accessToken"],
                fixture("new")["access_token"]
            );
            assert_eq!(
                exported["auth"]["refreshToken"],
                fixture("new")["refresh_token"]
            );
        }
        "write-failure" => {
            std::fs::create_dir_all(auth_path.parent().unwrap().parent().unwrap()).unwrap();
            std::fs::write(auth_path.parent().unwrap(), b"not a directory").unwrap();
            assert!(switch::switch_account(None, "fixture", false, false, &[], &[]).is_err());
            let guard = state_lock::try_auth().expect("Failed switch leaked auth lock");
            drop(guard);
        }
        "refresh-waits" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                // Both versions have no RT: even a regression cannot send a request.
                let mut old = fixture("old");
                old["refresh_token"] = json!("");
                account::save_accounts(&[old.clone()]).unwrap();
                let guard = state_lock::try_auth().unwrap();
                let pending = refresh::refresh_account_token(old.clone());
                tokio::pin!(pending);
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(120), &mut pending)
                        .await
                        .is_err(),
                    "Refresh did not wait for the export transaction"
                );
                assert_eq!(account::load_accounts()[0], old);
                let mut latest = fixture("new");
                latest["refresh_token"] = json!("");
                account::save_accounts(&[latest]).unwrap();
                drop(guard);
                let result = pending.await;
                assert_eq!(result["access_token"], fixture("new")["access_token"]);
                assert_eq!(result["needs_relogin"], true); // missing RT, not a lock error
                assert!(
                    state_lock::try_auth().is_ok(),
                    "Early return leaked the lock"
                );
            });
        }
        "lock-unavailable" => {
            let before = account::load_accounts();
            std::fs::create_dir(config::store_dir().join("auth-state.lock")).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(refresh::refresh_account_token(fixture("old")));
            assert!(result["refresh_error"].is_string());
            assert_ne!(result["needs_relogin"], true);
            assert_eq!(account::load_accounts(), before);
            assert!(!auth_path.exists());
        }
        _ => panic!("Unknown mode"),
    }
}

fn run(mode: &str) {
    let root = std::env::temp_dir().join(format!("wb-switch-auth-test-{}", uuid::Uuid::new_v4()));
    for suffix in ["", "profile/AppData/Roaming", "profile/AppData/Local"] {
        std::fs::create_dir_all(root.join(suffix)).unwrap();
    }
    let result = child(&root, mode).status().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(result.success(), "Worker mode {mode} failed");
}

#[test]
fn busy_switch_leaves_auth_and_backups_untouched() {
    run("busy");
}

#[test]
fn refresh_commit_cannot_land_between_snapshot_and_export() {
    run("interleave");
}

#[test]
fn export_failure_releases_auth_transaction() {
    run("write-failure");
}

#[test]
fn refresh_waits_and_reloads_credentials_after_export() {
    run("refresh-waits");
}

#[test]
fn local_lock_failure_does_not_mark_a_healthy_account_for_relogin() {
    run("lock-unavailable");
}
