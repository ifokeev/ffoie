//! wasm.rs — wasm32 WebSocket client for the FFOIE engine.
//!
//! This module is **wasm32-only** (gated by `#![cfg(target_arch = "wasm32")]`).
//! The native client path lives in `native.rs`.
//!
//! # Architecture
//!
//! Calling [`start`] registers a `visibilitychange` DOM listener, creates
//! `std::sync::mpsc` channels, and launches the async network loop via
//! `wasm_bindgen_futures::spawn_local`.  `start` returns immediately with a
//! [`NetworkHandle`]; the loop runs in the browser's microtask queue.
//!
//! The loop:
//!   1. Connects via `ewebsock::connect` (browser WebSocket).
//!   2. Sends `ClientMessage::Connect` after receiving `WsEvent::Opened`.
//!   3. Polls for WS events and commands every 10 ms
//!      (`gloo_timers::future::TimeoutFuture`).
//!   4. On disconnect, waits a jittered exponential back-off delay, then
//!      reconnects — forever.
//!   5. When the tab becomes visible (via `visibilitychange`), any ongoing
//!      back-off sleep is aborted so the reconnect fires immediately.
//!
//! # Visibility-change reconnect data flow
//!
//! ```text
//! visibilitychange callback   ──sets──►  Rc<RefCell<bool>> reconnect_flag
//!                                              │
//!                             network_loop polls │ during each backoff sleep
//!                             iteration; clears it and breaks early
//! ```
//!
//! The `Rc<RefCell<bool>>` is safe here because wasm32 is single-threaded;
//! the callback and the async loop both run on the same JS thread.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};

use ewebsock::{WsEvent, WsMessage, WsSender};
use ffoie_protocol::{ClientMessage, ServerMessage, Team};
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use winit::event_loop::EventLoopProxy;

use super::{AppEvent, NetworkCommand, NetworkEvent, NetworkHandle};

// ── Backoff math ──────────────────────────────────────────────────────────────

/// Full-jitter exponential back-off delay for a given attempt.
///
/// - Attempt 1: 0 ms (connect immediately).
/// - Attempt 2+: `delay = Math.random() * base_ms` where
///   `base_ms = min(30_000, 1000 * 2^(attempt-1))`.
///
/// Uses `js_sys::Math::random()` for jitter (fastrand is native-only).
fn backoff_delay_ms(attempt: u32) -> u32 {
    if attempt <= 1 {
        return 0;
    }
    let exponent = (attempt - 1).min(14);
    let base_ms = (1_000u32 << exponent).min(30_000);
    // js_sys::Math::random() returns f64 in [0.0, 1.0). Scale by (base_ms + 1)
    // and clamp so the result is inclusive [0, base_ms], matching native's
    // `fastrand::u64(0..=base_ms)`.
    ((js_sys::Math::random() * (base_ms as f64 + 1.0)) as u32).min(base_ms)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Serialize a `ClientMessage` and send it over the WebSocket.
fn ws_send(ws_tx: &mut WsSender, msg: &ClientMessage) {
    match serde_json::to_string(msg) {
        Ok(json) => ws_tx.send(WsMessage::Text(json)),
        Err(e) => log::warn!("[network/wasm] failed to serialize {:?}: {e}", msg),
    }
}

/// Translate a `ServerMessage` into a `NetworkEvent` and push it to the main
/// thread.  Returns `false` if the channel is disconnected (main thread gone).
fn dispatch(server_msg: ServerMessage, event_tx: &Sender<NetworkEvent>) -> bool {
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
        ServerMessage::Pong { seq } => {
            log::debug!("[network/wasm] pong seq={seq}");
            return true;
        }
        ServerMessage::RateLimited { reason } => {
            log::warn!("[network/wasm] rate limited: {reason}");
            return true;
        }
        ServerMessage::Error { reason } => {
            log::warn!("[network/wasm] server error: {reason}");
            return true;
        }
    };

    // T-04-02-01: serde_json returns Err on malformed input (handled at call
    // site); here we only push well-formed events.
    event_tx.send(event).is_ok()
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Start the wasm network loop and return a [`NetworkHandle`].
///
/// The chat URL is baked in at compile time via `option_env!("FFOIE_CHAT_URL")`
/// (T-04-02-02: the URL is a public endpoint, not a secret).  For production
/// builds pass `FFOIE_CHAT_URL=wss://ffoie.net/ws` to the Trunk command.
///
/// The `proxy` parameter is accepted for API parity with `native::start` but
/// is not used — on wasm32 the loop runs at rAF rate without needing to wake
/// the winit event loop.
pub fn start(_proxy: EventLoopProxy<AppEvent>) -> NetworkHandle {
    // D-01: compile-time URL bake-in (option_env! reads at build time).
    let url = option_env!("FFOIE_CHAT_URL")
        .unwrap_or("ws://localhost:8080/ws")
        .to_string();

    // Nickname: runtime env::var is unavailable on wasm32; use "guest" as the
    // anonymous default.  A future enhancement can read from a URL query param.
    let nick = "guest".to_string();

    let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<NetworkCommand>();

    // ── Visibility-change reconnect flag ──────────────────────────────────
    // Shared between the DOM callback and the async network loop.
    // When the tab becomes visible, the callback sets this to `true`; the
    // network loop checks it during each back-off sleep iteration and aborts
    // the sleep early if set (T-04-02-03: reconnect is debounced by the
    // existing back-off math — the flag is checked once per sleep tick, not
    // triggered in a tight loop).
    let reconnect_flag: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    // Register visibilitychange listener on `document`.
    {
        let flag_for_closure = reconnect_flag.clone();
        // Box<dyn Fn()> is required by Closure::wrap; the callback has no args.
        let closure = Closure::wrap(Box::new(move || {
            // document.hidden == false means the tab is now visible.
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                if !doc.hidden() {
                    *flag_for_closure.borrow_mut() = true;
                }
            }
        }) as Box<dyn Fn()>);

        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback(
                "visibilitychange",
                closure.as_ref().unchecked_ref(),
            );
        }
        // Leak the closure so it lives for the entire page lifetime.
        // The listener is registered once on startup; there is no teardown path.
        closure.forget();
    }

    spawn_local(async move {
        network_loop(url, nick, event_tx, cmd_rx, reconnect_flag).await;
    });

    NetworkHandle {
        tx: cmd_tx,
        rx: event_rx,
    }
}

