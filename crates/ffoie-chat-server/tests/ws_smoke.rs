//! Integration tests for ffoie-chat-server.
//!
//! Each test calls `spawn_test_server()` which binds on an OS-assigned
//! ephemeral port and returns the port number.  The test then connects one or
//! more WebSocket clients, exercises a behaviour, asserts, and exits.
//!
//! Connection-ordering note: each `handle_socket` task subscribes to the
//! broadcast channel before processing its first inbound message.  Because
//! tests establish all TCP connections before sending `Connect` messages, a
//! client may receive `JoinedLeft` notifications for other clients that joined
//! concurrently.  All helpers and tests are written to tolerate these
//! interleaved frames by skipping unexpected message types where safe to do so.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use ffoie_chat_server::build_app;
use ffoie_chat_server::config::Config;
use ffoie_protocol::{ClientMessage, ServerMessage, Team};

// ── Type aliases ──────────────────────────────────────────────────────────────

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>;
type WsStream = SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>;

// ── Test server helpers ───────────────────────────────────────────────────────

/// Build a Config with deterministic test defaults.
fn test_config(broadcast_capacity: usize) -> Config {
    Config {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        motd: "test motd".to_string(),
        heartbeat_secs: 60,
        max_msg_bytes: 500,
        rate_burst: 10,
        rate_refill_per_sec: 2,
        broadcast_capacity,
        scrollback_size: 50,
        max_lag_disconnects: 3,
    }
}

/// Spawn a full in-process server on an OS-assigned port and return the port.
async fn spawn_test_server() -> u16 {
    spawn_test_server_with_config(test_config(1024)).await
}

async fn spawn_test_server_with_config(config: Config) -> u16 {
    let (_, port) = spawn_test_server_with_state(config).await;
    port
}

async fn spawn_test_server_with_state(
    config: Config,
) -> (ffoie_chat_server::state::AppState, u16) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let port = listener.local_addr().unwrap().port();

    let (app, state) = build_app(config);
    let state_clone = state.clone();

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test server error");
    });

    (state_clone, port)
}

// ── Client helpers ────────────────────────────────────────────────────────────

/// Open a WS connection and return the split (Sink, Stream).
async fn connect_ws(port: u16) -> (WsSink, WsStream) {
    let url = format!("ws://127.0.0.1:{}/ws", port);
    let (ws, _response) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS connect failed");
    ws.split()
}

/// Send a `ClientMessage` as a JSON text frame.
async fn send(sink: &mut WsSink, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).expect("serialize ClientMessage");
    sink.send(Message::text(json))
        .await
        .expect("WS send failed");
}

/// Read the next frame from the stream within 3 seconds.
///
/// Returns `None` on stream close.  Panics on timeout or transport error.
async fn next_frame(stream: &mut WsStream) -> Option<Message> {
    tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("next_frame timed out after 3s")
        .transpose()
        .expect("WS receive error")
}

/// Decode a text frame as a `ServerMessage`.  Panics if the frame is not text
/// or the JSON is malformed.
#[allow(dead_code)]
fn decode(frame: Message) -> ServerMessage {
    match frame {
        Message::Text(t) => {
            serde_json::from_str::<ServerMessage>(&t).expect("deserialize ServerMessage")
        }
        Message::Close(_) => panic!("received WS Close frame"),
        other => panic!("unexpected non-text WS frame: {other:?}"),
    }
}

/// Receive the next `ServerMessage`, skipping non-text frames (Ping/Pong/Binary).
///
/// Panics on timeout (3 s) or stream close.
async fn recv(stream: &mut WsStream) -> ServerMessage {
    loop {
        let frame = next_frame(stream).await.expect("WS stream closed unexpectedly");
        match frame {
            Message::Text(t) => {
                return serde_json::from_str::<ServerMessage>(&t)
                    .expect("deserialize ServerMessage");
            }
            // Close or error during a recv is a hard failure.
            Message::Close(_) => panic!("WS stream closed while waiting for ServerMessage"),
            // Silently skip Ping/Pong/Binary frames at the application level.
            _ => continue,
        }
    }
}

