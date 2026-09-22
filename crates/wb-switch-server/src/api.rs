//! HTTP API 层：把 wb-switch-core 暴露为本地 REST 接口，供 webui（浏览器）调用。
//!
//! 路由设计对应 Python 版 server.py 与桌面端 commands.rs。仅绑定 127.0.0.1，
//! token 不出本机。

use std::collections::HashMap;
use std::sync::Mutex;
#[cfg(target_os = "windows")]
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Query, RawQuery};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::{json, Value};

use wb_switch_core::modules::{
    account, auth_file, checkin, codebuddy_cli, codebuddy_cn_ide, codebuddy_ide, config,
    credit_usage, credits, export_import, limits, notifications, oauth, process, rate_limit_events,
    rate_limit_hook, refresh, rotate, session, switch, token_stats, travel, update,
    variant::WbVariant, vscode_ext, vscode_session, vscode_session_sync,
};

/// WorkBuddy 运行状态缓存：Windows 上检测要跑 tasklist（慢），缓存几秒避免
/// 前端切 tab 频繁触发命令行导致卡顿/闪窗。按档位分别缓存。
#[cfg(target_os = "windows")]
static RUNNING_CACHE: Mutex<Option<(Instant, bool, WbVariant)>> = Mutex::new(None);

fn cached_workbuddy_running(variant: WbVariant) -> bool {
    #[cfg(target_os = "windows")]
    {
        let mut cache = RUNNING_CACHE.lock().unwrap();
        if let Some((t, v, cached_variant)) = cache.as_ref() {
            if *cached_variant == variant && t.elapsed() < Duration::from_secs(3) {
                return *v;
            }
        }
        let v = process::is_workbuddy_running(variant);
        *cache = Some((Instant::now(), v, variant));
        v
    }
    #[cfg(not(target_os = "windows"))]
    {
        process::is_workbuddy_running(variant)
    }
}

#[derive(RustEmbed)]
#[folder = "../../dist/"]
struct Assets;

/// 切换进度缓存：webui 通过 GET /api/switch/progress 轮询。
static SWITCH_PROGRESS: Mutex<Option<String>> = Mutex::new(None);
static SWITCH_RUNNING: Mutex<bool> = Mutex::new(false);

pub fn router() -> Router {
    Router::new()
        .route("/api/status", get(api_status))
        .route("/api/accounts", get(api_accounts))
        .route("/api/codebuddy-cli/status", get(api_codebuddy_cli_status))
        .route(
            "/api/codebuddy-cli/install-helper",
            post(api_codebuddy_cli_install_helper),
        )
        .route("/api/codebuddy-cli/switch", post(api_codebuddy_cli_switch))
        .route(
            "/api/codebuddy-cn-ide/status",
            get(api_codebuddy_cn_ide_status),
        )
        .route(
            "/api/codebuddy-cn-ide/switch",
            post(api_codebuddy_cn_ide_switch),
        )
        .route(
            "/api/codebuddy-cn-ide/detect",
            post(api_codebuddy_cn_ide_detect),
        )
        .route("/api/codebuddy-ide/status", get(api_codebuddy_ide_status))
        .route("/api/codebuddy-ide/switch", post(api_codebuddy_ide_switch))
        .route("/api/codebuddy-ide/detect", post(api_codebuddy_ide_detect))
        .route("/api/vscode-ext/status", get(api_vscode_ext_status))
        .route("/api/vscode-ext/sessions", get(api_vscode_ext_sessions))
        .route("/api/vscode-ext/switch", post(api_vscode_ext_switch))
        .route("/api/vscode-ext/detect", post(api_vscode_ext_detect))
        .route(
            "/api/vscode-ext/session-links",
            post(api_vscode_ext_session_links_preview),
        )
        .route("/api/delete", post(api_delete))
        .route("/api/oauth/start", post(api_oauth_start))
        .route("/api/oauth/status", post(api_oauth_status))
        .route("/api/import-local", post(api_import_local))
        .route("/api/export-accounts", post(api_export_accounts))
        .route(
            "/api/export-accounts-to-path",
            post(api_export_accounts_to_path),
        )
        .route("/api/import/preview", post(api_preview_import))
        .route("/api/import", post(api_import))
        .route("/api/switch", post(api_switch))
        .route("/api/switch/progress", get(api_switch_progress))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/copy", post(api_copy_sessions))
        .route(
            "/api/session-links/preview",
            post(api_session_links_preview),
        )
        .route("/api/checkin/status", get(api_checkin_status))
        .route("/api/credits", post(api_credits))
        .route("/api/credits/stats", get(api_credit_statistics))
        .route("/api/token-stats", get(api_token_statistics))
        .route("/api/rate-limits", get(api_rate_limits))
        .route(
            "/api/rate-limits/hook-status",
            get(api_rate_limit_hook_status),
        )
        .route(
            "/api/rate-limits/install-hook",
            post(api_install_rate_limit_hook),
        )
        .route(
            "/api/rate-limits/uninstall-hook",
            post(api_uninstall_rate_limit_hook),
        )
        .route(
            "/api/rate-limits/config",
            get(api_rate_limit_config).post(api_save_rate_limit_config),
        )
        .route("/api/checkin", post(api_checkin))
        .route("/api/checkin/all", post(api_checkin_all))
        .route(
            "/api/checkin/config",
            get(api_checkin_config).post(api_save_checkin_config),
        )
        .route("/api/checkin/logs", get(api_checkin_logs))
        .route("/api/notifications", get(api_notifications))
        .route("/api/notifications/record", post(api_record_notification))
        .route("/api/notifications/clear", post(api_clear_notifications))
        .route("/api/travel/status", get(api_travel_status))
        .route(
            "/api/travel/config",
            get(api_travel_config).post(api_save_travel_config),
        )
        .route(
            "/api/rotate/config",
            get(api_rotate_config).post(api_save_rotate_config),
        )
        .route("/api/rotate/status", get(api_rotate_status))
        .route("/api/rotate/run", post(api_rotate_run))
        .route("/api/rotate/logs", get(api_rotate_logs))
        .route("/api/refresh-token", post(api_refresh_token))
        .route("/api/update/check", get(api_update_check))
        .route(
            "/api/update/config",
            get(api_update_config).post(api_save_update_config),
        )
        .fallback(static_handler)
}

