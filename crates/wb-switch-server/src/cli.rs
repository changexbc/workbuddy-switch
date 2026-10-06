//! Headless account commands. Credentials never appear in command output.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use wb_switch_core::modules::{
    account, codebuddy_ide_session, codebuddy_ide_session_sync, export_import, oauth, session,
    variant::WbVariant, vscode_ext, vscode_session, vscode_session_sync,
};

const HELP: &str = r#"wb-switch accounts list [--lite]
workbuddy-switch accounts add [--variant cn|ai] [--no-open]
workbuddy-switch accounts add --local [--variant cn|ai]
workbuddy-switch accounts add --file accounts.json
wb-switch credits <index|account-id|all> [--lite]
wb-switch switch [service] <index|account-id|soonest|richest> [--lite] [--copy true|false] [--syn true|false]
    [--overwrite true|false] [--restart true|false] [--share true|false]

服务: workbuddy, ide, cli, vscode, jetbrains；省略服务则依次切换全部五种服务。
index 从 1 开始，按账号库顺序编号；删除账号后可能变化，固定引用请用 id。
soonest 选择可用积分最快过期的账号；richest 选择可用积分最多的账号，先实时查询。
--lite 使用表格；accounts list --lite 实时查询积分，普通 list 只读取本地账号元数据。
CodeBuddy IDE 国内/国际版由目标账号决定；也可显式使用 codebuddy-cn-ide。
参数也接受 copy true / syn true 或 --copy=true。默认 copy/syn/overwrite/share=false，restart=true。
syn=true 处理全部关联会话；冲突默认跳过，overwrite=true 允许用源内容覆盖冲突。
copy=true 复制全部可复制会话；不接受逐会话选择。JetBrains 操作全部已安装插件的 IDE。
restart=true 自动关闭/重开客户端；CodeBuddy CLI 总会关闭运行中的 CLI，且不重开。
账号命令输出脱敏 JSON；切换输出完整操作报告。错误退出码 1，部分会话失败/跳过为 2。
原有 serve、status、version 命令继续可用。"#;

pub fn handles(args: &[String]) -> bool {
    matches!(
        args.iter()
            .skip(1)
            .find(|arg| *arg != "--lite")
            .map(String::as_str),
        Some("accounts" | "credits" | "switch" | "help" | "--help" | "-h")
    )
}

fn options(args: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].trim_start_matches("--");
        let (key, value) = if let Some((k, v)) = raw.split_once('=') {
            (k.to_owned(), v.to_owned())
        } else if matches!(raw, "local" | "no-open") {
            (raw.to_owned(), "true".to_owned())
        } else {
            i += 1;
            (
                raw.to_owned(),
                args.get(i)
                    .ok_or_else(|| format!("参数 {raw} 缺少值"))?
                    .clone(),
            )
        };
        if !allowed.contains(&key.as_str()) || result.insert(key.clone(), value).is_some() {
            return Err(format!("未知或重复参数: {key}"));
        }
        i += 1;
    }
    Ok(result)
}

fn boolean(opts: &BTreeMap<String, String>, key: &str, default: bool) -> Result<bool, String> {
    match opts.get(key).map(String::as_str) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(format!("{key} 必须为 true 或 false")),
    }
}

fn variant(opts: &BTreeMap<String, String>) -> Result<WbVariant, String> {
    match opts.get("variant").map(String::as_str) {
        None | Some("cn") => Ok(WbVariant::Cn),
        Some("ai") => Ok(WbVariant::Ai),
        _ => Err("variant 必须为 cn 或 ai".into()),
    }
}

