//! Versioned, authenticated read-through caching for all catalog consumers.
//! Original media stays in Linux's page cache; only derived/serialized data
//! enters the shared L1. Writes publish a new revision before returning.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::body::HttpBody;
use axum::body::{Body, to_bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode};

use crate::auth::extract::AuthUser;
use crate::cache::{
    DIRECTORY_LISTING, FILE_METADATA, LIBRARY_COLLECTIONS, LIBRARY_LISTING, SEARCH_RESULTS,
};
use crate::state::AppState;

const MAX_RESPONSE: u64 = 32 << 20;
const TTL: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub struct QueryRuntime {
    locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    prefetch: Arc<tokio::sync::Semaphore>,
    reader_warm: Arc<tokio::sync::Semaphore>,
}

impl Default for QueryRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryRuntime {
    pub fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
            prefetch: Arc::new(tokio::sync::Semaphore::new(2)),
            reader_warm: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    async fn lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Weak references never retain results or live tasks. Sweep in batches
            // so warm reads don't scan the entire key map.
            if locks.len() >= 1024 {
                locks.retain(|_, value| value.strong_count() > 0);
            }
            if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key.to_owned(), Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }
}

#[derive(Debug)]
struct CachedResponse {
    headers: HeaderMap,
    body: Bytes,
}
impl CachedResponse {
    fn response(&self, hit: &'static str) -> Response {
        let mut response = Response::new(Body::from(self.body.clone()));
        *response.headers_mut() = self.headers.clone();
        response
            .headers_mut()
            .insert("x-revaro-cache", hit.parse().unwrap());
        response
    }
}

/// Source metadata shared by every media, reader and original-byte consumer.
/// Keep feature validation outside this resolver so existing status codes and
/// trash-preview behavior remain unchanged.
pub(crate) async fn file(
    state: &Arc<AppState>,
    id: String,
) -> Result<revaro_core::model::File, crate::db::DbError> {
    let revision = state.db.revision();
    let key = format!("model:{revision}:{}", serde_json::to_string(&id).unwrap());
    if let Ok(Some(file)) = state
        .cache
        .get_object::<revaro_core::model::File>(FILE_METADATA, &key)
    {
        return Ok((*file).clone());
    }
    let _guard = state.queries.lock(&key).await;
    if let Ok(Some(file)) = state
        .cache
        .get_object::<revaro_core::model::File>(FILE_METADATA, &key)
    {
        return Ok((*file).clone());
    }
    let file = state
        .db
        .call(move |connection| crate::file_routes::lookup_file_any(connection, &id))
        .await?;
    if state.db.revision() == revision {
        let size = serde_json::to_vec(&file)
            .map(|v| v.len() as i64 * 3 + 1024)
            .unwrap_or(4096);
        let _ = state
            .cache
            .put_object(FILE_METADATA, &key, Arc::new(file.clone()), size, TTL);
    }
    Ok(file)
}

fn class(path: &str, query: Option<&str>) -> Option<&'static str> {
    let search = query.is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes()).any(|(k, v)| k == "q" && !v.is_empty())
    });
    match path {
        "/api/files" => Some(if search {
            SEARCH_RESULTS
        } else {
            DIRECTORY_LISTING
        }),
        "/api/library/items" => Some(if search {
            SEARCH_RESULTS
        } else {
            LIBRARY_LISTING
        }),
        "/api/library/collections" | "/api/library/stacks" => Some(LIBRARY_COLLECTIONS),
        _ => {
            let rest = path.strip_prefix("/api/files/")?;
            let mut parts = rest.split('/');
            let id = parts.next()?;
            if !revaro_core::ids::is_uuid(id) {
                return None;
            }
            match (parts.next(), parts.next()) {
                (None, None) => Some(FILE_METADATA),
                (Some("children"), None) => Some(DIRECTORY_LISTING),
                _ => None,
            }
        }
    }
}

