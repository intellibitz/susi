//! `susi-paths`: `GET /paths`, `GET /ports` — the host path/port contract.

use axum::{routing::get, Json, Router};

pub(crate) fn router() -> Router {
    Router::new()
        .route(
            "/paths",
            get(|| async { Json(susi_paths::local_paths_json()) }),
        )
        .route("/ports", get(|| async { Json(susi_paths::ports_json()) }))
}
