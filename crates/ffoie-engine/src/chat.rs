//! chat.rs — Chat HUD state and egui panel renderer.
//!
//! This module owns:
//!   - [`ChatState`]: the message ring buffer, input buffer, connection status,
//!     and player identity (nick / team).
//!   - [`drain_network`] (native only): drains up to 32 [`NetworkEvent`]s per
//!     frame and applies them to [`ChatState`].
//!   - [`render_panel`] (native only): draws the chat bottom panel with egui
//!     and returns a [`NetworkCommand`] when the user submits input.
//!   - [`parse_colors`]: parses Quake `^N` color codes into `(Color32, String)`
//!     spans for rendering.
//!
//! # Platform notes
//!
//! The [`drain_network`] and [`render_panel`] functions reference
//! [`crate::network::NetworkCommand`] / [`crate::network::NetworkEvent`],
//! which live in a `#[cfg(not(target_arch = "wasm32"))]`-gated module.
//! Therefore those two functions are also gated.  [`ChatState`],
//! [`ConnectionStatus`], and [`parse_colors`] compile on all platforms.
//!
//! Phase 4 will add a wasm32 network path and lift the gate.

use std::collections::VecDeque;

use egui::Color32;
use ffoie_protocol::{Channel, ChatMessage, Team};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of chat messages kept in the ring buffer.
pub const CHAT_HISTORY_LEN: usize = 200;

/// Maximum number of system messages (MOTD, join/leave, /who results) to keep.
const SYSTEM_HISTORY_LEN: usize = 50;

/// Maximum [`NetworkEvent`]s drained per frame (per PITFALLS.md T-03-04-02).
const DRAIN_CAP: usize = 32;

// ── Connection status ─────────────────────────────────────────────────────────

/// Connection health visible to the HUD.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    /// No connected yet (initial state or post-disconnect before first retry).
    Disconnected,
    /// Connecting for the first time (or trying silently after drop).
    Connecting,
    /// Handshake accepted; session live.
    Connected,
    /// Reconnect loop is waiting before the next attempt.
    Reconnecting { attempt: u32, delay_ms: u64 },
}

// ── Chat state ────────────────────────────────────────────────────────────────

/// All mutable state for the chat HUD.
///
/// Owned by the engine's main `State` struct after plan 03-05 integration.
/// Constructed with [`ChatState::new`].
pub struct ChatState {
    /// Ring buffer of received chat messages (newest at back, oldest at front).
    /// Trimmed to [`CHAT_HISTORY_LEN`] on every insert.
    pub messages: VecDeque<ChatMessage>,
    /// System / server messages (joins, leaves, /who results, MOTD).
    /// Trimmed to 50 on every insert.
    pub system_messages: VecDeque<String>,
    /// Current text in the input box.
    pub input_buffer: String,
    /// Whether the chat input box is visible and accepting keystrokes.
    pub chat_active: bool,
    /// Channel selected for the next outbound message (`All` or `Team`).
    pub channel: Channel,
    /// Server-assigned nickname (set on `Connected`).
    pub nickname: Option<String>,
    /// Server-assigned team (set on `Connected`).
    pub team: Option<Team>,
    /// Current connection health.
    pub connection_status: ConnectionStatus,
    /// Monotonically-increasing ping sequence number (for heartbeat tracking).
    #[allow(dead_code)] // field written by network layer; read access deferred to v1.1.x missed-pong detection
    pub(crate) ping_seq: u32,
}

impl ChatState {
    /// Create a new [`ChatState`] with sensible defaults.
    pub fn new() -> Self {
        ChatState {
            messages: VecDeque::with_capacity(CHAT_HISTORY_LEN),
            system_messages: VecDeque::with_capacity(SYSTEM_HISTORY_LEN),
            input_buffer: String::new(),
            chat_active: false,
            channel: Channel::All,
            nickname: None,
            team: None,
            connection_status: ConnectionStatus::Disconnected,
            ping_seq: 0,
        }
    }

    /// Open the chat input for the given channel.
    ///
    /// Clears any leftover text from a previous session and sets `chat_active`.
    pub fn open_input(&mut self, channel: Channel) {
        self.channel = channel;
        self.input_buffer.clear();
        self.chat_active = true;
    }

    /// Close the chat input without sending.
    pub fn close_input(&mut self) {
        self.chat_active = false;
        self.input_buffer.clear();
    }

