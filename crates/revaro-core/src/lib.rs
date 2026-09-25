//! # revaro-core
//!
//! The single source of truth shared by every Rust crate in the Revaro
//! workspace: the Axum backend (`revaro-server`), the media engine
//! (`revaro-media`), the reader engine (`revaro-reader`) and the Leptos
//! frontend (`revaro-web`).
//!
//! Everything in this crate is deliberately runtime-agnostic. It must compile
//! for `x86_64-unknown-linux-gnu` (native server) *and*
//! `wasm32-unknown-unknown` (browser client), so it may not depend on tokio,
//! rusqlite, the filesystem or any other native facility.
//!
//! The crate is organised around four ideas:
//!
//! * [`model`] — the persisted domain model. Field names and wire shapes match
//!   the existing SQLite schema and HTTP API byte for byte, so the storage
//!   layout and any existing data stay valid.
//! * [`api`] — request and response payloads for the HTTP surface.
//! * [`error`] — one error model (`{"error":{"status","code","message"}}`) used
//!   by the backend, the task system and the frontend alike.
//! * [`validate`] — the input rules (names, sizes, MIME types, locators) that
//!   used to live in scattered Go helpers and TypeScript guards.

#![forbid(unsafe_code)]

pub mod api;
pub mod classify;
pub mod error;
pub mod hash;
pub mod ids;
pub mod keys;
pub mod limits;
pub mod media;
pub mod model;
pub mod reader;
pub mod storage;
pub mod time;
pub mod validate;

pub use error::{ApiError, ErrorCode, ErrorEnvelope, Result};
pub use time::Timestamp;
