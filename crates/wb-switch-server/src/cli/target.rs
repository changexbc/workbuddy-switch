//! Client-specific operations are dispatched here. Account storage and balances
//! remain shared services. Add a target only when its operations are implemented.
use serde_json::Value;
use wb_switch_core::modules::{codebuddy_cli, limits};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    #[default]
    CodebuddyCli,
}

impl Target {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "codebuddy-cli" => Ok(Self::CodebuddyCli),
            _ => Err("Only --target codebuddy-cli is supported".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodebuddyCli => "codebuddy-cli",
        }
    }

    /// Caller holds the shared authentication lock for this snapshot.
    pub fn status(self) -> Value {
        match self {
            Self::CodebuddyCli => codebuddy_cli::status(),
        }
    }

    /// Core owns the complete authentication transaction and process policy.
    pub fn switch(self, account_id: &str) -> Result<Value, String> {
        match self {
            Self::CodebuddyCli => codebuddy_cli::switch_active_account(account_id),
        }
    }

    /// A read-only OS process snapshot. It never claims that a detected CLI
    /// session is idle and never returns process command lines or history.
    pub fn switch_preview(self) -> Result<Value, String> {
        match self {
            Self::CodebuddyCli => codebuddy_cli::switch_process_preview(),
        }
    }

    pub fn limits(self) -> Value {
        match self {
            Self::CodebuddyCli => limits::get_cli_rate_limits_snapshot(),
        }
    }

    pub fn limits_source(self) -> &'static str {
        match self {
            Self::CodebuddyCli => "codebuddy-cli-logs-and-shared-account-hook-ledger",
        }
    }
}
