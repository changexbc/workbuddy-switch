//! Compact projections for AI callers, without changing the full JSON contract.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Display precision is deliberately separate from the original credit values.
/// `null` stays unknown/unlimited; no display value is invented for failed queries.
pub(super) fn display_number(value: &Value) -> Option<String> {
    let amount = value.as_f64().filter(|amount| amount.is_finite())?;
    let display = format!("{amount:.2}");
    Some(if display == "-0.00" {
        "0.00".into()
    } else {
        display
    })
}

pub(super) fn decorate_credit(credit: &mut Value) {
    let successful = credit["ok"] == true;
    let unlimited = successful && credit["unlimited"] == true;
    credit["balanceType"] = json!(if unlimited {
        "unlimited"
    } else if successful && credit["totalRemaining"].is_number() {
        "finite"
    } else {
        "unknown"
    });
    for field in [
        "totalRemaining",
        "totalCapacity",
        "expiringSoonRemaining",
        "expiredRemaining",
    ] {
        credit[format!("{field}Display")] = json!(if successful && !unlimited {
            display_number(&credit[field])
        } else {
            None
        });
    }
    credit["soonestExpireAtUtc"] = utc(&credit["soonestExpireAt"]);
}

pub(super) fn decorate_totals(totals: &mut Value) {
    for field in [
        "totalRemaining",
        "totalCapacity",
        "finiteTotalRemaining",
        "finiteTotalCapacity",
        "expiringSoonRemaining",
    ] {
        totals[format!("{field}Display")] = json!(display_number(&totals[field]));
    }
}

fn utc(value: &Value) -> Value {
    json!(value
        .as_i64()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| dt.to_rfc3339()))
}

fn fields(value: &Value, keys: &[&str]) -> Value {
    let mut projected = Map::new();
    for key in keys {
        if let Some(field) = value.get(*key) {
            projected.insert((*key).into(), field.clone());
        }
    }
    Value::Object(projected)
}

/// Preserve machine decisions and operation errors; omit verbose account
/// metadata/resources/settings. Projection is read-only and never queries again.
pub(super) fn project(value: &Value) -> Value {
    project_at(value, wb_switch_core::modules::config::now_ms())
}

fn project_at(value: &Value, as_of: i64) -> Value {
    let mut compact = fields(
        value,
        &[
            "schemaVersion",
            "ok",
            "command",
            "target",
            "scope",
            "elapsedMs",
            "exitCode",
            "error",
            "operationOk",
            "operationError",
            "interrupted",
            "summary",
            "queryStartedAt",
            "queriedAt",
            "clientStatusError",
            "limitsSource",
            "limitsScannedAt",
            "availabilityKnown",
        ],
    );
    compact["outputMode"] = json!("summary");
    compact["creditDisplay"] = json!({"unit":"credits","decimalPlaces":2,"presentationOnly":true});
    let inspect = value["command"] == "inspect";
    if inspect {
        compact["clientStatus"] = if value["clientStatus"].is_null() {
            Value::Null
        } else {
            fields(
                &value["clientStatus"],
                &[
                    "configured",
                    "authMode",
                    "settingsPresent",
                    "helperPresent",
                    "helperSupportsAccountIds",
                    "activeAccountId",
                    "activeAccountName",
                    "environmentOverride",
                    "migrationRequired",
                    "helperCurrent",
                    "syncPending",
                    "syncInProgress",
                    "activeAccountVariant",
                    "lastSwitchAt",
                ],
            )
        };
        // Retain the alias used by --text and existing callers.
        compact["codebuddyCli"] = compact["clientStatus"].clone();
    }
    compact["accounts"] = json!(value["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|account| {
            if inspect {
                let mut item = fields(
                    account,
                    &[
                        "id",
                        "variant",
                        "present",
                        "activeCli",
                        "activeForTarget",
                        "needsRelogin",
                        "needsReloginReason",
                        "isEnterprise",
                        "hasObservedLimits",
                        "availabilityKnown",
                        "modelStates",
                        "availabilityStatus",
                        "unattributedLimitCount",
                    ],
                );
                item["name"] = json!(wb_switch_core::modules::account::account_display_name(
                    account
                ));
                item["credits"] = compact_credit(&account["credits"], false, as_of);
                item["limited"] = json!(account["limited"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|limit| fields(
                        limit,
                        &[
                            "model",
                            "scope",
                            "status",
                            "state",
                            "resetAt",
                            "resetAtUtc",
                            "remainingSeconds",
                            "observedAt",
                            "lastObservedAt",
                            "observedAtUtc",
                            "source",
                            "sources",
                            "sourceClients",
                            "evidence",
                            "reason",
                            "scopeReason",
                            "hitCount",
                            "confidence",
                        ]
                    ))
                    .collect::<Vec<_>>());
                item
            } else {
                compact_credit(account, true, as_of)
            }
        })
        .collect::<Vec<_>>());
    compact
}

