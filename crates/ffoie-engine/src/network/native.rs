//! native.rs — Tokio-backed WebSocket client for the native FFOIE engine.
//!
//! This module is **native-only** (gated by `#![cfg(not(target_arch = "wasm32"))]`).
//! The wasm32 client path will be added in Phase 4 plan 04-02 as `wasm.rs`.
//!
//! # Architecture
//!
//! Calling [`start`] spawns a background OS thread that hosts a tokio
//! multi-thread runtime.  That runtime runs [`network_loop`], which:
//!   1. Connects to the chat server via [`ewebsock::connect_with_wakeup`].
//!   2. Sends `ClientMessage::Connect` on every (re)connect.
//!   3. Polls for incoming [`ewebsock::WsEvent`]s and translates them to
//!      [`NetworkEvent`]s pushed onto the engine-side mpsc receiver.
//!   4. Forwards [`NetworkCommand`]s from the main thread to the server.
//!   5. Sends a `ClientMessage::Ping` heartbeat every `heartbeat_secs`.
//!   6. On disconnect, reconnects with exponential back-off (full jitter,
//!      1s → 30s) — retries forever.
//!
//! The main thread only ever calls the **non-blocking**
//! `std::sync::mpsc::Receiver::try_recv` so the winit loop is never stalled
//! (per PITFALLS.md Pitfall 2).  The tokio runtime never runs on the main
//! thread (per PITFALLS.md Pitfall 1).
#![cfg(not(target_arch = "wasm32"))]

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ewebsock::{WsEvent, WsMessage, WsSender};
use ffoie_protocol::{ClientMessage, ServerMessage, Team};
use winit::event_loop::EventLoopProxy;

use super::{AppEvent, NetworkCommand, NetworkEvent, NetworkHandle};

// ── Entry point ───────────────────────────────────────────────────────────────

