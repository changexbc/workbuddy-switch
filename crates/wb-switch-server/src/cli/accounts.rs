//! Shared account selection; never expose stored credentials.
use super::{args::Options, protocol::Failure};
use serde_json::Value;
use wb_switch_core::modules::{account, config};

/// Unlike the GUI's forgiving loader, automation must not mistake corruption for
/// an empty account library. A missing library is a valid empty initial state.
pub(super) fn load_accounts() -> Result<Vec<Value>, Failure> {
    let path = config::accounts_file();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return Err(Failure::operation(
                "ACCOUNT_STORE_UNREADABLE",
                "Cannot read the account store",
            ))
        }
    };
    let accounts: Vec<Value> = serde_json::from_slice(&bytes).map_err(|_| {
        Failure::operation(
            "ACCOUNT_STORE_INVALID",
            "Account store must be a JSON array",
        )
    })?;
    if accounts.iter().any(|a| {
        !a.is_object()
            || a.get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| id.is_empty())
    }) {
        return Err(Failure::operation(
            "ACCOUNT_STORE_INVALID",
            "Every account must have a non-empty string ID",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    if accounts
        .iter()
        .any(|a| !ids.insert(a["id"].as_str().unwrap()))
    {
        return Err(Failure::operation(
            "ACCOUNT_STORE_INVALID",
            "Duplicate account IDs in the account store",
        ));
    }
    Ok(accounts)
}

pub(super) fn select_accounts(
    accounts: Vec<Value>,
    options: &Options,
) -> Result<Vec<Value>, Failure> {
    let accounts: Vec<_> = accounts
        .into_iter()
        .filter(|a| options.variant.is_none_or(|v| account::variant_of(a) == v))
        .collect();
    let Some(id) = &options.account_id else {
        return Ok(accounts);
    };
    if let Some(a) = accounts.iter().find(|a| a["id"].as_str() == Some(id)) {
        return Ok(vec![a.clone()]);
    }
    let matching: Vec<_> = accounts
        .into_iter()
        .filter(|a| a["uid"].as_str() == Some(id))
        .collect();
    match matching.len() {
        1 => Ok(matching),
        0 => Err(Failure {
            code: "ACCOUNT_NOT_FOUND",
            message: "Account not found in the selected variant; use accounts --json".into(),
            exit: 2,
        }),
        _ => Err(Failure {
            code: "AMBIGUOUS_ACCOUNT",
            message: "UID matches multiple accounts; use the stable account ID".into(),
            exit: 2,
        }),
    }
}
