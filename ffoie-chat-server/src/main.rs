use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

mod config;
use config::Config;

// ── AppState ──────────────────────────────────────────────────────────────────

/// Shared server state threaded through axum handlers via `State`.
///
/// Phase 2 Plan 01 holds only `config` and `started_at`.
/// Plans 02-02 and 02-03 will extend this with:
///   - `broadcast_tx: tokio::sync::broadcast::Sender<Arc<BroadcastEvent>>`
///   - `scrollback: Arc<parking_lot::Mutex<VecDeque<ChatMessage>>>`
///   - `connections: Arc<parking_lot::Mutex<HashMap<Uuid, ConnInfo>>>`
///   - `cancellation_token: tokio_util::sync::CancellationToken`
#[derive(Clone)]
#[allow(dead_code)] // config consumed by plans 02-02 and 02-03
struct AppState {
    config: Arc<Config>,
    started_at: Instant,
}

// ── Healthz response ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct HealthzResponse {
    status: &'static str,
    uptime_seconds: u64,
    connections: u32,
    version: &'static str,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn healthz(State(state): State<AppState>) -> Json<HealthzResponse> {
    Json(HealthzResponse {
        status: "ok",
        uptime_seconds: state.started_at.elapsed().as_secs(),
        connections: 0, // populated in plan 02-02 when connections map is added
        version: "v1.1",
    })
}

async fn ws_placeholder() -> StatusCode {
    // Real WebSocket upgrade handler lands in plan 02-03.
    StatusCode::NOT_IMPLEMENTED
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Load .env (ignore if absent; Config::from_env also calls this, but
    // calling it here first means RUST_LOG / RUST_LOG_FORMAT are available
    // before the subscriber is built).
    dotenvy::dotenv().ok();

    // Initialise tracing subscriber.
    // Plain formatter by default; JSON when RUST_LOG_FORMAT=json.
    let log_format = std::env::var("RUST_LOG_FORMAT").unwrap_or_default();
    if log_format.trim().eq_ignore_ascii_case("json") {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(EnvFilter::from_default_env())
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .init();
    }

    // Load configuration.
    let config = Config::from_env().unwrap_or_else(|e| {
        eprintln!("Configuration error: {e}");
        std::process::exit(1);
    });

    let bind_addr = config.bind;

    let state = AppState {
        config: Arc::new(config),
        started_at: Instant::now(),
    };

    // Build the router.
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_placeholder))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Bind and serve.
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        });

    tracing::info!("Listening on {bind_addr}");

    axum::serve(listener, app)
        .await
        .unwrap_or_else(|e| eprintln!("Server error: {e}"));
}
