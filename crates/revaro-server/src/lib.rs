//! The Revaro backend.
//!
//! This crate replaces the Go server. The port is staged: this module tree grows
//! one feature module at a time, and every stage keeps the binary building,
//! running and testable. The pieces that exist so far are the process
//! configuration, the shared error-to-response mapping, the security middleware
//! and static serving of the Leptos client.
//!
//! Deliberate architectural change from the Go server: the Rust media engine
//! becomes a library linked into this process, so the loopback HTTP sidecar,
//! its per-process bearer token and its supervision code all disappear.

#![forbid(unsafe_code)]

pub mod auth;
pub mod auth_routes;
pub mod config;
pub mod db;
pub mod error;
pub mod ids;
pub mod middleware;
pub mod proxy;
pub mod router;
pub mod state;
pub mod storage;
pub mod web;

pub use config::Config;
pub use error::ApiResult;
