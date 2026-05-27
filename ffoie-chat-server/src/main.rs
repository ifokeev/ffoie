// ffoie-chat-server — Phase 1 stub.
// Real server logic (axum, tokio, sqlx, broadcast) lands in Phase 2.
// This binary exists to satisfy the workspace members declaration and to
// verify the ffoie-protocol path dependency resolves at compile time.

use ffoie_protocol::ServerMessage;

fn main() {
    // Reference ServerMessage so the import is not flagged as unused.
    let _: Option<ServerMessage> = None;
    println!("ffoie-chat-server stub — see Phase 2 for real implementation");
}
