# FFOIE — Engine Concept & Architecture

This file is read by Claude Code at the start of every session. It exists to
get you (or a future Claude run) oriented in a couple of minutes: *what* this
project is, *how* the code is laid out, and *why* the non-obvious decisions
were made the way they were.

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

It is *not* a game yet — no weapons, enemies, levels, AI, audio, or netcode.
The bones (input, render, physics, asset pipeline, UI, deploy) are the focus.

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

## Single-binary, single-file architecture

The entire engine lives in `src/main.rs` (~2000 lines, heavily commented).
Staying single-file is deliberate while the codebase is small: fewer module
boundaries, fewer borrow-checker fights, easier to grep. Refactoring is on
the table when the file crosses ~3000 lines or a clean seam emerges.

### Per-frame flow

1. **Frame timing**: measure `now - last_frame`, push into `sim_accumulator`.
2. **Fixed-timestep simulation** at 120 Hz. Run zero or more `tick()` calls
   from the accumulator. `tick()` is: friction → wish-vel + accelerate →
   gravity → integrate-with-collision. Decoupled from render rate.
3. **Mouse-look** is applied per-frame (not per-tick) so look latency is
   render-rate, not sim-rate. Critical for Defrag feel.
4. **Upload uniforms** (view-proj, sky transform).
5. **Build the egui UI** (HUD + optional pause menu) — `egui_ctx.run_ui`.
6. **Acquire surface texture**, encode a single render pass with these
   draws in order: cubes (instanced) → fox (textured) → floor (procedural
   grid) → sky (depth `LessEqual`) → egui.
7. Submit, present, apply menu actions (Resume / Exit).

### Movement physics

Quake's PM_Accelerate + PM_Friction directly transcribed. Constants are VQ3
defaults converted from Quake units (32 u/m) to SI:

| Quake CVar | Code | Effect |
|---|---|---|
| `sv_accelerate = 10` | `GROUND_ACCEL` | how fast you spool up walking |
| `sv_airaccelerate = 1` | `AIR_ACCEL` | **the strafe-jump knob.** 1 = VQ3, 2-5 = CPM/Defrag taste |
| `sv_friction = 6` | `FRICTION` | ground friction |
| `sv_stopspeed = 100 u/s ≈ 3.0` | `STOP_SPEED` | friction floor (below this, friction is constant) |
| `sv_maxspeed = 320 u/s ≈ 10` | `MAX_SPEED` | wish-speed cap |
| `sv_jumpvelocity = 270 ≈ 8.5` | `JUMP_VELOCITY` | jump impulse |
| `sv_gravity = 800 ≈ 25` | `GRAVITY` | downward accel |

CPM-style "air control" is **not** implemented yet — that's the natural next
movement-feel iteration if/when the user wants it.

### Collision

Axis-separated swept AABB against a hand-arranged `Vec<Block>` (the
strafe-jump course). Order is Y (so a falling player lands on block tops
cleanly) → X → Z (slides along walls without losing speed on the other
axes). Floor at `y = 0` is a hard clamp. No step-up — short obstacles block
forward velocity. See `move_and_collide` + `standing_on_ground`.

### Renderer

wgpu 29. The cube pipeline accepts per-vertex (pos + normal) and per-instance
(pos + scale + color) buffers, so any unit-cube-derived geometry uses the
same WGSL. The fox glTF feeds through the same vertex layout but uses a
*separate* pipeline (`fox.wgsl`) because it samples a `baseColor` texture
through a bind group. Floor is one quad with a procedural grid shader
(`floor.wgsl`). Skybox is a full-screen triangle sampling a cubemap.

Surface format = adapter's first reported format; sRGB rendering via view
formats. Depth = `Depth32Float`. Present mode defaults to **Mailbox**
(display-rate, no stalls) — see the rationale comment next to `configure_surface`.

### Web port specifics

`main.rs` compiles for both native and wasm32 via `cfg`:

