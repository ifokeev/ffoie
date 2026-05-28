# ROADMAP — FFOIE v1.1: Online Chat MVP

**Milestone:** v1.1
**Created:** 2026-05-27
**Requirements:** 30 total (PROTO-01..03, INFRA-01..03, CHAT-01..10, SERVER-01..08, NET-01..04, OPS-01..02)
**Coverage:** 30/30 requirements mapped

---

## Phases

- [ ] **Phase 1: Workspace + Protocol Skeleton** — Restructure repo into Cargo workspace; define and test all wire types; zero runtime behavior change
- [ ] **Phase 2: Server Core** — Standalone chat server: axum WS, broadcast fan-out, nick/team assignment, rate limit, scrollback, MOTD, /who, graceful shutdown, /healthz
- [ ] **Phase 3: Native Engine Client** — ChatState + ewebsock native + egui chat HUD + T/Y/Esc keybinds + game-input suppression + reconnect + heartbeat
- [ ] **Phase 4: Wasm Engine Client** — ewebsock wasm32 path + ws/wss auto-detection + page-visibility reconnect
- [ ] **Phase 5: Deploy + Soak** — Docker Compose dev stack + 1k-connection soak test confirming bounded memory and zero panics

---

## Phase Details

### Phase 1: Workspace + Protocol Skeleton
**Goal**: The repo is a Cargo workspace and every wire type is defined, wasm32-compatible, and tested before any async code is written
**Depends on**: Nothing (first phase)
**Requirements**: PROTO-01, PROTO-02, PROTO-03, INFRA-01, INFRA-02, INFRA-03
**Success Criteria** (what must be TRUE):
  1. `cargo build --workspace` succeeds from repo root with no warnings about unresolved features or resolver mismatches
  2. `cargo build --target wasm32-unknown-unknown -p ffoie-engine` passes in CI without pulling in OpenSSL or `Security.framework`
  3. `cargo run` (native engine) and `trunk serve web-client/index.html` produce the same running game as before the restructure
  4. `cargo test -p ffoie-protocol` passes, exercising round-trip serde for every `ClientMessage` and `ServerMessage` variant
**Plans**: 4 plans

Plans:
- [x] 01-01-PLAN.md — Convert repo to Cargo workspace virtual manifest; move engine source to ffoie-engine/; update trunk href
- [ ] 01-02-PLAN.md — Create ffoie-protocol crate with all message types and round-trip serde tests
- [x] 01-03-PLAN.md — Create ffoie-chat-server stub binary; fix release-macos.yml for workspace
- [ ] 01-04-PLAN.md — Full zero-regression verification suite + human sign-off checkpoint

### Phase 2: Server Core
**Goal**: A standalone `ffoie-chat-server` binary accepts WebSocket connections, assigns nicknames and teams, fans out messages, enforces rate limits, delivers scrollback on join, and shuts down cleanly
**Depends on**: Phase 1
**Requirements**: SERVER-01, SERVER-02, SERVER-03, SERVER-04, SERVER-05, SERVER-06, SERVER-07, SERVER-08, NET-03
**Success Criteria** (what must be TRUE):
  1. Two `websocat` sessions connected to the server can exchange all-chat messages; one session's `SayTeam` is invisible to a differently-colored session
  2. A session that sends more than 10 messages in a burst receives a `RateLimited` error and subsequent over-limit messages are dropped; a message over 500 bytes is rejected immediately
  3. A new connection receives a `Welcome` envelope containing the last 50 messages (or all available if fewer) and the server MOTD before any further exchange
  4. `GET /healthz` returns `200 OK`; sending SIGTERM causes the server to cancel all active WS tasks and exit with code 0 within 5 seconds
  5. A simulated lagged receiver that cannot drain the broadcast ring gets disconnected by the server after three consecutive `Lagged` errors
**Plans**: 6 plans

