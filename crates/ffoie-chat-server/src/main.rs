use std::time::Duration;

use tracing_subscriber::EnvFilter;

use ffoie_chat_server::{build_app, config::Config};

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

    // Build router and shared AppState via the library helper.
    let (app, state) = build_app(config);

    // Bind and serve.
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        });

    tracing::info!("Listening on {bind_addr}");

    // ── Signal handler ────────────────────────────────────────────────────────
    //
    // Spawns a task that waits for SIGTERM or SIGINT (Ctrl+C), then cancels
    // the shared CancellationToken.  The cancellation propagates to:
    //   - axum's graceful_shutdown future (stops accepting new connections)
    //   - every ws.rs select! loop via `state.cancellation_token.cancelled()`
    {
        let token = state.cancellation_token.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                tokio::select! {
                    _ = sigterm.recv() => {
                        tracing::info!("SIGTERM received");
                    }
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("SIGINT (Ctrl+C) received");
                    }
                }
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c()
                    .await
                    .expect("failed to register Ctrl+C handler");
                tracing::info!("Ctrl+C received");
            }
            tracing::info!("shutdown signal received — cancelling tasks");
            token.cancel();
        });
    }

    // ── Serve with graceful shutdown ──────────────────────────────────────────
    //
    // `with_graceful_shutdown` stops axum from accepting new connections as
    // soon as the CancellationToken fires.  In-flight HTTP requests are given
    // time to complete naturally; WS tasks drain separately via task_tracker.
    let shutdown_future = state.cancellation_token.clone().cancelled_owned();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_future)
        .await
        .unwrap_or_else(|e| eprintln!("Server error: {e}"));

    // ── Drain WS tasks (5-second hard timeout) ────────────────────────────────
    //
    // After the HTTP server stops accepting, close the tracker (no more tasks
    // will be spawned) and wait up to 5 seconds for active WS handlers to
    // finish.  Each ws.rs task selects on `cancellation_token.cancelled()` so
    // they should exit promptly; the timeout is a safety net for hung tasks.
    tracing::info!("draining WebSocket tasks (5-second timeout)");
    state.task_tracker.close();
    if tokio::time::timeout(Duration::from_secs(5), state.task_tracker.wait())
        .await
        .is_err()
    {
        tracing::warn!("some WebSocket tasks did not finish within 5 seconds — proceeding anyway");
    }

    tracing::info!("graceful shutdown complete");
}