    /// Push a chat message into the ring buffer, evicting the oldest if full.
    fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push_back(msg);
        if self.messages.len() > CHAT_HISTORY_LEN {
            self.messages.pop_front();
        }
    }

    /// Push a system message, evicting the oldest if full.
    fn push_system(&mut self, text: String) {
        self.system_messages.push_back(text);
        if self.system_messages.len() > SYSTEM_HISTORY_LEN {
            self.system_messages.pop_front();
        }
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Network drain + egui panel (all platforms after Phase 4) ─────────────────

pub use native::drain_network;
pub use native::render_panel;
pub use native::submit;

mod native {
    use super::{ChatState, ConnectionStatus, DRAIN_CAP};
    use crate::network::{NetworkCommand, NetworkEvent};
    use egui::Color32;
    use ffoie_protocol::{Channel, Team};
    use std::sync::mpsc;

    // ── drain_network ─────────────────────────────────────────────────────────

    /// Drain at most [`DRAIN_CAP`] [`NetworkEvent`]s from `rx` and apply them
    /// to `state`.
    ///
    /// Returns the list of events consumed (callers may log them if desired).
    ///
    /// # Event handling
    ///
    /// | Event | Effect |
    /// |-------|--------|
    /// | `Connected` | Sets nick/team/status, pushes MOTD + scrollback |
    /// | `Message` | Pushes to the ring buffer |
    /// | `JoinedLeft` | Pushes a system notification |
    /// | `WhoList` | Pushes a formatted player-list system message |
    /// | `Reconnecting` | Updates `connection_status` |
    /// | `Disconnected` | Resets `connection_status` to `Connecting` |
    pub fn drain_network(
        rx: &mpsc::Receiver<NetworkEvent>,
        state: &mut ChatState,
    ) -> Vec<NetworkEvent> {
        let mut consumed = Vec::new();
        let mut count = 0usize;

        while count < DRAIN_CAP {
            match rx.try_recv() {
                Ok(event) => {
                    count += 1;
                    apply_event(&event, state);
                    consumed.push(event);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        consumed
    }

    /// Apply a single [`NetworkEvent`] to [`ChatState`].
    fn apply_event(event: &NetworkEvent, state: &mut ChatState) {
        match event {
            NetworkEvent::Connected {
                assigned_nick,
                assigned_team,
                motd,
                scrollback,
            } => {
                state.nickname = Some(assigned_nick.clone());
                state.team = Some(*assigned_team);
                state.connection_status = ConnectionStatus::Connected;

                // MOTD first, then scrollback (oldest → newest order preserved
                // because the server sends the buffer oldest-first already).
                state.push_system(format!("[server] {}", motd));
                for msg in scrollback {
                    state.push_message(msg.clone());
                }
            }
            NetworkEvent::Message(msg) => {
                state.push_message(msg.clone());
            }
            NetworkEvent::JoinedLeft {
                nickname,
                team,
                joined,
            } => {
                let action = if *joined { "joined" } else { "left" };
                let team_str = team_label(*team);
                state.push_system(format!(
                    "[server] {} ({}) {}",
                    nickname, team_str, action
                ));
            }
            NetworkEvent::WhoList(players) => {
                let list = players
                    .iter()
                    .map(|p| format!("{} ({})", p.nick, team_label(p.team)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let n = players.len();
                state.push_system(format!(
                    "[server] {} player{}: {}",
                    n,
                    if n == 1 { "" } else { "s" },
                    list
                ));
            }
            NetworkEvent::Reconnecting { attempt, delay_ms } => {
                state.connection_status = ConnectionStatus::Reconnecting {
                    attempt: *attempt,
                    delay_ms: *delay_ms,
                };
            }
            NetworkEvent::Disconnected => {
                state.connection_status = ConnectionStatus::Connecting;
            }
        }
    }

    // ── submit ────────────────────────────────────────────────────────────────

    /// Consume the current `input_buffer` and produce a [`NetworkCommand`].
    ///
    /// - `/who` (case-insensitive, any trailing chars) → `NetworkCommand::Who`
    /// - Non-empty text + `Channel::All` → `NetworkCommand::Say(text)`
    /// - Non-empty text + `Channel::Team` → `NetworkCommand::SayTeam(text)`
    /// - Empty input → `None`
    ///
    /// Always clears `input_buffer` and closes `chat_active` regardless of
    /// whether a command is produced (empty input = cancel = close).
    pub fn submit(state: &mut ChatState) -> Option<NetworkCommand> {
        let text = state.input_buffer.trim().to_string();
        state.input_buffer.clear();
        state.chat_active = false;

        if text.is_empty() {
            return None;
        }

        if text.to_ascii_lowercase().starts_with("/who") {
            return Some(NetworkCommand::Who);
        }

        Some(match state.channel {
            Channel::All => NetworkCommand::Say(text),
            Channel::Team => NetworkCommand::SayTeam(text),
        })
    }

    // ── render_panel ──────────────────────────────────────────────────────────

    /// Draw the chat bottom panel via egui.
    ///
    /// Returns `Some(NetworkCommand)` if the user submitted a message while
    /// this frame was being rendered (Enter pressed, input non-empty).
    ///
    /// Called once per frame inside the egui run closure.
    pub fn render_panel(ctx: &egui::Context, state: &mut ChatState) -> Option<NetworkCommand> {
        let mut pending_command: Option<NetworkCommand> = None;

        let window_height = ctx.content_rect().height();
        let has_content =
            state.chat_active || !state.messages.is_empty() || !state.system_messages.is_empty();
        let panel_height = if state.chat_active {
            (window_height * 0.30).max(80.0)
        } else {
            22.0
        };

        #[allow(deprecated)]
        egui::TopBottomPanel::bottom("chat")
            .exact_size(panel_height)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 178))
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 4,
                        bottom: 4,
                    }),
            )
            .show(ctx, |ui| {
                // ── Reconnecting badge ───────────────────────────────────────
                if let ConnectionStatus::Reconnecting { attempt, .. } =
                    &state.connection_status
                {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::TOP),
                        |ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 200, 60),
                                format!("Reconnecting… (attempt {})", attempt),
                            );
                        },
                    );
                }

                // ── Header: nick + team indicator ────────────────────────────
                if let (Some(nick), Some(team)) = (&state.nickname, &state.team) {
                    let team_color = team_to_color(*team);
                    ui.horizontal(|ui| {
                        ui.colored_label(team_color, format!("[{}]", nick));
                        if !state.chat_active {
                            ui.colored_label(
                                egui::Color32::from_gray(120),
                                "T: chat  Y: team",
                            );
                        }
                    });
                } else if matches!(
                    state.connection_status,
                    ConnectionStatus::Connecting | ConnectionStatus::Disconnected
                ) {
                    ui.colored_label(egui::Color32::from_gray(120), "Connecting…");
                }

                // ── Message scroll area ──────────────────────────────────────
                if has_content {
                    let scroll_height = if state.chat_active {
                        (panel_height - 48.0).max(0.0)
                    } else {
                        0.0 // collapsed; no scroll area needed
                    };

                    if state.chat_active {
                        egui::ScrollArea::vertical()
                            .max_height(scroll_height)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                // System messages in subdued gray.
                                for sys in state.system_messages.iter() {
                                    ui.colored_label(
                                        egui::Color32::from_gray(160),
                                        sys,
                                    );
                                }
                                // Chat messages with Quake color codes.
                                for msg in state.messages.iter() {
                                    let sender_color = team_to_color(msg.team);
                                    let channel_tag = match msg.channel {
                                        Channel::All => "",
                                        Channel::Team => "[TEAM] ",
                                    };
                                    ui.horizontal_wrapped(|ui| {
                                        ui.colored_label(
                                            sender_color,
                                            format!("{}{}: ", channel_tag, msg.from),
                                        );
                                        render_colored_text(ui, &msg.text);
                                    });
                                }
                            });
                    }
                }

                // ── Input box (only when chat_active) ────────────────────────
                if state.chat_active {
                    let (label, label_color) = match state.channel {
                        Channel::All => (
                            "[ALL] ",
                            egui::Color32::from_gray(200),
                        ),
                        Channel::Team => (
                            "[TEAM] ",
                            team_to_color(state.team.unwrap_or(Team::None)),
                        ),
                    };

                    ui.horizontal(|ui| {
                        ui.colored_label(label_color, label);
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut state.input_buffer)
                                .desired_width(ui.available_width())
                                .hint_text("type message…"),
                        );
                        response.request_focus();

                        if response.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            let text = state.input_buffer.trim().to_string();
                            if !text.is_empty() {
                                if text.to_ascii_lowercase().starts_with("/who") {
                                    pending_command = Some(NetworkCommand::Who);
                                } else {
                                    pending_command = Some(match state.channel {
                                        Channel::All => NetworkCommand::Say(text),
                                        Channel::Team => NetworkCommand::SayTeam(text),
                                    });
                                }
                            }
                            state.input_buffer.clear();
                            state.chat_active = false;
                        }
                    });
                }
            });

        pending_command
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Map a [`Team`] to an egui display color.
    fn team_to_color(team: Team) -> Color32 {
        match team {
            Team::Red => Color32::from_rgb(220, 80, 80),
            Team::Blue => Color32::from_rgb(80, 140, 220),
            Team::None => Color32::from_rgb(200, 200, 200),
        }
    }

    /// Short human-readable team label (used in system messages).
    fn team_label(team: Team) -> &'static str {
        match team {
            Team::Red => "red",
            Team::Blue => "blue",
            Team::None => "spectator",
        }
    }

    /// Render `text` as colored spans via [`super::parse_colors`].
    ///
    /// Spans are laid out on one line inside the caller's horizontal group.
    fn render_colored_text(ui: &mut egui::Ui, text: &str) {
        for (color, span) in super::parse_colors(text) {
            if !span.is_empty() {
                ui.label(egui::RichText::new(span).color(color));
            }
        }
    }
}

