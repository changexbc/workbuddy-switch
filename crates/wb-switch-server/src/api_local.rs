//! 本地专属 HTTP 路由（上游无此文件 → 与上游合并零冲突）。
//!
//! 这里只放本项目新增的接口：定时任务归属对齐。
//! `api.rs` 只保留一行 `.merge(api_local::router())`，避免在上游热点文件里堆代码。

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use wb_switch_core::modules::{account, automations};

fn json_ok(v: Value) -> Response {
    Json(v).into_response()
}

fn json_err(e: String, code: StatusCode) -> Response {
    (code, Json(json!({ "ok": false, "error": e }))).into_response()
}

/// 本地路由表（由 `api::router()` merge）。
pub fn router() -> Router {
    Router::new().route("/api/automations/align", post(api_align_automations))
}

/// 从账号记录里取 uid（空串 = 该账号缺 uid）。
fn account_uid(acc: &Value) -> String {
    acc.get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// POST /api/automations/align —— 自动化归属对齐（不切号）。需先完全退出 WorkBuddy。
async fn api_align_automations(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if account_id.trim().is_empty() {
        return json_err("缺少 accountId".to_string(), StatusCode::BAD_REQUEST);
    }
    let Some(target) = account::find_account(&account_id) else {
        return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    let uid = account_uid(&target);
    if uid.is_empty() {
        return json_err("该账号缺少 uid，无法对齐".to_string(), StatusCode::BAD_REQUEST);
    }
    match automations::align_automations_owner(&uid) {
        Some(v) => json_ok(v),
        None => json_err("workbuddy.db 不存在".to_string(), StatusCode::BAD_REQUEST),
    }
}
