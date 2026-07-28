//! Cleanup and transformation via a chat model.
//!
//! [`prompt`] holds the deterministic pre-pass and the system prompts;
//! [`client`] makes the HTTP call. Snippet expansion always happens in Rust
//! first — it is exact and free — and a failed cleanup degrades to the raw
//! transcript rather than failing the session.

pub mod client;
pub mod prompt;
