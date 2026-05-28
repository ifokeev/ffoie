//! Round-trip serde tests for every ClientMessage and ServerMessage variant.
//!
//! Pattern per test:
//!   1. Construct typed value.
//!   2. serde_json::to_string → JSON string.
//!   3. serde_json::from_str → round-tripped value.
//!   4. assert_eq!(original, round_tripped).
//!   5. assert!(json.contains(...)) — verify wire tag.
//!
//! No async, no tokio, no wasm-bindgen-test — pure Rust, native cargo test.

use ffoie_protocol::{Channel, ChatMessage, ClientMessage, PlayerEntry, ServerMessage, Team};

// ── Supporting type tests ─────────────────────────────────────────────────────

#[test]
fn team_serializes_lowercase() {
    assert_eq!(serde_json::to_string(&Team::Red).unwrap(), "\"red\"");
    assert_eq!(serde_json::to_string(&Team::Blue).unwrap(), "\"blue\"");
    assert_eq!(serde_json::to_string(&Team::None).unwrap(), "\"none\"");
    assert_eq!(serde_json::from_str::<Team>("\"red\"").unwrap(), Team::Red);
    assert_eq!(
        serde_json::from_str::<Team>("\"blue\"").unwrap(),
        Team::Blue
    );
    assert_eq!(
        serde_json::from_str::<Team>("\"none\"").unwrap(),
        Team::None
    );
}

#[test]
fn channel_serializes_lowercase() {
    assert_eq!(serde_json::to_string(&Channel::All).unwrap(), "\"all\"");
    assert_eq!(serde_json::to_string(&Channel::Team).unwrap(), "\"team\"");
    assert_eq!(
        serde_json::from_str::<Channel>("\"all\"").unwrap(),
        Channel::All
    );
    assert_eq!(
        serde_json::from_str::<Channel>("\"team\"").unwrap(),
        Channel::Team
    );
}

// ── Forward compatibility ─────────────────────────────────────────────────────

#[test]
fn unknown_field_on_known_variant_is_ignored() {
    // A newer server may add fields to an existing variant. serde's default is
    // to ignore unknown struct fields, so an older client must still decode it
    // (not error). This locks that behaviour in so a future `#[serde(deny_...)]`
    // can't silently break wire compatibility.
    let json = r#"{"type":"pong","seq":7,"server_version":"2.0","extra":true}"#;
    let parsed: ServerMessage = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, ServerMessage::Pong { seq: 7 });
}

#[test]
fn unknown_variant_is_a_clean_error_not_a_panic() {
    // A brand-new server message type must produce a recoverable Err (which the
    // engine logs and skips), never a panic.
    let json = r#"{"type":"system_broadcast","text":"server restarting"}"#;
    assert!(serde_json::from_str::<ServerMessage>(json).is_err());
}

// ── ClientMessage round-trip tests ────────────────────────────────────────────

#[test]
fn client_connect_round_trip() {
    let original = ClientMessage::Connect {
        nickname: "TestFox".to_string(),
        team: Team::Red,
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"connect\""), "json={json}");
    assert!(json.contains("\"team\":\"red\""), "json={json}");
    assert!(json.contains("\"nickname\":\"TestFox\""), "json={json}");
}

#[test]
fn client_say_round_trip() {
    let original = ClientMessage::Say {
        text: "hello world".to_string(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"say\""), "json={json}");
}

#[test]
fn client_say_team_round_trip() {
    let original = ClientMessage::SayTeam {
        text: "team only".to_string(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"say_team\""), "json={json}");
}

#[test]
fn client_who_round_trip() {
    let original = ClientMessage::Who;
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"who\""), "json={json}");
}

#[test]
fn client_ping_round_trip() {
    let original = ClientMessage::Ping { seq: 99 };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"ping\""), "json={json}");
    assert!(json.contains("\"seq\":99"), "json={json}");
}

// ── ServerMessage round-trip tests ────────────────────────────────────────────

fn make_chat_message() -> ChatMessage {
    ChatMessage {
        from: "Fox".to_string(),
        team: Team::Blue,
        channel: Channel::All,
        text: "hello chat".to_string(),
        ts: 1_700_000_000_000,
    }
}

#[test]
fn server_welcome_round_trip() {
    let original = ServerMessage::Welcome {
        assigned_nick: "Fox".to_string(),
        assigned_team: Team::Red,
        motd: "Welcome to the server!".to_string(),
        scrollback: vec![make_chat_message()],
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"welcome\""), "json={json}");
}

#[test]
fn server_message_round_trip() {
    let original = ServerMessage::Message {
        data: make_chat_message(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"message\""), "json={json}");
}

#[test]
fn server_joined_left_round_trip() {
    let original = ServerMessage::JoinedLeft {
        nickname: "Ranger".to_string(),
        team: Team::Blue,
        joined: true,
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"joined_left\""), "json={json}");
}

#[test]
fn server_who_list_round_trip() {
    let original = ServerMessage::WhoList {
        players: vec![
            PlayerEntry {
                nick: "Fox".to_string(),
                team: Team::Red,
            },
            PlayerEntry {
                nick: "Ranger".to_string(),
                team: Team::Blue,
            },
        ],
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"who_list\""), "json={json}");
}

#[test]
fn server_rate_limited_round_trip() {
    let original = ServerMessage::RateLimited {
        reason: "too fast".to_string(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"rate_limited\""), "json={json}");
}

#[test]
fn server_pong_round_trip() {
    let original = ServerMessage::Pong { seq: 7 };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"pong\""), "json={json}");
}

#[test]
fn server_error_round_trip() {
    let original = ServerMessage::Error {
        reason: "bad input".to_string(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(original, round_tripped);
    assert!(json.contains("\"type\":\"error\""), "json={json}");
}
