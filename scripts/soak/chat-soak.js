// ffoie chat-server k6 soak test
// Runs 1000 concurrent VUs for 5 minutes against ws://localhost:8080/ws
// Uses k6/experimental/websockets (ships with k6 v0.45+).
// Fallback: if experimental module is unavailable, swap the import for:
//   import ws from "k6/ws";
//   and use the ws.connect(url, params, function(socket){...}) callback API.

import { WebSocket } from "k6/experimental/websockets";
import { check, sleep } from "k6";
import { Counter } from "k6/metrics";

// ---------------------------------------------------------------------------
// Custom counters
// ---------------------------------------------------------------------------
const failed_connections = new Counter("failed_connections");
const unexpected_closes  = new Counter("unexpected_closes");

// ---------------------------------------------------------------------------
// Scenario options
// ---------------------------------------------------------------------------
export const options = {
  scenarios: {
    soak: {
      executor:          "constant-vus",
      vus:               1000,
      duration:          "5m30s",  // 30s implicit ramp + 5m hold
      gracefulRampDown:  "30s",
    },
  },
  thresholds: {
    // k6 built-in: every connected session — expect all VUs to stay the full duration
    ws_session_duration: ["p(95)<330000"],  // 330 s — just under scenario length
    // Custom: tolerate up to 5% failed connects during ramp-up
    failed_connections: ["count<50"],
  },
};

// ---------------------------------------------------------------------------
// Per-VU body
// ---------------------------------------------------------------------------
export default function () {
  const nick = `soak-vu-${__VU}-${Math.floor(Math.random() * 9000 + 1000)}`;
  const url  = "ws://localhost:8080/ws";

  let welcomed   = false;
  let scenarioDone = false;
  let socket;

  // Schedule periodic Say messages once welcomed
  function scheduleSay() {
    if (!socket || socket.readyState !== WebSocket.OPEN || !welcomed || scenarioDone) {
      return;
    }
    const body = JSON.stringify({
      type: "Say",
      text: `ping-${nick}-${Date.now()}`,
    });
    try {
      socket.send(body);
    } catch (_) {
      // drop on closed socket — unexpected_closes will record it
    }
    // Re-schedule while still running
    setTimeout(scheduleSay, 10000);
  }

  // Open the WebSocket
  try {
    socket = new WebSocket(url);
  } catch (e) {
    failed_connections.add(1);
    return;
  }

  socket.onopen = function () {
    // Send Connect envelope
    socket.send(JSON.stringify({ type: "Connect", nick }));
    // First Say fires after 10s
    setTimeout(scheduleSay, 10000);
  };

  socket.onmessage = function (event) {
    let msg;
    try {
      msg = JSON.parse(event.data);
    } catch (_) {
      return;  // ignore malformed frames
    }
    if (msg.type === "Welcome") {
      welcomed = true;
    }
    // Other server messages (e.g. broadcast "Said" events) — ignore
  };

  socket.onclose = function () {
    if (welcomed && !scenarioDone) {
      unexpected_closes.add(1);
    }
  };

  socket.onerror = function () {
    failed_connections.add(1);
  };

  // Hold the VU open for the scenario duration — k6 event loop keeps running
  // through the setTimeout chain above while we sleep here.
  // 5m30s scenario: sleep a bit less so gracefulRampDown can close the socket.
  sleep(310);  // 5 min 10s — scenario ends after that, k6 triggers graceful stop

  // Signal that scenario is ending so onclose doesn't count it as unexpected
  scenarioDone = true;

  // Assert welcome was received
  check(welcomed, {
    "received Welcome": (v) => v === true,
  });

  // Close cleanly if still open
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.close();
  }
}
