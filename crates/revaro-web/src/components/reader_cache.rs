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
const CACHE_NAME: &str = "revaro-reader-flow-v2";
const MAX_MANIFESTS: usize = 32;
const MAX_CHUNKS: u32 = 256;
const MAX_BYTES: usize = 32 << 20;
const TTL_MS: f64 = 7.0 * 86400.0 * 1000.0;
thread_local! { static EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; static WRITE_LOCK: std::rc::Rc<futures_util::lock::Mutex<()>> = std::rc::Rc::new(futures_util::lock::Mutex::new(())); }
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedManifest {
    saved_at: f64,
    manifest: FlowManifest,
}

pub fn clear_all() {
    EPOCH.with(|epoch| epoch.set(epoch.get().wrapping_add(1)));
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let keys = (0..storage.length().unwrap_or_default())
            .filter_map(|i| storage.key(i).ok().flatten())
            .filter(|k| k.starts_with(MANIFEST_PREFIX))
            .collect::<Vec<_>>();
        for key in keys {
            let _ = storage.remove_item(&key);
        }
    }
    let lock = WRITE_LOCK.with(Clone::clone);
    wasm_bindgen_futures::spawn_local(async move {
        let _guard = lock.lock().await;
        if let Some(storage) = web_sys::window().and_then(|w| w.caches().ok()) {
            let _ = JsFuture::from(storage.delete(CACHE_NAME)).await;
            let _ = JsFuture::from(storage.delete("revaro-reader-flow-v1")).await;
        }
    });
}
fn trim_manifests() {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    let mut entries = Vec::new();
    let keys = (0..storage.length().unwrap_or_default())
        .filter_map(|i| storage.key(i).ok().flatten())
        .filter(|k| k.starts_with(MANIFEST_PREFIX))
        .collect::<Vec<_>>();
    for key in keys {
        let saved = storage
            .get_item(&key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<SavedManifest>(&raw).ok());
        if let Some(saved) = saved.filter(|v| js_sys::Date::now() - v.saved_at < TTL_MS) {
            entries.push((key, saved.saved_at));
        } else {
            let _ = storage.remove_item(&key);
        }
    }
    entries.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (key, _) in entries.into_iter().skip(MAX_MANIFESTS) {
        let _ = storage.remove_item(&key);
    }
}

/// Read a previously validated manifest from local storage.
pub fn load_manifest(file_id: &str) -> Option<FlowManifest> {
    let raw = browser::local_storage_get(&format!("{MANIFEST_PREFIX}{file_id}"))?;
    let saved: SavedManifest = serde_json::from_str(&raw).ok()?;
    if js_sys::Date::now() - saved.saved_at >= TTL_MS {
        return None;
    }
    let manifest = saved.manifest;
    validate_manifest(&manifest).then_some(manifest)
}

/// Store a manifest as a best-effort fast-open hint.
///
/// The reference client invalidates a book's persisted chunks whenever the
/// manifest version or content fingerprint changes. The URL key already keeps
/// versions separate, but removing the old entries is still part of the
/// observable cache lifecycle: a later downgrade or test/open of the same file
/// must not reuse chunks left by an earlier version.
pub fn store_manifest(file_id: &str, manifest: &FlowManifest) {
    let previous = load_manifest(file_id);
    let Ok(raw) = serde_json::to_string(&SavedManifest {
        saved_at: js_sys::Date::now(),
        manifest: manifest.clone(),
    }) else {
        return;
    };
    browser::local_storage_set(&format!("{MANIFEST_PREFIX}{file_id}"), &raw);
    trim_manifests();
    let changed = previous.as_ref().is_none_or(|previous| {
        previous.book_key != manifest.book_key || previous.version != manifest.version
    });
    if changed {
        let file_id = file_id.to_owned();
        wasm_bindgen_futures::spawn_local(async move {
            purge_file_chunks(&file_id).await;
        });
    }
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
    let saved = response
        .headers()
        .get("x-revaro-saved")
        .ok()
        .flatten()
        .and_then(|s| s.parse::<f64>().ok())?;
    if js_sys::Date::now() - saved >= TTL_MS {
        let _ = JsFuture::from(cache.delete_with_str(key)).await;
        return None;
    }
    let text = response.text().ok()?;
    JsFuture::from(text).await.ok()?.as_string()
}

/// Store one successful flow chunk in Cache Storage. A cache failure never
/// turns a readable book into an error; the in-memory cache remains authoritative
/// for the current view.
pub async fn put_chunk(key: &str, html: &str) {
    if html.len() > MAX_BYTES {
        return;
    }
    // Serialize admissions so simultaneous prefetches cannot each observe spare capacity.
    let epoch = EPOCH.with(std::cell::Cell::get);
    let lock = WRITE_LOCK.with(Clone::clone);
    {
        let _guard = lock.lock().await;
        if EPOCH.with(std::cell::Cell::get) != epoch {
            return;
        }
        let Some(cache) = open_cache().await else {
            return;
        };
        let Ok(value) = JsFuture::from(cache.keys()).await else {
            return;
        };
        let requests = js_sys::Array::from(&value);
        let mut entries = Vec::new();
        let mut bytes = 0_usize;
        for value in requests.iter() {
            let Ok(request) = value.dyn_into::<web_sys::Request>() else {
                continue;
            };
            let Ok(value) = JsFuture::from(cache.match_with_request(&request)).await else {
                continue;
            };
            let Ok(response) = value.dyn_into::<web_sys::Response>() else {
                continue;
            };
            let saved = response
                .headers()
                .get("x-revaro-saved")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or_default();
            let size = response
                .headers()
                .get("x-revaro-size")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or_default();
            if js_sys::Date::now() - saved >= TTL_MS || request.url().ends_with(key) {
                let _ = JsFuture::from(cache.delete_with_str(&request.url())).await;
            } else {
                bytes = bytes.saturating_add(size);
                entries.push((request.url(), saved, size));
            }
        }
        entries.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut count = entries.len() as u32;
        for (url, _, size) in entries {
            if count < MAX_CHUNKS && bytes.saturating_add(html.len()) <= MAX_BYTES {
                break;
            }
            let _ = JsFuture::from(cache.delete_with_str(&url)).await;
            bytes = bytes.saturating_sub(size);
            count = count.saturating_sub(1);
        }
        let Ok(response) = web_sys::Response::new_with_opt_str(Some(html)) else {
            return;
        };
        let _ = response
            .headers()
            .set("x-revaro-saved", &js_sys::Date::now().to_string());
        let _ = response
            .headers()
            .set("x-revaro-size", &html.len().to_string());
        let _ = JsFuture::from(cache.put_with_str(key, &response)).await;
    }
}

async fn purge_file_chunks(file_id: &str) {
    let Some(cache) = open_cache().await else {
        return;
    };
    let Ok(value) = JsFuture::from(cache.keys()).await else {
        return;
    };
    let Ok(requests) = value.dyn_into::<js_sys::Array>() else {
        return;
    };
    let path = format!("/api/files/{file_id}/book/flow/chunks/");
    for value in requests.iter() {
        let Ok(request) = value.dyn_into::<web_sys::Request>() else {
            continue;
        };
        if request.url().contains(&path) {
            let _ = JsFuture::from(cache.delete_with_str(&request.url())).await;
        }
    }
}

async fn open_cache() -> Option<web_sys::Cache> {
    let window = web_sys::window()?;
    let storage = window.caches().ok()?;
    let _ = JsFuture::from(storage.delete("revaro-reader-flow-v1")).await;
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