fn json_ok(v: Value) -> Response {
    Json(v).into_response()
}

fn json_err(e: String, code: StatusCode) -> Response {
    (code, Json(json!({ "ok": false, "error": e }))).into_response()
}

/// 从 query string 解析档位（缺省国内版）。与 Tauri 命令的可选 `variant` 参数同义。
fn query_variant(query: Option<&str>) -> WbVariant {
    let raw = query.unwrap_or("").split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == "variant").then_some(value)
    });
    WbVariant::parse(raw)
}

/// 从请求体解析档位（缺省国内版）。与 Tauri 命令的可选 `variant` 参数同义。
fn body_variant(body: &Value) -> WbVariant {
    WbVariant::parse(body.get("variant").and_then(Value::as_str))
}

// ---------------------------------------------------------------------------
// 状态 / 账号
// ---------------------------------------------------------------------------

async fn api_status(RawQuery(query): RawQuery) -> Response {
    let variant = query_variant(query.as_deref());
    let auth = auth_file::read_auth_file(variant);
    let current = auth.as_ref().and_then(|a| {
        let acct = a.get("account").cloned().unwrap_or_else(|| json!({}));
        Some(json!({
            "uid": account::display_value(&acct, "uid"),
            "nickname": account::display_value(&acct, "nickname"),
            "email": account::display_value(&acct, "email"),
        }))
    });
    json_ok(json!({
        "running": cached_workbuddy_running(variant),
        "authFile": auth_file::auth_file_path(variant).to_string_lossy(),
        "current": current,
        "appPath": auth_file::workbuddy_app_path(variant).to_string_lossy(),
        "version": update::APP_VERSION,
        "variant": variant.as_str(),
    }))
}

/// GET /api/accounts —— 返回全部档位的账号，`current` 取请求档位的登录态。
async fn api_accounts(RawQuery(query): RawQuery) -> Response {
    let variant = query_variant(query.as_deref());
    json_ok(json!({
        "accounts": account::load_accounts()
            .iter()
            .map(account::account_meta)
            .collect::<Vec<_>>(),
        "current": auth_file::read_auth_file(variant)
            .and_then(|a| a.get("account").and_then(|x| x.get("uid")).and_then(|x| x.as_str()).map(String::from)),
        "variant": variant.as_str(),
    }))
}

async fn api_codebuddy_cli_status() -> Response {
    json_ok(codebuddy_cli::status())
}