- Native `fn main()` initialises `env_logger` and runs `event_loop.run_app`.
- Web `#[wasm_bindgen(start)] pub fn run_wasm()` initialises `console_log`
  + `console_error_panic_hook` and uses `EventLoopExtWebSys::spawn_app`
  because JS owns the requestAnimationFrame tick.

`pollster::block_on(State::new(...))` can't block on the web. Async init is
hoisted into `wasm_bindgen_futures::spawn_local`; the result lands in a
`Rc<RefCell<Option<State>>>` field on App, and the next event after init
adopts it.

Two HiDPI safety caps:

- `max_texture_dimension_2d` is requested at the adapter's real max (default
  8192 is too small for a 5K-class canvas).
- The render framebuffer is clamped to `MAX_RENDER_DIM = 4096` per axis;
  the browser upscales it to the canvas's CSS size.

### Mobile / embedded GPUs

For low-end Vulkan stacks (Mali-G52 / Bifrost v7) the device descriptor uses
`wgpu::Limits::downlevel_defaults()` instead of the WebGPU spec defaults —
PanVK on Mali only promises e.g. `max_texture_dimension_3d = 512`, well below
the spec floor of 2048. We then override `max_texture_dimension_2d` back to
the adapter's real max so HiDPI native displays still get a depth attachment
that matches.

To force Vulkan where wgpu would default to GL, use `run-vulkan.sh` — it
sets `WGPU_BACKEND=vulkan`, `WGPU_ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER=1`,
and the PanVK ICD path. **The `_from_env` variant of `InstanceDescriptor`
is what makes wgpu read these env vars** — using the non-`_from_env`
variant silently ignores them.

---

## File / directory map

```
ffoie/
├── Cargo.toml              wgpu/winit/egui/glam/bytemuck/gltf/image/ktx2
├── README.md               install + build per platform — read first
├── CLAUDE.md               this file
├── run-vulkan.sh           Mali-G52 / PanVK launcher
├── CNAME                   ffoie.net — drives the Pages routing logic below
├── .github/workflows/
│   └── deploy-pages.yml    GitHub Pages deploy (custom domain ffoie.net)
├── website/                landing site source (served at /)
│   └── index.html          hand-written, no framework; add images/videos here
├── web-client/             trunk manifest for the wasm game (served at /web/)
│   └── index.html          <link data-trunk rel="rust" ...> — loads the wasm
├── src/
│   ├── main.rs             everything (single-file engine)
│   ├── shader.wgsl         instanced lit geometry (cubes)
│   ├── floor.wgsl          procedural grid floor (notebook style)
│   ├── fox.wgsl            textured-mesh shader (Fox glTF)
│   ├── sky.wgsl            cubemap skybox
│   └── assets/
│       ├── skybox.ktx2     skybox cubemap (from wgpu's skybox example)
│       └── fox.glb         Khronos CC0 Fox glTF
└── debug/
    └── macos-panic/        kernel panic logs observed during dev
                            (all Apple-side bugs — none mention FFOIE)
```

### Pages route layout

- `ffoie.net/`         → `website/index.html` (landing)
- `ffoie.net/web/`     → trunk build of `web-client/index.html` (the game)

The workflow builds the game with `--public-url "/web/" --dist dist/web
web-client/index.html`, then `cp -R website/. dist/` lays the landing
files on top. Local trunk builds (`trunk serve web-client/index.html`)
still work for iterating on just the game.

Build-artifact paths:

- Native `target/` is redirected outside iCloud to `/Users/a/.cargo-target/foie/`
  via `../.cargo/config.toml` so iCloud doesn't try to sync GBs of objects.
- Web `dist/` (trunk output) is git-ignored.

---

## Conventions

- **Commit messages are terse single lines**: "Initial commit", "Fix Windows
  linker step", "Add fox texture pipeline". No body unless really needed.
  **No `Co-Authored-By:` footer.**
- Rust style is whatever `cargo fmt` produces; no extra rules.
- Constants at the top of `main.rs` are **designed to be tuned**. Most are
  movement-feel knobs.
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
