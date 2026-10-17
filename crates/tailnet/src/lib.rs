//! Everything Conductor needs from Tailscale: who is on the tailnet, who is
//! allowed in, and which machines are running Conductor.

mod cli;
mod discover;

pub use cli::{Gate, Node, Status, UserProfile, status};
pub use discover::{discover, get_json};