async fn api_codebuddy_cli_install_helper() -> Response {
    match codebuddy_cli::install_helper() {
        Ok(result) => json_ok(result),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}

async fn api_codebuddy_cli_switch(Json(body): Json<Value>) -> Response {
    let id = body.get("accountId").and_then(|v| v.as_str()).unwrap_or("");
    // 入参 `closeRunningCli` 已废弃：后端一律先关闭正在运行的 CLI 再写状态，忽略该值。
    // 无头模式不投递系统通知，切号结果（含关闭数量）照常返回给调用方。
    match codebuddy_cli::switch_active_account(id) {
        Ok(result) => json_ok(result),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}

async fn api_codebuddy_cn_ide_status() -> Response {
    json_ok(codebuddy_cn_ide::status())
}

async fn api_codebuddy_cn_ide_switch(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .or_else(|| body.get("account_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let restart = body
        .get("restart")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match codebuddy_cn_ide::switch_account(account_id, restart) {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_codebuddy_cn_ide_detect() -> Response {
    match codebuddy_cn_ide::detect_current_account() {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_vscode_ext_status() -> Response {
    json_ok(vscode_ext::status())
}

/// GET /api/vscode-ext/sessions —— 当前 VS Code 扩展账号可复制的会话（未登录返回空列表）。
async fn api_vscode_ext_sessions() -> Response {
    let result = tokio::task::spawn_blocking(|| match vscode_ext::active_ext_uid() {
        Some(uid) => vscode_session::list_vscode_sessions(&uid),
        None => json!({ "sourceUid": null, "sessions": [], "skipped": 0 }),
    })
    .await;
    match result {
        Ok(value) => json_ok(value),
        Err(error) => json_err(error.to_string(), StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn api_vscode_ext_switch(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .or_else(|| body.get("account_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // 默认重启（= 自动关闭并重开）：VS Code 运行时由后端先优雅退出再写入。
    // 显式传 restart=false 时退回「请先完全退出 VS Code」的手动模式。
    let restart = body.get("restart").and_then(|v| v.as_bool()).unwrap_or(true);
    // 可选：切换前把勾选会话复制到目标账号（与 /api/vscode-ext/* 命名风格一致）。
    // 任一条目非法即整包拒绝（与 Tauri 侧 `Option<Vec<CopyItem>>` 的 serde 整包报错同形），
    // 避免「部分成功 + 静默丢弃」让用户误以为全部复制成功。
    let copy_items: Vec<vscode_session::CopyItem> = match body
        .get("copySessions")
        .and_then(|v| v.as_array())
        .map(|array| {
            array
                .iter()
                .map(|item| serde_json::from_value::<vscode_session::CopyItem>(item.clone()))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
    {
        Ok(items) => items.unwrap_or_default(),
        Err(error) => {
            return json_err(
                format!("copySessions 条目非法：{error}"),
                StatusCode::BAD_REQUEST,
            )
        }
    };

    // 同步选择与桌面端同形（[{groupId, previewToken, mode}]），形状由 core 校验。
    let sync_selections = match session::parse_sync_selections(body.get("syncSelections")) {
        Ok(selections) => selections,
        Err(error) => return json_err(error, StatusCode::BAD_REQUEST),
    };

    let result = if copy_items.is_empty() && sync_selections.is_empty() {
        vscode_ext::switch_account(account_id, restart)
    } else {
        vscode_session::switch_vscode_ext_with_copy(
            account_id,
            restart,
            &copy_items,
            &sync_selections,
        )
    };
    match result {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/vscode-ext/session-links —— 预览当前插件账号 → 目标账号的关联会话同步项。
///
/// 与桌面端 `vscode_session_links_preview` 同形：直接返回 core 的只读预览
/// （`supported` / `storeStatus` / `groups`），每组的 `defaultChecked` 与 `availableModes`
/// 是前端的勾选权限来源。
async fn api_vscode_ext_session_links_preview(Json(body): Json<Value>) -> Response {
    let target_account_id = body
        .get("targetAccountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if target_account_id.trim().is_empty() {
        return json_err("缺少 targetAccountId".to_string(), StatusCode::BAD_REQUEST);
    }
    let result = tokio::task::spawn_blocking(move || {
        let target = account::find_account(&target_account_id).ok_or("目标账号不存在")?;
        vscode_session_sync::links_preview(&target)
    })
    .await;
    match result {
        Ok(Ok(value)) => json_ok(value),
        Ok(Err(error)) => json_err(error, StatusCode::BAD_REQUEST),
        Err(error) => json_err(error.to_string(), StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn api_codebuddy_ide_status() -> Response {
    json_ok(codebuddy_ide::status())
}

async fn api_codebuddy_ide_switch(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .or_else(|| body.get("account_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let restart = body
        .get("restart")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match codebuddy_ide::switch_account(account_id, restart) {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_vscode_ext_detect() -> Response {
    match vscode_ext::detect_current_account() {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_codebuddy_ide_detect() -> Response {
    match codebuddy_ide::detect_current_account() {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}



async fn api_delete(Json(body): Json<Value>) -> Response {
    let id = body.get("accountId").and_then(|v| v.as_str()).unwrap_or("");
    match account::delete_account(id) {
        Ok(()) => json_ok(json!({ "ok": true })),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/import-local —— 导入本机当前账号（body 可选 `variant`，缺省国内版）。
///
/// body 允许缺失，保持改造前的调用方式可用。
async fn api_import_local(body: Option<Json<Value>>) -> Response {
    let variant = body
        .as_ref()
        .map(|Json(value)| body_variant(value))
        .unwrap_or_else(|| WbVariant::parse(None));
    match account::import_local(variant) {
        Ok(acc) => json_ok(json!({ "ok": true, "account": acc })),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

// ---------------------------------------------------------------------------
// 导出 / 导入账号
// ---------------------------------------------------------------------------

async fn api_export_accounts(Json(body): Json<Value>) -> Response {
    let ids: Vec<String> = body
        .get("accountIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    match export_import::export_accounts(&ids) {
        Ok(records) => json_ok(json!({ "ok": true, "accounts": records })),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_export_accounts_to_path(Json(body): Json<Value>) -> Response {
    let ids: Vec<String> = body
        .get("accountIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let path = body
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match export_import::export_accounts_to_path(&ids, &path) {
        Ok(path) => json_ok(json!({ "ok": true, "path": path })),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_preview_import(Json(body): Json<Value>) -> Response {
    let text = body
        .get("fileText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match export_import::preview_accounts(&text) {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

async fn api_import(Json(body): Json<Value>) -> Response {
    let text = body
        .get("fileText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let indexes: Vec<usize> = body
        .get("indexes")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64().map(|n| n as usize))
                .collect()
        })
        .unwrap_or_default();
    match export_import::import_accounts(&text, &indexes) {
        Ok(result) => json_ok(json!({
            "ok": true,
            "imported": result.imported,
            "skipped": result.skipped,
            "overwritten": result.overwritten,
        })),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

// ---------------------------------------------------------------------------
// OAuth 登录
// ---------------------------------------------------------------------------

/// POST /api/oauth/start —— 发起扫码登录（body 可选 `variant`，缺省国内版）。
async fn api_oauth_start(body: Option<Json<Value>>) -> Response {
    let variant = body
        .as_ref()
        .map(|Json(value)| body_variant(value))
        .unwrap_or_else(|| WbVariant::parse(None));
    match oauth::oauth_start(variant).await {
        Ok(v) => json_ok(v),
        Err(e) => json_err(e, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/oauth/status —— 轮询采集结果（档位取发起时记录，无需传参）。
async fn api_oauth_status(Json(body): Json<Value>) -> Response {
    let login_id = body
        .get("loginId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json_ok(oauth::oauth_poll(&login_id).await)
}

// ---------------------------------------------------------------------------
// 切换
// ---------------------------------------------------------------------------

async fn api_switch(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if account_id.trim().is_empty() {
        return json_err("缺少 accountId".to_string(), StatusCode::BAD_REQUEST);
    }
    let restart = body
        .get("restart")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let share_sessions = body
        .get("shareSessions")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let copy_ids: Vec<String> = body
        .get("copySessionIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // 同步选择与桌面端同形（[{groupId, previewToken, mode}]），形状由 core 校验。
    let sync_selections = match session::parse_sync_selections(body.get("syncSelections")) {
        Ok(selections) => selections,
        Err(error) => return json_err(error, StatusCode::BAD_REQUEST),
    };

    {
        let mut running = SWITCH_RUNNING.lock().unwrap();
        if *running {
            return json_err("已有切换任务进行中".to_string(), StatusCode::CONFLICT);
        }
        *running = true;
        *SWITCH_PROGRESS.lock().unwrap() = Some("开始切换账号…".to_string());
    }

    let progress: switch::ProgressFn = Box::new(|msg| {
        *SWITCH_PROGRESS.lock().unwrap() = Some(msg.to_string());
    });

    let result = tokio::task::spawn_blocking(move || {
        switch::switch_account(
            Some(&progress),
            &account_id,
            restart,
            share_sessions,
            &copy_ids,
            &sync_selections,
        )
    })
    .await;

    *SWITCH_RUNNING.lock().unwrap() = false;

    match result {
        Ok(Ok(v)) => json_ok(v),
        Ok(Err(e)) => json_err(e, StatusCode::BAD_REQUEST),
        Err(e) => json_err(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn api_switch_progress() -> Response {
    let p = SWITCH_PROGRESS.lock().unwrap().clone();
    let running = *SWITCH_RUNNING.lock().unwrap();
    json_ok(json!({ "running": running, "progress": p }))
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

/// GET /api/sessions —— 当前账号的会话列表（query 可选 `variant`，缺省国内版）。
async fn api_sessions(RawQuery(query): RawQuery) -> Response {
    let variant = query_variant(query.as_deref());
    match session::current_user_uid(variant) {
        Some(uid) => json_ok(json!({
            "sessions": session::list_sessions_for_user(variant, &uid),
            "current": uid,
            "variant": variant.as_str(),
        })),
        None => json_ok(json!({ "sessions": [], "current": null, "variant": variant.as_str() })),
    }
}

async fn api_copy_sessions(Json(body): Json<Value>) -> Response {
    let target_account_id = body
        .get("targetAccountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let session_ids: Vec<String> = body
        .get("sessionIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let Some(target) = account::find_account(&target_account_id) else {
        return json_err("目标账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    // 档位取目标账号自身（源 uid 也从该档位的登录态读）。
    let variant = account::variant_of(&target);
    // 与桌面端同形：直接返回 core 的复制报告（copied / alreadyLinked / errors / needsRecovery）。
    let mut report = match session::copy_sessions_for_switch(&target, &session_ids) {
        Ok(report) => report,
        Err(error) => {
            return json_err(error, StatusCode::BAD_REQUEST);
        }
    };
    report["variant"] = json!(variant.as_str());
    json_ok(report)
}

/// POST /api/session-links/preview —— 预览当前账号 → 目标账号的关联会话同步项。
///
/// 与桌面端 `session_links_preview` 同形：直接返回 core 的只读预览（`supported` /
/// `storeStatus` / `groups`），每组的 `defaultChecked` 与 `availableModes` 是前端的
/// 勾选权限来源。`variant` 缺省取目标账号自身档位。
async fn api_session_links_preview(Json(body): Json<Value>) -> Response {
    let target_account_id = body
        .get("targetAccountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if target_account_id.trim().is_empty() {
        return json_err("缺少 targetAccountId".to_string(), StatusCode::BAD_REQUEST);
    }
    let Some(target) = account::find_account(&target_account_id) else {
        return json_err("目标账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    let variant = match body.get("variant").and_then(Value::as_str) {
        Some(raw) => WbVariant::parse(Some(raw)),
        None => account::variant_of(&target),
    };
    match session::session_links_preview(variant, &target) {
        Ok(report) => json_ok(report),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}

// ---------------------------------------------------------------------------
// 签到 / 保活
// ---------------------------------------------------------------------------

/// GET /api/checkin/status —— 传 accountId 时只查询该账号；缺省保留旧批量响应。
/// 两种形式都遵守单账号自动签到开关，避免展示状态时触发已关闭账号的请求。
async fn api_checkin_status(Query(query): Query<HashMap<String, String>>) -> Response {
    if let Some(id) = query.get("accountId") {
        let Some(acc) = account::find_account(id) else {
            return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
        };
        let status = checkin::get_checkin_status_for_display(&acc).await;
        return json_ok(checkin_status_item(&acc, status));
    }
    let list = account::load_accounts();
    let mut items = Vec::new();
    for acc in &list {
        let status = checkin::get_checkin_status_for_display(acc).await;
        items.push(checkin_status_item(acc, status));
    }
    json_ok(json!({ "accounts": items }))
}

fn checkin_status_item(account: &Value, mut status: Value) -> Value {
    status["accountId"] = account.get("id").cloned().unwrap_or(Value::Null);
    status["email"] = json!(account::account_display_name(account));
    status["variant"] = json!(account::variant_of(account).as_str());
    status
}

async fn api_credits(Json(body): Json<Value>) -> Response {
    let id = body.get("accountId").and_then(|v| v.as_str()).unwrap_or("");
    let Some(acc) = account::find_account(id) else {
        return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    json_ok(credits::get_credit_expiry(&acc).await)
}

fn query_flag_enabled(query: Option<&str>, name: &str) -> bool {
    query.unwrap_or("").split('&').any(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, "true"));
        key == name && matches!(value, "" | "1" | "true" | "yes")
    })
}

async fn api_credit_statistics(RawQuery(query): RawQuery) -> Response {
    json_ok(credit_usage::get_statistics(query_flag_enabled(query.as_deref(), "refresh")).await)
}

async fn api_token_statistics(RawQuery(query): RawQuery) -> Response {
    let days = query.as_deref().and_then(|value| {
        value
            .split('&')
            .find_map(|part| part.strip_prefix("days=")?.parse::<i64>().ok())
    });
    match tokio::task::spawn_blocking(move || token_stats::get_statistics(days)).await {
        Ok(statistics) => json_ok(statistics),
        Err(error) => json_err(
            format!("扫描 Token 统计失败: {error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// GET /api/rate-limits —— 模型限额台账（全部账号当前受限的模型与官方恢复时刻）。
///
/// 扫描本机日志文件，放 blocking 线程避免占用运行时线程；无受限模型时返回空数组。
async fn api_rate_limits() -> Response {
    match tokio::task::spawn_blocking(limits::get_rate_limits).await {
        Ok(payload) => json_ok(payload),
        Err(error) => json_err(
            format!("扫描模型限额失败: {error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// hook 状态 + 运行期字段（最近一次 hook 事件时刻）。
fn rate_limit_hook_status() -> Value {
    let mut status = rate_limit_hook::hook_status();
    status["lastEventAt"] = json!(rate_limit_events::last_event_at());
    status
}

/// GET /api/rate-limits/hook-status —— hook 安装状态（脚本 + 三处客户端配置逐项结果）。
async fn api_rate_limit_hook_status() -> Response {
    match tokio::task::spawn_blocking(rate_limit_hook_status).await {
        Ok(status) => json_ok(status),
        Err(error) => json_err(
            format!("查询限额 hook 状态失败: {error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// POST /api/rate-limits/install-hook —— 安装 hook（幂等，写前备份；同时清除「卸载过」标记）。
async fn api_install_rate_limit_hook() -> Response {
    match tokio::task::spawn_blocking(|| {
        let result = rate_limit_hook::install_hook();
        // 扫描范围随安装结果变化（只对未注册的来源扫日志），缓存必须作废。
        limits::invalidate_scan_cache();
        result.map(|_| rate_limit_hook_status())
    })
    .await
    {
        Ok(Ok(status)) => json_ok(status),
        Ok(Err(error)) => json_err(error, StatusCode::BAD_REQUEST),
        Err(error) => json_err(error.to_string(), StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// POST /api/rate-limits/uninstall-hook —— 卸载 hook（移除注册条目，尽量逐字节还原）。
///
/// 卸载即用户拒绝自动接入（`hookOptOut`），与安装逻辑同处 core，两个宿主共用同一语义。
async fn api_uninstall_rate_limit_hook() -> Response {
    match tokio::task::spawn_blocking(|| {
        let result = rate_limit_hook::uninstall_hook();
        limits::invalidate_scan_cache();
        result.map(|_| rate_limit_hook_status())
    })
    .await
    {
        Ok(Ok(status)) => json_ok(status),
        Ok(Err(error)) => json_err(error, StatusCode::BAD_REQUEST),
        Err(error) => json_err(error.to_string(), StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn api_rate_limit_config() -> Response {
    json_ok(config::load_rate_limit_config())
}

/// POST /api/rate-limits/config —— 保存限额监听配置。
///
/// 与桌面端同语义：`scanIdeLogs` 变化时作废扫描缓存，下一次按当前来源范围重算。
async fn api_save_rate_limit_config(Json(body): Json<Value>) -> Response {
    let submitted = body.get("config").unwrap_or(&body);
    match limits::save_rate_limit_config(submitted) {
        Ok(()) => json_ok(config::load_rate_limit_config()),
        Err(e) => json_err(e.to_string(), StatusCode::BAD_REQUEST),
    }
}

async fn api_checkin(Json(body): Json<Value>) -> Response {
    let id = body.get("accountId").and_then(|v| v.as_str()).unwrap_or("");
    let Some(acc) = account::find_account(id) else {
        return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    json_ok(checkin::checkin_account(&acc).await)
}

async fn api_checkin_all(body: Option<Json<Value>>) -> Response {
    // 缺省（无 body / 无 variant）= 全部档位，保持与桌面端 set 前的行为一致。
    let variant = body
        .as_ref()
        .and_then(|Json(value)| value.get("variant"))
        .and_then(|value| value.as_str())
        .map(|raw| WbVariant::parse(Some(raw)));
    json_ok(checkin::run_checkin_all(variant).await)
}

async fn api_checkin_config() -> Response {
    json_ok(config::load_checkin_config())
}

async fn api_save_checkin_config(Json(body): Json<Value>) -> Response {
    let submitted = body.get("config").unwrap_or(&body);
    match config::save_checkin_config(submitted) {
        Ok(()) => json_ok(config::load_checkin_config()),
        Err(e) => json_err(e.to_string(), StatusCode::BAD_REQUEST),
    }
}

/// GET /api/checkin/logs —— 签到日志（每行带 `variant`，便于前端按档位过滤）。
async fn api_checkin_logs() -> Response {
    json_ok(json!({ "logs": checkin::load_checkin_logs_with_variant() }))
}

async fn api_travel_status() -> Response {
    travel::reconcile_due_travel(None).await;
    let items = account::load_accounts()
        .iter()
        .map(|acc| {
            let id = acc.get("id").and_then(Value::as_str).unwrap_or("");
            let mut value = travel::travel_display(id);
            value["accountId"] = acc.get("id").cloned().unwrap_or(Value::Null);
            value["email"] = json!(account::account_display_name(acc));
            value
        })
        .collect::<Vec<_>>();
    json_ok(json!({ "accounts": items }))
}

async fn api_travel_config() -> Response {
    json_ok(config::load_travel_config())
}

async fn api_save_travel_config(Json(body): Json<Value>) -> Response {
    let submitted = body.get("config").unwrap_or(&body);
    match config::save_travel_config(submitted) {
        Ok(()) => {
            let saved = config::load_travel_config();
            if saved.get("enabled").and_then(Value::as_bool) == Some(true) {
                tokio::spawn(async {
                    let _ = travel::run_travel_cycle().await;
                    let _ = travel::run_travel_claim_cycle().await;
                });
            }
            json_ok(saved)
        }
        Err(e) => json_err(e.to_string(), StatusCode::BAD_REQUEST),
    }
}

async fn api_refresh_token(Json(body): Json<Value>) -> Response {
    let id = body.get("accountId").and_then(|v| v.as_str()).unwrap_or("");
    let Some(acc) = account::find_account(id) else {
        return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    json_ok(refresh::refresh_account_token(acc).await)
}

// ---------------------------------------------------------------------------
// 自动轮换（CodeBuddy CLI）
// ---------------------------------------------------------------------------

async fn api_rotate_config() -> Response {
    json_ok(config::load_auto_rotate_config())
}

async fn api_save_rotate_config(Json(body): Json<Value>) -> Response {
    match config::save_auto_rotate_config(&body) {
        Ok(()) => json_ok(json!({ "ok": true, "config": config::load_auto_rotate_config() })),
        Err(e) => json_err(e.to_string(), StatusCode::BAD_REQUEST),
    }
}

async fn api_rotate_status() -> Response {
    json_ok(rotate::rotate_status())
}

async fn api_rotate_run() -> Response {
    json_ok(rotate::run_rotate_cycle().await)
}

async fn api_rotate_logs() -> Response {
    json_ok(json!({ "logs": rotate::rotate_logs() }))
}

// ---------------------------------------------------------------------------
// 更新
// ---------------------------------------------------------------------------

async fn api_update_check() -> Response {
    json_ok(update::update_check(None, false).await)
}

async fn api_update_config() -> Response {
    json_ok(update::load_github_config())
}

async fn api_save_update_config(Json(body): Json<Value>) -> Response {
    match update::save_github_config(&body) {
        Ok(()) => json_ok(json!({ "ok": true, "config": update::load_github_config() })),
        Err(e) => json_err(e.to_string(), StatusCode::BAD_REQUEST),
    }
}

// ---------------------------------------------------------------------------
// 静态前端
// ---------------------------------------------------------------------------

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".js") || path.ends_with(".mjs") {
        "text/javascript"
    } else if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}

async fn static_handler(uri: Uri) -> Response {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() || path == "index.html" {
        path = "index.html".to_string();
    }
    // 前端路由回退到 index.html
    let data = Assets::get(&path).or_else(|| Assets::get("index.html"));
    match data {
        Some(f) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type(&path))
            .body(Body::from(f.data.into_owned()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::{body_variant, checkin_status_item, query_variant};
    use serde_json::json;
    use wb_switch_core::modules::variant::WbVariant;

    /// 缺省档位必须与改造前一致（不传 variant 即国内版）。
    #[test]
    fn variant_query_defaults_to_cn() {
        assert_eq!(query_variant(None), WbVariant::Cn);
        assert_eq!(query_variant(Some("")), WbVariant::Cn);
        assert_eq!(query_variant(Some("refresh=true")), WbVariant::Cn);
        assert_eq!(
            query_variant(Some("refresh=true&variant=cn")),
            WbVariant::Cn
        );
        assert_eq!(query_variant(Some("variant=ai")), WbVariant::Ai);
        assert_eq!(
            query_variant(Some("variant=ai&refresh=true")),
            WbVariant::Ai
        );
        assert_eq!(query_variant(Some("variant=unknown")), WbVariant::Cn);
    }

    #[test]
    fn variant_body_defaults_to_cn() {
        assert_eq!(body_variant(&json!({})), WbVariant::Cn);
        assert_eq!(body_variant(&json!({"variant": null})), WbVariant::Cn);
        assert_eq!(body_variant(&json!({"variant": "ai"})), WbVariant::Ai);
        assert_eq!(body_variant(&json!({"accountId": "x"})), WbVariant::Cn);
    }

    #[test]
    fn web_checkin_status_keeps_account_identity() {
        let item = checkin_status_item(
            &json!({"id": "account-1", "email": "user@example.com"}),
            json!({"ok": true, "todayCheckedIn": true}),
        );

        assert_eq!(item["accountId"], "account-1");
        assert_eq!(item["email"], "user@example.com");
        assert_eq!(item["todayCheckedIn"], true);
        assert_eq!(item["variant"], "cn");
    }

    #[test]
    fn web_checkin_status_preserves_failure_state() {
        let item = checkin_status_item(
            &json!({"id": "account-2"}),
            json!({"ok": false, "todayCheckedIn": false, "error": "status failed"}),
        );

        assert_eq!(item["accountId"], "account-2");
        assert_eq!(item["ok"], false);
        assert_eq!(item["error"], "status failed");
    }

    #[test]
    fn web_checkin_status_preserves_exclusion_without_inventing_today_status() {
        let item = checkin_status_item(
            &json!({"id": "excluded"}),
            json!({"ok": false, "result": "skipped", "reason": "auto_checkin_disabled"}),
        );
        assert_eq!(item["accountId"], "excluded");
        assert_eq!(item["reason"], "auto_checkin_disabled");
        assert_eq!(item["result"], "skipped");
        assert!(item.get("todayCheckedIn").is_none());
    }

    #[test]
    fn web_checkin_status_row_carries_variant() {
        let item = checkin_status_item(
            &json!({"id": "ai-1", "variant": "ai"}),
            json!({"ok": false, "statusUnsupported": true}),
        );

        assert_eq!(item["variant"], "ai");
        assert_eq!(item["statusUnsupported"], true);
    }
}

// ---------------------------------------------------------------------------
// 通知存档（toast 事后可查）
// ---------------------------------------------------------------------------

/// GET /api/notifications —— 最近的应用内提示（新的在前，最多 100 条）。
async fn api_notifications() -> Response {
    match notifications::list() {
        Ok(items) => json_ok(json!({ "items": items })),
        Err(error) => json_err(error, StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// POST /api/notifications/record —— 记录一条提示（前端 toast 同步写一份）。
async fn api_record_notification(Json(body): Json<Value>) -> Response {
    let level = body.get("level").and_then(|v| v.as_str()).unwrap_or("info");
    let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let description = body.get("description").and_then(|v| v.as_str());
    match notifications::record(level, title, description) {
        Ok(()) => json_ok(json!({ "recorded": true })),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/notifications/clear —— 清空通知存档。
async fn api_clear_notifications() -> Response {
    match notifications::clear() {
        Ok(()) => json_ok(json!({ "cleared": true })),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}
