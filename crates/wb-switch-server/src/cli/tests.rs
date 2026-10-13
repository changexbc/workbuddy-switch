use super::target::Target;
use super::*;
use super::{accounts::select_accounts, credits::summarize_credits};
use serde_json::json;
use wb_switch_core::modules::variant::WbVariant;

#[test]
fn structured_error_normalization_preserves_codes_on_repeated_calls() {
    let mut value = serde_json::json!({"ok":false,"error":"timeout","errorCode":"TIMEOUT"});
    super::protocol::normalize_account_error(&mut value);
    let original = value.clone();
    super::protocol::normalize_account_error(&mut value);
    assert_eq!(value, original);
    assert_eq!(value["error"]["retryable"], true);
}

#[test]
fn process_query_failure_is_explicit_and_does_not_claim_a_successful_switch() {
    let failure =
        super::protocol::Failure::operation("SWITCH_FAILED", "PROCESS_SCAN_FAILED: blocked");
    assert_eq!(failure.code, "PROCESS_SCAN_FAILED");
    let value = Context::from_args(&args(&["switch", "id"])).failure(&failure, 0);
    assert_eq!(value["ok"], false);
    assert!(value["error"]["hint"].is_string());
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}
fn parsed(items: &[&str]) -> Options {
    parse(&args(items)).unwrap_or_else(|e| panic!("{}", e.message))
}

#[test]
fn summary_is_opt_in_and_only_supported_for_inspect_and_credits() {
    for command in ["inspect", "credits"] {
        assert!(!parsed(&[command]).summary);
        let options = parsed(&[command, "--summary"]);
        assert!(options.summary);
        assert!(options.json);
        assert!(parsed(&[command, "--summary", "--ascii-json"]).ascii_json);
        assert!(!parsed(&[command, "--summary", "--text"]).json);
    }
    for items in [
        vec!["status", "--target=codebuddy-cli", "--summary"],
        vec!["switch", "a", "--summary"],
        vec!["accounts", "--summary"],
        vec!["limits", "--summary"],
        vec!["credits", "--summary", "--summary"],
        vec!["inspect", "--summary=true"],
    ] {
        assert!(parse(&args(&items)).is_err(), "{items:?}");
    }
}

#[test]
fn rejects_unknown_commands_options_and_conflicting_selectors() {
    for items in [
        vec!["swtich"],
        vec!["switch"],
        vec!["credits", "--all", "id"],
        vec!["accounts", "--variant", "typo"],
        vec!["status", "--port", "80"],
        vec!["serve", "--json"],
        vec!["credits", "--timeout", "0"],
        vec!["credits", "--concurrency=17"],
        vec!["limits", "--json=false"],
        vec!["switch", "id", "--target", "workbuddy"],
        vec!["credits", "--json", "--json"],
        vec!["accounts", "--variant"],
        vec!["accounts", "--text", "--json"],
        vec!["inspect", "--ascii-json", "--text"],
        vec!["help", "--text", "--json"],
        vec!["serve", "--text"],
    ] {
        assert!(parse(&args(&items)).is_err(), "{items:?}");
    }
}

#[test]
fn output_defaults_and_explicit_formats_preserve_legacy_commands() {
    for items in [
        vec!["accounts"],
        vec!["inspect"],
        vec!["switch", "id"],
        vec!["credits"],
        vec!["limits"],
        vec!["status", "--target=codebuddy-cli"],
    ] {
        assert!(parsed(&items).json, "{items:?}");
        let mut text = items.clone();
        text.push("--text");
        assert!(!parsed(&text).json, "{text:?}");
        assert!(Context::from_args(&args(&items)).json_output(&args(&items)));
        assert!(!Context::from_args(&args(&text)).json_output(&args(&text)));
    }
    for items in [
        vec![],
        vec!["serve"],
        vec!["status"],
        vec!["version"],
        vec!["--version"],
        vec!["-V"],
        vec!["help"],
        vec!["-h"],
        vec!["inspect", "--help"],
    ] {
        assert!(!parsed(&items).json, "{items:?}");
    }
    assert!(parsed(&["help", "--json"]).json);
    assert!(parsed(&["accounts", "--json"]).json);
    assert!(parsed(&["accounts", "--ascii-json"]).ascii_json);
}

#[test]
fn syntax_errors_use_the_requested_or_default_output_format() {
    for items in [
        vec!["switch"],
        vec!["inspect", "--unknown"],
        vec!["accounts", "--variant=invalid"],
        vec!["status", "--target=invalid"],
    ] {
        assert!(parse(&args(&items)).is_err());
        assert!(Context::from_args(&args(&items)).json_output(&args(&items)));
        let mut text = items;
        text.push("--text");
        assert!(!Context::from_args(&args(&text)).json_output(&args(&text)));
    }
}

#[test]
fn parses_global_flags_equals_values_and_legacy_serve() {
    assert_eq!(parsed(&[]).command, Command::Serve);
    assert_eq!(parsed(&["--no-open", "--port=58000"]).port, 58000);
    let options = parsed(&["--json", "--variant=ai", "accounts", "list"]);
    assert_eq!(options.command, Command::Accounts);
    assert_eq!(options.variant, Some(WbVariant::Ai));
    assert!(options.json);
    assert_eq!(parsed(&["status"]).command, Command::LegacyStatus);
    assert_eq!(parsed(&["status"]).variant, Some(WbVariant::Cn));
    assert_eq!(
        parsed(&["status", "--target=codebuddy-cli"]).command,
        Command::Status
    );
    assert_eq!(parsed(&["credits"]).variant, None);
    assert_eq!(parsed(&["--version", "--json"]).command, Command::Version);
    assert!(parsed(&["switch", "--dry-run", "id"]).dry_run);
}

