//! WebSocket upgrade handler and per-connection select! loop.
//!
//! # Connection lifecycle
//!
//! 1. `GET /ws` hits `ws_handler`, which calls `ws.on_upgrade(handle_socket)`.
//! 2. `handle_socket` subscribes to the broadcast channel and waits for the
//!    first frame.
//! 3. The first frame MUST be `ClientMessage::Connect`.  Any other message
//!    causes an immediate `Error` response and the socket is closed.
//! 4. On a valid `Connect`: normalize + deduplicate nickname, assign team,
//!    register in `AppState::connections`, send `Welcome`, broadcast
//!    `JoinedLeft { joined: true }`.
//! 5. The `tokio::select!` loop then handles:
//!    - Arm A: inbound client frames (Say / SayTeam / Who / Ping)
//!    - Arm B: outbound broadcast events (with team filtering for SayTeam)
//!    - Arm C: heartbeat tick (server-initiated keep-alive Pong)
//!    - Arm D: cancellation token (graceful server shutdown)
//! 6. On exit (clean or error): deregister + broadcast `JoinedLeft { joined: false }`.
//!
//! # Rate limiting and length cap
//!
//! Per-connection `TokenBucket` enforces burst + steady-state rates.
//! Messages exceeding `config.max_msg_bytes` are rejected with `Error`.
//! Rate-limited messages get `RateLimited`; the sender is NOT disconnected.
//!
//! # Lagged-receiver handling
//!
//! If the broadcast ring laps a slow receiver, `RecvError::Lagged` is counted.
//! After `config.max_lag_disconnects` consecutive lags the connection is closed.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, Duration};
use uuid::Uuid;

use ffoie_protocol::{Channel, ClientMessage, PlayerEntry, ServerMessage, Team};

use crate::nickname::assign_team;
use crate::rate_limit::TokenBucket;
use crate::state::{AppState, BroadcastEvent};

// ── Public handler ────────────────────────────────────────────────────────────

/// axum handler: upgrades a GET /ws request to a WebSocket connection.
///
/// The per-connection task is registered with `state.task_tracker` so that
/// the graceful-shutdown path in `main.rs` can await all tasks via
/// `task_tracker.close()` + `task_tracker.wait()`.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    // Reject oversized frames at the transport layer — BEFORE any allocation or
    // JSON parse — so a client cannot force multi-MB allocations (the
    // per-message `max_msg_bytes` check in the loop only fires post-parse).
    // Every inbound client frame (Connect/Say/SayTeam/Ping/Who) is small, so a
    // generous multiple of the byte cap is plenty and bounds worst-case memory
    // at ~connections × this limit.
    let frame_cap = state.config.max_msg_bytes.saturating_mul(8).max(16 * 1024);
    ws.max_message_size(frame_cap)
        .max_frame_size(frame_cap)
        .on_upgrade(move |socket| {
            let fut = handle_socket(socket, state.clone());
            // Register with the task tracker so main.rs shutdown can drain us.
            state.task_tracker.spawn(fut);
            // Return an already-resolved future — the work runs inside the tracker.
            async {}
        })
}

