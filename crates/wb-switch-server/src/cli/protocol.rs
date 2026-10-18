//! Stable JSON envelope and machine-readable errors for every command.
use super::target::Target;
use serde_json::{json, Value};

pub struct Failure {
    pub code: &'static str,
    pub message: String,
    pub exit: u8,
}

impl Failure {
    pub(super) fn usage(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_ARGUMENT",
            message: message.into(),
            exit: 2,
        }
    }

    pub(super) fn operation(code: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        let code = if message.starts_with(wb_switch_core::modules::state_lock::BUSY_PREFIX) {
            "AUTH_STATE_BUSY"
        } else if message.starts_with("PROCESS_SCAN_FAILED:") {
            "PROCESS_SCAN_FAILED"
        } else {
            code
        };
        Self {
            code,
            message,
            exit: 1,
        }
    }
}
/// `target` is the operation context, not ownership of the account's credits.
pub(super) fn response(command: &str, target: Target) -> Value {
    json!({"schemaVersion": 1, "ok": true, "command": command,
        "target": if matches!(command, "version" | "help") { None } else { Some(target.as_str()) },
        "scope": scope(command), "exitCode": 0, "error": null})
}

fn scope(command: &str) -> Option<&'static str> {
    match command {
        "accounts" | "credits" => Some("account"),
        "status" | "switch" | "limits" => Some("client"),
        "inspect" => Some("mixed"),
        _ => None,
    }
}

pub(super) fn error(code: &str, message: &str) -> Value {
    let retryable = match code {
        "AUTH_STATE_BUSY" | "TIMEOUT" => Some(true),
        "INVALID_ARGUMENT"
        | "INTERRUPTED"
        | "ACCOUNT_NOT_FOUND"
        | "AMBIGUOUS_ACCOUNT"
        | "ACCOUNT_STORE_INVALID"
        | "AUTH_RELOGIN_REQUIRED" => Some(false),
        _ => None,
    };
    let hint = match code {
        "PROCESS_SCAN_FAILED" => {
            Some("无法可靠查询 CLI 进程，未关闭会话或写入认证配置；请检查系统进程查询权限后重试。")
        }
        "INTERRUPTED" => Some(
            "已停止排队并等待执行中的认证事务完成；重试切号前请先检查当前状态，避免再次关闭会话。",
        ),
        "AUTH_STATE_BUSY" => {
            Some("等待超过 --wait 期限；可提高 --wait。不要删除锁文件或终止正在刷新凭据的进程。")
        }
        "ACCOUNT_NOT_FOUND" | "AMBIGUOUS_ACCOUNT" => Some("运行 accounts 获取稳定账号 ID。"),
        "AUTH_RELOGIN_REQUIRED" => Some("请在 GUI 中重新登录此账号。"),
        _ => None,
    };
    json!({"code":code, "message":message, "retryable":retryable, "hint":hint})
}

pub(super) fn normalize_account_error(result: &mut Value) {
    if result["ok"] == true {
        result["error"] = Value::Null;
        return;
    }
    let message = result["error"]
        .as_str()
        .or_else(|| result["error"]["message"].as_str())
        .unwrap_or("Credit query failed")
        .to_string();
    let code = if message.starts_with(wb_switch_core::modules::state_lock::BUSY_PREFIX) {
        "AUTH_STATE_BUSY"
    } else if message.starts_with("ACCOUNT_NOT_FOUND:") {
        "ACCOUNT_NOT_FOUND"
    } else {
        result["errorCode"]
            .as_str()
            .or_else(|| result["error"]["code"].as_str())
            .unwrap_or("CREDIT_QUERY_FAILED")
    }
    .to_string();
    result["error"] = error(&code, &message);
    result.as_object_mut().unwrap().remove("errorCode");
}

/// Preserve requested context even when syntax validation fails. No execution.
pub struct Context {
    command: String,
    target: Option<String>,
}

impl Context {
    /// Syntax failures use the same output default as successful invocations.
    pub fn json_output(&self, args: &[String]) -> bool {
        if args.iter().any(|a| a == "--json" || a == "--ascii-json") {
            return true;
        }
        if args
            .iter()
            .any(|a| a == "--text" || a == "--help" || a == "-h")
        {
            return false;
        }
        super::args::json_by_default(&self.command, self.target.is_some())
    }

    pub fn from_args(args: &[String]) -> Self {
        let mut command = None;
        let mut target = None;
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if let Some(value) = arg.strip_prefix("--target=") {
                target = Some(value.to_string());
            } else if arg == "--target" {
                target = args.get(i + 1).cloned();
                i += 1;
            } else if matches!(
                arg.as_str(),
                "--variant" | "--timeout" | "--concurrency" | "--model" | "--port" | "--wait"
            ) {
                i += 1;
            } else if arg == "--" {
                command = command.or_else(|| args.get(i + 1).cloned());
                break;
            } else if !arg.starts_with('-') && command.is_none() {
                command = Some(arg.clone());
            }
            i += 1;
        }
        let command = command.unwrap_or_else(|| {
            if args.iter().any(|a| a == "--version" || a == "-V") {
                "version"
            } else {
                "serve"
            }
            .into()
        });
        if matches!(
            command.as_str(),
            "accounts" | "credits" | "inspect" | "switch" | "limits"
        ) {
            target = target.or_else(|| Some(Target::default().as_str().into()));
        }
        Self { command, target }
    }

    pub fn failure(&self, failure: &Failure, elapsed_ms: u64) -> Value {
        json!({"schemaVersion":1, "ok":false, "command":self.command, "target":self.target,
            "scope":scope(&self.command), "elapsedMs":elapsed_ms, "exitCode":failure.exit,
            "error":error(failure.code, &failure.message)})
    }
}