pub async fn run(raw_args: &[String]) -> Result<(), String> {
    let lite = raw_args.iter().any(|arg| arg == "--lite");
    if raw_args.iter().filter(|arg| *arg == "--lite").count() > 1 {
        return Err("重复参数 --lite".into());
    }
    let args: Vec<String> = raw_args
        .iter()
        .filter(|arg| *arg != "--lite")
        .cloned()
        .collect();
    if matches!(args[1].as_str(), "help" | "--help" | "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if args[1] == "switch" {
        return switch(&args, lite).await;
    }
    if args[1] == "credits" {
        if args.len() != 3 {
            return Err("用法: wb-switch credits <index|account-id|all> [--lite]".into());
        }
        let accounts = account::load_accounts();
        let selected = if args[2] == "all" {
            accounts.clone()
        } else {
            vec![crate::cli_accounts::resolve(&accounts, &args[2])?.1]
        };
        let credits = crate::cli_accounts::query(&selected).await;
        let mut rows = crate::cli_accounts::rows(&selected, Some(&credits));
        for row in &mut rows {
            *row = crate::cli_accounts::add_index(row.clone());
        }
        crate::cli_accounts::print(&json!(rows), lite);
        if credits.iter().any(|c| c["ok"] != true) {
            std::process::exit(2);
        }
        return Ok(());
    }
    match args.get(2).map(String::as_str) {
        Some("list") if args.len() == 3 => {
            let accounts = account::load_accounts();
            let results = if lite {
                Some(crate::cli_accounts::query(&accounts).await)
            } else {
                None
            };
            crate::cli_accounts::print(
                &json!(crate::cli_accounts::rows(&accounts, results.as_deref())),
                lite,
            );
            if results.is_some_and(|items| items.iter().any(|c| c["ok"] != true)) {
                std::process::exit(2);
            }
            Ok(())
        }
        Some("add") => add(&args[3..], lite).await,
        _ => Err(format!("账号命令用法:\n{HELP}")),
    }
}