// ── Core async loop ───────────────────────────────────────────────────────────

/// The async network loop running inside `spawn_local`.
///
/// Outer loop: exponential back-off reconnect.
/// Inner loop (per connection): WS receive + command dispatch (10 ms tick).
async fn network_loop(
    url: String,
    nick: String,
    event_tx: Sender<NetworkEvent>,
    cmd_rx: Receiver<NetworkCommand>,
    reconnect_flag: Rc<RefCell<bool>>,
) {
    let mut attempt: u32 = 0;

    'reconnect: loop {
        attempt += 1;

        // ── Back-off delay ─────────────────────────────────────────────────
        let delay_ms = backoff_delay_ms(attempt);
        if delay_ms > 0 {
            log::info!("[network/wasm] reconnecting (attempt {attempt}), waiting {delay_ms}ms");
            if event_tx
                .send(NetworkEvent::Reconnecting {
                    attempt,
                    delay_ms: delay_ms as u64,
                })
                .is_err()
            {
                break 'reconnect; // main thread exited
            }

            // Sleep in 50 ms chunks so we can react to visibilitychange quickly.
            // T-04-02-03: the flag is checked once per chunk — no tight spin.
            let mut elapsed: u32 = 0;
            while elapsed < delay_ms {
                // Check the visibility flag; if set, reconnect immediately.
                if *reconnect_flag.borrow() {
                    *reconnect_flag.borrow_mut() = false;
                    log::info!("[network/wasm] tab visible — bypassing backoff");
                    break;
                }
                let chunk = 50u32.min(delay_ms - elapsed);
                TimeoutFuture::new(chunk).await;
                elapsed += chunk;
            }
            // Clear the flag regardless (may have been set during the last chunk).
            *reconnect_flag.borrow_mut() = false;
        }

        // ── Connect ────────────────────────────────────────────────────────
        log::info!("[network/wasm] connecting to {url} (attempt {attempt})");
        let connect_result = ewebsock::connect(&url, ewebsock::Options::default());

        let (mut ws_tx, ws_rx) = match connect_result {
            Ok(pair) => pair,
            Err(e) => {
                log::warn!("[network/wasm] connect failed: {e}");
                continue 'reconnect;
            }
        };

        // ── Wait for WsEvent::Opened ───────────────────────────────────────
        // NOTE: we deliberately do NOT reset `attempt` before/after the open
        // check. Resetting before `wait_for_open` let a flapping server (accepts
        // then closes before Opened) drive a zero-delay reconnect storm. After a
        // *healthy* session the backoff is reset by the Closed/Error arms in the
        // per-connection loop below, so the "reconnect promptly after a good
        // session" behaviour is preserved without a redundant reset here.
        if !wait_for_open(&ws_rx).await {
            let _ = event_tx.send(NetworkEvent::Disconnected);
            continue 'reconnect;
        }

        // ── Send Connect handshake ─────────────────────────────────────────
        ws_send(
            &mut ws_tx,
            &ClientMessage::Connect {
                nickname: nick.clone(),
                team: Team::None,
            },
        );

        // ── Per-connection poll loop ───────────────────────────────────────
        // No tokio::select! on wasm32; instead we alternate between draining
        // the WS receiver and draining the command channel each tick.
        // Note: heartbeat is intentionally omitted for v1.1 on wasm —
        // the server's ping timeout catches stale connections, and
        // visibilitychange reconnect covers tab-hide recovery.  Wasm heartbeat
        // is deferred to v1.1.x.
        loop {
            // 1. Drain incoming WS events (up to 32 per tick).
            let mut ws_count = 0usize;
            loop {
                if ws_count >= 32 {
                    break;
                }
                match ws_rx.try_recv() {
                    Some(WsEvent::Message(WsMessage::Text(json))) => {
                        ws_count += 1;
                        // T-04-02-01: serde_json::from_str returns Err on
                        // malformed input; log and continue, never panic.
                        match serde_json::from_str::<ServerMessage>(&json) {
                            Ok(msg) => {
                                if !dispatch(msg, &event_tx) {
                                    break 'reconnect; // main thread gone
                                }
                            }
                            Err(e) => {
                                log::warn!("[network/wasm] invalid JSON: {e}");
                            }
                        }
                    }
                    Some(WsEvent::Closed) => {
                        log::info!("[network/wasm] WS closed");
                        let _ = event_tx.send(NetworkEvent::Disconnected);
                        attempt = 0; // incremented at top of reconnect loop
                        continue 'reconnect;
                    }
                    Some(WsEvent::Error(e)) => {
                        log::warn!("[network/wasm] WS error: {e}");
                        let _ = event_tx.send(NetworkEvent::Disconnected);
                        attempt = 0;
                        continue 'reconnect;
                    }
                    Some(WsEvent::Opened) => {
                        // May arrive again; safe to ignore.
                    }
                    Some(_) => {
                        // Unknown variant — ignore.
                    }
                    None => break, // no more events this tick
                }
            }

            // 2. Drain outgoing command channel.
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
                        log::info!("[network/wasm] shutdown command received");
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        log::info!("[network/wasm] command channel closed — shutting down");
                        return;
                    }
                }
            }

            // 3. Yield to the JS event loop (10 ms keeps chat responsive
            //    without burning CPU at rAF rate).
            TimeoutFuture::new(10).await;
        }
    }

    log::info!("[network/wasm] network_loop exiting");
}

/// Poll `ws_rx` until `WsEvent::Opened`, `WsEvent::Closed`, or
/// `WsEvent::Error`.  Returns `true` if the connection was opened.
async fn wait_for_open(ws_rx: &ewebsock::WsReceiver) -> bool {
    // Bounded by a 10s deadline so a half-open connect can't stall the reconnect
    // loop (or block a pending Shutdown) indefinitely.
    let mut waited_ms: u32 = 0;
    loop {
        match ws_rx.try_recv() {
            Some(WsEvent::Opened) => return true,
            Some(WsEvent::Closed) | Some(WsEvent::Error(_)) => return false,
            Some(_) => {}
            None => {
                if waited_ms >= 10_000 {
                    log::warn!("[network/wasm] timed out waiting for WS to open");
                    return false;
                }
                TimeoutFuture::new(5).await;
                waited_ms += 5;
            }
        }
    }
}
