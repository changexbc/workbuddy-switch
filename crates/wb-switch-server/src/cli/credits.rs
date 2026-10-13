//! Shared-account credit queries, bounded concurrency and partial totals.
use super::{
    args::Options,
    control::Control,
    output::progress,
    protocol::{error, normalize_account_error, response},
    summary::{decorate_credit, decorate_totals},
    target::Target,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use wb_switch_core::modules::{account, config, credits};

async fn query_one(account: Value, options: &Options, started: Instant) -> Value {
    let query_started_at = config::now_ms();
    let query = credits::get_credit_expiry(&account);
    tokio::pin!(query);
    let mut result = match tokio::time::timeout(
        Duration::from_secs(options.timeout),
        query.as_mut(),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            progress(
                options,
                "timeout_cleanup",
                "查询已超时，正在完成认证更新以免凭据损坏，请等待安全退出",
                started,
                Some(json!({"accountId":account["id"]})),
            );
            // A remote token rotation must finish its local commit before exit.
            let _ = query.await;
            json!({"ok": false, "error": "Credit query timed out; pending authentication updates have finished", "errorCode": "TIMEOUT"})
        }
    };
    if result["error"].as_str().is_some_and(|message| {
        message.starts_with(wb_switch_core::modules::state_lock::BUSY_PREFIX)
    }) {
        result["errorCode"] = json!("AUTH_STATE_BUSY");
    }
    // Keep failed and successful results identifiable, including legacy responses.
    result["accountId"] = account["id"].clone();
    result["accountName"] = json!(account::account_display_name(&account));
    result["variant"] = json!(account::variant_of(&account).as_str());
    result["queryStartedAt"] = json!(query_started_at);
    result["queriedAt"] = json!(config::now_ms());
    normalize_account_error(&mut result);
    result
}

pub(super) async fn query_credits(
    accounts: Vec<Value>,
    options: &Options,
    started: Instant,
    control: &Control,
) -> (Value, u8) {
    query_credits_with(
        accounts,
        options,
        started,
        control,
        |account, owned_options, started| async move {
            query_one(account, &owned_options, started).await
        },
    )
    .await
}