fn compact_credit(credit: &Value, identity: bool, as_of: i64) -> Value {
    let mut value = fields(
        credit,
        &[
            "ok",
            "error",
            "balanceType",
            "unlimited",
            "totalRemaining",
            "totalCapacity",
            "totalRemainingDisplay",
            "totalCapacityDisplay",
            "expiringSoonRemaining",
            "expiringSoonRemainingDisplay",
            "expiredRemaining",
            "expiredRemainingDisplay",
            "soonestExpireAt",
            "soonestExpireAtUtc",
            "expiringSoon",
            "expired",
            "updatedAt",
            "queryStartedAt",
            "queriedAt",
            "source",
            "warning",
            "warnings",
        ],
    );
    if identity {
        for key in [
            "accountId",
            "accountName",
            "variant",
            "activeCli",
            "activeForTarget",
        ] {
            if let Some(field) = credit.get(key) {
                value[key] = field.clone();
            }
        }
    }
    if credit["ok"] == true {
        let balances = expiry_balances(credit, as_of);
        // The raw query can contain old positive balances. Do not let its
        // already expired earliest date guide account selection in a summary.
        if credit["resources"].is_array() {
            value["soonestExpireAt"] = json!(balances
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|balance| balance["expireAt"].as_i64())
                .min());
            value["soonestExpireAtUtc"] = utc(&value["soonestExpireAt"]);
        }
        value["expiryBalances"] = balances;
        value["expiryBalancesAsOf"] = json!(as_of);
    }
    value
}

