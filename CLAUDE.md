# FFOIE — Engine Concept & Architecture

This file is read by Claude Code at the start of every session. It exists to
get you (or a future Claude run) oriented in a couple of minutes: *what* this
project is, *how* the code is laid out, and *why* the non-obvious decisions
were made the way they were.

Deep reference material lives in path-scoped `.claude/rules/*.md` that auto-load
only when you edit the relevant code (`engine-internals.md` for
`crates/ffoie-engine/`, `deploy-release.md` for CI/packaging), keeping this file
lean. See those for engine internals and the release pipeline.

---

## What FFOIE is

**FFOIE — Freaking Fast Open Interactive Environment** — is a Quake-style FPS
engine prototype in Rust. Two product non-negotiables shape every choice:

1. **Cold start under 2-5 seconds** on every target platform. Current native
   number on an M4 Mac mini is ~300 ms; the same on Flipper One (Mali-G52 via
   Panfrost) is ~290 ms.
2. **Defrag-grade movement feel.** Raw mouse input, fixed-timestep simulation,
   classic Quake `PM_Accelerate` / `PM_AirAccelerate` / `PM_Friction` so
   real strafe-jumping just works.

It is *not* a game yet — no weapons, enemies, levels, AI, or full netcode.
The bones (input, render, physics, asset pipeline, UI, deploy) are the focus.
v1.1 adds real-time in-game chat: two engine windows on the same machine (or
network) can exchange messages through the `ffoie-chat-server` over WebSockets.

---

## Target platforms & backend selection

| Platform | Backend wgpu picks |
|---|---|
| macOS | Metal |
| Windows | DirectX 12 |
| Linux (most desktop GPUs) | Vulkan |
| Linux on Mali-G52 / Bifrost v7 | OpenGL via Panfrost (Vulkan via PanVK works with `run-vulkan.sh` + Mesa 26.1) |
| Web (Chrome/Edge ≥ 113, Safari ≥ 18, Firefox ≥ 141) | WebGPU |

The on-screen HUD shows the live backend (the **API** line) so backend
mismatches are visible at a glance instead of hidden in logs.

---

## Single-binary architecture (engine crate)

The engine binary lives in `crates/ffoie-engine/`. The core logic is the large,
heavily-commented single-file `src/main.rs` (~2.5k lines). New functionality goes
in sibling modules (`chat.rs`, `network/`) rather than extending `main.rs` inline
(see the module policy under "Workspace layout"); a full modular refactor of
`main.rs` is a later milestone.

> **Deep engine internals** — the per-frame loop, the Quake movement-physics
> constants, the renderer pipelines, the native/wasm split, HiDPI/GPU caps, and
> the build-time chat URL — live in **`.claude/rules/engine-internals.md`**,
> which auto-loads when you edit anything under `crates/ffoie-engine/`.

---

## Workspace layout

The repo is a Cargo workspace with `resolver = "2"`. Three member crates live
under `crates/`:

| Crate | Type | Compiles for wasm32? | Purpose |
|-------|------|----------------------|---------|
| `ffoie-engine` | bin | Yes | Engine, renderer, input, chat HUD |
| `ffoie-protocol` | lib | Yes — pure data, no OS deps | Shared wire types |
| `ffoie-chat-server` | bin | No — tokio/axum | Chat server |

The workspace root `Cargo.toml` is a virtual manifest (no `[package]`); it sets
`default-members = ["crates/ffoie-engine"]` so a bare `cargo build` builds the
engine.

**Module policy (relaxed from the original single-file rule):**

Starting with v1.1, new engine functionality goes in sibling modules under
`crates/ffoie-engine/src/` (e.g. `network/`, `chat.rs`). The existing
single-file `main.rs` (~2.5k LOC, well under the ~3000-line split threshold)
stays as-is for now; a full modular refactor is
planned for a later milestone. Until that refactor, `main.rs` may grow
`mod foo;` declarations and a small number of integration call sites for new
modules — it does **not** grow new subsystems inline.

---

## File / directory map