async fn query_credits_with<F, Fut>(
    accounts: Vec<Value>,
    options: &Options,
    started: Instant,
    control: &Control,
    mut query: F,
) -> (Value, u8)
where
    F: FnMut(Value, Options, Instant) -> Fut,
    Fut: std::future::Future<Output = Value> + Send + 'static,
{
    let query_started_at = config::now_ms();
    let count = accounts.len();
    let identities: Vec<_> = accounts.iter().map(account::account_meta).collect();
    let mut pending = accounts.into_iter().enumerate();
    let mut tasks = tokio::task::JoinSet::new();
    let mut results = BTreeMap::new();
    let mut scheduled = 0;
    loop {
        while !control.is_interrupted() && tasks.len() < options.concurrency {
            let Some((index, account)) = pending.next() else {
                break;
            };
            let owned_options = options.clone();
            scheduled += 1;
            let pending_query = query(account, owned_options, started);
            tasks.spawn(async move { (index, pending_query.await) });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        match joined {
            Ok((index, value)) => {
                progress(
                    options,
                    "account_completed",
                    "账号积分查询完成",
                    started,
                    Some(
                        json!({"completed":results.len()+1, "total":count, "accountId":value["accountId"], "accountName":value["accountName"], "ok":value["ok"]}),
                    ),
                );
                results.insert(index, value);
            }
            Err(_) => {
                // Drain siblings: aborting them could interrupt a remote token rotation.
                progress(
                    options,
                    "account_failed",
                    "一个查询任务失败；其余查询将继续完成",
                    started,
                    None,
                );
            }
        }
    }
    for (index, meta) in identities.iter().enumerate() {
        results.entry(index).or_insert_with(|| {
            let skipped = control.is_interrupted() && index >= scheduled;
            json!({"ok":false, "accountId":meta["id"], "accountName":account::account_display_name(meta), "variant":meta["variant"],
                "errorCode":if skipped { "INTERRUPTED" } else { "CREDIT_QUERY_FAILED" },
                "error":if skipped { "Query not started because the command was interrupted" } else { "Credit query task failed" }})
        });
    }
    let (mut value, exit) =
        summarize_credits(results.into_values().collect(), count, options.target);
    value["queryStartedAt"] = json!(query_started_at);
    value["queriedAt"] = json!(config::now_ms());
    (value, exit)
}

pub(super) fn summarize_credits(
    mut results: Vec<Value>,
    count: usize,
    target: Target,
) -> (Value, u8) {
    for result in &mut results {
        normalize_account_error(result);
        decorate_credit(result);
    }
    let successful: Vec<_> = results.iter().filter(|r| r["ok"] == true).collect();
    let success_count = successful.len();
    let unlimited_count = successful.iter().filter(|r| r["unlimited"] == true).count();
    let finite: Vec<_> = successful
        .iter()
        .copied()
        .filter(|r| {
            r["unlimited"] != true
                && r["totalRemaining"].is_number()
                && r["totalCapacity"].is_number()
        })
        .collect();
    let finite_count = finite.len();
    let unknown_count = success_count - unlimited_count - finite_count;
    let mut value = response("credits", target);
    value["ok"] = json!(success_count == count);
    let sum = |field: &str| {
        finite
            .iter()
            .filter_map(|r| r[field].as_f64())
            .fold(0.0, |total, amount| total + amount)
    };
    let finite_remaining = sum("totalRemaining");
    let finite_capacity = sum("totalCapacity");
    value["summary"] = json!({"accountCount": count, "successfulAccounts": success_count,
        "failedAccounts": count - success_count, "complete": success_count == count,
        "hasUnlimitedAccounts": unlimited_count > 0, "unlimitedAccounts": unlimited_count,
        "hasUnknownBalances": unknown_count > 0, "unknownBalanceAccounts": unknown_count,
        "balanceComplete": success_count == count && unknown_count == 0,
        "finiteAccounts": finite_count, "finiteTotalRemaining": finite_remaining, "finiteTotalCapacity": finite_capacity,
        "totalRemaining": if unlimited_count > 0 || unknown_count > 0 { None } else { Some(finite_remaining) },
        "totalCapacity": if unlimited_count > 0 || unknown_count > 0 { None } else { Some(finite_capacity) },
        "expiringSoonRemaining": sum("expiringSoonRemaining")});
    decorate_totals(&mut value["summary"]);
    value["accounts"] = json!(results);
    if success_count != count {
        value["error"] = error(
            "CREDIT_QUERY_FAILED",
            "Totals include successful accounts only; inspect per-account errors",
        );
    }
    let exit = if success_count == count {
        0
    } else if success_count == 0 {
        1
    } else {
        3
    };
    value["exitCode"] = json!(exit);
    (value, exit)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn interruption_drains_started_auth_commit_and_skips_queued_accounts() {
        let control = Control::new();
        let options =
            super::super::parse(&["credits".into(), "--concurrency=1".into(), "--quiet".into()])
                .unwrap_or_else(|failure| panic!("{}", failure.message));
        let (rotated_tx, rotated_rx) = tokio::sync::oneshot::channel();
        let rotated_tx = Arc::new(std::sync::Mutex::new(Some(rotated_tx)));
        let commit_allowed = Arc::new(tokio::sync::Notify::new());
        let committed = Arc::new(AtomicBool::new(false));
        let query_count = Arc::new(AtomicUsize::new(0));
        let task_control = control.clone();
        let task_commit_allowed = commit_allowed.clone();
        let task_committed = committed.clone();
        let task_query_count = query_count.clone();
        let task = tokio::spawn(async move {
            query_credits_with(vec![json!({"id":"started"}),json!({"id":"queued"})], &options, Instant::now(), &task_control,
                move |account, _, _| {
                    let rotated_tx = rotated_tx.clone();
                    let commit_allowed = task_commit_allowed.clone();
                    let committed = task_committed.clone();
                    let query_count = task_query_count.clone();
                    async move {
                        query_count.fetch_add(1, Ordering::SeqCst);
                        rotated_tx.lock().unwrap().take().unwrap().send(()).unwrap();
                        commit_allowed.notified().await;
                        committed.store(true, Ordering::SeqCst);
                        json!({"ok":true,"accountId":account["id"],"totalRemaining":12,"totalCapacity":20})
                    }
                }).await
        });
        rotated_rx.await.unwrap();
        control.request();
        assert!(
            !task.is_finished(),
            "interrupt must not abort the pending local commit"
        );
        commit_allowed.notify_one();
        let (value, _) = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(committed.load(Ordering::SeqCst));
        assert_eq!(query_count.load(Ordering::SeqCst), 1);
        assert_eq!(value["accounts"][0]["ok"], true);
        assert_eq!(value["accounts"][1]["error"]["code"], "INTERRUPTED");
        assert_eq!(value["summary"]["totalRemaining"], 12.0);
        assert_eq!(value["summary"]["complete"], false);
    }
}