/// Send `Connect` and loop-read until a `Welcome` is received.
///
/// Because a client subscribes to the broadcast before sending `Connect`,
/// it may receive `JoinedLeft` frames for other clients that connected
/// concurrently.  This helper skips any non-Welcome messages until it finds
/// the `Welcome` response.
///
/// Returns `(assigned_nick, assigned_team)`.
async fn send_connect(
    sink: &mut WsSink,
    stream: &mut WsStream,
    nick: &str,
    team: Team,
) -> (String, Team) {
    send(sink, &ClientMessage::Connect { nickname: nick.to_string(), team }).await;

    loop {
        let msg = recv(stream).await;
        match msg {
            ServerMessage::Welcome { assigned_nick, assigned_team, .. } => {
                assert!(!assigned_nick.is_empty(), "assigned_nick must not be empty");
                return (assigned_nick, assigned_team);
            }
            // Tolerate interleaved JoinedLeft / Message frames while waiting for Welcome.
            ServerMessage::JoinedLeft { .. } | ServerMessage::Message { .. } => continue,
            other => panic!("expected Welcome, got {other:?}"),
        }
    }
}

/// Drain `count` messages from `stream`, discarding them.
///
/// Used to consume known notifications before an assertion window.
async fn drain(stream: &mut WsStream, count: usize) {
    for _ in 0..count {
        recv(stream).await;
    }
}