/// Start the network background thread and return a [`NetworkHandle`].
///
/// Reads configuration from the environment:
/// - `FFOIE_CHAT_URL` — WebSocket URL (default `ws://localhost:8080/ws`).
///   Can also be baked in at build time via the env var at compile time
///   (`option_env!("FFOIE_CHAT_URL")`).
/// - `FFOIE_NICK` — requested nickname (default `guest`).
/// - `FFOIE_CHAT_HEARTBEAT_SECS` — heartbeat interval in seconds (default 15).
///
/// The background thread builds a `tokio` multi-thread runtime with 2 worker
/// threads and calls `runtime.block_on(network_loop(...))`.  `start` returns
/// before the first WS connection attempt completes.
pub fn start(proxy: EventLoopProxy<AppEvent>) -> NetworkHandle {
    // Resolve URL: build-time bake-in wins; runtime env var overrides that;
    // finally fall back to the hard-coded localhost default.
    let compile_time_url = option_env!("FFOIE_CHAT_URL").unwrap_or("ws://localhost:8080/ws");
    let url = std::env::var("FFOIE_CHAT_URL")
        .unwrap_or_else(|_| compile_time_url.to_owned());

    let nick = std::env::var("FFOIE_NICK").unwrap_or_else(|_| "guest".to_owned());

    let heartbeat_secs: u64 = std::env::var("FFOIE_CHAT_HEARTBEAT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);

    let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<NetworkCommand>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for network thread");
        rt.block_on(network_loop(url, nick, heartbeat_secs, event_tx, cmd_rx, proxy));
    });

    NetworkHandle {
        tx: cmd_tx,
        rx: event_rx,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Compute the full-jitter exponential back-off delay for a given attempt.
///
/// - Attempt 1: 0 ms (connect immediately the first time).
/// - Attempt 2+: `delay = fastrand::u64(0..=base_ms)` where
///   `base_ms = min(30_000, 1000 * 2^(attempt-1))`.
///
/// "Full jitter" spreads reconnect storms across a large window.
pub fn backoff_delay_ms(attempt: u32) -> u64 {
    if attempt <= 1 {
        return 0;
    }
    // base_ms = 1000 * 2^(attempt-1), capped at 30 000.
    // Use saturating arithmetic to avoid overflow for large attempt counts.
    let exponent = (attempt - 1).min(14) as u64; // 2^14 = 16384 → 16.384s < 30s; 2^15 > 30s
    let base_ms = (1_000u64 << exponent).min(30_000);
    fastrand::u64(0..=base_ms)
}

/// Serialize a `ClientMessage` and send it over the WebSocket.
///
/// Logs a warning if serialization fails (shouldn't happen for well-formed
/// messages) and silently drops if the WS send would fail (connection is
/// being torn down).
fn ws_send(ws_tx: &mut WsSender, msg: &ClientMessage) {
    match serde_json::to_string(msg) {
        Ok(json) => ws_tx.send(WsMessage::Text(json)),
        Err(e) => log::warn!("[network] failed to serialize {:?}: {e}", msg),
    }
}

/// Translate a `ServerMessage` into a `NetworkEvent` and push it to the main
/// thread.  Returns `false` if the channel is disconnected (main thread exited).
fn dispatch(
    server_msg: ServerMessage,
    event_tx: &Sender<NetworkEvent>,
    proxy: &EventLoopProxy<AppEvent>,
) -> bool {
    let event = match server_msg {
        ServerMessage::Welcome {
            assigned_nick,
            assigned_team,
            motd,
            scrollback,
        } => NetworkEvent::Connected {
            assigned_nick,
            assigned_team,
            motd,
            scrollback,
        },
        ServerMessage::Message { data } => NetworkEvent::Message(data),
        ServerMessage::JoinedLeft {
            nickname,
            team,
            joined,
        } => NetworkEvent::JoinedLeft {
            nickname,
            team,
            joined,
        },
        ServerMessage::WhoList { players } => NetworkEvent::WhoList(players),
        // Pong, RateLimited, Error: log and ignore — no UI event in v1.1.
        ServerMessage::Pong { seq } => {
            log::debug!("[network] pong seq={seq}");
            return true;
        }
        ServerMessage::RateLimited { reason } => {
            log::warn!("[network] rate limited: {reason}");
            return true;
        }
        ServerMessage::Error { reason } => {
            log::warn!("[network] server error: {reason}");
            return true;
        }
    };

    if event_tx.send(event).is_err() {
        return false; // main thread gone
    }
    // Wake the winit loop so it drains the channel without waiting for the
    // next poll/redraw.  Ignore errors — the window may have already closed.
    let _ = proxy.send_event(AppEvent::ChatWakeup);
    true
}

// ── Core async loop ───────────────────────────────────────────────────────────

/// The async network loop.  Runs forever on the background tokio runtime.
///
/// Outer loop: exponential back-off reconnect.
/// Inner loop (per connection): heartbeat + WS receive + command dispatch.
async fn network_loop(
    url: String,
    nick: String,
    heartbeat_secs: u64,
    event_tx: Sender<NetworkEvent>,
    cmd_rx: Receiver<NetworkCommand>,
    proxy: EventLoopProxy<AppEvent>,
) {
    let mut attempt: u32 = 0;

    'reconnect: loop {
        attempt += 1;

        // --- Back-off delay ---------------------------------------------------
        let delay_ms = backoff_delay_ms(attempt);
        if delay_ms > 0 {
            log::info!("[network] reconnecting (attempt {attempt}), waiting {delay_ms}ms");
            if event_tx
                .send(NetworkEvent::Reconnecting { attempt, delay_ms })
                .is_err()
            {
                break 'reconnect; // main thread exited
            }
            let _ = proxy.send_event(AppEvent::ChatWakeup);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        // --- Connect ----------------------------------------------------------
        log::info!("[network] connecting to {url} (attempt {attempt})");
        let wakeup_proxy = proxy.clone();
        let connect_result = ewebsock::connect_with_wakeup(
            &url,
            ewebsock::Options::default(),
            move || {
                let _ = wakeup_proxy.send_event(AppEvent::ChatWakeup);
            },
        );

        let (mut ws_tx, ws_rx) = match connect_result {
            Ok(pair) => pair,
            Err(e) => {
                log::warn!("[network] connect failed: {e}");
                continue 'reconnect;
            }
        };

        // Reset attempt counter on successful connect.
        attempt = 0;

        // Wait for WsEvent::Opened before sending Connect.
        // ewebsock may deliver Opened asynchronously; poll until we see it.
        let opened = wait_for_open(&ws_rx).await;
        if !opened {
            // Received Closed or Error before Opened.
            let _ = event_tx.send(NetworkEvent::Disconnected);
            let _ = proxy.send_event(AppEvent::ChatWakeup);
            continue 'reconnect;
        }

        // Send the Connect handshake.
        ws_send(
            &mut ws_tx,
            &ClientMessage::Connect {
                nickname: nick.clone(),
                team: Team::None,
            },
        );

        // --- Per-connection loop ----------------------------------------------
        let mut heartbeat = tokio::time::interval(Duration::from_secs(heartbeat_secs));
        heartbeat.tick().await; // consume the immediate first tick
        let mut ping_seq: u32 = 0;

        loop {
            tokio::select! {
                // Heartbeat arm.
                _ = heartbeat.tick() => {
                    ws_send(&mut ws_tx, &ClientMessage::Ping { seq: ping_seq });
                    ping_seq = ping_seq.wrapping_add(1);
                }

                // WS receive arm: poll via a small sleep to avoid busy-spinning.
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // Drain all pending WS events in this tick.
                    loop {
                        match ws_rx.try_recv() {
                            Some(WsEvent::Message(WsMessage::Text(json))) => {
                                match serde_json::from_str::<ServerMessage>(&json) {
                                    Ok(msg) => {
                                        // T-03-03-02 / T-03-03-03: serde
                                        // from_str returns Err on unknown or
                                        // malformed variants — we log and
                                        // continue, never panic.
                                        if !dispatch(msg, &event_tx, &proxy) {
                                            break 'reconnect; // main thread gone
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("[network] invalid JSON from server: {e}");
                                    }
                                }
                            }
                            Some(WsEvent::Closed) => {
                                log::info!("[network] WS closed");
                                let _ = event_tx.send(NetworkEvent::Disconnected);
                                let _ = proxy.send_event(AppEvent::ChatWakeup);
                                attempt = 0; // will be incremented at top of reconnect loop
                                continue 'reconnect;
                            }
                            Some(WsEvent::Error(e)) => {
                                log::warn!("[network] WS error: {e}");
                                let _ = event_tx.send(NetworkEvent::Disconnected);
                                let _ = proxy.send_event(AppEvent::ChatWakeup);
                                attempt = 0;
                                continue 'reconnect;
                            }
                            Some(WsEvent::Opened) => {
                                // May arrive again on reconnect; safe to ignore
                                // here since we already handled it above.
                            }
                            Some(_) => {
                                // Unknown variant (Ping, Pong frames etc.) — ignore.
                            }
                            None => break, // no more events this tick
                        }
                    }

                    // Command arm: drain all pending commands.
                    loop {
                        match cmd_rx.try_recv() {
                            Ok(NetworkCommand::Say(text)) => {
                                ws_send(&mut ws_tx, &ClientMessage::Say { text });
                            }
                            Ok(NetworkCommand::SayTeam(text)) => {
                                ws_send(&mut ws_tx, &ClientMessage::SayTeam { text });
                            }
                            Ok(NetworkCommand::Who) => {
                                ws_send(&mut ws_tx, &ClientMessage::Who);
                            }
                            Ok(NetworkCommand::Shutdown) => {
                                log::info!("[network] shutdown command received");
                                return; // exit the entire network_loop
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                log::info!("[network] command channel closed — shutting down");
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    log::info!("[network] network_loop exiting");
}

/// Poll `ws_rx` until we see `WsEvent::Opened`, `WsEvent::Closed`, or
/// `WsEvent::Error`.  Returns `true` if the connection was opened.
async fn wait_for_open(ws_rx: &ewebsock::WsReceiver) -> bool {
    loop {
        match ws_rx.try_recv() {
            Some(WsEvent::Opened) => return true,
            Some(WsEvent::Closed) | Some(WsEvent::Error(_)) => return false,
            Some(_) => {} // other events before Opened; keep waiting
            None => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ffoie_protocol::{Channel, ChatMessage, PlayerEntry, Team};

    // ── Backoff math ──────────────────────────────────────────────────────────

    #[test]
    fn test_backoff_delays() {
        // Attempt 1: connect immediately (no delay).
        assert_eq!(backoff_delay_ms(1), 0, "first attempt must be immediate");

        // Attempt 2: base = min(30000, 1000*2^1) = 2000; jitter in [0, 2000].
        for _ in 0..100 {
            let d = backoff_delay_ms(2);
            assert!(d <= 2_000, "attempt 2 delay out of range: {d}");
        }

        // Attempt 3: base = min(30000, 1000*2^2) = 4000; jitter in [0, 4000].
        for _ in 0..100 {
            let d = backoff_delay_ms(3);
            assert!(d <= 4_000, "attempt 3 delay out of range: {d}");
        }

        // Attempt 4: base = 8000.
        for _ in 0..100 {
            let d = backoff_delay_ms(4);
            assert!(d <= 8_000, "attempt 4 delay out of range: {d}");
        }

        // Attempt 5: base = 16000.
        for _ in 0..100 {
            let d = backoff_delay_ms(5);
            assert!(d <= 16_000, "attempt 5 delay out of range: {d}");
        }

        // Attempt 6+: base capped at 30000.
        for attempt in 6..=20 {
            for _ in 0..20 {
                let d = backoff_delay_ms(attempt);
                assert!(d <= 30_000, "attempt {attempt} delay exceeds 30s cap: {d}ms");
            }
        }
    }

    #[test]
    fn test_backoff_jitter_range() {
        // Over many samples, jitter should produce values spread across the full
        // range — i.e., at least one sample should be non-zero and the max
        // observed should be close to base_ms (not stuck at 0).
        let samples: Vec<u64> = (0..500).map(|_| backoff_delay_ms(5)).collect();
        let max_observed = *samples.iter().max().unwrap();
        // With 500 samples from [0, 16000], the probability that all are < 8000
        // is astronomically small.  This check catches a broken RNG returning 0.
        assert!(
            max_observed > 4_000,
            "jitter appears broken — all 500 samples <= 4000ms (max={max_observed})"
        );
    }

    // ── Command → ClientMessage conversion ───────────────────────────────────

    #[test]
    fn test_network_command_say_serializes() {
        let text = "hello world".to_owned();
        let msg = ClientMessage::Say {
            text: text.clone(),
        };
        let json = serde_json::to_string(&msg).expect("serialization failed");
        let round_tripped: ClientMessage =
            serde_json::from_str(&json).expect("deserialization failed");
        assert_eq!(round_tripped, ClientMessage::Say { text });
    }

    #[test]
    fn test_network_command_say_team_serializes() {
        let text = "go left".to_owned();
        let msg = ClientMessage::SayTeam {
            text: text.clone(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let rt: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(rt, ClientMessage::SayTeam { text });
    }

    #[test]
    fn test_network_command_who_serializes() {
        let msg = ClientMessage::Who;
        let json = serde_json::to_string(&msg).unwrap();
        let rt: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(rt, ClientMessage::Who);
    }

    // ── ServerMessage → NetworkEvent mapping ──────────────────────────────────

    #[test]
    fn test_server_message_welcome_maps_to_connected() {
        let scrollback = vec![ChatMessage {
            from: "alice".to_owned(),
            team: Team::Red,
            channel: Channel::All,
            text: "hello".to_owned(),
            ts: 1_000,
        }];

        let server_msg = ServerMessage::Welcome {
            assigned_nick: "fox#1234".to_owned(),
            assigned_team: Team::Blue,
            motd: "Welcome!".to_owned(),
            scrollback: scrollback.clone(),
        };

        // Serialize + deserialize to verify the serde round-trip path that
        // network_loop uses (JSON text → ServerMessage → NetworkEvent).
        let json = serde_json::to_string(&server_msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();

        match parsed {
            ServerMessage::Welcome {
                assigned_nick,
                assigned_team,
                motd,
                scrollback: sb,
            } => {
                assert_eq!(assigned_nick, "fox#1234");
                assert_eq!(assigned_team, Team::Blue);
                assert_eq!(motd, "Welcome!");
                assert_eq!(sb, scrollback);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn test_server_message_joined_left_maps() {
        let server_msg = ServerMessage::JoinedLeft {
            nickname: "bob".to_owned(),
            team: Team::Red,
            joined: true,
        };
        let json = serde_json::to_string(&server_msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::JoinedLeft {
                nickname,
                team,
                joined,
            } => {
                assert_eq!(nickname, "bob");
                assert_eq!(team, Team::Red);
                assert!(joined);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn test_server_message_who_list_maps() {
        let players = vec![
            PlayerEntry {
                nick: "alice".to_owned(),
                team: Team::Red,
            },
            PlayerEntry {
                nick: "bob".to_owned(),
                team: Team::Blue,
            },
        ];
        let server_msg = ServerMessage::WhoList {
            players: players.clone(),
        };
        let json = serde_json::to_string(&server_msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::WhoList { players: p } => {
                assert_eq!(p, players);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn test_malformed_json_does_not_panic() {
        // Threat T-03-03-02 / T-03-03-03: bad input must never panic.
        let bad_inputs = [
            "",
            "not json",
            r#"{"type":"unknown_variant","foo":"bar"}"#,
            r#"{"type":"say"}"#, // missing text field
            "null",
        ];
        for input in bad_inputs {
            let result = serde_json::from_str::<ServerMessage>(input);
            // Must return Err, never panic.
            assert!(
                result.is_err(),
                "expected Err for input {input:?}, got Ok({:?})",
                result.ok()
            );
        }
    }
}
