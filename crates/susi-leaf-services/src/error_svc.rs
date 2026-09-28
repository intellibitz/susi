//! `susi-error`: `POST /log_error` (open, write-only) and
//! `GET /errors/recent?n=` (supervisor-token gated: history carries paths).

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

#[derive(serde::Deserialize)]
struct RecentQuery {
    n: Option<usize>,
}

async fn log_error(Json(event): Json<susi_error::PostedErrorEvent>) -> StatusCode {
    match susi_error::append_posted_event(event) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn recent_errors(Query(q): Query<RecentQuery>) -> Json<serde_json::Value> {
    let entries = susi_error::recent_error_entries(q.n.unwrap_or(50));
    Json(serde_json::json!({ "returned": entries.len(), "entries": entries }))
}

pub(crate) fn router() -> Router {
    Router::new()
        .route(
            "/errors/recent",
            get(recent_errors).layer(axum::middleware::from_fn(crate::auth::supervisor)),
        )
        .route("/log_error", post(log_error))
}
