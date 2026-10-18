//! One-shot orchestration; no HTTP service, hooks or background jobs.
use super::{
    accounts::{load_accounts, select_accounts},
    args::{Command, Options},
    control::{Control, Signals},
    credits::query_credits,
    output::progress,
    protocol::{response, Failure},
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use wb_switch_core::modules::{account, config};

pub async fn execute(options: &Options) -> Result<(Value, u8), Failure> {
    let started = Instant::now();
    let control = Control::new();
    // Metadata has no authentication transaction to protect and must continue
    // to work in environments where console/signal registration is restricted.
    if matches!(options.command, Command::Help | Command::Version) {
        let (mut value, exit) = execute_inner(options, started, &control).await?;
        value["exitCode"] = json!(exit);
        value["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
        return Ok((value, exit));
    }
    let mut signals = Signals::new()?;
    if matches!(
        options.command,
        Command::Credits | Command::Inspect | Command::Switch
    ) {
        progress(
            options,
            "started",
            "开始执行；最终结果写入 stdout，进度写入 stderr",
            started,
            None,
        );
    }
    let operation = execute_inner(options, started, &control);
    tokio::pin!(operation);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(2),
        Duration::from_secs(2),
    );
    loop {
        tokio::select! {
            biased;
            _ = signals.recv() => {
                control.request();
                progress(options, "interrupted", "已收到中断；停止启动新查询，正在完成已开始的认证更新与切号，请等待安全退出", started, None);
            },
            result = &mut operation => {
                return result.map_err(|failure| {
                    if control.is_interrupted() && failure.code != "INTERRUPTED" {
                        Failure { code: "INTERRUPTED", message: format!("{} Operation ended with {}: {}", super::control::interrupted().message, failure.code, failure.message), exit: 130 }
                    } else { failure }
                }).map(|(mut value, exit)| {
                    let (finished, exit) = control.finish(value, exit);
                    value = finished;
                    value["exitCode"] = json!(exit);
                    value["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
                    if options.summary {
                        value = super::summary::project(&value);
                    }
                    (value, exit)
                });
            },
            _ = heartbeat.tick() => progress(options, "heartbeat", "仍在处理，等待共享状态或网络请求完成", started, None),
        }
    }
}
async fn auth_snapshot(
    options: &Options,
    started: Instant,
    control: &Control,
) -> Result<wb_switch_core::modules::state_lock::StateLock, Failure> {
    control.check()?;
    use wb_switch_core::modules::state_lock;
    match state_lock::try_auth() {
        Ok(guard) => Ok(guard),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && options.wait > 0 => {
            progress(
                options,
                "waiting",
                "认证状态正在更新，自动等待锁释放",
                started,
                Some(json!({"waitSeconds":options.wait})),
            );
            tokio::select! {
                biased;
                _ = control.cancelled() => Err(super::control::interrupted()),
                result = state_lock::wait_auth(Duration::from_secs(options.wait)) =>
                    result.map_err(|error| Failure::operation("STATE_LOCK_FAILED", error.to_string())),
            }
        }
        Err(error) => Err(Failure::operation("STATE_LOCK_FAILED", error.to_string())),
    }
}

async fn execute_inner(
    options: &Options,
    started: Instant,
    control: &Control,
) -> Result<(Value, u8), Failure> {
    if options.command == Command::Help {
        let mut value = response("help", options.target);
        value["usage"] = json!(super::HELP);
        value["supportedTargets"] = json!([options.target.as_str()]);
        return Ok((value, 0));
    }
    if options.command == Command::Version {
        let mut value = response("version", options.target);
        value["version"] = json!(env!("CARGO_PKG_VERSION"));
        return Ok((value, 0));
    }
    let snapshot_guard = auth_snapshot(options, started, control).await?;
    let selected = select_accounts(load_accounts()?, options)?;
    let cli_status = if matches!(options.command, Command::Accounts | Command::Status) {
        Some(options.target.status())
    } else {
        None
    };
    drop(snapshot_guard);
    let value = match options.command {
        Command::Accounts => {
            let mut value = response("accounts", options.target);
            let current = cli_status.as_ref().unwrap()["activeAccountId"].clone();
            value["activeCliAccountId"] = current.clone();
            value["activeAccountId"] = current.clone();
            value["accounts"] = json!(selected
                .iter()
                .map(|a| {
                    let mut meta = account::account_meta(a);
                    meta["activeCli"] = json!(meta["id"] == current);
                    meta["activeForTarget"] = meta["activeCli"].clone();
                    meta
                })
                .collect::<Vec<_>>());
            value
        }
        Command::Status => {
            let mut value = response("status", options.target);
            value["clientStatus"] = cli_status.unwrap();
            value["codebuddyCli"] = value["clientStatus"].clone();
            value["accountCount"] = json!(selected.len());
            value
        }
        Command::Switch => {
            let selected = &selected[0];
            let mut value = response("switch", options.target);

            value["account"] = account::account_meta(selected);
            value["dryRun"] = json!(options.dry_run);
            if options.dry_run {
                let target = options.target;
                value["impact"] = tokio::task::spawn_blocking(move || target.switch_preview())
                    .await
                    .map_err(|_| {
                        Failure::operation("PROCESS_SCAN_FAILED", "Process preview task failed")
                    })?
                    .map_err(|message| Failure::operation("PROCESS_SCAN_FAILED", message))?;
                value["willCloseRunningCli"] = json!(true);
                value["autoReopened"] = json!(false);
                value["note"] = json!(
                    "Preview only; process impact is a point-in-time snapshot. Credentials and client configuration have not been validated"
                );
            } else {
                let id = selected["id"].as_str().unwrap().to_string();
                let waiting = Instant::now();
                let mut reported_wait = false;
                loop {
                    control.check()?;
                    let id = id.clone();
                    let target = options.target;
                    let switched = tokio::task::spawn_blocking(move || target.switch(&id))
                        .await
                        .map_err(|_| Failure::operation("SWITCH_FAILED", "Switch task failed"))?;
                    match switched {
                        Err(message)
                            if message
                                .starts_with(wb_switch_core::modules::state_lock::BUSY_PREFIX)
                                && waiting.elapsed() < Duration::from_secs(options.wait) =>
                        {
                            if !reported_wait {
                                progress(
                                    options,
                                    "waiting",
                                    "其他进程正在更新认证状态，自动等待后切号",
                                    started,
                                    None,
                                );
                                reported_wait = true;
                            }
                            tokio::select! {
                                _ = control.cancelled() => return Err(super::control::interrupted()),
                                _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                            }
                        }
                        result => {
                            value["result"] = result
                                .map_err(|message| Failure::operation("SWITCH_FAILED", message))?;
                            break;
                        }
                    }
                }
            }
            value
        }
        Command::Credits => return Ok(query_credits(selected, options, started, control).await),
        Command::Inspect => {
            let query_started_at = config::now_ms();
            progress(
                options,
                "limits_scan_started",
                "正在并行查询积分与扫描本机模型限额日志",
                started,
                Some(json!({"total":selected.len()})),
            );
            let credit_query = async {
                let result = query_credits(selected.clone(), options, started, control).await;
                progress(
                    options,
                    "credits_completed",
                    "积分查询已全部结束，等待限额扫描并汇总结果",
                    started,
                    Some(result.0["summary"].clone()),
                );
                result
            };
            let scan = tokio::task::spawn_blocking({
                let target = options.target;
                move || target.limits()
            });
            let ((credit_result, exit), snapshot) = tokio::join!(credit_query, scan);
            let snapshot =
                snapshot.map_err(|_| Failure::operation("LIMITS_FAILED", "Limit scan failed"))?;
            let limit_result = limits_response(&selected, options, snapshot);
            let final_snapshot = async {
                let guard = auth_snapshot(options, started, control).await?;
                let current = options.target.status();
                let latest = load_accounts()?;
                drop(guard);
                Ok((current, latest))
            }
            .await;
            return Ok(inspect_response(
                &selected,
                options,
                credit_result,
                exit,
                limit_result,
                query_started_at,
                final_snapshot,
            ));
        }
        Command::Limits => {
            progress(
                options,
                "limits_scan_started",
                "正在扫描本机模型限额日志",
                started,
                None,
            );
            let snapshot = tokio::task::spawn_blocking({
                let target = options.target;
                move || target.limits()
            })
            .await
            .map_err(|_| Failure::operation("LIMITS_FAILED", "Limit scan failed"))?;
            limits_response(&selected, options, snapshot)
        }
        _ => unreachable!("metadata commands handled above; serve dispatched by main"),
    };
    Ok((value, 0))
}

fn inspect_response(
    selected: &[Value],
    options: &Options,
    credit_result: Value,
    credit_exit: u8,
    limit_result: Value,
    query_started_at: i64,
    final_snapshot: Result<(Value, Vec<Value>), Failure>,
) -> (Value, u8) {
    // A final GUI/contention/store-read failure must not discard already
    // completed queries or force an AI to query every account again.
    let (current, latest, status_error) = match final_snapshot {
        Ok((current, latest)) => (current, Some(latest), None),
        Err(failure) => (
            Value::Null,
            None,
            Some(super::protocol::error(failure.code, &failure.message)),
        ),
    };
    let mut value = response("inspect", options.target);
    value["ok"] = credit_result["ok"].clone();
    value["summary"] = credit_result["summary"].clone();
    value["codebuddyCli"] = current.clone();
    value["clientStatus"] = current.clone();
    value["queryStartedAt"] = json!(query_started_at);
    value["queriedAt"] = json!(config::now_ms());
    value["limitsSource"] = limit_result["source"].clone();
    value["limitsScannedAt"] = limit_result["scannedAt"].clone();
    value["monitoringConfig"] = limit_result["monitoringConfig"].clone();
    value["availabilityKnown"] = json!(false);
    value["accounts"] = json!(selected
        .iter()
        .enumerate()
        .map(|(index, a)| {
            let newest = latest
                .as_ref()
                .into_iter()
                .flatten()
                .find(|newest| newest["id"] == a["id"])
                .unwrap_or(a);
            let mut meta = account::account_meta(newest);
            meta["present"] = json!(latest
                .as_ref()
                .map(|items| items.iter().any(|newest| newest["id"] == a["id"])));
            meta["activeCli"] = if current.is_null() {
                Value::Null
            } else {
                json!(a["id"] == current["activeAccountId"])
            };
            meta["activeForTarget"] = meta["activeCli"].clone();
            meta["credits"] = credit_result["accounts"][index].clone();
            meta["limited"] = limit_result["accounts"][index]["limited"].clone();
            meta["hasObservedLimits"] =
                limit_result["accounts"][index]["hasObservedLimits"].clone();
            for field in [
                "availabilityStatus",
                "modelStates",
                "unattributedLimitCount",
            ] {
                meta[field] = limit_result["accounts"][index][field].clone();
            }
            meta
        })
        .collect::<Vec<_>>());
    value["error"] = credit_result["error"].clone();
    let exit = if let Some(error) = status_error {
        value["ok"] = json!(false);
        value["clientStatusError"] = error.clone();
        value["error"] = error;
        if credit_exit == 1 {
            1
        } else {
            3
        }
    } else {
        credit_exit
    };
    (value, exit)
}

fn limits_response(selected: &[Value], options: &Options, snapshot: Value) -> Value {
    let now = config::now_ms();
    let mut value = response("limits", options.target);
    value["source"] = json!(options.target.limits_source());
    value["availabilityKnown"] = json!(false);
    value["queriedAt"] = json!(now);
    value["scannedAt"] = snapshot["scannedAt"].clone();
    value["windowDays"] = snapshot["windowDays"].clone();
    value["monitoringConfig"] = config::load_rate_limit_config();
    value["accounts"] = json!(selected
        .iter()
        .map(|a| {
            let entries = snapshot["accounts"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["accountId"] == a["id"]))
                .and_then(|item| item["limited"].as_array());
            limit_account_response(
                a,
                options,
                entries.map(Vec::as_slice).unwrap_or_default(),
                now,
            )
        })
        .collect::<Vec<_>>());
    value
}

fn limit_account_response(
    account: &Value,
    options: &Options,
    observed: &[Value],
    now: i64,
) -> Value {
    let active: Vec<_> = observed
        .iter()
        .filter(|e| e["resetAt"].as_i64().is_some_and(|reset| reset > now))
        .collect();
    // An unassigned model is evidence of a limit with unknown scope. It must
    // never become an account-wide prohibition, even when --model filters it out.
    let unattributed = active
        .iter()
        .filter(|e| e["model"].as_str().is_none())
        .count();
    let entries: Vec<_> = active
        .into_iter()
        .filter(|e| {
            options
                .model
                .as_ref()
                .is_none_or(|m| e["model"].as_str() == Some(m))
        })
        .map(|e| {
            let mut entry = e.clone();
            let reset = entry["resetAt"].as_i64().unwrap();
            entry["remainingSeconds"] = json!(reset.saturating_sub(now).saturating_add(999) / 1000);
            entry["resetAtUtc"] =
                json!(chrono::DateTime::from_timestamp_millis(reset).map(|dt| dt.to_rfc3339()));
            entry
        })
        .collect();
    let mut model_states: Vec<_> = entries
        .iter()
        .filter(|e| e["model"].is_string())
        .map(|e| {
            json!({"model":e["model"], "scope":"model", "status":"limited",
            "observedAt":e["observedAt"], "checkedAt":now,
            "resetAt":e["resetAt"], "resetAtUtc":e["resetAtUtc"],
            "remainingSeconds":e["remainingSeconds"], "reason":"MODEL_LIMIT_OBSERVED"})
        })
        .collect();
    if let Some(model) = &options.model {
        if model_states.is_empty() {
            model_states.push(json!({"model":model,"scope":"model","status":"unknown",
                "observedAt":null,"checkedAt":now,"resetAt":null,"resetAtUtc":null,
                "remainingSeconds":null,"reason":"NO_ACTIVE_LIMIT_OBSERVED"}));
        }
    }
    json!({"accountId":account["id"], "accountName":account::account_display_name(account),
        "variant":account::variant_of(account).as_str(), "availabilityStatus":"unknown",
        "hasObservedLimits":!entries.is_empty(), "unattributedLimitCount":unattributed,
        "modelStates":model_states,"limited":entries})
}

#[cfg(test)]
mod inspect_tests {
    use super::*;

    #[test]
    fn model_state_stays_unknown_without_active_evidence_and_preserves_unassigned_limits() {
        let options = super::super::parse(&["limits".into(), "--model=hy3".into()])
            .unwrap_or_else(|failure| panic!("{}", failure.message));
        let observed = vec![
            json!({"model":"hy3","resetAt":1000,"observedAt":500,"scope":"model"}),
            json!({"model":null,"resetAt":3000,"observedAt":600,"scope":"unknown"}),
            json!({"model":"other","resetAt":4000,"observedAt":700,"scope":"model"}),
        ];
        let value = limit_account_response(&json!({"id":"a"}), &options, &observed, 1000);
        assert_eq!(value["availabilityStatus"], "unknown");
        assert_eq!(value["unattributedLimitCount"], 1);
        assert_eq!(value["limited"], json!([]));
        assert_eq!(value["modelStates"][0]["model"], "hy3");
        assert_eq!(value["modelStates"][0]["status"], "unknown");
        assert!(value["modelStates"][0]["observedAt"].is_null());
        assert!(value["modelStates"][0]["resetAt"].is_null());
        assert_eq!(value["modelStates"][0]["checkedAt"], 1000);
    }

    #[test]
    fn only_bound_models_get_limited_state_and_account_availability_stays_unknown() {
        let options = super::super::parse(&["limits".into()])
            .unwrap_or_else(|failure| panic!("{}", failure.message));
        let observed = vec![
            json!({"model":"hy3","resetAt":2001,"observedAt":500,"scope":"model"}),
            json!({"model":null,"resetAt":3000,"observedAt":600,"scope":"unknown"}),
        ];
        let value = limit_account_response(&json!({"id":"a"}), &options, &observed, 1000);
        assert_eq!(value["availabilityStatus"], "unknown");
        assert_eq!(value["limited"].as_array().unwrap().len(), 2);
        assert_eq!(value["modelStates"].as_array().unwrap().len(), 1);
        assert_eq!(value["modelStates"][0]["status"], "limited");
        assert_eq!(value["modelStates"][0]["observedAt"], 500);
        assert_eq!(value["modelStates"][0]["remainingSeconds"], 2);
        assert_eq!(value["limited"][1]["scope"], "unknown");
    }

    fn fixture(successful_accounts: usize) -> (Vec<Value>, Options, Value, u8, Value) {
        let selected = vec![
            json!({"id":"a","nickname":"Alpha","access_token":"private-token"}),
            json!({"id":"b","nickname":"Beta"}),
        ];
        let options = super::super::parse(&["inspect".into()])
            .unwrap_or_else(|failure| panic!("{}", failure.message));
        let results = selected.iter().enumerate().map(|(index, account)| {
            if index < successful_accounts {
                json!({"ok":true,"accountId":account["id"],"totalRemaining":12,"totalCapacity":20})
            } else {
                json!({"ok":false,"accountId":account["id"],"error":"offline"})
            }
        }).collect();
        let (credits, exit) = super::super::credits::summarize_credits(results, 2, options.target);
        let limits = json!({"source":"fixture-logs","scannedAt":1234,"monitoringConfig":{"enabled":true},"accounts":[
            {"hasObservedLimits":true,"limited":[{"model":"model-x","resetAt":4321,"remainingSeconds":30}]},
            {"hasObservedLimits":false,"limited":[]}
        ]});
        (selected, options, credits, exit, limits)
    }

    #[test]
    fn final_status_failure_retains_completed_queries_and_credit_completeness() {
        for (successful_accounts, expected_exit) in [(2, 3), (1, 3), (0, 1)] {
            let (selected, options, credits, credit_exit, limits) = fixture(successful_accounts);
            let original_summary = credits["summary"].clone();
            let original_queries = credits["accounts"].clone();
            let failure = Failure::operation(
                "AUTH_STATE_BUSY",
                "AUTH_STATE_BUSY: another host is refreshing",
            );
            let (value, exit) = inspect_response(
                &selected,
                &options,
                credits,
                credit_exit,
                limits,
                1000,
                Err(failure),
            );
            assert_eq!(exit, expected_exit);
            assert_eq!(value["ok"], false);
            assert_eq!(value["summary"], original_summary);
            assert_eq!(value["summary"]["complete"], successful_accounts == 2);
            assert_eq!(value["clientStatusError"]["code"], "AUTH_STATE_BUSY");
            assert_eq!(value["clientStatusError"]["retryable"], true);
            assert_eq!(value["error"], value["clientStatusError"]);
            assert!(value["clientStatus"].is_null());
            assert!(value["codebuddyCli"].is_null());
            assert_eq!(value["limitsScannedAt"], 1234);
            assert_eq!(value["accounts"][0]["limited"][0]["model"], "model-x");
            for index in 0..2 {
                assert_eq!(value["accounts"][index]["credits"], original_queries[index]);
                assert!(value["accounts"][index]["present"].is_null());
                assert!(value["accounts"][index]["activeCli"].is_null());
                assert!(value["accounts"][index]["activeForTarget"].is_null());
            }
            assert!(!value.to_string().contains("private-token"));
        }
    }

    #[test]
    fn final_store_failure_reports_actual_error_without_overwriting_completed_balances() {
        let (selected, options, credits, credit_exit, limits) = fixture(2);
        let failure = Failure::operation(
            "ACCOUNT_STORE_INVALID",
            "Account store changed during the query",
        );
        let (value, exit) = inspect_response(
            &selected,
            &options,
            credits,
            credit_exit,
            limits,
            1000,
            Err(failure),
        );
        assert_eq!(exit, 3);
        assert_eq!(value["clientStatusError"]["code"], "ACCOUNT_STORE_INVALID");
        assert_eq!(
            value["clientStatusError"]["message"],
            "Account store changed during the query"
        );
        assert_eq!(value["summary"]["totalRemaining"], 24.0);
        assert_eq!(value["summary"]["complete"], true);
    }

    #[test]
    fn interrupted_inspect_still_returns_130_and_preserves_completed_payload() {
        let (selected, options, credits, credit_exit, limits) = fixture(1);
        let (value, exit) = inspect_response(
            &selected,
            &options,
            credits,
            credit_exit,
            limits,
            1000,
            Err(super::super::control::interrupted()),
        );
        let control = Control::new();
        control.request();
        let (value, exit) = control.finish(value, exit);
        assert_eq!(exit, 130);
        assert_eq!(value["error"]["code"], "INTERRUPTED");
        assert_eq!(value["accounts"][0]["credits"]["ok"], true);
        assert_eq!(value["summary"]["totalRemaining"], 12.0);
        assert_eq!(value["accounts"][0]["limited"][0]["model"], "model-x");
    }
}
