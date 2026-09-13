//! Browser-owned cache for reader manifests and flow chunks.
//!
//! The manifest is small enough for `localStorage`; HTML chunks belong in the
//! browser Cache API so a large book does not consume the synchronous storage
//! quota or block the main thread. Cache keys include the server flow version
//! and source fingerprint, so reusing a file id for new bytes cannot expose an
//! old book to the reader.

use revaro_core::reader::FlowManifest;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::browser;
use crate::logic::reader::validate_manifest;

const MANIFEST_PREFIX: &str = "revaro-reader-manifest:";
const CACHE_NAME: &str = "revaro-reader-flow-v1";

/// Read a previously validated manifest from local storage.
pub fn load_manifest(file_id: &str) -> Option<FlowManifest> {
    let raw = browser::local_storage_get(&format!("{MANIFEST_PREFIX}{file_id}"))?;
    let manifest = serde_json::from_str(&raw).ok()?;
    validate_manifest(&manifest).then_some(manifest)
}

/// Store a manifest as a best-effort fast-open hint.
pub fn store_manifest(file_id: &str, manifest: &FlowManifest) {
    let Ok(raw) = serde_json::to_string(manifest) else {
        return;
    };
    browser::local_storage_set(&format!("{MANIFEST_PREFIX}{file_id}"), &raw);
}

/// Build a cache key that isolates file ids, flow versions and source keys.
pub fn chunk_cache_key(file_id: &str, manifest: &FlowManifest, index: i32) -> String {
    format!(
        "/api/files/{file_id}/book/flow/chunks/{index}?reader_cache={}-{}-{}",
        manifest.version,
        hex_token(&manifest.book_key),
        layout_token(manifest)
    )
}

/// Read one flow chunk from Cache Storage, returning `None` for unsupported or
/// unavailable browser storage.
pub async fn get_chunk(key: &str) -> Option<String> {
    let cache = open_cache().await?;
    let value = JsFuture::from(cache.match_with_str(key)).await.ok()?;
    if value.is_null() || value.is_undefined() {
        return None;
    }
    let response = value.dyn_into::<web_sys::Response>().ok()?;
    if !response.ok() {
        return None;
    }
    let text = response.text().ok()?;
    JsFuture::from(text).await.ok()?.as_string()
}

/// Store one successful flow chunk in Cache Storage. A cache failure never
/// turns a readable book into an error; the in-memory cache remains authoritative
/// for the current view.
pub async fn put_chunk(key: &str, html: &str) {
    let Some(cache) = open_cache().await else {
        return;
    };
    let Ok(response) = web_sys::Response::new_with_opt_str(Some(html)) else {
        return;
    };
    let _ = JsFuture::from(cache.put_with_str(key, &response)).await;
}

async fn open_cache() -> Option<web_sys::Cache> {
    let window = web_sys::window()?;
    let storage = window.caches().ok()?;
    let value = JsFuture::from(storage.open(CACHE_NAME)).await.ok()?;
    value.dyn_into::<web_sys::Cache>().ok()
}

fn hex_token(value: &str) -> String {
    let mut token = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        use std::fmt::Write;
        let _ = write!(token, "{byte:02x}");
    }
    if token.is_empty() {
        "empty".to_owned()
    } else {
        token
    }
}

fn layout_token(manifest: &FlowManifest) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    let mut feed = |value: &str| {
        for byte in value.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    };
    feed(&manifest.version.to_string());
    feed(&manifest.format);
    feed(&manifest.total_chars.to_string());
    feed(&manifest.book_key);
    for spine in &manifest.spines {
        feed(&spine.block_start.to_string());
        feed(&spine.block_count.to_string());
    }
    for chunk in &manifest.chunks {
        feed(&chunk.index.to_string());
        feed(&chunk.block_start.to_string());
        feed(&chunk.block_count.to_string());
        feed(&chunk.chars.to_string());
    }
    for entry in &manifest.toc {
        feed(&entry.label);
        feed(&entry.depth.to_string());
        feed(&entry.spine.to_string());
        feed(&entry.block.to_string());
        feed(&entry.nav_anchor);
        for path in &entry.text_path {
            feed(&path.to_string());
        }
        feed(&entry.text_offset.to_string());
        feed(&entry.chunk.to_string());
        feed(&entry.source_path);
        feed(&entry.source_fragment);
    }
    format!("{hash:016x}")
}