async fn add(args: &[String], lite: bool) -> Result<(), String> {
    let opts = options(args, &["variant", "local", "file", "no-open"])?;
    for key in ["local", "no-open"] {
        if opts.get(key).is_some_and(|value| value != "true") {
            return Err(format!("{key} 是无值开关，使用 --{key}"));
        }
    }
    let v = variant(&opts)?;
    if opts.contains_key("local") && opts.contains_key("file") {
        return Err("local 和 file 不能同时指定".into());
    }
    if let Some(path) = opts.get("file") {
        if opts.contains_key("variant") || opts.contains_key("no-open") {
            return Err("file 导入不接受 variant/no-open".into());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("读取导入文件失败: {e}"))?;
        let count = export_import::parse_accounts_json(&text)?.len();
        let result = export_import::import_accounts(&text, &(0..count).collect::<Vec<_>>())?;
        crate::cli_accounts::print(
            &json!({"imported":result.imported,"skipped":result.skipped,"overwritten":result.overwritten}),
            lite,
        );
        if result.skipped > 0 {
            std::process::exit(2);
        }
        return Ok(());
    }
    if opts.contains_key("local") {
        if opts.contains_key("no-open") {
            return Err("local 不接受 no-open".into());
        }
        crate::cli_accounts::print(
            &crate::cli_accounts::add_index(account::import_local(v)?),
            lite,
        );
        return Ok(());
    }
    let start = oauth::oauth_start(v).await?;
    let uri = start["verificationUri"]
        .as_str()
        .ok_or("登录响应缺少 URL")?;
    eprintln!("请打开以下地址完成登录: {uri}");
    if !opts.contains_key("no-open") {
        crate::open_browser_url(uri);
    }
    let id = start["loginId"].as_str().ok_or("登录响应缺少 loginId")?;
    loop {
        let result = oauth::oauth_poll(id).await;
        if result["done"] == true {
            if let Some(error) = result["error"].as_str() {
                return Err(error.into());
            }
            crate::cli_accounts::print(
                &crate::cli_accounts::add_index(result["result"].clone()),
                lite,
            );
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

fn selections(preview: &Value, overwrite: bool) -> (Vec<Value>, Vec<Value>) {
    let mut selected = Vec::new();
    let mut skipped = Vec::new();
    for group in preview["groups"].as_array().into_iter().flatten() {
        let modes = group["availableModes"].as_array();
        let mode = modes
            .and_then(|m| m.iter().find(|v| **v == "fastForward"))
            .or_else(|| {
                if overwrite {
                    modes.and_then(|m| m.iter().find(|v| **v == "overwrite"))
                } else {
                    None
                }
            });
        if let Some(mode) = mode.filter(|_| group["previewToken"].is_string()) {
            selected.push(json!({"groupId":group["groupId"],"previewToken":group["previewToken"],"mode":mode}));
        } else if group["verdict"] != "identical" {
            skipped.push(json!({"groupId":group["groupId"],"verdict":group["verdict"],"reason":group["reason"]}));
        }
    }
    (selected, skipped)
}

fn is_service(s: &str) -> bool {
    matches!(
        s,
        "workbuddy"
            | "ide"
            | "cli"
            | "vscode"
            | "jetbrains"
            | "codebuddy-ide"
            | "codebuddy-cli"
            | "codebuddy-cn-ide"
    )
}

async fn switch(args: &[String], lite: bool) -> Result<(), String> {
    let first = args.get(2).ok_or("缺少账号 index、id 或 soonest/richest")?;
    let (service, selector, start) = if is_service(first) {
        (
            Some(first.as_str()),
            args.get(3).ok_or("缺少目标账号")?.as_str(),
            4,
        )
    } else {
        (None, first.as_str(), 3)
    };
    let opts = options(
        &args[start..],
        &["copy", "syn", "overwrite", "restart", "share"],
    )?;
    // Validate all option values before any query or switch.
    for key in ["copy", "syn", "overwrite", "restart", "share"] {
        boolean(&opts, key, key == "restart")?;
    }
    if boolean(&opts, "overwrite", false)? && !boolean(&opts, "syn", false)? {
        return Err("overwrite true 需要 syn true".into());
    }
    if !boolean(&opts, "restart", true)?
        && (boolean(&opts, "copy", false)?
            || boolean(&opts, "syn", false)?
            || boolean(&opts, "share", false)?)
        && (service.is_none() || service == Some("workbuddy"))
    {
        return Err("WorkBuddy 会话操作需要 restart true".into());
    }
    let accounts = account::load_accounts();
    let mut selection_errors = Vec::new();
    let (index, acc) = if matches!(selector, "soonest" | "richest") {
        let results = crate::cli_accounts::query(&accounts).await;
        selection_errors = results.iter().filter(|r| r["ok"] != true).map(|r|
            json!({"accountId":r["accountId"],"error":r["error"].as_str().unwrap_or("积分查询失败")})
        ).collect();
        let refreshed: Vec<_> = accounts
            .iter()
            .map(|a| {
                a["id"]
                    .as_str()
                    .and_then(account::find_account)
                    .unwrap_or_else(|| a.clone())
            })
            .collect();
        let i = crate::cli_accounts::choose(
            &refreshed,
            &results,
            selector,
            crate::cli_accounts::now(),
        )?;
        (i, refreshed[i].clone())
    } else {
        crate::cli_accounts::resolve(&accounts, selector)?
    };
    let id = acc["id"].as_str().ok_or("目标账号缺少 id")?;
    let reports = dispatch(service, &opts, |target, target_opts| {
        let id = id.to_owned();
        async move { switch_one(&target, &id, &target_opts).await }
    })
    .await?;
    let report = json!({"index":index + 1,"accountId":id,"strategy":if matches!(selector,"soonest"|"richest") {Some(selector)} else {None},"selectionErrors":selection_errors,"results":reports});
    crate::cli_accounts::print(&report, lite);
    if has_issues(&report) {
        std::process::exit(2);
    }
    Ok(())
}

async fn dispatch<F, Fut>(
    service: Option<&str>,
    opts: &BTreeMap<String, String>,
    mut execute: F,
) -> Result<Vec<Value>, String>
where
    F: FnMut(String, BTreeMap<String, String>) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let targets: Vec<&str> = service
        .map(|s| vec![s])
        .unwrap_or_else(|| vec!["workbuddy", "ide", "cli", "vscode", "jetbrains"]);
    let mut reports = Vec::new();
    for target in targets {
        let mut target_opts = opts.clone();
        if service.is_none() {
            if matches!(target, "cli" | "jetbrains") {
                for k in ["copy", "syn", "overwrite", "share"] {
                    target_opts.remove(k);
                }
            }
            if target != "workbuddy" {
                target_opts.remove("share");
            }
            if target == "cli" {
                target_opts.remove("restart");
            }
        }
        match execute(target.to_owned(), target_opts).await {
            Ok(result) => reports.push(json!({"service":target,"result":result})),
            Err(error) => {
                if service.is_some() {
                    return Err(error);
                }
                reports.push(json!({"service":target,"error":error}));
            }
        }
    }
    Ok(reports)
}

async fn switch_one(
    service: &str,
    id: &str,
    opts: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let service = match service {
        "ide" => "codebuddy-ide",
        "cli" => "codebuddy-cli",
        s => s,
    };
    let copy = boolean(&opts, "copy", false)?;
    let syn = boolean(&opts, "syn", false)?;
    let overwrite = boolean(&opts, "overwrite", false)?;
    let restart = boolean(&opts, "restart", true)?;
    let share = boolean(&opts, "share", false)?;
    let acc =
        account::find_account(id).ok_or("账号不存在；请使用 accounts list 中的 index 或 id")?;
    let v = account::variant_of(&acc);
    let service = match service {
        "codebuddy-ide" if v == WbVariant::Cn => "codebuddy-cn-ide",
        "workbuddy" | "codebuddy-ide" | "codebuddy-cn-ide" | "codebuddy-cli" | "vscode"
        | "jetbrains" => service,
        _ => return Err("未知服务；运行 --help 查看服务列表".into()),
    };
    if overwrite && !syn {
        return Err("overwrite true 需要 syn true".into());
    }
    if service != "workbuddy" && share {
        return Err("share 仅支持 WorkBuddy".into());
    }
    if matches!(service, "codebuddy-cli" | "jetbrains") && (copy || syn) {
        return Err("该服务不支持会话复制/同步".into());
    }
    if service == "codebuddy-cli" && opts.contains_key("restart") {
        return Err("CodeBuddy CLI 不支持 restart 参数".into());
    }
    if service == "workbuddy" && !restart && (copy || syn || share) {
        return Err("WorkBuddy 会话操作需要 restart true".into());
    }
    let mut body = json!({"accountId":id,"restart":restart,"shareSessions":share});
    let mut copy_scan_skipped = 0;
    if copy {
        let list = match service {
            "workbuddy" => {
                let uid = session::current_user_uid(v)
                    .ok_or("未检测到当前 WorkBuddy 登录账号，无法复制会话")?;
                session::list_sessions_for_user(v, &uid)
            }
            "codebuddy-cn-ide" => codebuddy_ide_session::list_current_codebuddy_ide_sessions(),
            "codebuddy-ide" => codebuddy_ide_session::list_current_intl_ide_sessions(),
            _ => vscode_ext::active_ext_uid()
                .map(|uid| vscode_session::list_vscode_sessions(&uid))
                .unwrap_or(json!({"sessions":[]})),
        };
        if service != "workbuddy" {
            if !list["sourceUid"].is_string() {
                return Err("未检测到当前客户端登录账号，无法复制会话".into());
            }
            if list["dataRoot"].is_null() {
                return Err("未找到当前客户端会话数据目录，无法复制会话".into());
            }
            copy_scan_skipped = list["skipped"].as_u64().unwrap_or(0);
        }
        let items = if service == "workbuddy" {
            &list
        } else {
            &list["sessions"]
        };
        let copies: Vec<Value> = items
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                if service == "workbuddy" {
                    s["id"].clone()
                } else {
                    json!({"workspaceHash":s["workspaceHash"],"conversationId":s["id"]})
                }
            })
            .collect();
        body[if service == "workbuddy" {
            "copySessionIds"
        } else {
            "copySessions"
        }] = json!(copies);
    }
    let mut skipped = Vec::new();
    if syn {
        let preview = match service {
            "workbuddy" => session::session_links_preview(v, &acc)?,
            "codebuddy-cn-ide" => codebuddy_ide_session_sync::links_preview(&acc)?,
            "codebuddy-ide" => codebuddy_ide_session_sync::links_preview_intl(&acc)?,
            _ => vscode_session_sync::links_preview(&acc)?,
        };
        if preview["supported"] == false
            || preview["storeStatus"] == "unsupported"
            || preview["storeStatus"] == "unavailable"
        {
            return Err("当前客户端会话同步不可用".into());
        }
        let (selected, omitted) = selections(&preview, overwrite);
        body["syncSelections"] = json!(selected);
        skipped = omitted;
    }
    let mut result = crate::api::cli_switch(service, body).await?;
    result["cliSkippedSync"] = json!(skipped);
    result["cliCopyScan"] = json!({"skipped":copy_scan_skipped});
    Ok(result)
}

