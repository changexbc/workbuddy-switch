//! Command syntax and compatibility with the original webui entry point.
use super::{protocol::Failure, target::Target};
use std::collections::BTreeMap;
use wb_switch_core::modules::variant::WbVariant;

pub const HELP: &str = "workbuddy-switch [COMMAND] [OPTIONS]

Commands:
  serve                         Start webui (default when no command is given)
  status --target codebuddy-cli  Show CodeBuddy CLI authentication status
  accounts [list]               List account metadata (never credentials)
  inspect [ACCOUNT_ID]           One call: accounts, CLI status, fresh credits and model limits
  switch ACCOUNT_ID             Switch CodeBuddy CLI; closes running CLI processes
  credits [ACCOUNT_ID | --all]   Query credit balances (default: all accounts)
  limits [ACCOUNT_ID]            Read observed model limits and recovery times
  version                       Show version

One-shot options:
  --text                        Human-readable output instead of default JSON
  --json                        Explicit JSON output (kept for existing scripts)
  --ascii-json                  JSON with Unicode escapes for legacy pipes (implies --json)
  --summary                     Compact inspect/credits output; keeps raw balances and 2-decimal display strings
  --variant cn|ai|all            Filter accounts (default: all)
  --target codebuddy-cli         Client target (default: codebuddy-cli)
  --dry-run                     Preview switch target without writing or closing CLI
  --timeout SECONDS             Credit query timeout per account (default: 30)
  --concurrency N               Credit queries in flight (default: 4, maximum: 16)
  --model MODEL                 Filter observed limits by exact model name
  --wait SECONDS                Wait to read/switch auth state (default: 30; 0: fail fast)
  --quiet                       Suppress progress on stderr (stdout remains unchanged)
  -h, --help                    Show help

Serve options: --port PORT (default: 57890), --no-open
Legacy status without --target still shows WorkBuddy (optional --variant cn|ai).
Exit codes: 0 success; 1 operation failed; 2 invalid arguments/account; 3 partial credit results.
Exit code 130: interrupted; in-flight authentication updates finish before exit.
New one-shot commands default to JSON; help, version and legacy status default to text.
Start with inspect for IDs, balances and recovery times in one call.
Use inspect --summary for a compact AI overview without full account/resource details.
Credit *Display strings are rounded to 2 decimals for presentation; numeric fields keep original precision.
Progress goes to stderr; in JSON mode, progress is JSON Lines and stdout is one final object.
Streams use UTF-8 without BOM; --ascii-json escapes non-ASCII characters losslessly.
No observed limit does not prove a model is available.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Serve,
    Status,
    LegacyStatus,
    Accounts,
    Inspect,
    Switch,
    Credits,
    Limits,
    Version,
    Help,
}

impl Command {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Serve => "serve",
            Self::Status | Self::LegacyStatus => "status",
            Self::Accounts => "accounts",
            Self::Inspect => "inspect",
            Self::Switch => "switch",
            Self::Credits => "credits",
            Self::Limits => "limits",
            Self::Version => "version",
            Self::Help => "help",
        }
    }
}

pub(super) fn json_by_default(command: &str, explicit_target: bool) -> bool {
    matches!(
        command,
        "accounts" | "inspect" | "switch" | "credits" | "limits"
    ) || (command == "status" && explicit_target)
}

