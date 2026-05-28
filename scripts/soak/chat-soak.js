// ffoie chat-server k6 soak test
// Runs 1000 concurrent VUs for 5 minutes against ws://localhost:47820/ws
// Override the target with `k6 run -e WS_URL=ws://host:port/ws ...`.
// Uses k6/websockets (stable in k6 v0.52+; replaces k6/experimental/websockets).
//
// Protocol (ffoie-protocol, serde internally-tagged, rename_all = "snake_case"):
//   ClientMessage::Connect  → {"type":"connect","nickname":"<str>","team":"none|red|blue"}
//   ClientMessage::Say      → {"type":"say","text":"<str>"}
//   ClientMessage::Ping     → {"type":"ping","seq":<u32>}
//   ServerMessage::Welcome  → {"type":"welcome","assigned_nick":"<str>","assigned_team":"...","motd":"...","scrollback":[...]}
//   ServerMessage::Message  → {"type":"message","data":{...}}
//   ServerMessage::Pong     → {"type":"pong","seq":<u32>}
//   ServerMessage::Error    → {"type":"error","reason":"<str>"}
//
// Fallback: if k6/websockets is unavailable, use k6/ws:
//   import ws from "k6/ws";
//   ws.connect(url, params, function(socket) { socket.on("open", ...) });

import { WebSocket } from "k6/websockets";
import { Counter } from "k6/metrics";

// ---------------------------------------------------------------------------
// Custom counters
// ---------------------------------------------------------------------------
const failed_connections = new Counter("failed_connections");   // connect errors
const unexpected_closes  = new Counter("unexpected_closes");    // server-initiated closes before welcome
const welcomes_received  = new Counter("welcomes_received");    // successful handshakes
const messages_dropped   = new Counter("messages_dropped");     // send-side errors

// ---------------------------------------------------------------------------
// Scenario options
// ---------------------------------------------------------------------------
export const options = {
  scenarios: {
    soak: {
      executor:     "constant-vus",
      vus:          1000,
      duration:     "5m",
      gracefulStop: "30s",
    },
  },
  thresholds: {
    // 95%+ of VUs must receive a Welcome message (pass criteria: >= 950/1000)
    "welcomes_received": ["count>=950"],
    // Zero or near-zero failed connections
    "failed_connections": ["count<50"],
  },
};

// ---------------------------------------------------------------------------
// Per-VU body
// ---------------------------------------------------------------------------
export default function () {
  const nick = `soak-vu-${__VU}-${Math.floor(Math.random() * 9000 + 1000)}`;
  const url  = __ENV.WS_URL || "ws://localhost:47820/ws";

  let welcomed = false;
  let socket;

  try {
    socket = new WebSocket(url);
  } catch (_) {
    failed_connections.add(1);
    return;
  }

  // Periodic Say scheduler — fires every 10 s after open
  function scheduleSay() {
    if (!socket) return;
    try {
      // snake_case "say" type, per ffoie-protocol serde rename_all = "snake_case"
      socket.send(JSON.stringify({
        type: "say",
        text: `ping-${nick}-${Date.now()}`,
      }));
    } catch (_) {
      messages_dropped.add(1);
    }
    setTimeout(scheduleSay, 10000);
  }

  socket.onopen = function () {
    // snake_case "connect" type; team required (use "none" for soak VUs)
    socket.send(JSON.stringify({
      type:     "connect",
      nickname: nick,
      team:     "none",
    }));
    // Start periodic Say every 10 s
    setTimeout(scheduleSay, 10000);
  };

  socket.onmessage = function (event) {
    let msg;
    try {
      msg = JSON.parse(event.data);
    } catch (_) {
      return;
    }
    if (msg.type === "welcome" && !welcomed) {
      welcomed = true;
      // Increment counter inside the active event loop — this fires reliably
      // during the session and is not dropped when k6 interrupts the VU.
      welcomes_received.add(1);
    }
    // "message" (broadcast), "joined_left", "pong", "error" — ignored
  };

  socket.onclose = function () {
    if (!welcomed) {
      unexpected_closes.add(1);
    }
  };

  socket.onerror = function () {
    failed_connections.add(1);
  };

  // VU function returns here. k6 keeps VUs alive until scenario ends (5m),
  // then triggers gracefulStop (30s) during which k6 closes all sockets.
}