```
ffoie/
├── Cargo.toml              workspace virtual manifest (resolver = "2")
├── README.md               install + build per platform — read first
├── CLAUDE.md               this file
├── run-vulkan.sh           Mali-G52 / PanVK launcher
├── CNAME                   ffoie.net — drives the Pages routing logic below
├── .github/workflows/
│   ├── deploy-pages.yml    GitHub Pages deploy (custom domain ffoie.net)
│   └── release-macos.yml   tag v* → signed, notarized macOS DMG → Release
├── packaging/macos/        macOS bundle metadata (used by release-macos.yml)
│   ├── Info.plist.template templated at build time (envsubst)
│   └── entitlements.plist  hardened-runtime entitlements (empty by default)
├── website/                landing site source (served at /)
│   └── index.html          hand-written, no framework; add images/videos here
├── web-client/             trunk manifest for the wasm engine (served at /web/)
│   └── index.html          <link data-trunk rel="rust" ...> — loads the wasm
├── crates/
│   ├── ffoie-engine/       engine binary (native + wasm32)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs     engine core (~2.5k LOC; single-file by convention, new code in modules)
│   │       ├── network/    WS client (mod/types/native/wasm — ewebsock, reconnect, heartbeat; new in v1.1)
│   │       ├── chat.rs     chat HUD state + render (new in v1.1)
│   │       ├── shader.wgsl instanced lit geometry (cubes)
│   │       ├── floor.wgsl  procedural grid floor (notebook style)
│   │       ├── fox.wgsl    textured-mesh shader (Fox glTF)
│   │       ├── sky.wgsl    cubemap skybox
│   │       └── assets/
│   │           ├── skybox.ktx2  skybox cubemap (from wgpu's skybox example)
│   │           └── fox.glb      Khronos CC0 Fox glTF
│   ├── ffoie-protocol/     shared wire types (native + wasm32)
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── ffoie-chat-server/  standalone chat server binary (native only)
│       ├── Cargo.toml
│       └── src/main.rs
└── debug/
    └── macos-panic/        kernel panic logs observed during dev
                            (all Apple-side bugs — none mention FFOIE)
```

> **Deploy & release details** — GitHub Pages routing (`ffoie.net/` landing +
> `/web/` game) and the macOS signing/notarization pipeline (`release-macos.yml`,
> required secrets, artifact paths) — live in **`.claude/rules/deploy-release.md`**,
> which auto-loads when you touch `.github/workflows/`, `packaging/`, Docker,
> `compose.yml`, `Makefile`, or the web/landing dirs.

---

## Conventions

- **Commit messages are terse single lines**: "Initial commit", "Fix Windows
  linker step", "Add fox texture pipeline". No body unless really needed.
  **No `Co-Authored-By:` footer.**
- Rust style is whatever `cargo fmt` produces; no extra rules.
- Constants at the top of `crates/ffoie-engine/src/main.rs` are **designed to
  be tuned**. Most are movement-feel knobs.
- New engine functionality in v1.1+ lives in modules under
  `crates/ffoie-engine/src/` (e.g. `network/`, `chat.rs`). The single-file
  `main.rs` convention is relaxed for new code; a full modular refactor of the
  existing ~2.5k-line `main.rs` is a future task.
- WebGPU/Vulkan/Metal optional features stay off the default path unless
  necessary — the build must work on the minimum-spec GPU per platform with
  no explicit feature negotiation.

---

## Known platform issues (not FFOIE bugs)

- **macOS kernel panics on the dev Mac mini M4** in `fileproviderd`,
  `universalaccessd`, `AppleCS42L84Audio`, and `com.apple.sptm`. None of the
  panic backtraces contain FFOIE, wgpu, Metal, or AGX. The same signatures
  reproduce while playing other games on the same machine. See
  `debug/macos-panic/README.md` — these get reported to Apple via Feedback
  Assistant, nothing for FFOIE to fix.
- **PanVK on Mali-G52 r1** refuses to expose the GPU as a Vulkan device by
  default ("not well-tested on v7"). With Mesa 26.1 + `run-vulkan.sh` it
  works. When upstream Mesa stops printing the warning for Bifrost v7, the
  env-var dance goes away — no FFOIE change needed.

---

## Roadmap shape (loose)

Next likely directions, roughly in priority order:

1. **Step-up collision** so the player doesn't dead-stop on short walls.
2. **CPM-style air control** for proper Defrag feel.
3. **Audio** (jump, land, footsteps — even simple beeps would help feel).
4. **Particle / debug-shape immediate-mode drawing** for tuning physics.
5. **Map format** (probably either a JSON/RON `Vec<Block>` or a real BSP).
6. **Networking** (well-defined client-side prediction first).
7. **Weapons / hit detection.**

None of this is started; the prototype is intentionally a sandbox right now.

## v1.1 — Shipped: Online Chat MVP

v1.1 shipped a WebSocket chat server (`crates/ffoie-chat-server`) with in-memory fan-out, scrollback, rate limiting, and graceful shutdown. The engine client (native + wasm) connects automatically on start and renders chat in the egui HUD. Dev stack: `make docker-up` brings up the chat server + nginx-served wasm engine and prints both URLs; `make soak` runs the 1k-connection 5-minute load test. Host ports default to non-standard values (overridable via `FFOIE_CHAT_PORT` / `FFOIE_WEB_PORT`) to avoid clashing with other local services — see `compose.yml` / `Makefile` for the values. The server binds `8080` inside the container and for a bare `cargo run -p ffoie-chat-server`; Docker maps it to the non-standard host port.
