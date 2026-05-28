//! Shared server state — AppState, BroadcastEvent, ConnInfo.
//!
//! AppState is cheaply clonable (all fields are Arc or Copy) and is threaded
//! through every axum handler + WebSocket task via axum's `State` extractor.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use ffoie_protocol::{ChatMessage, PlayerEntry, ServerMessage, Team};

use crate::config::Config;

// ── Broadcast event ───────────────────────────────────────────────────────────

/// A value published on the broadcast channel.
///
/// `team_filter` is `Some(team)` for SayTeam events; receivers whose team
/// does not match silently drop the event without re-serializing.
#[derive(Debug, Clone)]
pub struct BroadcastEvent {
    pub msg: ServerMessage,
    pub team_filter: Option<Team>,
}

// ── Connection info ───────────────────────────────────────────────────────────

/// Per-connection metadata stored in `AppState::connections`.
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub session_id: Uuid,
    pub nickname: String,
    pub team: Team,
}

// ── AppState ──────────────────────────────────────────────────────────────────

/// Shared state threaded through all axum handlers and WS tasks.
///
/// All fields are `Arc`-wrapped (or cheaply clonable) so `Clone` is O(1) —
/// axum clones this per-request when calling `State` extractors.
///
/// `task_tracker` is used by ws.rs to register per-connection tasks so that
/// main.rs can await their completion on graceful shutdown.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub broadcast_tx: broadcast::Sender<Arc<BroadcastEvent>>,
    pub scrollback: Arc<Mutex<VecDeque<ChatMessage>>>,
    pub connections: Arc<Mutex<HashMap<Uuid, ConnInfo>>>,
    pub cancellation_token: CancellationToken,
    /// Tracks all spawned WebSocket handler tasks.
    ///
    /// `ws.rs` spawns tasks via `state.task_tracker.spawn(...)` so that the
    /// shutdown path in `main.rs` can call `task_tracker.close()` +
    /// `task_tracker.wait()` (with a 5-second hard timeout) to drain them.
    pub task_tracker: TaskTracker,
    pub started_at: Instant,
}

impl AppState {
    /// Construct a fresh AppState.
    ///
    /// Creates a broadcast channel with `config.broadcast_capacity` slots
    /// (must be a power of two — the default 1024 satisfies this).
    pub fn new(config: Arc<Config>) -> Self {
        let (broadcast_tx, _) = broadcast::channel(config.broadcast_capacity);
        Self {
            config,
            broadcast_tx,
            scrollback: Arc::new(Mutex::new(VecDeque::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
            cancellation_token: CancellationToken::new(),
            task_tracker: TaskTracker::new(),
            started_at: Instant::now(),
        }
    }

    // ── Connection registry ───────────────────────────────────────────────────

    /// Return the current number of open connections.
    pub fn connection_count(&self) -> usize {
        self.connections.lock().len()
    }

    /// Register a new connection.  Logs the updated count.
    pub fn register_conn(&self, info: ConnInfo) {
        let mut map = self.connections.lock();
        map.insert(info.session_id, info);
        tracing::info!(connections = map.len(), "connection registered");
    }

    /// Remove a connection by session ID.  Logs the updated count.
    pub fn remove_conn(&self, session_id: Uuid) {
        let mut map = self.connections.lock();
        map.remove(&session_id);
        tracing::info!(connections = map.len(), "connection removed");
    }

    // ── WhoList ───────────────────────────────────────────────────────────────

    /// Return a snapshot of currently connected players (for WhoList replies).
    pub fn who_list(&self) -> Vec<PlayerEntry> {
        self.connections
            .lock()
            .values()
            .map(|c| PlayerEntry {
                nick: c.nickname.clone(),
                team: c.team,
            })
            .collect()
    }

    // ── Scrollback ring ───────────────────────────────────────────────────────

    /// Clone the entire scrollback ring into a `Vec` for Welcome delivery.
    pub fn scrollback_snapshot(&self) -> Vec<ChatMessage> {
        self.scrollback.lock().iter().cloned().collect()
    }

    /// Push a message onto the scrollback ring.
    ///
    /// Evicts the oldest entry when the ring is at capacity so it never
    /// grows beyond `config.scrollback_size`.
    pub fn push_scrollback(&self, msg: ChatMessage) {
        let mut ring = self.scrollback.lock();
        ring.push_back(msg);
        if ring.len() > self.config.scrollback_size {
            ring.pop_front();
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ffoie_protocol::{Channel, ServerMessage};

    fn test_config() -> Arc<Config> {
        Arc::new(Config::from_env().unwrap())
    }

    fn make_msg(from: &str) -> ChatMessage {
        ChatMessage {
            from: from.to_string(),
            team: Team::None,
            channel: Channel::All,
            text: "hi".to_string(),
            ts: 0,
        }
    }

    #[test]
    fn scrollback_eviction() {
        let config = test_config();
        let state = AppState::new(config.clone());
        let capacity = config.scrollback_size; // default 50

        // Push capacity + 1 messages.
        for i in 0..=capacity {
            state.push_scrollback(make_msg(&format!("user{i}")));
        }

        let snap = state.scrollback_snapshot();
        assert_eq!(snap.len(), capacity, "ring should be at capacity after eviction");
        // The first message ("user0") must have been evicted.
        assert!(
            !snap.iter().any(|m| m.from == "user0"),
            "first message should have been evicted"
        );
        // The last message ("user{capacity}") must still be present.
        assert!(
            snap.iter().any(|m| m.from == format!("user{capacity}")),
            "last message should be present"
        );
    }

    #[test]
    fn who_list_returns_all_registered() {
        let state = AppState::new(test_config());

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        state.register_conn(ConnInfo { session_id: id1, nickname: "Alice".into(), team: Team::Red });
        state.register_conn(ConnInfo { session_id: id2, nickname: "Bob".into(), team: Team::Blue });

        let list = state.who_list();
        assert_eq!(list.len(), 2);
        let nicks: Vec<_> = list.iter().map(|p| p.nick.as_str()).collect();
        assert!(nicks.contains(&"Alice"));
        assert!(nicks.contains(&"Bob"));
    }

    #[test]
    fn remove_conn_decrements_count() {
        let state = AppState::new(test_config());
        let id = Uuid::new_v4();
        state.register_conn(ConnInfo { session_id: id, nickname: "Ghost".into(), team: Team::Red });
        assert_eq!(state.connection_count(), 1);
        state.remove_conn(id);
        assert_eq!(state.connection_count(), 0);
    }

    #[test]
    fn broadcast_event_is_clone() {
        // Compile-time check that BroadcastEvent implements Clone.
        let ev = BroadcastEvent {
            msg: ServerMessage::Pong { seq: 1 },
            team_filter: Some(Team::Red),
        };
        let _ = ev.clone();
    }
}