fn cache_key(request: &Request, revision: u64) -> String {
    let mut query =
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
            .into_owned()
            .collect::<Vec<_>>();
    query.sort();
    // Normalize optional defaults so warmup and independent API consumers share
    // entries. Retain duplicate keys: Axum rejects those and they must never
    // alias a valid request already in the cache.
    if !query.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        let path = request.uri().path();
        query.retain(|(name, value)| {
            let common = matches!(
                (name.as_str(), value.as_str()),
                ("q", "") | ("sort", "") | ("offset", "0")
            );
            let files_default = path == "/api/files"
                && (common
                    || (name == "descending" && value == "false")
                    || (name == "sort" && value == "name")
                    || (name == "limit" && value == "100")
                    || (name == "parent_id" && value == revaro_core::ids::ROOT_ID));
            let library_default = path == "/api/library/items"
                && (common
                    || matches!(
                        (name.as_str(), value.as_str()),
                        ("favorite", "false")
                            | ("recent", "false")
                            | ("opened_only", "false")
                            | ("group_stacks", "false")
                    )
                    || (name == "limit" && value == "60")
                    || (name == "sort" && value == "created"));
            !(files_default || library_default)
        });
    }
    // Length-delimited JSON keeps arbitrary searches out of the key grammar.
    format!(
        "{revision}:{}:{}",
        request.uri().path(),
        serde_json::to_string(&query).unwrap()
    )
}

pub async fn catalog(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if request.method() != Method::GET || request.uri().to_string().len() > 4096 {
        return next.run(request).await;
    }
    let Some(class) = class(request.uri().path(), request.uri().query()) else {
        return next.run(request).await;
    };
    let (mut parts, body) = request.into_parts();
    let user = match AuthUser::from_request_parts(&mut parts, &state).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    parts.extensions.insert(user);
    let request = Request::from_parts(parts, body);
    let revision = state.db.revision();
    let key = cache_key(&request, revision);
    if let Ok(Some(cached)) = state.cache.get_object::<CachedResponse>(class, &key) {
        return cached.response("hit");
    }
    let _guard = state.queries.lock(&key).await;
    if let Ok(Some(cached)) = state.cache.get_object::<CachedResponse>(class, &key) {
        return cached.response("coalesced");
    }
    let warm_books = request.uri().path() == "/api/library/items";
    let prefetch = next_page_request(&request);
    let prefetch_next = next.clone();
    let response = next.run(request).await;
    if response.status() != StatusCode::OK
        || response
            .body()
            .size_hint()
            .upper()
            .is_none_or(|size| size > MAX_RESPONSE)
    {
        return response;
    }
    let (parts, body) = response.into_parts();
    let body = match to_bytes(body, MAX_RESPONSE as usize).await {
        Ok(body) => body,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let cached = Arc::new(CachedResponse {
        headers: parts.headers,
        body,
    });
    if state.db.revision() == revision {
        let size = cached.body.len() as i64 + key.len() as i64 + 1024;
        let _ = state
            .cache
            .put_object(class, &key, cached.clone(), size, TTL);
        if warm_books
            && state.cache.stats().memory_limit >= 256 << 20
            && let Ok(listing) =
                serde_json::from_slice::<revaro_core::library::LibraryListing>(&cached.body)
        {
            for item in listing
                .items
                .into_iter()
                .filter(|item| item.kind == "book" && item.file.size <= 16 << 20)
                .take(1)
            {
                let Ok(permit) = state.queries.reader_warm.clone().try_acquire_owned() else {
                    break;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if state.reader.work_slots.available_permits() > 1 {
                        let _ = crate::reader_routes::load_book(state, &item.file).await;
                    }
                });
            }
        }
        // Read ahead only one page, only when there is headroom. Prefetches
        // neither recursively prefetch nor compete in an unbounded work queue.
        if let Some(request) = prefetch
            && page_has_more(&cached.body)
            && state.cache.stats().memory_limit >= 64 << 20
            && let Ok(permit) = state.queries.prefetch.clone().try_acquire_owned()
        {
            let state = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let revision = state.db.revision();
                let key = cache_key(&request, revision);
                if state.cache.has(class, &key).unwrap_or(false) {
                    return;
                }
                let _guard = state.queries.lock(&key).await;
                if state.cache.has(class, &key).unwrap_or(false) {
                    return;
                }
                let response = prefetch_next.run(request).await;
                if response.status() != StatusCode::OK
                    || response
                        .body()
                        .size_hint()
                        .upper()
                        .is_none_or(|size| size > MAX_RESPONSE)
                {
                    return;
                }
                let (parts, body) = response.into_parts();
                if let Ok(body) = to_bytes(body, MAX_RESPONSE as usize).await
                    && state.db.revision() == revision
                {
                    let size = body.len() as i64 + key.len() as i64 + 1024;
                    let _ = state.cache.put_object(
                        class,
                        &key,
                        Arc::new(CachedResponse {
                            headers: parts.headers,
                            body,
                        }),
                        size,
                        TTL,
                    );
                }
            });
        }
    }
    cached.response("miss")
}

