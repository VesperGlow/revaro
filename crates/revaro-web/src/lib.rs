//! The Revaro browser client.
//!
//! Rendered client-side with [Leptos]: the server ships this module plus a small
//! HTML shell, and everything after that runs in the browser. A single-page
//! application is the right shape here because the product's core is
//! long-lived interactive state — the reader's pagination, media playback with
//! progress sync, the upload queue and the task centre — none of which benefit
//! from server round-trips per navigation.
//!
//! The crate compiles to an empty stub on non-wasm targets so that
//! `cargo check --workspace` and `cargo clippy --workspace` stay meaningful on a
//! developer machine without the wasm toolchain.
//!
//! [Leptos]: https://leptos.dev

#![cfg_attr(target_arch = "wasm32", forbid(unsafe_code))]

#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
pub use app::*;

/// Version of the client, surfaced in the UI and used to detect a stale bundle.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
