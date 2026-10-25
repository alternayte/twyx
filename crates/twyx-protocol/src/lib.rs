//! Wire types for the holder protocol, the client protocol and API v1.
//!
//! The holder and client protocols use one compact binary encoding. API v1
//! uses JSON and is documented separately.

pub mod api;
pub mod client;
pub mod connection;
pub mod dialog;
pub mod grid;
pub mod holder;
pub mod input;
pub mod palette;
pub mod peer;
pub mod sidebar;
pub mod view;
pub mod wire;
pub mod workspaces;

/// This build's identity. A client and the daemon run the same build.
pub const BUILD_ID: &str = env!("TWYX_BUILD_ID");