fn next_page_request(request: &Request) -> Option<Request> {
    if !matches!(request.uri().path(), "/api/files" | "/api/library/items") {
        return None;
    }
    let mut query = url::form_urlencoded::parse(request.uri().query()?.as_bytes())
        .into_owned()
        .collect::<Vec<_>>();
    let offset = query
        .iter()
        .find(|(k, _)| k == "offset")
        .and_then(|(_, v)| v.parse::<i64>().ok())
        .unwrap_or(0);
    let limit = query
        .iter()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.parse::<i64>().ok())
        .unwrap_or(if request.uri().path() == "/api/files" {
            100
        } else {
            60
        });
    if offset < 0 || !(1..=200).contains(&limit) {
        return None;
    }
    query.retain(|(k, _)| k != "offset");
    query.push(("offset".into(), offset.checked_add(limit)?.to_string()));
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query)
        .finish();
    let mut next = Request::builder()
        .uri(format!("{}?{query}", request.uri().path()))
        .body(Body::empty())
        .ok()?;
    *next.headers_mut() = request.headers().clone();
    *next.extensions_mut() = request.extensions().clone();
    Some(next)
}

fn page_has_more(body: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Page {
        offset: i64,
        limit: i64,
        total: i64,
    }
    serde_json::from_slice::<Page>(body)
        .is_ok_and(|page| page.offset.saturating_add(page.limit) < page.total)
}

