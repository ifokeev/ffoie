# FFOIE
<img width="2564" height="1502" alt="23d (1)" src="https://github.com/user-attachments/assets/d85c0643-fc99-4f99-a87f-dc09947b3adc" />


**Freaking Fast Open Interactive Environment** — a Quake-style FPS engine
prototype focused on movement feel, low-latency input, and a sub-2-second
cold start. Cross-platform: macOS, Windows, Linux.

An early prototype — no weapons, no gameplay loop yet — focused on getting the
*bones* (input, render, physics, asset pipeline) right before any game logic
lands. It's a small Cargo workspace: the engine plus a WebSocket chat server and
a shared protocol crate (see [Architecture](#architecture)).

## Contents

- [Features](#features)
- [Build & run](#build--run)
- [Environment variables](#environment-variables)
- [Controls](#controls)
- [Architecture](#architecture)
- [Known issues](#known-issues)
- [Licences](#asset--dependency-licences)

## Features

- **Renderer**: `wgpu 29` — picks Metal on macOS, DirectX 12 on Windows, Vulkan
  on Linux automatically. The on-screen widget shows which backend is live.
- **Physics**: Quake `PM_Accelerate` + `PM_Friction` + `PM_AirAccelerate`
  (VQ3 defaults). Real strafe-jumping works. Tunable constants at the top of
  `crates/ffoie-engine/src/main.rs` (`GROUND_ACCEL`, `AIR_ACCEL`, `MAX_SPEED`, `FRICTION`,
  `JUMP_VELOCITY`, `GRAVITY`, `FOV_DEG`, `MOUSE_SENSITIVITY`).
- **Fixed-timestep simulation** at 120 Hz, decoupled from render rate.
- **Raw-input mouse-look** via `winit::DeviceEvent::MouseMotion` with
  `CursorGrabMode::Locked` — no OS smoothing / acceleration.
- **AABB collision** against ~25 hand-arranged blocks forming a strafe-jump
  course. Y-then-X-then-Z axis-separated sweep.
- **Skybox** from a KTX2 cubemap.
- **Procedural floor** with green grid lines (notebook-style).
- **glTF model loading** (the corner Fox is from the Khronos CC0 sample
  assets) including baseColor texture sampling.
- **egui HUD** showing FPS, frame time, GPU/API/backend info, present mode,
  resolution; a thin colour-graded speed bar under the crosshair; pause menu
  with Resume / Exit.
- **Online text chat** over WebSockets via a separate `ffoie-chat-server`
  binary — start it with the [Docker dev stack](#dev-stack-docker).

## Build & run

The same Rust source compiles on macOS, Linux, and Windows. You need the
Rust toolchain plus a C/C++ linker (provided by each OS's standard build
tools). On first build Cargo will fetch and compile ~300 dependencies — expect
~30 s with a warm cache, longer on a clean machine. After that, edit-rebuild
cycles are seconds.

Always use `--release` for measurements — debug builds are ~10× slower at
runtime and give misleading FPS / startup numbers.

### macOS

```sh
# 1. Xcode Command Line Tools (provides clang + linker):
xcode-select --install

# 2. Rust toolchain (interactive — press Enter for default install):
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Build and run:
cd ffoie
cargo run --release
```

Backend: **Metal**.

### Linux (Ubuntu / Debian)

```sh
# 1. Build tools + windowing/input/Vulkan libraries:
sudo apt update
sudo apt install -y build-essential pkg-config \
    libwayland-dev libxkbcommon-dev libudev-dev \
    libvulkan1 mesa-vulkan-drivers

# 2. Rust toolchain:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Build and run:
cd ffoie
cargo run --release
```

On Fedora: `sudo dnf install gcc pkgconf-pkg-config wayland-devel libxkbcommon-devel systemd-devel vulkan-loader mesa-vulkan-drivers`.
On Arch: `sudo pacman -S base-devel pkgconf wayland libxkbcommon systemd vulkan-icd-loader mesa`.

Backend: **Vulkan** (falls back to OpenGL on systems without Vulkan).

### Windows

1. **Install the MSVC linker** (Rust calls into it for the final link step).
   This is the step people miss — installing Rust alone is **not enough**, you
   will get `error: linker link.exe not found` until this is done.

   Easiest, from an admin PowerShell:
   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools --override "--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
   GUI alternative: download `vs_BuildTools.exe` from
   <https://aka.ms/vs/17/release/vs_BuildTools.exe> and tick the
   **"Desktop development with C++"** workload.

   Verify by searching the filesystem (Build Tools does **not** add MSVC to
   PATH — Rust finds it via the registry instead):
   ```powershell
   Get-ChildItem "C:\Program Files\Microsoft Visual Studio","C:\Program Files (x86)\Microsoft Visual Studio" -Filter link.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 FullName
   # Should print: ...\BuildTools\VC\Tools\MSVC\14.xx\bin\Hostx64\x64\link.exe
   ```
   (Don't use `where.exe link` — it only searches PATH, which MSVC isn't on
   unless you launched a "Developer" shell.)

   If Rust still can't find the linker after a successful install, open
   **"x64 Native Tools Command Prompt for VS 2022"** from the Start menu and
   run `cargo run --release` from there — that shell has MSVC on PATH.

2. Install **Rust** from <https://rustup.rs/> — accept the defaults, it picks
   `x86_64-pc-windows-msvc`.

3. Open a fresh PowerShell so the updated `PATH` is picked up:
   ```powershell
   cd ffoie
   cargo run --release
   ```

Backend: **DirectX 12**.

### Verify the install (any OS)

```sh
rustc --version    # rustc 1.87+ expected
cargo --version
```

### Forcing a non-default backend

The on-screen HUD shows which API wgpu picked. To override, set
`WGPU_BACKEND` before launch:

```sh
WGPU_BACKEND=vulkan cargo run --release   # macOS (via MoltenVK), Linux, Windows
WGPU_BACKEND=gl     cargo run --release   # OpenGL fallback (slowest, broadest)
WGPU_BACKEND=dx12   cargo run --release   # Windows only
WGPU_BACKEND=metal  cargo run --release   # macOS only
```

The release binary lives at the path printed by `cargo build --release`
(default `target/release/ffoie`; on the dev machine it's redirected to
`/Users/a/.cargo-target/foie/release/ffoie` via `../.cargo/config.toml` so
iCloud doesn't sync several GB of build artefacts).

### Dev stack (Docker)

The chat server and web client run together as a Docker stack — the quickest way
to bring up a backend and a playable web build:

```sh
make docker-up      # builds + starts the chat server and the web client
# open http://localhost:47821  → the web client connects automatically
make docker-down    # tear down
```

Default host ports are **47820** (chat) and **47821** (web) — change them via
[Environment variables](#environment-variables). To run a *native* engine (built
above) against that server, point it at the chat URL:

```sh
FFOIE_CHAT_URL=ws://localhost:47820/ws cargo run --release
```

`make soak` runs a 1000-connection load test.

## Environment variables

Everything has a default, so nothing is required to run locally.

| Variable | Default | What it does |
| --- | --- | --- |
| `FFOIE_CHAT_URL` | `ws://localhost:8080/ws` | chat server the engine connects to (native reads it at runtime; the web build bakes it in at compile time) |
| `FFOIE_NICK` | `guest` | your display nickname (native engine) |
| `FFOIE_CHAT_PORT` | `47820` | host port the Docker chat server is published on |
| `FFOIE_WEB_PORT` | `47821` | host port the Docker web client is published on |

The chat server has more knobs (MOTD, message-size cap, rate limit, scrollback,
heartbeat), each documented with its default in
[`crates/ffoie-chat-server/.env.example`](crates/ffoie-chat-server/.env.example).

## Controls

| Action | Key |
| --- | --- |
| Capture mouse / start playing | **Click in window** |
| Release mouse / open pause menu | **Esc** |
| Move | **W A S D** |
| Jump (auto-hops while held) | **Space** |
| Crouch / move down (when on ground: nothing yet) | **Left Ctrl** |
| Sprint | **Left Shift** |

## Architecture

FFOIE is a Cargo workspace (`resolver = "2"`); a bare `cargo run` builds the
engine. Three crates under `crates/`:

- **`ffoie-engine`** — the engine binary (native + wasm32; binary name `ffoie`).
- **`ffoie-chat-server`** — the WebSocket chat server (`axum`, native-only).
- **`ffoie-protocol`** — the shared client↔server message types; pure `serde`
  data that also compiles to wasm.

The split keeps the protocol shareable and wasm-safe while the server's
`tokio`/`axum` dependencies stay out of the web build.

## Known issues

### macOS kernel panics (Apple side, **not** FFOIE)

Four full-system kernel panics were observed in a single day on the
development machine (Mac mini M4, macOS 26.4.1). Detailed logs and analysis
are preserved in [`debug/macos-panic/`](debug/macos-panic/). **None of them
name FFOIE or any graphics driver in the backtrace.** They blame, in turn:

- SoC-level hardware diagnostic
- `AppleCS42L84Audio` codec power-state transition timeout
- `universalaccessd` watchdog
- `com.apple.sptm` (Secure Page Table Monitor) watchdog
- `fileproviderd` watchdog

The same crash signature reproduces with other games on this hardware, so
the cause is **not** project-specific. The common factor is a kernel
watchdog timeout — whichever daemon happens to be due for a check-in gets
named, but the underlying problem is a global kernel stall (probably AGX
driver / power-state / SPTM bugs on Apple Silicon under sustained game
load). See `debug/macos-panic/README.md` for the full analysis and
recommended user-side next steps.

Userspace apps cannot legitimately cause kernel panics. These should be filed
with Apple via Feedback Assistant — see `debug/macos-panic/README.md` for
exact instructions.

### Things FFOIE doesn't do yet

- No game logic (no weapons, no enemies, no scoring, no levels)
- No *game-state* netcode — movement and entities are local-only. v1.1 adds
  text **chat** over WebSockets, but there's no gameplay networking or
  client-side prediction yet.
- No sound
- No anti-aliasing or shadows
- Block collision is axis-separated — corners can briefly hitch, no step-up
- No "air control" / CPM strafe-acceleration term (only VQ3-style
  `PM_AirAccelerate`)

## Asset & dependency licences

- **Fox model** (`src/assets/fox.glb`): CC0 1.0 — Khronos glTF Sample Assets
- **Skybox** (`src/assets/skybox.ktx2`): sourced from the official `wgpu`
  examples (`examples/features/src/skybox/`)
- **Rust crates**: each under its own licence; see `Cargo.lock` and individual
  crate manifests in `~/.cargo/registry/`
