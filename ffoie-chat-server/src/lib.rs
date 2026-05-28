// ffoie-chat-server — library target
//
// Exposes modules and a `build_app` helper so that integration tests in
// tests/ can spin up a real in-process server without duplicating the
// router-construction logic from main.rs.

pub mod config;
pub mod nickname;
pub mod rate_limit;
pub mod state;
pub mod ws;

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use config::Config;
use state::AppState;

// ── Healthz ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct HealthzResponse {
    pub status: &'static str,
    pub uptime_seconds: u64,
    pub connections: usize,
    pub version: &'static str,
}

pub async fn healthz_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<HealthzResponse> {
    Json(HealthzResponse {
        status: "ok",
        uptime_seconds: state.started_at.elapsed().as_secs(),
        connections: state.connection_count(),
        version: "v1.1",
    })
}

// ── Router builder ────────────────────────────────────────────────────────────

/// Build the axum Router wired with `/healthz` and `/ws`, plus CORS and
/// tracing middleware.  Both main.rs and integration tests call this.
pub fn build_app(config: Config) -> (Router, AppState) {
    let state = AppState::new(Arc::new(config));

    let router = Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/ws", get(ws::ws_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    (router, state)
}
