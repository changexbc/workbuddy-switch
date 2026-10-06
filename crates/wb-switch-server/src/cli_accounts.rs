use serde_json::{json, Value};
use wb_switch_core::modules::{
    account, auth_file, codebuddy_cli, codebuddy_cn_ide, codebuddy_ide, config, credits, jetbrains,
    vscode_ext,
};

/// Indices are a presentation of store order, never persisted over account metadata.
pub fn indexed(accounts: &[Value]) -> Vec<Value> {
    accounts
        .iter()
        .enumerate()
        .map(|(i, acc)| {
            let mut meta = account::account_meta(acc);
            meta["index"] = json!(i + 1);
            meta
        })
        .collect()
}

pub fn resolve(accounts: &[Value], selector: &str) -> Result<(usize, Value), String> {
    // Explicit IDs take precedence, including numeric IDs imported from backups.
    if let Some((i, acc)) = accounts
        .iter()
        .enumerate()
        .find(|(_, a)| a["id"].as_str() == Some(selector))
    {
        return Ok((i, acc.clone()));
    }
    if let Ok(index) = selector.parse::<usize>() {
        if let Some(acc) = index.checked_sub(1).and_then(|i| accounts.get(i)) {
            return Ok((index - 1, acc.clone()));
        }
    }
    Err("账号不存在；请使用 accounts list 中的 index 或 id".into())
}

pub fn add_index(mut meta: Value) -> Value {
    if let Some(i) = account::load_accounts()
        .iter()
        .position(|acc| acc["id"] == meta["id"])
    {
        meta["index"] = json!(i + 1);
    }
    meta
}

fn services(accounts: &[Value]) -> Vec<Vec<String>> {
    let mut result = vec![Vec::new(); accounts.len()];
    let state_services = [
        (
            "cli",
            codebuddy_cli::status()["activeAccountId"]
                .as_str()
                .map(String::from),
        ),
        ("ide:cn", codebuddy_cn_ide::active_account_id_from_state()),
        ("ide:ai", codebuddy_ide::active_account_id_from_state()),
        ("vscode", vscode_ext::active_account_id_from_state()),
        ("jetbrains", jetbrains::active_account_id_from_state()),
    ];
    for (i, acc) in accounts.iter().enumerate() {
        for (service, id) in &state_services {
            if id.as_deref().is_some_and(|id| acc["id"] == id) {
                result[i].push(service.to_string());
            }
        }
        let v = account::variant_of(acc);
        let auth = auth_file::read_auth_file(v);
        if auth
            .as_ref()
            .and_then(|a| account::get_str(&a["account"], "uid"))
            .is_some_and(|uid| account::get_str(acc, "uid").as_deref() == Some(&uid))
        {
            result[i].push(format!("workbuddy:{}", v.as_str()));
        }
    }
    result
}

/// Query sequentially: token refresh performs an unlocked read-modify-write of
/// the shared account store. Parallel account queries could lose fresh tokens.
pub async fn query(accounts: &[Value]) -> Vec<Value> {
    let mut results = Vec::new();
    for acc in accounts {
        results.push(
            match tokio::time::timeout(
                std::time::Duration::from_secs(90),
                credits::get_credit_expiry(acc),
            )
            .await
            {
                Ok(value) => value,
                Err(_) => json!({"ok":false,"accountId":acc["id"],"error":"积分查询超时"}),
            },
        );
    }
    results
}

pub fn rows(accounts: &[Value], credit_results: Option<&[Value]>) -> Vec<Value> {
    let assignments = services(accounts);
    indexed(accounts)
        .into_iter()
        .enumerate()
        .map(|(i, mut meta)| {
            meta["services"] = json!(assignments[i]);
            if let Some(credits) = credit_results {
                meta["credits"] = credits[i].clone();
                // Credit queries may have updated needs_relogin via token refresh.
                if let Some(latest) = meta["id"].as_str().and_then(account::find_account) {
                    meta["needsRelogin"] = account::account_meta(&latest)["needsRelogin"].clone();
                }
            }
            meta
        })
        .collect()
}

fn spendable(credit: &Value, now: i64) -> Option<(f64, Option<i64>)> {
    if credit["ok"] != true {
        return None;
    }
    if let Some(resources) = credit["resources"].as_array() {
        let valid: Vec<_> = resources
            .iter()
            .filter(|r| {
                r["expired"] != true
                    && r["expireAt"].as_i64().is_none_or(|t| t > now)
                    && r["remaining"].as_f64().is_some_and(|n| n > 0.0)
            })
            .collect();
        let total: f64 = valid.iter().filter_map(|r| r["remaining"].as_f64()).sum();
        let soonest = valid.iter().filter_map(|r| r["expireAt"].as_i64()).min();
        return (total > 0.0).then_some((total, soonest));
    }
    let total = credit["totalRemaining"].as_f64()?;
    let expiry = credit["soonestExpireAt"].as_i64();
    (total > 0.0 && credit["expired"] != true && expiry.is_none_or(|t| t > now))
        .then_some((total, expiry))
}

pub fn choose(
    accounts: &[Value],
    results: &[Value],
    strategy: &str,
    now: i64,
) -> Result<usize, String> {
    let mut best: Option<(usize, f64, Option<i64>)> = None;
    for (i, (acc, credit)) in accounts.iter().zip(results).enumerate() {
        if acc["needs_relogin"] == true {
            continue;
        }
        let Some((total, expiry)) = spendable(credit, now) else {
            continue;
        };
        if strategy == "soonest" && expiry.is_none() {
            continue;
        }
        let better = best.is_none_or(|(_, btotal, bexpiry)| {
            if strategy == "richest" {
                total > btotal
            } else {
                expiry < bexpiry
            }
        });
        if better {
            best = Some((i, total, expiry));
        }
    }
    best.map(|(i, _, _)| i).ok_or_else(|| {
        "没有符合条件的账号（需查询成功、无需重新登录且有可用积分；soonest 还需有未来过期时间）"
            .into()
    })
}