#[derive(Debug, Clone)]
pub struct Options {
    pub command: Command,
    pub target: Target,
    pub json: bool,
    pub ascii_json: bool,
    pub summary: bool,
    pub variant: Option<WbVariant>,
    pub account_id: Option<String>,
    pub dry_run: bool,
    pub timeout: u64,
    pub concurrency: usize,
    pub model: Option<String>,
    pub port: u16,
    pub no_open: bool,
    pub wait: u64,
    pub quiet: bool,
}
pub fn parse(args: &[String]) -> Result<Options, Failure> {
    let mut options = Options {
        command: Command::Serve,
        target: Target::default(),
        json: false,
        ascii_json: false,
        summary: false,
        variant: None,
        account_id: None,
        dry_run: false,
        timeout: 30,
        concurrency: 4,
        model: None,
        port: 57890,
        no_open: false,
        wait: 30,
        quiet: false,
    };
    let mut positional = Vec::new();
    let mut flags = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            positional.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !arg.starts_with('-') {
            positional.push(arg.clone());
            i += 1;
            continue;
        }
        let (key, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
        let key = match key {
            "-h" => "--help",
            "-V" => "--version",
            other => other,
        };
        let takes_value = matches!(
            key,
            "--variant"
                | "--target"
                | "--timeout"
                | "--concurrency"
                | "--model"
                | "--port"
                | "--wait"
        );
        if !takes_value
            && !matches!(
                key,
                "--json"
                    | "--text"
                    | "--ascii-json"
                    | "--all"
                    | "--dry-run"
                    | "--no-open"
                    | "--help"
                    | "--version"
                    | "--quiet"
                    | "--summary"
            )
        {
            return Err(Failure::usage(format!("Unknown option: {key}")));
        }
        let value = if takes_value {
            match inline {
                Some(value) if !value.is_empty() => value.to_string(),
                Some(_) => return Err(Failure::usage(format!("Missing value for {key}"))),
                None => {
                    i += 1;
                    args.get(i)
                        .filter(|v| !v.starts_with('-'))
                        .cloned()
                        .ok_or_else(|| Failure::usage(format!("Missing value for {key}")))?
                }
            }
        } else {
            if inline.is_some() {
                return Err(Failure::usage(format!("{key} does not take a value")));
            }
            String::new()
        };
        if flags.insert(key.to_string(), value).is_some() {
            return Err(Failure::usage(format!("Duplicate option: {key}")));
        }
        i += 1;
    }
    options.ascii_json = flags.contains_key("--ascii-json");
    options.json = flags.contains_key("--json") || options.ascii_json;
    if flags.contains_key("--text") && options.json {
        return Err(Failure::usage(
            "--text cannot be combined with --json or --ascii-json",
        ));
    }
    let cmd = positional.first().map(String::as_str).unwrap_or("serve");
    options.command = match cmd {
        "serve" => Command::Serve,
        "status" if flags.contains_key("--target") => Command::Status,
        "status" => Command::LegacyStatus,
        "accounts" => Command::Accounts,
        "inspect" => Command::Inspect,
        "switch" => Command::Switch,
        "credits" => Command::Credits,
        "limits" => Command::Limits,
        "version" => Command::Version,
        "help" => Command::Help,
        _ => {
            return Err(Failure::usage(format!(
                "Unknown command: {cmd}. Use --help."
            )))
        }
    };
    if flags.contains_key("--help") || options.command == Command::Help {
        options.command = Command::Help;
        return Ok(options);
    }
    if flags.contains_key("--version") {
        if !positional.is_empty() {
            return Err(Failure::usage(
                "--version cannot be combined with a command",
            ));
        }
        options.command = Command::Version;
    }
    options.json |= !flags.contains_key("--text")
        && json_by_default(options.command.as_str(), flags.contains_key("--target"));
    if options.command == Command::LegacyStatus && options.json {
        return Err(Failure::usage(
            "Use status --target codebuddy-cli for CLI authentication status",
        ));
    }
    for key in flags.keys() {
        let allowed = match key.as_str() {
            "--json" | "--ascii-json" | "--text" => options.command != Command::Serve,
            "--variant" => matches!(
                options.command,
                Command::Accounts
                    | Command::Inspect
                    | Command::LegacyStatus
                    | Command::Switch
                    | Command::Credits
                    | Command::Limits
            ),
            "--target" => matches!(
                options.command,
                Command::Status
                    | Command::Inspect
                    | Command::Accounts
                    | Command::Switch
                    | Command::Credits
                    | Command::Limits
            ),
            "--dry-run" => options.command == Command::Switch,
            "--all" | "--timeout" | "--concurrency" => {
                matches!(options.command, Command::Credits | Command::Inspect)
            }
            "--model" => matches!(options.command, Command::Limits | Command::Inspect),
            "--summary" => matches!(options.command, Command::Inspect | Command::Credits),
            "--wait" | "--quiet" => matches!(
                options.command,
                Command::Accounts
                    | Command::Status
                    | Command::Switch
                    | Command::Credits
                    | Command::Limits
                    | Command::Inspect
            ),
            "--port" | "--no-open" => options.command == Command::Serve,
            "--version" => options.command == Command::Version,
            _ => false,
        };
        if !allowed {
            return Err(Failure::usage(format!("{key} is not supported for {cmd}")));
        }
    }
    if let Some(value) = flags.get("--variant") {
        options.variant = match value.to_ascii_lowercase().as_str() {
            "cn" => Some(WbVariant::Cn),
            "ai" => Some(WbVariant::Ai),
            "all" => None,
            _ => return Err(Failure::usage("--variant must be cn, ai, or all")),
        };
        if options.command == Command::LegacyStatus && options.variant.is_none() {
            return Err(Failure::usage(
                "Legacy status supports --variant cn|ai; use --target codebuddy-cli for CLI status",
            ));
        }
    } else if options.command == Command::LegacyStatus {
        options.variant = Some(WbVariant::Cn);
    }
    if let Some(target) = flags.get("--target") {
        options.target = Target::parse(target).map_err(Failure::usage)?;
    }
    if let Some(value) = flags.get("--timeout") {
        options.timeout = positive_number(value, "--timeout", 3600)?;
    }
    if let Some(value) = flags.get("--concurrency") {
        options.concurrency = positive_number(value, "--concurrency", 16)? as usize;
    }
    if let Some(value) = flags.get("--port") {
        options.port = positive_number(value, "--port", u16::MAX as u64)? as u16;
    }
    options.model = flags.get("--model").cloned();
    options.dry_run = flags.contains_key("--dry-run");
    options.no_open = flags.contains_key("--no-open");
    options.quiet = flags.contains_key("--quiet");
    options.summary = flags.contains_key("--summary");
    if let Some(value) = flags.get("--wait") {
        options.wait = value
            .parse::<u64>()
            .ok()
            .filter(|v| *v <= 3600)
            .ok_or_else(|| Failure::usage("--wait must be an integer between 0 and 3600"))?;
    }
    let mut rest = positional.into_iter().skip(1).collect::<Vec<_>>();
    if options.command == Command::Accounts && rest.first().is_some_and(|v| v == "list") {
        rest.remove(0);
    }
    match options.command {
        Command::Switch if rest.len() != 1 => {
            return Err(Failure::usage("switch requires exactly one ACCOUNT_ID"))
        }
        Command::Switch | Command::Credits | Command::Limits | Command::Inspect
            if rest.len() <= 1 =>
        {
            options.account_id = rest.pop()
        }
        _ if !rest.is_empty() => return Err(Failure::usage("Unexpected positional arguments")),
        _ => {}
    }
    if flags.contains_key("--all") && options.account_id.is_some() {
        return Err(Failure::usage("--all cannot be combined with ACCOUNT_ID"));
    }
    Ok(options)
}

fn positive_number(value: &str, flag: &str, max: u64) -> Result<u64, Failure> {
    value
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0 && *v <= max)
        .ok_or_else(|| Failure::usage(format!("{flag} must be an integer between 1 and {max}")))
}
