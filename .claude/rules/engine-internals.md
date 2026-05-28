---
paths:
  - "crates/ffoie-engine/**"
---

# Engine internals (`crates/ffoie-engine`)

Deep reference for the engine binary. Loads only when you're working in
`crates/ffoie-engine/`. The core logic is the large, heavily-commented
single-file `src/main.rs` (~2.5k lines); new functionality goes in sibling
modules (`chat.rs`, `network/`) per the module policy in the root `CLAUDE.md`.

## Per-frame flow

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

## Movement physics

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

## Collision

Axis-separated swept AABB against a hand-arranged `Vec<Block>` (the
strafe-jump course). Order is Y (so a falling player lands on block tops
cleanly) → X → Z (slides along walls without losing speed on the other
axes). Floor at `y = 0` is a hard clamp. No step-up — short obstacles block
forward velocity. See `move_and_collide` + `standing_on_ground` in
`crates/ffoie-engine/src/main.rs`.

## Renderer

wgpu 29. The cube pipeline accepts per-vertex (pos + normal) and per-instance
(pos + scale + color) buffers, so any unit-cube-derived geometry uses the
same WGSL. The fox glTF feeds through the same vertex layout but uses a
*separate* pipeline (`fox.wgsl`) because it samples a `baseColor` texture
through a bind group. Floor is one quad with a procedural grid shader
(`floor.wgsl`). Skybox is a full-screen triangle sampling a cubemap.

Surface format = adapter's first reported format; sRGB rendering via view
formats. Depth = `Depth32Float`. Present mode defaults to **Mailbox**
(display-rate, no stalls) — see the rationale comment next to `configure_surface`.

## Web port specifics

`crates/ffoie-engine/src/main.rs` compiles for both native and wasm32 via `cfg`:

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

## Chat URL (build-time)

The wasm32 chat client reads the WebSocket server URL from a compile-time
environment variable:

```
option_env!("FFOIE_CHAT_URL").unwrap_or("ws://localhost:8080/ws")
```

- **Production builds** (via `deploy-pages.yml`): `FFOIE_CHAT_URL=wss://ffoie.net/ws`
  is set in the workflow env block before `trunk build` runs. The `wss://` URL
  is baked into the wasm bundle; the browser's mixed-content policy requires
  `wss://` from an `https://` origin.
- **Local dev**: unset (falls back to `ws://localhost:8080/ws`). Run the chat
  server alongside `trunk serve web-client/index.html` and the dev build
  connects automatically.
- **Custom deploys**: `FFOIE_CHAT_URL=wss://your-host/ws trunk build ...`

Do NOT set `FFOIE_CHAT_URL` on the native build — the native engine reads the
URL at runtime via `std::env::var("FFOIE_CHAT_URL")` and falls back the same way.

## Mobile / embedded GPUs

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
