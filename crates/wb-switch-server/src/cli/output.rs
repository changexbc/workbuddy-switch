//! Human-readable output and UTF-8/ASCII JSON rendering.
use super::{
    args::Options,
    protocol::{Context, Failure},
};
use serde_json::{json, Value};
use std::io::Write;
use std::time::Instant;
use wb_switch_core::modules::account;

pub(super) fn progress(
    options: &Options,
    event: &str,
    message: &str,
    started: Instant,
    details: Option<Value>,
) {
    if options.quiet {
        return;
    }
    if options.json {
        write_progress(
            &mut std::io::stderr().lock(),
            &encode_json(
                &json!({"schemaVersion":1, "type":"progress", "command":options.command.as_str(), "target":options.target.as_str(), "event":event, "elapsedSeconds":started.elapsed().as_secs(), "message":message, "details":details}),
                options.ascii_json,
            ),
        );
    } else {
        let details = details.map(|v| format!(" {v}")).unwrap_or_default();
        write_progress(
            &mut std::io::stderr().lock(),
            &format!("[{}s] {message}{details}", started.elapsed().as_secs()),
        );
    }
}

fn write_progress(writer: &mut impl Write, line: &str) {
    // A disconnected progress consumer must not unwind and cancel a remote
    // token rotation. The operation still drains even when stderr is closed.
    let _ = writeln!(writer, "{line}");
}
/// Serialize first so existing JSON escapes remain intact, then escape Unicode
/// UTF-16 units (including surrogate pairs for emoji and supplementary CJK).
fn encode_json(value: &Value, ascii: bool) -> String {
    let serialized = value.to_string();
    if !ascii || serialized.is_ascii() {
        return serialized;
    }
    let mut encoded = String::with_capacity(serialized.len());
    for ch in serialized.chars() {
        if ch.is_ascii() {
            encoded.push(ch);
        } else {
            use std::fmt::Write;
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(encoded, "\\u{unit:04x}").expect("writing to String cannot fail");
            }
        }
    }
    encoded
}

pub fn print_failure(
    failure: &Failure,
    json_output: bool,
    ascii_json: bool,
    context: &Context,
    elapsed_ms: u64,
) {
    if json_output {
        let value = context.failure(failure, elapsed_ms);
        println!("{}", encode_json(&value, ascii_json));
    } else {
        eprintln!("{}: {}", failure.code, failure.message);
    }
}

pub fn print_result(value: &Value, json_output: bool, ascii_json: bool) {
    if json_output {
        println!("{}", encode_json(value, ascii_json));
        return;
    }
    match value["command"].as_str().unwrap_or_default() {
        "help" => println!("{}", super::HELP),
        "status" => {
            let status = &value["clientStatus"];
            println!("客户端: {}", text(value, "target"));
            println!("认证已配置: {}", status["configured"]);
            println!(
                "当前账号: {} ({})",
                text(status, "activeAccountName"),
                text(status, "activeAccountId")
            );
            println!("账号数: {}", value["accountCount"]);
        }
        "switch" => {
            let selected = &value["account"];
            if value["dryRun"] == true {
                println!(
                    "切号预览: {} ({})",
                    account::account_display_name(selected),
                    text(selected, "id")
                );
                println!("未修改配置或关闭会话；实际切号会关闭运行中的 CodeBuddy CLI。凭据与客户端配置尚未验证。");
                let impact = &value["impact"];
                println!(
                    "当前快照: {} 个匹配 CLI 进程，{} 个可能受影响进程；任务是否忙碌未知。",
                    impact["matchedProcessCount"], impact["processCount"]
                );
                for process in impact["processes"].as_array().into_iter().flatten() {
                    println!(
                        "  PID {}  启动: {}  类型: {}",
                        process["pid"],
                        text(process, "startedAtUtc"),
                        text(process, "kind")
                    );
                }
                println!("实际切号会重新扫描；不自动重开 CLI。");
            } else {
                println!(
                    "已切换: {} ({})",
                    account::account_display_name(selected),
                    text(&value["result"], "activeAccountId")
                );
                println!(
                    "尝试关闭 CLI 进程数: {}（未逐进程验证退出）",
                    value["result"]["closeAttemptedProcessCount"]
                );
                println!("CLI 未自动重开；本次未创建持久备份。");
            }
        }
        "inspect" => {
            println!(
                "CodeBuddy CLI: {} (configured: {})",
                text(&value["codebuddyCli"], "activeAccountName"),
                value["codebuddyCli"]["configured"]
            );
            println!("ID\tVARIANT\tNAME\tACTIVE CLI\tREMAINING / CAPACITY");
            for a in value["accounts"].as_array().into_iter().flatten() {
                let balance = if a["credits"]["ok"] == true {
                    credit_balance(&a["credits"])
                } else {
                    format!("ERROR: {}", text(&a["credits"]["error"], "message"))
                };
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    text(a, "id"),
                    text(a, "variant"),
                    display_name(a),
                    a["activeCli"],
                    balance
                );
                for limit in a["limited"].as_array().into_iter().flatten() {
                    println!(
                        "  {}: {} 秒后恢复 ({})",
                        limit["model"].as_str().unwrap_or("未知模型（范围未知）"),
                        limit["remainingSeconds"],
                        text(limit, "resetAtUtc")
                    );
                }
            }
            println!("{}", credit_summary(&value["summary"]));
            println!("未观察到限额不代表模型可用；各项时间戳反映其各自查询时间。");
        }
        "accounts" => {
            println!("ID\tVARIANT\tNAME\tACTIVE CLI");
            for a in value["accounts"].as_array().into_iter().flatten() {
                println!(
                    "{}\t{}\t{}\t{}",
                    text(a, "id"),
                    text(a, "variant"),
                    account::account_display_name(a),
                    a["activeCli"]
                );
            }
        }
        "credits" => {
            println!("ID\tVARIANT\tNAME\tREMAINING\tCAPACITY");
            for a in value["accounts"].as_array().into_iter().flatten() {
                if a["ok"] == true {
                    let remaining = if a["unlimited"] == true {
                        "不限量".to_string()
                    } else {
                        credit_amount(a, "totalRemaining")
                    };
                    let capacity = if a["unlimited"] == true {
                        "不限量".to_string()
                    } else {
                        credit_amount(a, "totalCapacity")
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        text(a, "accountId"),
                        text(a, "variant"),
                        text(a, "accountName"),
                        remaining,
                        capacity
                    );
                } else {
                    println!(
                        "{}\t{}\t{}\tERROR: {}",
                        text(a, "accountId"),
                        text(a, "variant"),
                        text(a, "accountName"),
                        text(&a["error"], "message")
                    );
                }
            }
            println!("{}", credit_summary(&value["summary"]));
        }
        "limits" => {
            println!("ID\tNAME\tMODEL\tRESET (UTC)\tSECONDS LEFT");
            for a in value["accounts"].as_array().into_iter().flatten() {
                let entries = a["limited"].as_array().unwrap();
                if entries.is_empty() {
                    println!(
                        "{}\t{}\tNo observed limits (availability unknown)",
                        text(a, "accountId"),
                        text(a, "accountName")
                    );
                }
                for e in entries {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        text(a, "accountId"),
                        text(a, "accountName"),
                        e["model"].as_str().unwrap_or("未知模型（范围未知）"),
                        text(e, "resetAtUtc"),
                        e["remainingSeconds"]
                    );
                }
            }
        }
        "version" => println!("workbuddy-switch {}", text(value, "version")),
        _ => println!("{}", serde_json::to_string_pretty(value).unwrap()),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("-")
}

