//! System health snapshots and the status SSE stream.
//!
//! The status panel consumes a small, authenticated stream rather than a
//! stream of individual database or storage events. A single process-wide
//! snapshot has two useful properties: the normal JSON endpoint and every SSE
//! subscriber observe the same answer, and a burst of browser connections does
//! not multiply the SQLite and filesystem probes.
//!
//! The snapshot is refreshed immediately when the server starts and every
//! fifteen seconds afterwards. Subscribers use a `watch` channel, which keeps
//! only the newest value (the equivalent of the Go channel with capacity one),
//! so a slow browser cannot retain an unbounded queue or delay the next probe.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::stream;
use http::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE, HeaderName, HeaderValue};
use revaro_core::ApiError;
use revaro_core::api::system::Status;
use tokio::sync::{Mutex, watch};
use tokio::time::{self, Instant, Interval};
use tokio_util::sync::CancellationToken;

use crate::auth::extract::AuthUser;
use crate::cache::CacheManager;
use crate::db::Database;
use crate::state::AppState;
use crate::storage::LocalStore;

/// How often the process-wide health snapshot is recomputed.
pub const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
/// How often an idle SSE connection receives a comment frame.
pub const STATUS_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// Runtime state shared by the JSON status endpoint and all status streams.
#[derive(Debug, Clone)]
pub struct StatusRuntime {
    snapshot: watch::Sender<Option<Status>>,
    refresh_lock: Arc<Mutex<()>>,
    started: Arc<AtomicBool>,
    shutdown: CancellationToken,
}

impl StatusRuntime {
    /// Create a runtime with no snapshot. The first request can populate it
    /// synchronously when the runtime is used without the binary's lifecycle.
    #[must_use]
    pub fn new() -> Self {
        let (snapshot, _) = watch::channel(None);
        Self {
            snapshot,
            refresh_lock: Arc::new(Mutex::new(())),
            started: Arc::new(AtomicBool::new(false)),
            shutdown: CancellationToken::new(),
        }
    }

    /// Start the process-owned initial refresh and fifteen-second ticker.
    pub fn start(&self, database: Database, store: LocalStore, cache: CacheManager) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let runtime = self.clone();
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.refresh_now(&database, &store, &cache).await {
                tracing::warn!(%error, "initial system status refresh failed");
            }
            let mut ticker = time::interval_at(
                Instant::now() + STATUS_REFRESH_INTERVAL,
                STATUS_REFRESH_INTERVAL,
            );
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(error) = runtime.refresh_now(&database, &store, &cache).await {
                            tracing::warn!(%error, "system status refresh failed");
                        }
                    }
                }
            }
        });
    }

    /// Stop the refresh ticker during graceful shutdown.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Return the current snapshot, populating it when a lifecycle task has
    /// not been started yet (which is useful for in-process route tests).
    async fn current_or_refresh(
        &self,
        database: &Database,
        store: &LocalStore,
        cache: &CacheManager,
    ) -> Result<Status, ApiError> {
        if self.started.load(Ordering::Acquire) {
            if let Some(status) = self.current() {
                return Ok(status);
            }
            return self.ensure_snapshot(database, store, cache).await;
        }

        // An AppState constructed directly by an embedded caller has no
        // background owner. Recompute on each request instead of returning a
        // stale test or library snapshot after the database changes.
        self.refresh_now(database, store, cache).await
    }

    async fn ensure_snapshot(
        &self,
        database: &Database,
        store: &LocalStore,
        cache: &CacheManager,
    ) -> Result<Status, ApiError> {
        if let Some(status) = self.current() {
            return Ok(status);
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some(status) = self.current() {
            return Ok(status);
        }
        self.collect_and_publish(database, store, cache).await
    }

    /// Refresh now and publish the new value to every subscriber.
    pub async fn refresh_now(
        &self,
        database: &Database,
        store: &LocalStore,
        cache: &CacheManager,
    ) -> Result<Status, ApiError> {
        let _guard = self.refresh_lock.lock().await;
        self.collect_and_publish(database, store, cache).await
    }

    fn current(&self) -> Option<Status> {
        self.snapshot.borrow().clone()
    }

    async fn collect_and_publish(
        &self,
        database: &Database,
        store: &LocalStore,
        cache: &CacheManager,
    ) -> Result<Status, ApiError> {
        let mut status = collect_system_status(database, cache).await?;
        // Probing the store is kept outside the SQLite worker. It exercises
        // the same write/delete check used during startup and makes a full
        // disk or permission failure visible as a degraded storage component.
        if store.ping().await.is_err() {
            status.storage.status = "degraded".to_owned();
            status.status = "degraded".to_owned();
        }
        self.snapshot.send_replace(Some(status.clone()));
        Ok(status)
    }

    fn subscribe(&self) -> watch::Receiver<Option<Status>> {
        self.snapshot.subscribe()
    }
}

