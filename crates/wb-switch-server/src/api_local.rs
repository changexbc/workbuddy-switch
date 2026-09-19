//! 本地专属 HTTP 路由（上游无此文件 → 与上游合并零冲突）。
//!
//! 这里只放本项目新增的接口：账号发现 / 补录。
//! `api.rs` 只保留一行 `.merge(api_local::router())`，避免在上游热点文件里堆代码。

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};

use wb_switch_core::modules::discover;

fn json_ok(v: Value) -> Response {
    Json(v).into_response()
}

fn json_err(e: String, code: StatusCode) -> Response {
    (code, Json(json!({ "ok": false, "error": e }))).into_response()
}

/// 本地路由表（由 `api::router()` merge）。
pub fn router() -> Router {
    Router::new()
        .route("/api/accounts/discover", get(api_discover_accounts))
        .route("/api/accounts/adopt", post(api_adopt_account))
}

/// GET /api/accounts/discover —— 识别本机曾登录/留有数据的账号（对照在册）。
async fn api_discover_accounts() -> Response {
    json_ok(discover::discover_known_accounts())
}

/// POST /api/accounts/adopt —— 用最新 auth 历史备份补录指定 uid 进账号库。
async fn api_adopt_account(Json(body): Json<Value>) -> Response {
    let uid = body
        .get("uid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if uid.trim().is_empty() {
        return json_err("缺少 uid".to_string(), StatusCode::BAD_REQUEST);
    }
    match discover::adopt_account(&uid) {
        Ok(meta) => json_ok(json!({ "ok": true, "account": meta })),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}