pub(crate) fn has_issues(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, v)| {
            (matches!(
                key.as_str(),
                "error" | "errors" | "skipped" | "cliSkippedSync"
            ) && (v.as_str().is_some_and(|s| !s.is_empty())
                || v.as_array().is_some_and(|a| !a.is_empty())
                || v.as_u64().is_some_and(|n| n > 0)))
                || (key == "needsRecovery" && *v == true)
                || has_issues(v)
        }),
        Value::Array(items) => items.iter().any(has_issues),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn all_services_continue_after_failure_and_scope_options() {
        let opts = BTreeMap::from([
            ("copy".into(), "true".into()),
            ("syn".into(), "true".into()),
            ("share".into(), "true".into()),
            ("restart".into(), "false".into()),
        ]);
        let reports = dispatch(None, &opts, |service, options| async move {
            if matches!(service.as_str(), "cli" | "jetbrains") {
                assert!(!options.contains_key("copy"));
                assert!(!options.contains_key("syn"));
            }
            if service != "workbuddy" {
                assert!(!options.contains_key("share"));
            }
            if service == "cli" {
                assert!(!options.contains_key("restart"));
            }
            if service == "ide" {
                Err("IDE unavailable".into())
            } else {
                Ok(json!({"ok":true}))
            }
        })
        .await
        .unwrap();
        assert_eq!(
            reports
                .iter()
                .map(|r| r["service"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["workbuddy", "ide", "cli", "vscode", "jetbrains"]
        );
        assert_eq!(reports[1]["error"], "IDE unavailable");
        assert!(has_issues(&json!({"results":reports})));
        assert!(
            dispatch(Some("ide"), &opts, |_, _| async { Err("failed".into()) })
                .await
                .is_err()
        );
    }
    #[test]
    fn strict_options_and_boolean_forms() {
        let args = ["copy", "true", "--syn=false", "--restart", "false"].map(String::from);
        let opts = options(&args, &["copy", "syn", "restart"]).unwrap();
        assert!(boolean(&opts, "copy", false).unwrap());
        assert!(!boolean(&opts, "syn", true).unwrap());
        assert!(!boolean(&opts, "restart", true).unwrap());
        assert!(options(&["--copy".into()], &["copy"]).is_err());
        assert!(options(&["copy=true".into(), "copy=false".into()], &["copy"]).is_err());
        assert!(boolean(
            &BTreeMap::from([("copy".into(), "yes".into())]),
            "copy",
            false
        )
        .is_err());
    }
    #[test]
    fn all_sync_preserves_preview_permissions_and_reports_conflicts() {
        let preview = json!({"groups":[
            {"groupId":"a","previewToken":"p","availableModes":["fastForward"],"verdict":"fastForward"},
            {"groupId":"b","previewToken":"q","availableModes":["overwrite"],"verdict":"diverge"},
            {"groupId":"c","availableModes":[],"verdict":"unknown"},
            {"groupId":"d","availableModes":[],"verdict":"identical"}
        ]});
        let (selected, skipped) = selections(&preview, false);
        assert_eq!(selected.len(), 1);
        assert_eq!(skipped.len(), 2);
        assert_eq!(selections(&preview, true).0.len(), 2);
        assert!(has_issues(
            &json!({"sessionCopy":{"errors":[{"error":"failed"}]}})
        ));
        assert!(!has_issues(
            &json!({"sessionCopy":{"errors":[],"copied":[{}]}})
        ));
    }
}
