//! network — WebSocket client module for the FFOIE engine.
//!
//! Platform implementations live in:
//! - `native.rs` — tokio-backed implementation (native targets)
//! - `wasm.rs`   — wasm32 implementation (wasm32 target, Phase 4 plan 04-02)
//!
//! `types.rs` contains the shared type definitions compiled on all platforms.

pub mod types;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;

// Re-export shared types at the module root so all call sites stay unchanged.
pub use types::{AppEvent, NetworkCommand, NetworkEvent, NetworkHandle};

// Re-export the start() entry point and the backoff helper for native builds.
// (`backoff_delay_ms` is consumed by native.rs's unit tests; the re-export
// keeps it reachable so the binary crate doesn't flag it as dead code.)
#[cfg(not(target_arch = "wasm32"))]
pub use native::{backoff_delay_ms, start};

#[cfg(target_arch = "wasm32")]
pub mod wasm;

// Re-export wasm start() so call sites use network::start() on all platforms.
#[cfg(target_arch = "wasm32")]
pub use wasm::start;