/// Wait up to `timeout` for the next `ServerMessage`.
///
/// Returns `Ok(msg)` if one arrived or `Err(())` on timeout.
async fn recv_timeout(stream: &mut WsStream, timeout: Duration) -> Result<ServerMessage, ()> {
    tokio::time::timeout(timeout, recv(stream))
        .await
        .map_err(|_| ())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// client-A sends Say; client-B receives Message with the same text.
#[tokio::test]
async fn two_clients_chat() {
    let port = spawn_test_server().await;

    let (mut sink_a, mut stream_a) = connect_ws(port).await;
    // Connect A first; wait for Welcome before B even connects so there is no
    // broadcast-subscription race.
    send_connect(&mut sink_a, &mut stream_a, "Alice", Team::Red).await;

    let (mut sink_b, mut stream_b) = connect_ws(port).await;
    send_connect(&mut sink_b, &mut stream_b, "Bob", Team::Blue).await;

    // A sees B's JoinedLeft{joined:true}.  Drain it so A's stream is clean.
    let notif_a = recv(&mut stream_a).await;
    assert!(
        matches!(notif_a, ServerMessage::JoinedLeft { joined: true, .. }),
        "A should see B join, got {notif_a:?}"
    );

    // B also receives its own JoinedLeft{joined:true} from the broadcast.
    // Drain it before sending A's message so B's stream is clean.
    let notif_b = recv(&mut stream_b).await;
    assert!(
        matches!(notif_b, ServerMessage::JoinedLeft { joined: true, .. }),
        "B should see own join notification, got {notif_b:?}"
    );

    send(&mut sink_a, &ClientMessage::Say { text: "hello from A".to_string() }).await;

    // B receives the broadcast.
    let msg = recv(&mut stream_b).await;
    match msg {
        ServerMessage::Message { data } => {
            assert_eq!(data.text, "hello from A");
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

/// SayTeam from a client only reaches same-team members; the other team sees nothing.
///
/// The server assigns teams randomly (50/50).  With only 3 clients the
/// all-same-team case occurs ~25% of the time.  To guarantee we always test
/// the filter-exclusion path, we retry connecting 3 clients until we observe
/// at least one Red AND one Blue in the assigned teams.  With a cap of 10
/// attempts the probability of never finding a split is (1/4)^10 < 10^-6.
#[tokio::test]
async fn team_filter() {
    let port = spawn_test_server().await;

    // Retry loop: connect 3 clients, check for a team split, retry if all same.
    let max_attempts = 10;
    for attempt in 0..max_attempts {
        // Connect clients sequentially so each Welcome arrives cleanly.
        let (mut sink_a, mut stream_a) = connect_ws(port).await;
        let (_nick_a, team_a) =
            send_connect(&mut sink_a, &mut stream_a, "PlayerA", Team::Red).await;

        let (mut sink_b, mut stream_b) = connect_ws(port).await;
        let (_nick_b, team_b) =
            send_connect(&mut sink_b, &mut stream_b, "PlayerB", Team::Blue).await;

        let (mut sink_c, mut stream_c) = connect_ws(port).await;
        let (_nick_c, team_c) =
            send_connect(&mut sink_c, &mut stream_c, "PlayerC", Team::Red).await;

        // Settle: give all join-broadcast notifications time to fan out to every
        // subscriber before we start draining fixed counts.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Drain all join notifications from each stream before asserting.
        // On attempt > 0, there are prior-client JoinedLeft{joined:false} frames
        // queued ahead of new JoinedLeft{joined:true} frames.  Use a generous
        // drain that skips any message type rather than a fixed count.
        //
        // Drain each stream for up to 500 ms, stopping early once the stream
        // goes quiet (no frame within 50 ms = notifications have settled).
        async fn drain_until_quiet(stream: &mut WsStream) {
            loop {
                match recv_timeout(stream, Duration::from_millis(50)).await {
                    Ok(_) => continue,
                    Err(_) => break, // quiet
                }
            }
        }
        drain_until_quiet(&mut stream_a).await;
        drain_until_quiet(&mut stream_b).await;
        drain_until_quiet(&mut stream_c).await;

        // Check for a team split.  All three on the same team means there is no
        // outsider to assert against — drop everything and retry.
        let all_same = team_a == team_b && team_b == team_c;
        if all_same {
            // Drop connections; the server will clean them up.
            drop((sink_a, stream_a, sink_b, stream_b, sink_c, stream_c));
            // Allow the server tasks to process the disconnections before the
            // next attempt so join-notification counts stay predictable.
            tokio::time::sleep(Duration::from_millis(150)).await;
            if attempt == max_attempts - 1 {
                panic!(
                    "team_filter: all {max_attempts} attempts produced all-same-team assignments; \
                     this is astronomically unlikely — check server team-assignment logic"
                );
            }
            continue; // retry
        }

        // We have a split.  Identify sender (majority team), teammate, outsider.
        //
        // Exactly one of the following is true when not all-same:
        //   (1) team_a == team_b  (C is outsider)
        //   (2) team_a == team_c  (B is outsider)
        //   (3) team_b == team_c  (A is outsider)
        //
        // There are only two teams (Red/Blue), so at least two clients always
        // share a team.  The "all different teams" branch is impossible here.
        let (sender_sink, sender_stream, teammate_stream, outsider_stream) =
            if team_a == team_b {
                // C is on the other team
                (&mut sink_a, &mut stream_a, &mut stream_b, &mut stream_c)
            } else if team_a == team_c {
                // B is on the other team
                (&mut sink_a, &mut stream_a, &mut stream_c, &mut stream_b)
            } else {
                // team_b == team_c; A is on the other team
                (&mut sink_b, &mut stream_b, &mut stream_c, &mut stream_a)
            };

        send(sender_sink, &ClientMessage::SayTeam { text: "team-only".to_string() }).await;

        // Sender receives the broadcast (team_filter == sender's team).
        let got_sender = recv(sender_stream).await;
        assert!(
            matches!(got_sender, ServerMessage::Message { .. }),
            "sender should receive own SayTeam, got {got_sender:?}"
        );

        // Teammate also receives it.
        let got_teammate = recv(teammate_stream).await;
        assert!(
            matches!(got_teammate, ServerMessage::Message { .. }),
            "teammate should receive SayTeam, got {got_teammate:?}"
        );

        // Outsider must NOT receive it within 1 second.
        let nothing = recv_timeout(outsider_stream, Duration::from_secs(1)).await;
        assert!(
            nothing.is_err(),
            "outsider must NOT receive SayTeam, but got: {nothing:?}"
        );

        return; // test passed
    }
}

/// Sending 11 messages in a burst triggers at least one `RateLimited` response.
#[tokio::test]
async fn rate_limit() {
    let port = spawn_test_server().await;

    let (mut sink, mut stream) = connect_ws(port).await;
    send_connect(&mut sink, &mut stream, "Spammer", Team::Red).await;

    // Drain own join notification.
    drain(&mut stream, 1).await;

    // Send 11 Say messages as fast as possible.
    for i in 0..11u32 {
        send(&mut sink, &ClientMessage::Say { text: format!("msg{i}") }).await;
    }

    // Collect responses — some will be Message (broadcast), at least one RateLimited.
    let mut got_rate_limited = false;
    for _ in 0..12 {
        match recv_timeout(&mut stream, Duration::from_secs(3)).await {
            Ok(ServerMessage::RateLimited { .. }) => {
                got_rate_limited = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break, // timeout
        }
    }

    assert!(got_rate_limited, "expected at least one RateLimited after 11 burst messages");
}

/// Sending a 501-byte message gets `Error { reason: "... too long ..." }`.
#[tokio::test]
async fn length_cap() {
    let port = spawn_test_server().await;

    let (mut sink, mut stream) = connect_ws(port).await;
    send_connect(&mut sink, &mut stream, "Bigmouth", Team::Blue).await;

    // Drain own join notification.
    drain(&mut stream, 1).await;

    let long_text = "x".repeat(501);
    send(&mut sink, &ClientMessage::Say { text: long_text }).await;

    let response = recv(&mut stream).await;
    match response {
        ServerMessage::Error { reason } => {
            assert!(
                reason.to_lowercase().contains("too long"),
                "Error reason should contain 'too long', got: {reason:?}"
            );
        }
        other => panic!("expected Error for over-cap message, got {other:?}"),
    }
}

/// Client C connects after A sent 3 messages; Welcome carries them in `scrollback`.
#[tokio::test]
async fn scrollback_on_connect() {
    let port = spawn_test_server().await;

    // Client A connects and sends 3 messages.
    let (mut sink_a, mut stream_a) = connect_ws(port).await;
    send_connect(&mut sink_a, &mut stream_a, "Historian", Team::Red).await;
    drain(&mut stream_a, 1).await; // own join notification

    for i in 0..3u32 {
        send(&mut sink_a, &ClientMessage::Say { text: format!("history{i}") }).await;
        // Small pause so each Say is processed before the next.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Drop A — server will detect the closed socket and clean up.
    drop(sink_a);
    drop(stream_a);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Client C connects; its Welcome should include 3 scrollback entries.
    let (mut sink_c, mut stream_c) = connect_ws(port).await;
    // Send Connect directly and read Welcome; we can use send_connect here because
    // there are no other live clients to cause interleaved JoinedLeft frames.
    send(&mut sink_c, &ClientMessage::Connect { nickname: "Newcomer".to_string(), team: Team::Blue }).await;

    let frame = recv(&mut stream_c).await;
    match frame {
        ServerMessage::Welcome { scrollback, .. } => {
            assert_eq!(
                scrollback.len(),
                3,
                "Welcome.scrollback must have 3 entries, got {}",
                scrollback.len()
            );
            assert_eq!(scrollback[0].text, "history0");
            assert_eq!(scrollback[1].text, "history1");
            assert_eq!(scrollback[2].text, "history2");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// B sees `JoinedLeft{joined:true}` when A connects; then `JoinedLeft{joined:false}` when A disconnects.
#[tokio::test]
async fn joined_left_notifications() {
    let port = spawn_test_server().await;

    // Connect B first so it is subscribed before A joins.
    let (mut sink_b, mut stream_b) = connect_ws(port).await;
    send_connect(&mut sink_b, &mut stream_b, "Watcher", Team::Blue).await;
    // Drain B's own join notification.
    drain(&mut stream_b, 1).await;

    // Connect A — B should see JoinedLeft{joined:true}.
    let (mut sink_a, mut stream_a) = connect_ws(port).await;
    let (nick_a, _) = send_connect(&mut sink_a, &mut stream_a, "Leaver", Team::Red).await;

    let joined_notif = recv(&mut stream_b).await;
    match &joined_notif {
        ServerMessage::JoinedLeft { nickname, joined: true, .. } => {
            assert_eq!(nickname, &nick_a, "join notification should name A's nick");
        }
        other => panic!("expected JoinedLeft(joined=true) for A on B's stream, got {other:?}"),
    }

    // Disconnect A.
    drop(sink_a);
    drop(stream_a);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // B should see JoinedLeft{joined:false}.
    let left_notif = recv(&mut stream_b).await;
    match left_notif {
        ServerMessage::JoinedLeft { nickname, joined: false, .. } => {
            assert_eq!(nickname, nick_a, "left notification should name A's nick");
        }
        other => panic!("expected JoinedLeft(joined=false), got {other:?}"),
    }
}

/// Two clients claim the same nickname; the second gets a deduplicated suffix.
#[tokio::test]
async fn nickname_collision() {
    let port = spawn_test_server().await;

    let (mut sink_a, mut stream_a) = connect_ws(port).await;
    // A connects first and claims "Samebot".
    let (nick_a, _) = send_connect(&mut sink_a, &mut stream_a, "Samebot", Team::Red).await;

    let (mut sink_b, mut stream_b) = connect_ws(port).await;
    // B also requests "Samebot"; it should get a suffixed variant.
    let (nick_b, _) = send_connect(&mut sink_b, &mut stream_b, "Samebot", Team::Blue).await;

    assert_eq!(nick_a, "Samebot", "first client should keep the requested nick");
    assert_ne!(
        nick_b, "Samebot",
        "second client must receive a deduplicated nick, got {nick_b:?}"
    );
    // Suffix appended by nickname.rs contains the original name.
    assert!(
        nick_b.contains("Samebot"),
        "deduplicated nick should be a variant of 'Samebot', got {nick_b:?}"
    );
}

/// A slow client is disconnected after the broadcast ring laps it
/// `max_lag_disconnects` times.
///
/// Mechanism: we inject broadcast events directly into the server's ring via
/// `AppState::broadcast_tx`, bypassing the per-client rate limit and the TCP
/// stack entirely.  This lets us flood the ring while slow's `handle_socket`
/// is busy processing (socket.send) a previous message, reliably triggering
/// `RecvError::Lagged` without needing TCP backpressure.
///
/// On a multi-thread runtime the slow client's `handle_socket` task runs on
/// a separate OS thread.  While it is executing `socket.send()`, our test
/// thread injects capacity+1 = 5 events, wrapping the ring.  When
/// `handle_socket` returns from `socket.send()`, the next `bcast_rx.recv()`
/// sees the ring has lapped it and returns `Lagged`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lag_disconnect() {
    use std::sync::Arc;
    use ffoie_chat_server::state::BroadcastEvent;
    use ffoie_protocol::{Channel, ChatMessage};

    let config = Config {
        broadcast_capacity: 4,
        max_lag_disconnects: 3,
        ..test_config(4)
    };
    let (server_state, port) = spawn_test_server_with_state(config).await;

    // ── Slow client ───────────────────────────────────────────────────────────
    let (mut slow_sink, mut slow_stream) = connect_ws(port).await;
    send_connect(&mut slow_sink, &mut slow_stream, "Slug", Team::Red).await;
    // Drain slow's own join notification.
    drain(&mut slow_stream, 1).await;

    // ── Slow reader task ──────────────────────────────────────────────────────
    // Reads with a 30ms delay to give the broadcast injector time to wrap the
    // ring while slow's handle_socket is busy with socket.send().
    let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _slow_sink = slow_sink; // keep WS connection alive
        let mut stream = slow_stream;
        loop {
            tokio::time::sleep(Duration::from_millis(30)).await;
            match stream.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => {
                    let _ = disconnect_tx.send(());
                    return;
                }
                Some(Ok(_)) => { /* keep reading slowly */ }
            }
        }
    });

    // ── Broadcast injector ────────────────────────────────────────────────────
    // Directly injects events into the server's broadcast ring without going
    // through a real WebSocket client, bypassing rate limits entirely.
    //
    // The injector runs continuously until the slow client is disconnected.
    // With a multi-thread runtime, the injector saturates the ring (capacity=4)
    // faster than handle_socket can consume it when handle_socket is stuck in
    // socket.send().  Combined with the slow reader's 30ms delay, the steady
    // state is:
    //   1. Ring saturates (injector outpaces handle_socket)
    //   2. TCP buffer fills (handle_socket sends faster than slow reads)
    //   3. socket.send() blocks (TCP flow control kicks in)
    //   4. Ring overflows (injector continues) → Lagged fires
    //   5. Slow reads 1 message (unblocks socket.send())
    //   6. bcast_rx.recv() returns Lagged → lag_count++
    //   7. After 3 Lagged events, server disconnects slow → disconnect_rx fires
    let tx = server_state.broadcast_tx.clone();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let inject_handle = tokio::task::spawn_blocking(move || {
        // Use spawn_blocking so the injector runs on a dedicated blocking thread,
        // not competing with tokio's async worker threads.  This ensures the
        // injector can inject messages without yielding to the async scheduler.
        let mut j = 0u32;
        loop {
            // Check for stop signal without blocking.
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let ev = Arc::new(BroadcastEvent {
                msg: ServerMessage::Message {
                    data: ChatMessage {
                        from: "injector".to_string(),
                        team: Team::Red,
                        channel: Channel::All,
                        text: format!("x{j}"),
                        ts: 0,
                    },
                },
                team_filter: None,
            });
            let _ = tx.send(ev);
            j += 1;
            // Throttle to ~100K messages/sec to avoid overwhelming the runtime.
            if j % 1000 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    });
    // Injector runs concurrently with the 5-second timeout.
    // Once disconnect_rx fires (server closed slow), we stop the injector.
    let disconnect_result = tokio::time::timeout(Duration::from_secs(5), async {
        disconnect_rx.await.ok()
    })
    .await;
    let _ = stop_tx.send(()); // stop the injector
    let _ = inject_handle.await;

    // ── Assert ────────────────────────────────────────────────────────────────
    assert!(
        disconnect_result.is_ok(),
        "slow client should have been disconnected within 5s due to lag"
    );
}