// ── Quake color code parser (all platforms) ───────────────────────────────────

/// Parse a Quake `^N` color-coded string into `(Color32, String)` spans.
///
/// # Color mapping
///
/// | Code | Color | RGB |
/// |------|-------|-----|
/// | `^0` | Black | (25, 25, 25) |
/// | `^1` | Red | (215, 15, 15) |
/// | `^2` | Green | (15, 200, 15) |
/// | `^3` | Yellow | (220, 220, 15) |
/// | `^4` | Blue | (15, 15, 200) |
/// | `^5` | Cyan | (15, 210, 210) |
/// | `^6` | Magenta | (210, 15, 210) |
/// | `^7` | White | (220, 220, 220) |
///
/// # Behaviour for edge cases
///
/// - **Plain text** with no codes → one span, default white.
/// - **Trailing `^`** at end-of-string → emitted as a literal `^` character.
/// - **Unknown `^X`** where `X` is not in `0..=7` (e.g. `^9`, `^Z`) → the
///   entire `^X` sequence is emitted as literal text in the current color.
/// - **Empty string** → empty `Vec` (no spans).
///
/// Empty spans (e.g. two consecutive color codes `^1^2text`) are retained in
/// the returned vec but will be skipped by [`render_colored_text`] when
/// rendering so they produce no visible output.
pub fn parse_colors(text: &str) -> Vec<(Color32, String)> {
    /// Default text color when no `^N` code has been seen yet.
    const DEFAULT: Color32 = Color32::from_rgb(220, 220, 220); // ^7 white

    fn quake_color(digit: u8) -> Option<Color32> {
        match digit {
            b'0' => Some(Color32::from_rgb(25, 25, 25)),
            b'1' => Some(Color32::from_rgb(215, 15, 15)),
            b'2' => Some(Color32::from_rgb(15, 200, 15)),
            b'3' => Some(Color32::from_rgb(220, 220, 15)),
            b'4' => Some(Color32::from_rgb(15, 15, 200)),
            b'5' => Some(Color32::from_rgb(15, 210, 210)),
            b'6' => Some(Color32::from_rgb(210, 15, 210)),
            b'7' => Some(Color32::from_rgb(220, 220, 220)),
            _ => None,
        }
    }

    if text.is_empty() {
        return Vec::new();
    }

    let mut result: Vec<(Color32, String)> = Vec::new();
    let mut current_color = DEFAULT;
    let mut current_span = String::new();

    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'^' {
            // Peek at the next byte.
            if let Some(&next) = bytes.get(i + 1) {
                if let Some(color) = quake_color(next) {
                    // Flush the current span (even if empty — callers skip empties).
                    result.push((current_color, std::mem::take(&mut current_span)));
                    current_color = color;
                    i += 2; // consume `^N`
                    continue;
                }
                // Unknown code (^8, ^9, ^Z, ^^, etc.) — emit as literal text.
                current_span.push('^');
                current_span.push(next as char);
                i += 2;
            } else {
                // Trailing `^` at end of string — emit as literal.
                current_span.push('^');
                i += 1;
            }
        } else {
            // Safety: all valid UTF-8; we index by byte but push full char.
            // Re-decode the character properly to handle multi-byte sequences.
            let ch = text[i..].chars().next().unwrap();
            current_span.push(ch);
            i += ch.len_utf8();
        }
    }

    // Flush the final span.
    result.push((current_color, current_span));

    result
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_colors tests ────────────────────────────────────────────────────

    fn white() -> Color32 {
        Color32::from_rgb(220, 220, 220)
    }
    fn red() -> Color32 {
        Color32::from_rgb(215, 15, 15)
    }
    fn yellow() -> Color32 {
        Color32::from_rgb(220, 220, 15)
    }
    fn green() -> Color32 {
        Color32::from_rgb(15, 200, 15)
    }

    #[test]
    fn test_parse_colors_plain_text() {
        let spans = parse_colors("hello");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, white());
        assert_eq!(spans[0].1, "hello");
    }

    #[test]
    fn test_parse_colors_single_code() {
        let spans = parse_colors("^1red text");
        // One empty initial flush + one span for "red text".
        // Filter empties as the renderer does.
        let non_empty: Vec<_> = spans.into_iter().filter(|(_, s)| !s.is_empty()).collect();
        assert_eq!(non_empty.len(), 1);
        assert_eq!(non_empty[0].0, red());
        assert_eq!(non_empty[0].1, "red text");
    }

    #[test]
    fn test_parse_colors_mixed() {
        let spans = parse_colors("^3FFOIE^7 chat");
        let non_empty: Vec<_> = spans.into_iter().filter(|(_, s)| !s.is_empty()).collect();
        assert_eq!(non_empty.len(), 2);
        assert_eq!(non_empty[0].0, yellow());
        assert_eq!(non_empty[0].1, "FFOIE");
        assert_eq!(non_empty[1].0, white());
        assert_eq!(non_empty[1].1, " chat");
    }

    #[test]
    fn test_parse_colors_trailing_caret() {
        let spans = parse_colors("end^");
        let non_empty: Vec<_> = spans.into_iter().filter(|(_, s)| !s.is_empty()).collect();
        assert_eq!(non_empty.len(), 1);
        assert_eq!(non_empty[0].0, white());
        assert_eq!(non_empty[0].1, "end^");
    }

    #[test]
    fn test_parse_colors_unknown_code() {
        // ^9 is not in 0..=7 — should be emitted as literal "^9".
        let spans = parse_colors("^9unknown");
        let non_empty: Vec<_> = spans.into_iter().filter(|(_, s)| !s.is_empty()).collect();
        assert_eq!(non_empty.len(), 1);
        assert_eq!(non_empty[0].0, white());
        assert_eq!(non_empty[0].1, "^9unknown");
    }

    #[test]
    fn test_parse_colors_empty() {
        let spans = parse_colors("");
        assert!(spans.is_empty());
    }

    #[test]
    fn test_parse_colors_all_codes() {
        // Verify each ^0..^7 code produces a non-empty flush and switches color.
        let text = "^0a^1b^2c^3d^4e^5f^6g^7h";
        let spans = parse_colors(text);
        let non_empty: Vec<_> = spans.into_iter().filter(|(_, s)| !s.is_empty()).collect();
        // Expect 8 non-empty spans: a, b, c, d, e, f, g, h.
        assert_eq!(non_empty.len(), 8);
        assert_eq!(non_empty[0].1, "a");
        assert_eq!(non_empty[7].1, "h");
        assert_eq!(non_empty[7].0, white());
    }

    // ── ChatState ring buffer tests ───────────────────────────────────────────

    #[test]
    fn test_max_messages_cap() {
        let mut state = ChatState::new();
        for i in 0..250u64 {
            state.push_message(ChatMessage {
                from: "x".to_string(),
                team: Team::None,
                channel: Channel::All,
                text: i.to_string(),
                ts: i,
            });
        }
        assert_eq!(
            state.messages.len(),
            CHAT_HISTORY_LEN,
            "ring buffer must not exceed CHAT_HISTORY_LEN"
        );
        // Oldest (ts=0..49) should have been evicted; newest (ts=200..249) kept.
        let ts_oldest = state.messages.front().unwrap().ts;
        assert_eq!(ts_oldest, 50, "oldest evicted messages should be gone");
    }

    // ── submit tests (native only) ────────────────────────────────────────────

    #[cfg(not(target_arch = "wasm32"))]
    mod native_tests {
        use super::*;
        use crate::network::NetworkCommand;

        #[test]
        fn test_submit_who() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::All;
            state.input_buffer = "/who".to_string();
            let cmd = submit(&mut state);
            assert!(matches!(cmd, Some(NetworkCommand::Who)));
            assert!(!state.chat_active, "chat_active must be cleared after submit");
            assert!(state.input_buffer.is_empty());
        }

        #[test]
        fn test_submit_who_with_args() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::All;
            state.input_buffer = "/who all".to_string();
            let cmd = submit(&mut state);
            assert!(matches!(cmd, Some(NetworkCommand::Who)));
        }

        #[test]
        fn test_submit_who_case_insensitive() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::All;
            state.input_buffer = "/WHO".to_string();
            let cmd = submit(&mut state);
            assert!(matches!(cmd, Some(NetworkCommand::Who)));
        }

        #[test]
        fn test_submit_say_all() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::All;
            state.input_buffer = "hello".to_string();
            let cmd = submit(&mut state);
            assert!(matches!(cmd, Some(NetworkCommand::Say(ref s)) if s == "hello"));
        }

        #[test]
        fn test_submit_say_team() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::Team;
            state.input_buffer = "go left".to_string();
            let cmd = submit(&mut state);
            assert!(matches!(cmd, Some(NetworkCommand::SayTeam(ref s)) if s == "go left"));
        }

        #[test]
        fn test_submit_empty() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::All;
            state.input_buffer = "".to_string();
            let cmd = submit(&mut state);
            assert!(cmd.is_none());
            assert!(!state.chat_active, "chat_active must be cleared even on empty submit");
        }

        #[test]
        fn test_submit_whitespace_only() {
            let mut state = ChatState::new();
            state.chat_active = true;
            state.channel = Channel::All;
            state.input_buffer = "   ".to_string();
            let cmd = submit(&mut state);
            assert!(cmd.is_none());
        }

        // ── drain_network tests ───────────────────────────────────────────────

        #[test]
        fn test_drain_connected_populates_state() {
            use crate::network::NetworkEvent;
            use ffoie_protocol::{Channel, ChatMessage, PlayerEntry, Team};
            use std::sync::mpsc;

            let (tx, rx) = mpsc::channel::<NetworkEvent>();

            let scrollback = vec![ChatMessage {
                from: "alice".to_string(),
                team: Team::Red,
                channel: Channel::All,
                text: "hi".to_string(),
                ts: 1_000,
            }];

            tx.send(NetworkEvent::Connected {
                assigned_nick: "fox#1234".to_string(),
                assigned_team: Team::Blue,
                motd: "Welcome!".to_string(),
                scrollback: scrollback.clone(),
            })
            .unwrap();

            let mut state = ChatState::new();
            let consumed = drain_network(&rx, &mut state);

            assert_eq!(consumed.len(), 1);
            assert_eq!(state.nickname, Some("fox#1234".to_string()));
            assert_eq!(state.team, Some(Team::Blue));
            assert!(matches!(
                state.connection_status,
                ConnectionStatus::Connected
            ));
            // MOTD should appear in system messages.
            assert!(
                state
                    .system_messages
                    .iter()
                    .any(|s| s.contains("Welcome!")),
                "MOTD must appear in system_messages"
            );
            // Scrollback should appear in messages.
            assert_eq!(state.messages.len(), 1);
            assert_eq!(state.messages[0].text, "hi");
        }

        #[test]
        fn test_drain_cap_at_32() {
            use crate::network::NetworkEvent;
            use ffoie_protocol::{Channel, ChatMessage, Team};
            use std::sync::mpsc;

            let (tx, rx) = mpsc::channel::<NetworkEvent>();

            // Send 50 messages — only 32 should be drained.
            for i in 0..50u64 {
                tx.send(NetworkEvent::Message(ChatMessage {
                    from: "x".to_string(),
                    team: Team::None,
                    channel: Channel::All,
                    text: i.to_string(),
                    ts: i,
                }))
                .unwrap();
            }

            let mut state = ChatState::new();
            let consumed = drain_network(&rx, &mut state);

            assert_eq!(consumed.len(), 32, "drain must be capped at 32 per frame");
            assert_eq!(state.messages.len(), 32);
        }

        #[test]
        fn test_drain_reconnecting_updates_status() {
            use crate::network::NetworkEvent;
            use std::sync::mpsc;

            let (tx, rx) = mpsc::channel::<NetworkEvent>();
            tx.send(NetworkEvent::Reconnecting {
                attempt: 3,
                delay_ms: 4_000,
            })
            .unwrap();

            let mut state = ChatState::new();
            drain_network(&rx, &mut state);

            assert!(
                matches!(
                    state.connection_status,
                    ConnectionStatus::Reconnecting { attempt: 3, .. }
                ),
                "status must be Reconnecting after drain"
            );
        }
    }
}
