//! ffoie-protocol — shared wire types for client ↔ server chat.
//!
//! Compiled for both native and wasm32-unknown-unknown.
//! No platform-specific dependencies, no tokio, no chrono.

use serde::{Deserialize, Serialize};

// ── Supporting enums ──────────────────────────────────────────────────────────

/// Player team assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Team {
    None,
    Red,
    Blue,
}

/// Chat channel scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    All,
    Team,
}

// ── Supporting structs ────────────────────────────────────────────────────────

/// A single chat message as stored and transmitted.
///
/// `ts` is Unix time in milliseconds (u64 avoids chrono, which is not
/// wasm32-friendly in all configurations).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub from: String,
    pub team: Team,
    pub channel: Channel,
    pub text: String,
    /// Unix timestamp in milliseconds.
    pub ts: u64,
}

/// A single entry in the WhoList response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerEntry {
    pub nick: String,
    pub team: Team,
}

// ── Client → Server ───────────────────────────────────────────────────────────

/// Messages sent by the game client over the WebSocket.
///
/// Wire format (internally tagged):
///   `{"type":"say","text":"hello"}`
///   `{"type":"connect","nickname":"Fox","team":"red"}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message after WS open — associates nickname and team.
    Connect { nickname: String, team: Team },
    /// Global (all-chat) message. Equivalent to Quake `say`.
    Say { text: String },
    /// Team-filtered message. Equivalent to Quake `say_team`.
    SayTeam { text: String },
    /// Request the current player list; server replies with WhoList.
    Who,
    /// Keep-alive / round-trip latency probe.
    Ping { seq: u32 },
}

// ── Server → Client ───────────────────────────────────────────────────────────

/// Messages sent by the server to connected clients.
///
/// Wire format (internally tagged):
///   `{"type":"pong","seq":42}`
///   `{"type":"rate_limited","reason":"too fast"}`
///
/// Note: `Message` uses a struct variant (not tuple variant) because serde's
/// internally-tagged representation (`tag = "type"`) does not support
/// tuple (newtype) variants — it requires `content` for adjacently-tagged
/// layout.  Using a struct variant keeps `tag = "type"` and produces:
///   `{"type":"message","data":{...}}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent immediately after the server accepts a Connect.
    Welcome {
        assigned_nick: String,
        assigned_team: Team,
        motd: String,
        scrollback: Vec<ChatMessage>,
    },
    /// A chat message broadcast to this client.
    Message { data: ChatMessage },
    /// A player joined or left.
    JoinedLeft {
        nickname: String,
        team: Team,
        joined: bool,
    },
    /// Response to ClientMessage::Who.
    WhoList { players: Vec<PlayerEntry> },
    /// Client is sending too fast.
    RateLimited { reason: String },
    /// Reply to ClientMessage::Ping.
    Pong { seq: u32 },
    /// Protocol or policy error.
    Error { reason: String },
}
