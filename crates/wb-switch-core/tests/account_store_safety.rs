//! Damaged account libraries must remain byte-for-byte intact on every writer.
use serde_json::json;
use wb_switch_core::modules::{account, config, export_import};

#[test]
fn worker() {
    if std::env::var_os("WB_ACCOUNT_STORE_SAFETY_WORKER").is_none() {
        return;
    }
    std::fs::create_dir_all(config::store_dir()).unwrap();
    let incoming = json!({"id":"new","uid":"uid","access_token":"fixture"});
    for original in [
        b"[{\"id\":\"kept\", BROKEN".as_slice(),
        b"{}",
        b"[123]",
        &[0xff, 0xfe],
    ] {
        std::fs::write(config::accounts_file(), original).unwrap();
        assert!(account::save_accounts(std::slice::from_ref(&incoming)).is_err());
        assert!(account::save_collected_account(incoming.clone()).is_err());
        assert!(account::upsert_account(&incoming).is_err());
        assert!(account::update_account_display("kept", &json!({"note":"x"})).is_err());
        assert!(account::delete_account("kept").is_err());
        assert!(
            export_import::import_accounts(&json!([incoming.clone()]).to_string(), &[0]).is_err()
        );
        assert_eq!(std::fs::read(config::accounts_file()).unwrap(), original);
    }
    std::fs::remove_file(config::accounts_file()).unwrap();
    account::save_collected_account(incoming.clone()).unwrap();
    assert_eq!(account::load_accounts().len(), 1);
    let valid = std::fs::read(config::accounts_file()).unwrap();
    assert!(account::save_accounts(&[json!(123)]).is_err());
    assert_eq!(std::fs::read(config::accounts_file()).unwrap(), valid);
    // Legacy object records are valid; adding another account must retain them.
    std::fs::write(
        config::accounts_file(),
        json!([{"uid":"legacy","note":"kept"}]).to_string(),
    )
    .unwrap();
    account::save_collected_account(incoming).unwrap();
    assert_eq!(account::load_accounts().len(), 2);
    assert_eq!(account::load_accounts()[0]["note"], "kept");
}

#[test]
fn all_writers_preserve_damaged_libraries() {
    let root =
        std::env::temp_dir().join(format!("wb-account-store-safety-{}", uuid::Uuid::new_v4()));
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "worker", "--nocapture"])
        .env("WB_SWITCH_HOME", &root)
        .env("WB_ACCOUNT_STORE_SAFETY_WORKER", "1")
        .status()
        .unwrap();
    assert!(result.success());
    std::fs::remove_dir_all(root).unwrap();
}
