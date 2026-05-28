//! types.rs — Platform-neutral type definitions for the FFOIE network module.
//!
//! These types are compiled on **all** targets (native and wasm32).
//! The runtime implementations live in `native.rs` (tokio) and will be
//! added to `wasm.rs` (wasm32) in Phase 4 plan 04-02.

use std::sync::mpsc::{Receiver, Sender};

use ffoie_protocol::{ChatMessage, PlayerEntry, Team};

// ── App event (winit type parameter) ─────────────────────────────────────────

/// User-defined winit event for waking the main loop from `ControlFlow::Wait`.
///
/// The event loop must be created as `EventLoop::<AppEvent>::new()` (plan
/// 03-05 performs that wiring).  The network background thread sends
/// `ChatWakeup` through the `EventLoopProxy` whenever a new [`NetworkEvent`]
/// is pushed, so the main thread wakes immediately instead of waiting for the
/// next timer tick.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Signals the main thread that at least one [`NetworkEvent`] is ready.
    ChatWakeup,
}

// ── Network events (background → main thread) ─────────────────────────────────

/// Events emitted by the network background thread and consumed by the main thread.
///
/// Drain at most 32 per frame with `Receiver::try_recv` to bound per-frame
/// work (see PITFALLS.md Pitfall 13 / CONTEXT.md drain-cap decision).
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// Server accepted our `Connect`; carries the session bootstrap data.
    Connected {
        assigned_nick: String,
        assigned_team: Team,
        motd: String,
        scrollback: Vec<ChatMessage>,
    },
    /// An ordinary chat message broadcast to this client.
    Message(ChatMessage),
    /// A player joined or left.
    JoinedLeft {
        nickname: String,
        team: Team,
        joined: bool,
    },
    /// Response to `NetworkCommand::Who`.
    WhoList(Vec<PlayerEntry>),
    /// The reconnect loop is waiting before the next attempt.
    Reconnecting { attempt: u32, delay_ms: u64 },
    /// WebSocket connection was closed (raised before `Reconnecting`).
    Disconnected,
}

// ── Network commands (main thread → background) ────────────────────────────────

/// Commands the main thread sends to the network background thread.
#[derive(Debug, Clone)]
pub enum NetworkCommand {
    /// Send a global (all-chat) message.
    Say(String),
    /// Send a team-filtered message.
    SayTeam(String),
    /// Request the current player list.
    Who,
    /// Shut down the background thread cleanly.
    Shutdown,
}

// ── Public handle ─────────────────────────────────────────────────────────────

/// Owned handle returned by [`super::start`].
///
/// The caller keeps `rx` (drains [`NetworkEvent`]s each frame) and
/// `tx` (sends [`NetworkCommand`]s from keyboard input etc.).
pub struct NetworkHandle {
    /// Send commands to the background thread.
    pub tx: Sender<NetworkCommand>,
    /// Receive events from the background thread (non-blocking `try_recv`).
    pub rx: Receiver<NetworkEvent>,
}
