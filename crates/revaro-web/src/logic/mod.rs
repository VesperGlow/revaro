//! Pure client logic, compiled for every target.
//!
//! These modules are deliberately free of DOM, Leptos and WASM APIs. They are
//! the Rust home of the small TypeScript utilities the shell already needs
//! (`format.ts`, `imageGeometry.ts`, `taskStatus.ts`) plus a new guard for the
//! stylesheet cascade. Because they do not depend on the browser they carry
//! their unit tests natively, so `cargo test -p revaro-web` exercises the same
//! code the wasm build ships.
//!
//! Anything that touches the DOM lives in `crate::app` / `crate::components`
//! behind `#[cfg(target_arch = "wasm32")]` instead.

pub mod format;
pub mod image_geometry;
pub mod routing;
pub mod stylesheet;
pub mod task_status;