fn display_name(value: &Value) -> String {
    value["name"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| account::account_display_name(value))
}

fn credit_amount(credit: &Value, field: &str) -> String {
    credit[format!("{field}Display")]
        .as_str()
        .map(str::to_string)
        .or_else(|| super::summary::display_number(&credit[field]))
        .unwrap_or_else(|| "未知".into())
}

fn credit_balance(credit: &Value) -> String {
    if credit["unlimited"] == true {
        "不限量".into()
    } else {
        format!(
            "{} / {}",
            credit_amount(credit, "totalRemaining"),
            credit_amount(credit, "totalCapacity")
        )
    }
}

fn credit_summary(summary: &Value) -> String {
    let total = if summary["hasUnknownBalances"] == true {
        format!(
            "包含 {} 个余额未知账号、{} 个不限量账号；已知有限配额小计: {} / capacity: {}",
            summary["unknownBalanceAccounts"],
            summary["unlimitedAccounts"],
            credit_amount(summary, "finiteTotalRemaining"),
            credit_amount(summary, "finiteTotalCapacity")
        )
    } else if summary["hasUnlimitedAccounts"] == true {
        format!(
            "包含 {} 个不限量账号；有限配额小计: {} / capacity: {}",
            summary["unlimitedAccounts"],
            credit_amount(summary, "finiteTotalRemaining"),
            credit_amount(summary, "finiteTotalCapacity")
        )
    } else {
        format!(
            "Total remaining: {} / capacity: {}",
            credit_amount(summary, "totalRemaining"),
            credit_amount(summary, "totalCapacity")
        )
    };
    format!(
        "{total} (successful accounts: {}/{}, complete: {})",
        summary["successfulAccounts"], summary["accountCount"], summary["complete"]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_progress_pipe_does_not_panic() {
        struct ClosedPipe;
        impl Write for ClosedPipe {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        write_progress(&mut ClosedPipe, "still committing authentication");
    }

    #[test]
    fn unlimited_balances_and_summaries_are_not_printed_as_zero_or_null() {
        assert_eq!(
            credit_balance(&json!({"unlimited":true,"totalRemaining":null,"totalCapacity":null})),
            "不限量"
        );
        let text = credit_summary(
            &json!({"hasUnlimitedAccounts":true,"unlimitedAccounts":2,"finiteTotalRemaining":12.5,"finiteTotalCapacity":20,"successfulAccounts":3,"accountCount":4,"complete":false}),
        );
        assert!(text.contains("2 个不限量账号"));
        assert!(text.contains("有限配额小计: 12.50"));
        assert!(text.contains("3/4, complete: false"));
        assert!(!text.contains("null"));
    }

    #[test]
    fn human_balances_use_two_decimals_and_preserve_unknown_values() {
        assert_eq!(
            credit_balance(&json!({"totalRemaining":1346.71000549,"totalCapacity":2000.0})),
            "1346.71 / 2000.00"
        );
        assert_eq!(
            credit_balance(&json!({"totalRemaining":null,"totalCapacity":null})),
            "未知 / 未知"
        );
        assert_eq!(display_name(&json!({"name":"摘要账号"})), "摘要账号");
    }
}