Plans:
- [ ] 02-01-PLAN.md — Cargo.toml with all Phase 2 deps, Config struct from env, main.rs bootstrap, /healthz endpoint
- [ ] 02-02-PLAN.md — AppState (broadcast channel, scrollback ring, connections map, CancellationToken), nickname collision logic, team assignment
- [ ] 02-03-PLAN.md — WebSocket upgrade handler, per-connection select! loop, all ClientMessage variants, rate limit, length cap, lagged-client disconnect
- [ ] 02-04-PLAN.md — Graceful shutdown: SIGTERM/SIGINT signal handler, JoinSet drain, 5-second hard timeout + human checkpoint
- [ ] 02-05-PLAN.md — Integration test suite (7 tests: two-client chat, team filter, rate limit, length cap, scrollback, join/left, lag disconnect)
- [ ] 02-06-PLAN.md — Final verification: cargo build/test/clippy + two-websocat demo + human sign-off

### Phase 3: Native Engine Client
**Goal**: The running native engine connects to the chat server, renders a chat panel in the egui HUD, and all user-visible chat behaviors work end-to-end on desktop
**Depends on**: Phase 2
**Requirements**: CHAT-01, CHAT-02, CHAT-03, CHAT-04, CHAT-05, CHAT-06, CHAT-07, CHAT-08, CHAT-09, CHAT-10, NET-01, NET-02
**Success Criteria** (what must be TRUE):
  1. Player presses T, types a message, and presses Enter — the message appears in the HUD for all connected players; pressing Y sends the same message only to same-team players; pressing Esc closes the input without sending
  2. While the chat input is open, WASD, mouse-look, jump, T, and Y are fully suppressed — the player cannot move, rotate the view, or accidentally open a second chat box
  3. On connect, the chat panel shows the MOTD as the first line, followed by the last 50 scrollback messages; the server-assigned nickname and team color appear in the panel header
  4. Message text containing `^1`–`^7` Quake color codes renders the following characters in the corresponding color; `/who` returns the active player list in the chat panel
  5. If the server is unreachable or drops the connection, the engine silently retries with exponential backoff (base 1s, max 30s); the HUD shows "Reconnecting…" during backoff; missed heartbeats trigger reconnect
**Plans**: TBD
**UI hint**: yes

### Phase 4: Wasm Engine Client
**Goal**: The wasm build of the engine connects to the chat server over the correct scheme (ws:// or wss://) based on page origin, and chat remains functional across browser tab visibility changes
**Depends on**: Phase 3
**Requirements**: NET-04
**Success Criteria** (what must be TRUE):
  1. Loading the game from `https://ffoie.net/web/` opens a `wss://` WebSocket connection (not `ws://`), confirmed via browser DevTools Network tab — no mixed-content error
  2. Hiding the browser tab and restoring it reconnects the chat WebSocket within a few seconds and the chat panel resumes receiving messages without a page reload
**Plans**: TBD
**UI hint**: yes

### Phase 5: Deploy + Soak
**Goal**: The chat server runs in Docker Compose for local dev and survives a 5-minute 1000-connection soak test with bounded memory and zero panics
**Depends on**: Phase 4
**Requirements**: OPS-01, OPS-02
**Success Criteria** (what must be TRUE):
  1. `docker compose up` from repo root starts a single chat-server container; `curl http://localhost:8080/healthz` returns `200 OK` within 10 seconds of startup
  2. `scripts/soak.sh` runs 1000 concurrent WebSocket connections for 5 minutes; server RSS stays below 200 MB and `docker logs` shows zero panic lines when the test completes
**Plans**: TBD

---

## Progress Table

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Workspace + Protocol Skeleton | 2/4 | In Progress|  |
| 2. Server Core | 0/6 | Planned | - |
| 3. Native Engine Client | 0/0 | Not started | - |
| 4. Wasm Engine Client | 0/0 | Not started | - |
| 5. Deploy + Soak | 0/0 | Not started | - |

---

*Last updated: 2026-05-28 — Phase 2 planned (6 plans, 5 waves)*