pub fn timestamp(value: &Value) -> String {
    value
        .as_i64()
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".into())
}

fn cell(value: &Value) -> String {
    let s = if value.is_null() {
        "-".into()
    } else if let Some(s) = value.as_str() {
        s.into()
    } else {
        value.to_string()
    };
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn points(value: &Value) -> String {
    value
        .as_f64()
        .map(|n| format!("{n:.2}"))
        .unwrap_or_else(|| "-".into())
}

// CJK/emoji occupy two terminal columns. Keep full IDs and names without truncation.
fn width(s: &str) -> usize {
    s.chars().map(|c| if c >= '\u{1100}' { 2 } else { 1 }).sum()
}
pub fn table(headers: &[&str], rows: Vec<Vec<String>>) {
    let widths: Vec<_> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|r| width(&r[i]))
                .max()
                .unwrap_or(0)
                .max(width(h))
        })
        .collect();
    for row in
        std::iter::once(headers.iter().map(|s| s.to_string()).collect::<Vec<_>>()).chain(rows)
    {
        println!(
            "{}",
            row.iter()
                .enumerate()
                .map(|(i, s)| format!("{}{}", s, " ".repeat(widths[i] - width(s))))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }
}

pub fn print(value: &Value, lite: bool) {
    if !lite {
        println!("{}", serde_json::to_string_pretty(value).unwrap());
        return;
    }
    if let Some(rows) = value
        .as_array()
        .filter(|a| a.first().is_none_or(|r| r.get("id").is_some()))
    {
        table(
            &[
                "index",
                "ac id",
                "积分",
                "积分最近过期时间",
                "nickname",
                "needrelogin",
                "服务",
            ],
            rows.iter()
                .map(|r| {
                    vec![
                        cell(&r["index"]),
                        cell(&r["id"]),
                        points(&r["credits"]["totalRemaining"]),
                        timestamp(&r["credits"]["soonestExpireAt"]),
                        cell(&r["nickname"]),
                        cell(&r["needsRelogin"]),
                        r["services"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "-".into()),
                    ]
                })
                .collect(),
        );
        for row in rows {
            if let Some(error) = row["credits"]["error"].as_str() {
                eprintln!(
                    "账号 {} 积分查询失败: {}",
                    cell(&row["index"]),
                    cell(&json!(error))
                );
            }
        }
    } else if let Some(results) = value["results"].as_array() {
        for error in value["selectionErrors"].as_array().into_iter().flatten() {
            eprintln!(
                "账号 {} 未参与快捷选择: {}",
                cell(&error["accountId"]),
                cell(&error["error"])
            );
        }
        table(
            &["index", "ac id", "服务", "结果", "说明"],
            results
                .iter()
                .map(|r| {
                    vec![
                        cell(&value["index"]),
                        cell(&value["accountId"]),
                        cell(&r["service"]),
                        if r["error"].is_string() {
                            "失败".into()
                        } else if super::cli::has_issues(&r["result"]) {
                            "部分完成".into()
                        } else {
                            "成功".into()
                        },
                        cell(r.get("error").unwrap_or(&r["result"]["message"])),
                    ]
                })
                .collect(),
        );
        if super::cli::has_issues(value) {
            eprintln!("存在失败、跳过或待恢复项；不加 --lite 可查看完整操作报告。");
        }
    } else {
        let fields = [
            "index",
            "id",
            "nickname",
            "imported",
            "skipped",
            "overwritten",
        ];
        let shown: Vec<_> = fields
            .iter()
            .filter(|key| value.get(**key).is_some())
            .collect();
        table(
            &shown.iter().map(|k| **k).collect::<Vec<_>>(),
            vec![shown.iter().map(|k| cell(&value[**k])).collect()],
        );
    }
}

pub fn now() -> i64 {
    config::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indices_ids_and_bounds() {
        let accounts = vec![json!({"id":"a"}), json!({"id":"b"})];
        assert_eq!(indexed(&accounts)[1]["index"], 2);
        assert_eq!(resolve(&accounts, "2").unwrap().1["id"], "b");
        assert_eq!(resolve(&accounts, "a").unwrap().0, 0);
        assert!(resolve(&accounts, "0").is_err());
        assert!(resolve(&accounts, "3").is_err());
    }
    #[test]
    fn ranking_excludes_expired_failed_and_relogin_and_breaks_ties_by_index() {
        let accounts = vec![
            json!({}),
            json!({}),
            json!({"needs_relogin":true}),
            json!({}),
        ];
        let credits = vec![
            json!({"ok":true,"resources":[{"remaining":999,"expireAt":1},{"remaining":2,"expireAt":300}]}),
            json!({"ok":true,"resources":[{"remaining":8,"expireAt":500}]}),
            json!({"ok":true,"totalRemaining":100,"soonestExpireAt":200}),
            json!({"ok":false,"totalRemaining":1000}),
        ];
        assert_eq!(choose(&accounts, &credits, "soonest", 100).unwrap(), 0);
        assert_eq!(choose(&accounts, &credits, "richest", 100).unwrap(), 1);
        assert!(choose(&accounts, &[], "richest", 100).is_err());
        assert_eq!(
            choose(
                &[json!({}), json!({})],
                &[credits[1].clone(), credits[1].clone()],
                "richest",
                100
            )
            .unwrap(),
            0
        );
    }
}