#[test]
fn account_resolution_prefers_id_and_rejects_ambiguous_uid() {
    let accounts = vec![
        json!({"id":"cn-id", "uid":"shared"}),
        json!({"id":"ai-id", "uid":"shared", "variant":"ai"}),
    ];
    assert_eq!(
        select_accounts(accounts.clone(), &parsed(&["switch", "cn-id"]))
            .unwrap_or_else(|e| panic!("{}", e.message))[0]["id"],
        "cn-id"
    );
    assert!(select_accounts(accounts.clone(), &parsed(&["switch", "shared"])).is_err());
    assert!(select_accounts(accounts, &parsed(&["switch", "cn-id", "--variant=ai"])).is_err());
}

#[test]
fn partial_and_failed_credit_totals_are_explicit() {
    let good = json!({"ok":true, "totalRemaining":12.5, "totalCapacity":20.0});
    let bad = json!({"ok":false, "error":"offline", "totalRemaining":999});
    let (value, exit) = summarize_credits(vec![good, bad.clone()], 2, Target::default());
    assert_eq!(exit, 3);
    assert_eq!(value["summary"]["totalRemaining"], 12.5);
    assert_eq!(value["summary"]["complete"], false);
    assert_eq!(summarize_credits(vec![bad], 1, Target::default()).1, 1);
    assert_eq!(summarize_credits(vec![], 0, Target::default()).1, 0);
}

#[test]
fn nullable_successful_balances_do_not_become_zero_or_complete_finite_totals() {
    let finite = json!({"ok":true,"totalRemaining":12.5,"totalCapacity":20});
    for unknown in [
        json!({"ok":true,"totalRemaining":null,"totalCapacity":null}),
        json!({"ok":true,"totalRemaining":10,"totalCapacity":null}),
        json!({"ok":true,"totalRemaining":null,"totalCapacity":20}),
    ] {
        let (value, exit) =
            summarize_credits(vec![finite.clone(), unknown.clone()], 2, Target::default());
        assert_eq!(exit, 0, "queries themselves succeeded");
        assert_eq!(value["summary"]["complete"], true);
        assert_eq!(value["summary"]["balanceComplete"], false);
        assert_eq!(value["summary"]["unknownBalanceAccounts"], 1);
        assert_eq!(value["summary"]["finiteAccounts"], 1);
        assert_eq!(value["summary"]["finiteTotalRemaining"], 12.5);
        assert!(value["summary"]["totalRemaining"].is_null());
        assert!(value["summary"]["totalCapacity"].is_null());
        assert!(value["summary"]["totalRemainingDisplay"].is_null());
        assert_eq!(
            value["accounts"][1]["totalRemaining"],
            unknown["totalRemaining"]
        );
    }
}

#[test]
fn unlimited_accounts_are_explicit_and_keep_finite_subtotals() {
    let finite =
        json!({"ok":true,"totalRemaining":12.5,"totalCapacity":20,"expiringSoonRemaining":2});
    let unlimited = json!({"ok":true,"unlimited":true,"totalRemaining":null,"totalCapacity":null});
    let failed = json!({"ok":false,"error":"offline","totalRemaining":999,"unlimited":true});
    let (value, exit) = summarize_credits(
        vec![finite, unlimited.clone(), failed],
        3,
        Target::default(),
    );
    assert_eq!(exit, 3);
    assert_eq!(value["summary"]["hasUnlimitedAccounts"], true);
    assert_eq!(value["summary"]["unlimitedAccounts"], 1);
    assert_eq!(value["summary"]["finiteAccounts"], 1);
    assert_eq!(value["summary"]["finiteTotalRemaining"], 12.5);
    assert_eq!(value["summary"]["finiteTotalCapacity"], 20.0);
    assert_eq!(value["summary"]["expiringSoonRemaining"], 2.0);
    assert!(value["summary"]["totalRemaining"].is_null());
    assert!(value["summary"]["totalCapacity"].is_null());
    assert_eq!(value["summary"]["complete"], false);
    let (value, exit) = summarize_credits(vec![unlimited], 1, Target::default());
    assert_eq!(exit, 0);
    assert_eq!(value["summary"]["complete"], true);
    assert_eq!(value["summary"]["finiteTotalRemaining"], 0.0);
    assert!(!value["summary"]["finiteTotalRemaining"]
        .as_f64()
        .unwrap()
        .is_sign_negative());
    assert_eq!(value["summary"]["finiteAccounts"], 0);
    assert!(value["summary"]["totalRemaining"].is_null());
    let (value, exit) = summarize_credits(
        vec![json!({"ok":true,"totalRemaining":7,"totalCapacity":10})],
        1,
        Target::default(),
    );
    assert_eq!(exit, 0);
    assert_eq!(value["summary"]["hasUnlimitedAccounts"], false);
    assert_eq!(value["summary"]["totalRemaining"], 7.0);
}
