//! Wire types for the holder protocol, the client protocol and API v1.
//!
//! The holder and client protocols use one compact binary encoding. API v1
//! uses JSON and is documented separately.

pub mod client;
pub mod connection;
pub mod grid;
pub mod holder;
pub mod input;
pub mod view;
pub mod wire;