impl Default for StatusRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Route table for the authenticated system status surface.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/system/status", get(system_status))
        .route("/system/status/stream", get(system_status_stream))
}

/// `GET /api/system/status`.
async fn system_status(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<Status>, ApiError> {
    Ok(Json(
        state
            .status
            .current_or_refresh(&state.db, &state.store, &state.cache)
            .await?,
    ))
}

/// `GET /api/system/status/stream`.
async fn system_status_stream(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Response, ApiError> {
    // Subscribe before ensuring the first snapshot. If the first refresh races
    // with this handler, the watch receiver still observes it; marking the
    // current value as seen below avoids emitting that same initial value twice.
    let mut receiver = state.status.subscribe();
    let initial = state
        .status
        .current_or_refresh(&state.db, &state.store, &state.cache)
        .await?;
    let _ = receiver.borrow_and_update();

    let stream = stream::unfold(
        StatusStream {
            initial: Some(initial),
            receiver,
            keepalive: time::interval_at(
                Instant::now() + STATUS_KEEPALIVE_INTERVAL,
                STATUS_KEEPALIVE_INTERVAL,
            ),
        },
        |mut state| async move {
            if let Some(status) = state.initial.take() {
                return Some((Ok::<Bytes, Infallible>(status_frame(&status)), state));
            }
            loop {
                tokio::select! {
                    result = state.receiver.changed() => match result {
                        Ok(()) => {
                            let status = state.receiver.borrow_and_update().clone();
                            if let Some(status) = status {
                                return Some((Ok::<Bytes, Infallible>(status_frame(&status)), state));
                            }
                        }
                        Err(_) => return None,
                    },
                    _ = state.keepalive.tick() => {
                        return Some((Ok::<Bytes, Infallible>(Bytes::from_static(b": keepalive\n\n")), state));
                    }
                }
            }
        },
    );

    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(response)
}

struct StatusStream {
    initial: Option<Status>,
    receiver: watch::Receiver<Option<Status>>,
    keepalive: Interval,
}

fn status_frame(status: &Status) -> Bytes {
    let mut frame = b"event: status\ndata: ".to_vec();
    frame.extend(
        serde_json::to_vec(status).expect("the status DTO contains only serializable fields"),
    );
    frame.extend_from_slice(b"\n\n");
    frame.into()
}

async fn collect_system_status(
    database: &Database,
    cache: &CacheManager,
) -> Result<Status, ApiError> {
    let cache_status = cache.system_status();
    let cache_healthy = cache_status.status == "ok";
    database
        .call_api(move |connection| {
            let mut status = Status {
                status: if cache_healthy {
                    "ok".to_owned()
                } else {
                    "degraded".to_owned()
                },
                database: revaro_core::api::system::Component {
                    status: "ok".to_owned(),
                    bytes: 0,
                },
                storage: revaro_core::api::system::Storage {
                    status: "ok".to_owned(),
                    bytes: 0,
                    trash_bytes: 0,
                    file_count: 0,
                },
                cache: cache_status,
            };

            let pages = connection.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0));
            let page_size =
                connection.query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0));
            match (pages, page_size) {
                (Ok(pages), Ok(page_size)) => status.database.bytes = pages * page_size,
                _ => {
                    status.database.status = "degraded".to_owned();
                    status.status = "degraded".to_owned();
                }
            }

            let storage = connection.query_row(
                "SELECT COALESCE(SUM(size),0), \
COALESCE(SUM(CASE WHEN deleted_at IS NOT NULL THEN size ELSE 0 END),0), COUNT(*) \
FROM files WHERE kind = 'file' AND status = 'ready'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            );
            match storage {
                Ok((bytes, trash_bytes, file_count)) => {
                    status.storage.bytes = bytes;
                    status.storage.trash_bytes = trash_bytes;
                    status.storage.file_count = file_count;
                }
                Err(error) => {
                    tracing::warn!(%error, "could not measure storage");
                    status.storage.status = "degraded".to_owned();
                    status.status = "degraded".to_owned();
                }
            }
            Ok(status)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use revaro_core::ids::ROOT_ID;
    use tower::ServiceExt as _;

    async fn state() -> Arc<AppState> {
        let root = std::env::temp_dir().join(format!(
            "revaro-status-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent".to_owned()),
            "APP_CACHES_DIR" => Some(root.join("caches").display().to_string()),
            _ => None,
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let store = LocalStore::open(&root).await.unwrap();
        let database = Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(database.clone());
        let state = AppState::new(Arc::new(config), database, store, auth);
        seed_session(&state).await;
        state
    }

    async fn seed_session(state: &Arc<AppState>) {
        let token = "status-test-session";
        let hash = crate::auth::token_hash(token);
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT INTO settings(key,value,updated_at) VALUES('admin_username','admin','2024-01-01T00:00:00Z')",
                        [],
                    )
                    .map_err(crate::db::DbError::Query)?;
                connection
                    .execute(
                        "INSERT INTO sessions(id,token_hash,created_at,expires_at) VALUES('status-session',?1,'2024-01-01T00:00:00Z','2999-01-01T00:00:00Z')",
                        [hash],
                    )
                    .map_err(crate::db::DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    fn authenticated_request(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(
                "cookie",
                format!("{}=status-test-session", crate::auth::SESSION_COOKIE),
            )
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn status_json_and_stream_require_authentication() {
        let state = state().await;
        for uri in ["/api/system/status", "/api/system/status/stream"] {
            let response = crate::router::build(state.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn stream_sends_status_updates_with_exact_sse_headers() {
        let state = state().await;
        let response = crate::router::build(state.clone())
            .oneshot(authenticated_request("/api/system/status/stream"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-cache, no-transform");
        assert_eq!(response.headers()[CONNECTION], "keep-alive");
        assert_eq!(response.headers()["x-accel-buffering"], "no");

        let mut body = response.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        let first = String::from_utf8(first.to_vec()).unwrap();
        assert!(first.starts_with("event: status\ndata: "));
        assert!(first.ends_with("\n\n"));
        let json = first
            .strip_prefix("event: status\ndata: ")
            .unwrap()
            .strip_suffix("\n\n")
            .unwrap();
        let status: Status = serde_json::from_str(json).unwrap();
        assert_eq!(status.cache.status, "ok");
        let classes = status.cache.classes.as_ref().unwrap();
        assert!(classes.contains_key(crate::cache::READER_FLOW_MANIFEST));

        state
            .db
            .call(|connection| {
                connection
                    .execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) VALUES(?1,?2,'status.txt','file','blobs/status',7,'text/plain','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                        ["status-file", ROOT_ID],
                    )
                    .map_err(crate::db::DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
        state
            .status
            .refresh_now(&state.db, &state.store, &state.cache)
            .await
            .unwrap();
        let next = body.frame().await.unwrap().unwrap().into_data().unwrap();
        let next = String::from_utf8(next.to_vec()).unwrap();
        assert!(next.contains("\"file_count\":1"));
        drop(body);
    }

    #[tokio::test]
    async fn status_snapshot_counts_trashed_ready_files() {
        let state = state().await;
        state
            .db
            .call(|connection| {
                connection
                    .execute_batch(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) VALUES('live',NULL,'live.txt','file','blobs/live',100,'text/plain','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');\
                         INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at,deleted_at) VALUES('trash',NULL,'trash.txt','file','blobs/trash',60,'text/plain','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');",
                    )
                    .map_err(crate::db::DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
        let response = crate::router::build(state)
            .oneshot(authenticated_request("/api/system/status"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let status: Status = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.storage.bytes, 160);
        assert_eq!(status.storage.trash_bytes, 60);
        assert_eq!(status.storage.file_count, 2);
    }
}