/// Group unexpired positive balances by expiry without a row limit.
/// An absent expiry remains explicitly unknown, including long-lived placeholders.
fn expiry_balances(credit: &Value, as_of: i64) -> Value {
    let mut buckets: BTreeMap<Option<i64>, (f64, bool, bool)> = BTreeMap::new();
    for resource in credit["resources"].as_array().into_iter().flatten() {
        let expire_at = resource["expireAt"].as_i64();
        // Check the date at output time as well as the source flag: a resource
        // can expire while querying other accounts, after its flag was computed.
        if resource["expired"] == true || expire_at.is_some_and(|expiry| expiry <= as_of) {
            continue;
        }
        let Some(remaining) = resource["remaining"]
            .as_f64()
            .filter(|amount| amount.is_finite() && *amount > 0.0)
        else {
            continue;
        };
        let entry = buckets.entry(expire_at).or_default();
        entry.0 += remaining;
        entry.1 |= resource["expired"] == true;
        entry.2 |= resource["expiringSoon"] == true;
    }
    json!(buckets.into_iter().map(|(expire_at, (remaining, expired, expiring_soon))| {
        json!({"expireAt":expire_at,"expireAtUtc":utc(&json!(expire_at)),"expiryKnown":expire_at.is_some(),
            "remaining":remaining,"remainingDisplay":display_number(&json!(remaining)),"expired":expired,"expiringSoon":expiring_soon})
    }).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_preserves_raw_precision_and_does_not_turn_unknown_into_zero() {
        let raw = 1346.71000549;
        let mut finite = json!({"ok":true,"totalRemaining":raw,"totalCapacity":2000.00000004});
        decorate_credit(&mut finite);
        assert_eq!(finite["totalRemaining"], raw);
        assert_eq!(finite["totalRemainingDisplay"], "1346.71");
        assert_eq!(finite["totalCapacityDisplay"], "2000.00");
        assert_eq!(finite["balanceType"], "finite");
        let mut totals = json!({"totalRemaining":12895.840009579999,"totalCapacity":null,
            "finiteTotalRemaining":12895.840009579999,"finiteTotalCapacity":20000});
        decorate_totals(&mut totals);
        assert_eq!(totals["totalRemaining"], 12895.840009579999);
        assert_eq!(totals["totalRemainingDisplay"], "12895.84");
        assert!(totals["totalCapacityDisplay"].is_null());
        assert_eq!(totals["finiteTotalCapacityDisplay"], "20000.00");
        assert_eq!(display_number(&json!(-0.000001)), Some("0.00".into()));
        for mut credit in [
            json!({"ok":true,"unlimited":true,"totalRemaining":null}),
            json!({"ok":true,"totalRemaining":null}),
            json!({"ok":false,"totalRemaining":999}),
        ] {
            decorate_credit(&mut credit);
            assert!(credit["totalRemainingDisplay"].is_null());
            assert_ne!(credit["balanceType"], "finite");
        }
    }

    #[test]
    fn compact_inspect_keeps_decisions_failures_and_times_without_verbose_details() {
        let mut credit = json!({"ok":true,"totalRemaining":10.0000001,"totalCapacity":20,
            "updatedAt":100,"queryStartedAt":90,"queriedAt":110,"soonestExpireAt":2000,
            "resources":[{"packageName":"verbose","remaining":3,"expireAt":2000},
                {"remaining":4,"expireAt":2000},{"remaining":3,"expireAt":null},{"remaining":0,"expireAt":9999}]});
        decorate_credit(&mut credit);
        let value = json!({"schemaVersion":1,"command":"inspect","ok":false,"exitCode":3,"elapsedMs":100,
            "queryStartedAt":50,"queriedAt":120,"limitsScannedAt":115,"availabilityKnown":false,
            "clientStatus":{"configured":true,"activeAccountId":"a","activeAccountName":"测试","settingsPath":"verbose/path"},
            "clientStatusError":{"code":"AUTH_STATE_BUSY","retryable":true},"error":{"code":"AUTH_STATE_BUSY"},
            "accounts":[{"id":"a","nickname":"测试","displayField":"note","note":"测试","email":"verbose@example.com","activeCli":true,"credits":credit,
                "availabilityStatus":"unknown","unattributedLimitCount":1,
                "modelStates":[{"model":"hy3","state":"limited"}],
                "limited":[{"model":null,"scope":"unknown","resetAt":2000,"observedAt":10,"source":"multiple","sources":["local-log","client-hook"],"sourceClients":["codebuddy-cli"],"evidence":{"reason":"model missing"}}]},
                {"id":"b","nickname":"failed","activeCli":null,"needsRelogin":true,"needsReloginReason":"expired-refresh-token","isEnterprise":true,"credits":{"ok":false,"balanceType":"unknown","queriedAt":115,"error":{"code":"TIMEOUT","retryable":true}},"limited":[]}]});
        let projected = project_at(&value, 1000);
        assert_eq!(projected["exitCode"], 3);
        assert_eq!(projected["clientStatusError"]["retryable"], true);
        assert_eq!(projected["queryStartedAt"], 50);
        assert_eq!(projected["accounts"][0]["activeCli"], true);
        assert_eq!(projected["accounts"][0]["name"], "测试");
        assert_eq!(
            projected["accounts"][0]["credits"]["totalRemaining"],
            10.0000001
        );
        assert_eq!(
            projected["accounts"][0]["credits"]["totalRemainingDisplay"],
            "10.00"
        );
        let expiry = projected["accounts"][0]["credits"]["expiryBalances"]
            .as_array()
            .unwrap();
        assert_eq!(expiry.len(), 2);
        assert_eq!(expiry[0]["expiryKnown"], false);
        assert_eq!(expiry[1]["remaining"], 7.0);
        assert_eq!(projected["accounts"][0]["limited"][0]["model"], Value::Null);
        assert_eq!(projected["accounts"][0]["limited"][0]["scope"], "unknown");
        assert_eq!(
            projected["accounts"][0]["limited"][0]["sources"],
            json!(["local-log", "client-hook"])
        );
        assert_eq!(
            projected["accounts"][0]["limited"][0]["sourceClients"],
            json!(["codebuddy-cli"])
        );
        assert_eq!(projected["accounts"][0]["availabilityStatus"], "unknown");
        assert_eq!(projected["accounts"][0]["unattributedLimitCount"], 1);
        assert_eq!(
            projected["accounts"][0]["modelStates"][0]["state"],
            "limited"
        );
        assert_eq!(
            projected["accounts"][1]["credits"]["error"]["code"],
            "TIMEOUT"
        );
        assert!(projected["accounts"][1]["activeCli"].is_null());
        assert_eq!(projected["accounts"][1]["needsRelogin"], true);
        assert_eq!(
            projected["accounts"][1]["needsReloginReason"],
            "expired-refresh-token"
        );
        assert_eq!(projected["accounts"][1]["isEnterprise"], true);
        assert!(!projected.to_string().contains("verbose"));
        assert_eq!(
            value["accounts"][0]["email"], "verbose@example.com",
            "projection is non-mutating"
        );
    }

    #[test]
    fn compact_credits_keeps_account_identity_and_unlimited_and_interrupted_results() {
        let mut credit = json!({"accountId":"a","accountName":"Alpha","variant":"cn","ok":true,"unlimited":true,"totalRemaining":null,"totalCapacity":null,"resources":[]});
        decorate_credit(&mut credit);
        let value = json!({"command":"credits","ok":false,"exitCode":130,"operationOk":true,
            "error":{"code":"INTERRUPTED"},"summary":{"complete":true,"totalRemaining":null},"accounts":[credit]});
        let projected = project(&value);
        assert_eq!(projected["exitCode"], 130);
        assert_eq!(projected["operationOk"], true);
        assert_eq!(projected["accounts"][0]["accountId"], "a");
        assert_eq!(projected["accounts"][0]["balanceType"], "unlimited");
        assert!(projected["accounts"][0]["totalRemaining"].is_null());
        assert!(projected["accounts"][0]["totalRemainingDisplay"].is_null());
    }

    #[test]
    fn summary_size_does_not_scale_with_repeated_raw_resource_details() {
        let resources = (0..40).map(|_| json!({"remaining":1,"expireAt":2000,"packageName":"long-package-name".repeat(40)})).collect::<Vec<_>>();
        let mut credit =
            json!({"ok":true,"totalRemaining":40,"totalCapacity":50,"resources":resources});
        decorate_credit(&mut credit);
        let accounts = (0..8).map(|index| json!({"id":format!("id-{index}"),"nickname":"测试账号","activeCli":index==0,"credits":credit,"limited":[]})).collect::<Vec<_>>();
        let full =
            json!({"command":"inspect","clientStatus":{"configured":true},"accounts":accounts});
        let compact = project_at(&full, 1000).to_string();
        assert!(compact.len() < 8_000, "{} bytes", compact.len());
        assert!(compact.len() * 10 < full.to_string().len());
    }

    #[test]
    fn expired_details_are_hidden_at_output_time_without_truncating_future_or_unknown_balances() {
        let as_of = 10_000;
        let mut resources = vec![
            json!({"remaining":50,"expireAt":as_of-1,"expired":false}),
            json!({"remaining":40,"expireAt":as_of,"expired":false}),
            json!({"remaining":30,"expireAt":as_of+1,"expired":true}),
            json!({"remaining":20,"expireAt":null,"expired":true}),
            json!({"remaining":10,"expireAt":null,"expired":false}),
            json!({"remaining":0,"expireAt":as_of+1}),
        ];
        resources.extend(
            (1..=60)
                .map(|index| json!({"remaining":1,"expireAt":as_of+index*1000,"expired":false})),
        );
        let mut credit = json!({"ok":true,"totalRemaining":210,"totalCapacity":250,
            "soonestExpireAt":as_of-1,"resources":resources});
        decorate_credit(&mut credit);
        let original = credit.clone();
        for inspect in [false, true] {
            let full = if inspect {
                json!({"command":"inspect","accounts":[{"id":"a","credits":credit}]})
            } else {
                json!({"command":"credits","accounts":[credit]})
            };
            let compact = project_at(&full, as_of);
            let actual = if inspect {
                &compact["accounts"][0]["credits"]
            } else {
                &compact["accounts"][0]
            };
            assert_eq!(actual["totalRemaining"], 210);
            assert_eq!(actual["totalRemainingDisplay"], "210.00");
            assert_eq!(actual["expiryBalancesAsOf"], as_of);
            assert_eq!(actual["soonestExpireAt"], as_of + 1000);
            let balances = actual["expiryBalances"].as_array().unwrap();
            assert_eq!(
                balances.len(),
                61,
                "all sixty future expiry groups and the unknown group must remain"
            );
            assert_eq!(balances[0]["expiryKnown"], false);
            assert_eq!(balances[0]["remaining"], 10.0);
            assert!(balances.iter().all(|entry| entry["expired"] == false));
            assert!(balances
                .iter()
                .filter_map(|entry| entry["expireAt"].as_i64())
                .all(|expiry| expiry > as_of));
            assert_eq!(balances.last().unwrap()["expireAt"], as_of + 60_000);
        }
        assert_eq!(
            credit, original,
            "projection never changes the full query or stored resources"
        );
    }

    #[test]
    fn summary_has_no_upcoming_expiry_when_only_expired_or_unknown_resources_remain() {
        let credit = json!({"ok":true,"soonestExpireAt":500,"resources":[
            {"remaining":9,"expireAt":500,"expired":true},
            {"remaining":2,"expireAt":null}
        ]});
        let summary = compact_credit(&credit, false, 1000);
        assert!(summary["soonestExpireAt"].is_null());
        assert!(summary["soonestExpireAtUtc"].is_null());
        assert_eq!(summary["expiryBalances"].as_array().unwrap().len(), 1);
        assert_eq!(summary["expiryBalances"][0]["expiryKnown"], false);
        let only_expired = json!({"ok":true,"resources":[{"remaining":9,"expireAt":500}]});
        assert_eq!(
            compact_credit(&only_expired, false, 1000)["expiryBalances"],
            json!([])
        );
    }
}