/// Warm small entry pages through the same handlers and cache policy. This
/// server-created extension never comes from HTTP headers; session checking
/// remains mandatory for every external request, including cache hits.
pub async fn warm_catalog(state: Arc<AppState>) -> Result<(), String> {
    use tower::ServiceExt;
    let app = crate::router::build(state.clone());
    for path in [
        "/api/library/collections",
        "/api/files?limit=100&offset=0&sort=name",
        "/api/library/items?kind=book&group_stacks=true&limit=60&offset=0",
        "/api/library/items?kind=audio&limit=60&offset=0",
        "/api/library/items?kind=image&limit=60&offset=0",
        "/api/library/items?kind=video&limit=60&offset=0",
    ] {
        if state.cache.stats().memory_limit < 256 << 20 {
            break;
        }
        let request = Request::builder()
            .uri(path)
            .extension(AuthUser {
                username: String::new(),
                token: String::new(),
            })
            .body(Body::empty())
            .map_err(|e| e.to_string())?;
        let response = app
            .clone()
            .oneshot(request)
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("warmup {path}: {}", response.status()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    async fn response(state: &Arc<AppState>, path: &str, authenticated: bool) -> Response {
        let mut request = Request::builder().uri(path);
        if authenticated {
            request = request.header(
                "cookie",
                format!("{}=catalog-test", crate::auth::SESSION_COOKIE),
            );
        }
        crate::router::build(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn cached_catalog_checks_auth_and_committed_writes_invalidate_all_consumers() {
        let state = crate::router::tests::test_state().await;
        let hash = crate::auth::token_hash("catalog-test");
        state.db.call(move |c| {
            c.execute("INSERT INTO settings(key,value,updated_at) VALUES('admin_username','admin','2026-01-01T00:00:00Z')",[])?;
            c.execute("INSERT INTO sessions(id,token_hash,created_at,expires_at) VALUES('catalog-test',?1,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')",[hash])?;
            Ok(())
        }).await.unwrap();
        let first = response(&state, "/api/library/collections", true).await;
        assert_eq!(first.headers()["x-revaro-cache"], "miss");
        let second = response(&state, "/api/library/collections", true).await;
        assert_eq!(second.headers()["x-revaro-cache"], "hit");
        assert_eq!(second.headers()["cache-control"], "no-store");
        assert_eq!(
            response(&state, "/api/library/collections", false)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        state.db.call(|c|{c.execute("INSERT INTO library_collections(id,name,kind,created_at) VALUES('shelf','轻小说','book','2026-01-01T00:00:00Z')",[])?;Ok(())}).await.unwrap();
        let changed = response(&state, "/api/library/collections", true).await;
        assert_eq!(changed.headers()["x-revaro-cache"], "miss");
        let body = to_bytes(changed.into_body(), MAX_RESPONSE as usize)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("轻小说"));
        state.cache.set_memory_limit(0);
        response(&state, "/api/library/collections", true).await;
        assert_eq!(state.cache.stats().memory_bytes, 0);
        state.cache.set_memory_limit(state.memory.cache_bytes);
        response(&state, "/api/library/collections", true).await;
        state
            .db
            .call(|c| {
                c.execute("DELETE FROM sessions", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            response(&state, "/api/library/collections", true)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        tokio::fs::remove_dir_all(state.store.root()).await.unwrap();
    }
    #[test]
    fn warmup_keys_match_clients_without_aliasing_rejected_queries() {
        let req = |query: &str| {
            Request::builder()
                .uri(format!("/api/library/items?{query}"))
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            cache_key(&req("kind=book&limit=60&offset=0&favorite=false&sort="), 1),
            cache_key(&req("kind=book"), 1)
        );
        assert_ne!(
            cache_key(&req("kind=book&offset=0&offset=1"), 1),
            cache_key(&req("kind=book&offset=1"), 1)
        );
        assert_ne!(
            cache_key(&req("kind=book&limit=0"), 1),
            cache_key(&req("kind=book"), 1)
        );
    }
    #[test]
    fn unknown_defaults_cannot_bypass_route_query_validation() {
        let request = |path: &str| Request::builder().uri(path).body(Body::empty()).unwrap();
        assert_ne!(
            cache_key(&request("/api/files?favorite=false"), 1),
            cache_key(&request("/api/files"), 1)
        );
        assert_ne!(
            cache_key(&request("/api/library/items?descending=false"), 1),
            cache_key(&request("/api/library/items"), 1)
        );
    }
    #[tokio::test]
    async fn concurrent_catalog_reads_share_one_result_and_file_models_follow_writes() {
        let state = crate::router::tests::test_state().await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let app = axum::Router::new()
            .route(
                "/api/library/collections",
                axum::routing::get(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        axum::Json(Vec::<String>::new())
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(state.clone(), catalog))
            .with_state(state.clone());
        let requests = (0..32).map(|_| {
            let app = app.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .uri("/api/library/collections")
                        .extension(AuthUser {
                            username: "internal".into(),
                            token: String::new(),
                        })
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        });
        let results = futures_util::future::join_all(requests).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(results.iter().all(|r| r.status() == StatusCode::OK));
        assert!(
            results
                .iter()
                .any(|r| r.headers()["x-revaro-cache"] == "coalesced")
        );
        state.db.call(|c| {
            c.execute("INSERT INTO files(id,parent_id,name,kind,object_key,size,status,etag,created_at,updated_at) VALUES('model-test',?1,'book.txt','file','blobs/model-test',12,'ready','first','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",[revaro_core::ids::ROOT_ID])?;Ok(())
        }).await.unwrap();
        assert_eq!(
            file(&state, "model-test".into()).await.unwrap().etag,
            "first"
        );
        state
            .db
            .call(|c| {
                c.execute("UPDATE files SET etag='second' WHERE id='model-test'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            file(&state, "model-test".into()).await.unwrap().etag,
            "second"
        );
        tokio::fs::remove_dir_all(state.store.root()).await.unwrap();
    }
}