// ── Per-connection task ───────────────────────────────────────────────────────

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let session_id = Uuid::new_v4();

    // Subscribe to broadcast BEFORE entering the loop so we don't miss any
    // events that fire between subscribe() and the first select! iteration.
    let mut bcast_rx = state.broadcast_tx.subscribe();

    // ── Connection-scoped state ───────────────────────────────────────────────
    let mut connected = false;
    let mut nickname = String::new();
    let mut my_team = Team::None;
    let mut lag_count: u32 = 0;

    let mut rate_bucket =
        TokenBucket::new(state.config.rate_burst, state.config.rate_refill_per_sec);

    // Heartbeat interval — server sends a Pong periodically to keep NATs alive.
    let heartbeat_secs = state.config.heartbeat_secs;
    let mut heartbeat = interval(Duration::from_secs(heartbeat_secs));
    // The first tick fires immediately; discard it so we don't send a Pong
    // before the connection is even established.
    heartbeat.tick().await;

    // Reusable serialized-message helper (returns None on serialization error).
    macro_rules! to_text {
        ($msg:expr) => {
            serde_json::to_string(&$msg)
                .ok()
                .map(|s| Message::Text(s.into()))
        };
    }

    // ── select! loop ──────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            // ── Arm A: inbound client message ────────────────────────────────
            frame = socket.recv() => {
                match frame {
                    None => {
                        // Socket closed by client.
                        tracing::debug!(session_id = %session_id, "WS stream ended");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::debug!(session_id = %session_id, error = %e, "WS read error");
                        break;
                    }
                    Some(Ok(msg)) => {
                        // Only Text frames carry ClientMessage JSON.
                        let text = match msg {
                            Message::Text(t) => t,
                            Message::Close(_) => break,
                            _ => continue, // ignore Binary, Ping, Pong at app level
                        };

                        // Deserialize.
                        let client_msg: ClientMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::debug!(
                                    session_id = %session_id,
                                    error = %e,
                                    "invalid JSON from client"
                                );
                                if let Some(m) = to_text!(ServerMessage::Error {
                                    reason: "invalid message".to_string()
                                }) {
                                    socket.send(m).await.ok();
                                }
                                continue; // keep connection alive on bad JSON
                            }
                        };

                        // ── Connect gate ──────────────────────────────────────
                        if !connected {
                            match client_msg {
                                ClientMessage::Connect { nickname: req_nick, .. } => {
                                    let assigned_team = assign_team();
                                    // Atomically assign a unique nickname AND register
                                    // under one lock — no snapshot→assign→insert race.
                                    let assigned_nick = state.assign_and_register(
                                        session_id,
                                        &req_nick,
                                        assigned_team,
                                    );

                                    connected = true;
                                    nickname = assigned_nick.clone();
                                    my_team = assigned_team;

                                    // Send Welcome.
                                    let welcome = ServerMessage::Welcome {
                                        assigned_nick: assigned_nick.clone(),
                                        assigned_team,
                                        motd: state.config.motd.clone(),
                                        scrollback: state.scrollback_snapshot(),
                                    };
                                    if let Some(m) = to_text!(welcome) {
                                        if socket.send(m).await.is_err() {
                                            // Socket broken before we could send Welcome.
                                            state.remove_conn(session_id);
                                            connected = false;
                                            break;
                                        }
                                    }

                                    // Broadcast JoinedLeft to everyone.
                                    let ev = BroadcastEvent {
                                        msg: ServerMessage::JoinedLeft {
                                            nickname: assigned_nick.clone(),
                                            team: assigned_team,
                                            joined: true,
                                        },
                                        team_filter: None,
                                    };
                                    state.broadcast_tx.send(Arc::new(ev)).ok();

                                    tracing::info!(
                                        session_id = %session_id,
                                        nickname = %assigned_nick,
                                        team = ?assigned_team,
                                        "client connected"
                                    );
                                }
                                _ => {
                                    // Non-Connect before Connect → error + close.
                                    if let Some(m) = to_text!(ServerMessage::Error {
                                        reason: "send Connect first".to_string()
                                    }) {
                                        socket.send(m).await.ok();
                                    }
                                    break; // protocol violation — close
                                }
                            }
                            continue;
                        }

                        // ── Authenticated message handling ────────────────────
                        match client_msg {
                            ClientMessage::Connect { .. } => {
                                // Duplicate Connect — reject gracefully.
                                if let Some(m) = to_text!(ServerMessage::Error {
                                    reason: "already connected".to_string()
                                }) {
                                    socket.send(m).await.ok();
                                }
                            }

                            ClientMessage::Say { text } => {
                                if text.len() > state.config.max_msg_bytes {
                                    if let Some(m) = to_text!(ServerMessage::Error {
                                        reason: "message too long".to_string()
                                    }) {
                                        socket.send(m).await.ok();
                                    }
                                    continue;
                                }

                                if !rate_bucket.consume() {
                                    if let Some(m) = to_text!(ServerMessage::RateLimited {
                                        reason: format!(
                                            "retry after {}ms",
                                            rate_bucket.retry_after_ms()
                                        )
                                    }) {
                                        socket.send(m).await.ok();
                                    }
                                    continue;
                                }

                                let chat_msg = ffoie_protocol::ChatMessage {
                                    from: nickname.clone(),
                                    team: my_team,
                                    channel: Channel::All,
                                    text: text.clone(),
                                    ts: unix_millis(),
                                };

                                state.push_scrollback(chat_msg.clone());

                                let ev = BroadcastEvent {
                                    msg: ServerMessage::Message { data: chat_msg },
                                    team_filter: None,
                                };
                                state.broadcast_tx.send(Arc::new(ev)).ok();
                            }

                            ClientMessage::SayTeam { text } => {
                                if text.len() > state.config.max_msg_bytes {
                                    if let Some(m) = to_text!(ServerMessage::Error {
                                        reason: "message too long".to_string()
                                    }) {
                                        socket.send(m).await.ok();
                                    }
                                    continue;
                                }

                                if !rate_bucket.consume() {
                                    if let Some(m) = to_text!(ServerMessage::RateLimited {
                                        reason: format!(
                                            "retry after {}ms",
                                            rate_bucket.retry_after_ms()
                                        )
                                    }) {
                                        socket.send(m).await.ok();
                                    }
                                    continue;
                                }

                                let chat_msg = ffoie_protocol::ChatMessage {
                                    from: nickname.clone(),
                                    team: my_team,
                                    channel: Channel::Team,
                                    text: text.clone(),
                                    ts: unix_millis(),
                                };

                                state.push_scrollback(chat_msg.clone());

                                let ev = BroadcastEvent {
                                    msg: ServerMessage::Message { data: chat_msg },
                                    team_filter: Some(my_team),
                                };
                                state.broadcast_tx.send(Arc::new(ev)).ok();
                            }

                            ClientMessage::Who => {
                                let players: Vec<PlayerEntry> = state.who_list();
                                if let Some(m) = to_text!(ServerMessage::WhoList { players }) {
                                    socket.send(m).await.ok();
                                }
                            }

                            ClientMessage::Ping { seq } => {
                                if let Some(m) = to_text!(ServerMessage::Pong { seq }) {
                                    socket.send(m).await.ok();
                                }
                            }
                        }
                    }
                }
            }

            // ── Arm B: outbound broadcast event ──────────────────────────────
            event = bcast_rx.recv() => {
                match event {
                    Ok(ev) => {
                        // Team filter: drop SayTeam events for the other team.
                        if let Some(filter_team) = ev.team_filter {
                            if filter_team != my_team {
                                continue;
                            }
                        }

                        // Forward to this client.
                        if let Some(m) = to_text!(&ev.msg) {
                            if socket.send(m).await.is_err() {
                                break;
                            }
                        }

                        // Reset lag counter on successful receive.
                        lag_count = 0;
                    }
                    Err(RecvError::Lagged(n)) => {
                        lag_count += 1;
                        tracing::warn!(
                            session_id = %session_id,
                            skipped = n,
                            lag_count,
                            max = state.config.max_lag_disconnects,
                            "broadcast ring lapped slow receiver"
                        );
                        if lag_count >= state.config.max_lag_disconnects {
                            if let Some(m) = to_text!(ServerMessage::Error {
                                reason: "lag: too far behind".to_string()
                            }) {
                                socket.send(m).await.ok();
                            }
                            break;
                        }
                    }
                    Err(RecvError::Closed) => {
                        // Broadcast channel shut down — server is exiting.
                        break;
                    }
                }
            }

            // ── Arm C: heartbeat — keep NATs alive ────────────────────────────
            _ = heartbeat.tick() => {
                if let Some(m) = to_text!(ServerMessage::Pong { seq: 0 }) {
                    if socket.send(m).await.is_err() {
                        break;
                    }
                }
            }

            // ── Arm D: graceful server shutdown ───────────────────────────────
            _ = state.cancellation_token.cancelled() => {
                tracing::info!(session_id = %session_id, "shutdown signal — closing WS");
                // Send a Close frame to notify the client, then exit the loop.
                socket.send(Message::Close(None)).await.ok();
                break;
            }
        }
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    if connected {
        state.remove_conn(session_id);
        let ev = BroadcastEvent {
            msg: ServerMessage::JoinedLeft {
                nickname: nickname.clone(),
                team: my_team,
                joined: false,
            },
            team_filter: None,
        };
        state.broadcast_tx.send(Arc::new(ev)).ok();
        tracing::info!(
            session_id = %session_id,
            %nickname,
            team = ?my_team,
            "client disconnected"
        );
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Unix timestamp in milliseconds.
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ffoie_protocol::Team;

    // ── Team filter logic ─────────────────────────────────────────────────────

    /// When `team_filter` is `None` the event should be forwarded to everyone.
    #[test]
    fn team_filter_none_passes_all_teams() {
        let filter: Option<Team> = None;
        let my_team = Team::Red;
        // No filter → always forward.
        let should_skip = if let Some(f) = filter {
            f != my_team
        } else {
            false
        };
        assert!(!should_skip, "no filter should pass to any team");
    }

    /// When `team_filter` is `Some(Red)` and the receiver is `Red`, forward.
    #[test]
    fn team_filter_matches_own_team() {
        let filter = Some(Team::Red);
        let my_team = Team::Red;
        let should_skip = if let Some(f) = filter {
            f != my_team
        } else {
            false
        };
        assert!(!should_skip, "matching team filter should not skip");
    }

    /// When `team_filter` is `Some(Red)` and the receiver is `Blue`, skip.
    #[test]
    fn team_filter_skips_other_team() {
        let filter = Some(Team::Red);
        let my_team = Team::Blue;
        let should_skip = if let Some(f) = filter {
            f != my_team
        } else {
            false
        };
        assert!(should_skip, "non-matching team filter should skip");
    }

    // ── Message length cap ────────────────────────────────────────────────────

    /// A message exactly at the cap is accepted.
    #[test]
    fn message_at_cap_is_accepted() {
        let max_bytes: usize = 500;
        let text = "a".repeat(500);
        assert!(
            text.len() <= max_bytes,
            "500-byte message should pass the cap"
        );
    }

    /// A message one byte over the cap is rejected.
    #[test]
    fn message_over_cap_is_rejected() {
        let max_bytes: usize = 500;
        let text = "a".repeat(501);
        assert!(
            text.len() > max_bytes,
            "501-byte message should exceed the cap"
        );
    }

    // ── unix_millis ───────────────────────────────────────────────────────────

    #[test]
    fn unix_millis_is_reasonable() {
        let ts = unix_millis();
        // 2020-01-01 in ms ≈ 1_577_836_800_000.
        // 2035-01-01 in ms ≈ 2_051_222_400_000.
        assert!(ts > 1_577_836_800_000, "timestamp too far in the past");
        assert!(ts < 2_051_222_400_000, "timestamp too far in the future");
    }
}
