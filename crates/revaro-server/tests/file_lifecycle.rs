//! End-to-end lifecycle test through the real router.
//!
//! Every other test in this workspace drives one module. This one drives the
//! product the way a client does — authenticate, create, upload, browse, copy,
//! share, trash, restore, purge — so that a change in one module which breaks
//! the *contract between* modules fails here even when each module's own tests
//! still pass.
//!
//! It deliberately asserts observable behaviour only (status codes, JSON
//! fields, byte counts), never internals, so it stays valid as the
//! implementation is refactored.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use revaro_server::config::Config;
use revaro_server::db::Database;
use revaro_server::state::AppState;
use revaro_server::storage::LocalStore;
use revaro_server::{auth, router};
use tower::ServiceExt as _;

const ROOT_ID: &str = "00000000-0000-0000-0000-000000000000";
const PASSWORD: &str = "correct-horse-battery";

/// A running-in-process server with a scratch object store.
struct Harness {
    state: Arc<AppState>,
    cookie: String,
    _root: ScratchDir,
}

/// A unique temporary directory removed on drop.
struct ScratchDir(std::path::PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Harness {
    async fn start() -> Self {
        let scratch = ScratchDir(std::env::temp_dir().join(format!(
            "revaro-lifecycle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )));
        let _ = std::fs::remove_dir_all(&scratch.0);

        let config = Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent".to_owned()),
            "APP_DATA_DIR" => Some(scratch.0.display().to_string()),
            _ => None,
        })
        .expect("configuration is valid");

        let database = Database::open_in_memory().expect("in-memory database");
        let store = LocalStore::open(scratch.0.join("objects"))
            .await
            .expect("object store");
        let auth = auth::AuthService::new(database.clone());
        // Bootstrap the administrator exactly as startup does; without this
        // there is no account to sign in with.
        auth.initialize("admin", PASSWORD)
            .await
            .expect("administrator bootstrap");
        let state = AppState::new(Arc::new(config), database, store, auth);

        let mut harness = Self {
            state,
            cookie: String::new(),
            _root: scratch,
        };
        harness.cookie = harness.login().await;
        harness
    }

    fn router(&self) -> Router {
        router::build(self.state.clone())
    }

    /// Sign in and return the session cookie pair.
    async fn login(&self) -> String {
        let response = self
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("origin", "http://localhost:8080")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username": "admin", "password": PASSWORD}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "login must succeed");
        response
            .headers()
            .get("set-cookie")
            .expect("login sets a session cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    /// An authenticated request. Writes also carry the same-origin header the
    /// origin guard requires.
    async fn request(
        &self,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
        content_type: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", &self.cookie)
            .header("origin", "http://localhost:8080");
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        let response = self
            .router()
            .oneshot(builder.body(Body::from(body.unwrap_or_default())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    async fn json(
        &self,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        self.request(
            method,
            uri,
            Some(body.to_string().into_bytes()),
            Some("application/json"),
        )
        .await
    }
}

#[tokio::test]
async fn a_file_can_be_created_uploaded_browsed_copied_shared_trashed_and_purged() {
    let harness = Harness::start().await;

    // A fresh installation exposes only the virtual root.
    let (status, root) = harness
        .json(
            "GET",
            &format!("/api/files/{ROOT_ID}/children"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(root["items"].as_array().unwrap().is_empty());

    // Create a directory to hold the upload.
    let (status, folder) = harness
        .json(
            "POST",
            "/api/directories",
            serde_json::json!({"parent_id": ROOT_ID, "name": "Documents"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let folder_id = folder["id"].as_str().unwrap().to_owned();

    // Upload a file into it, in one request, and let the server commit it.
    let payload = b"hello world\n".to_vec();
    let (status, upload) = harness
        .json(
            "POST",
            "/api/uploads",
            serde_json::json!({
                "parent_id": folder_id,
                "name": "greeting.txt",
                "size": payload.len(),
                "mime_type": "text/plain"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(upload["mode"], "single");
    let upload_id = upload["upload_id"].as_str().unwrap().to_owned();
    let file_id = upload["file_id"].as_str().unwrap().to_owned();

    // A pending upload *is* listed, carrying `status: "pending"` — the listing
    // filters only on `deleted_at`, and the UI renders in-progress uploads from
    // that status (there is a StatusBadge component for exactly this). It must
    // not count towards the directory's ready-file aggregate, though.
    let (_, listing) = harness
        .json(
            "GET",
            &format!("/api/files/{folder_id}/children"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    assert_eq!(listing["items"][0]["status"], "pending");
    assert_eq!(
        listing["file_count"], 0,
        "pending bytes are not counted as ready"
    );
    assert_eq!(listing["total_bytes"], 0);

    let (status, _) = harness
        .request(
            "PUT",
            &format!("/api/uploads/{upload_id}/data"),
            Some(payload.clone()),
            Some("application/octet-stream"),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, committed) = harness
        .json(
            "POST",
            &format!("/api/uploads/{upload_id}/complete"),
            serde_json::json!({"parts": []}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(committed["status"], "ready");
    assert_eq!(committed["size"], payload.len() as i64);
    assert_eq!(
        committed["content_hash"].as_str().unwrap().len(),
        64,
        "a committed upload records a sha256"
    );

    // A client may retry cleanup after losing the completion response. It is
    // idempotent and must not remove the object that the commit transaction
    // already made visible.
    let (status, _) = harness
        .json(
            "DELETE",
            &format!("/api/uploads/{upload_id}"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, still_available) = harness
        .json(
            "GET",
            &format!("/api/files/{file_id}/content"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still_available["content"], "hello world\n");

    // Now it appears, with the directory's aggregate updated by the trigger.
    let (_, listing) = harness
        .json(
            "GET",
            &format!("/api/files/{folder_id}/children"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    assert_eq!(listing["file_count"], 1);
    assert_eq!(listing["total_bytes"], payload.len() as i64);

    // The bytes survive the round trip through the document reader.
    let (status, document) = harness
        .json(
            "GET",
            &format!("/api/files/{file_id}/content"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["content"], "hello world\n");

    // Copying produces a suffixed sibling sharing the same bytes.
    let (status, copied) = harness
        .json(
            "POST",
            &format!("/api/files/{file_id}/copy"),
            serde_json::json!({"parent_id": folder_id}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(copied["name"], "greeting - 副本.txt");
    assert_eq!(copied["size"], payload.len() as i64);

    // Sharing yields a public URL with a 43-character token.
    let (status, share) = harness
        .json(
            "POST",
            &format!("/api/files/{file_id}/share"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = share["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(token.len(), 43);

    // Library and storage views see the file.
    let (_, counts) = harness
        .json("GET", "/api/library/counts", serde_json::Value::Null)
        .await;
    assert_eq!(counts["file"], 2, "the copy counts too");
    assert_eq!(
        counts["book"], 2,
        "both the file and its copy are .txt books"
    );
    let (_, stats) = harness
        .json("GET", "/api/storage/stats", serde_json::Value::Null)
        .await;
    assert_eq!(stats["file_count"], 2);

    // Trashing detaches both files from the directory and hides them.
    let (status, _) = harness
        .json(
            "DELETE",
            &format!("/api/files/{folder_id}"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, root) = harness
        .json(
            "GET",
            &format!("/api/files/{ROOT_ID}/children"),
            serde_json::Value::Null,
        )
        .await;
    assert!(root["items"].as_array().unwrap().is_empty());
    let (_, trash) = harness
        .json("GET", "/api/trash", serde_json::Value::Null)
        .await;
    assert_eq!(
        trash["items"].as_array().unwrap().len(),
        1,
        "only the trash root is listed"
    );
    // Live bytes are gone; the status view still counts the trashed bytes.
    let (_, live) = harness
        .json("GET", "/api/storage/stats", serde_json::Value::Null)
        .await;
    assert_eq!(live["file_count"], 0);
    let (_, status_view) = harness
        .json("GET", "/api/system/status", serde_json::Value::Null)
        .await;
    assert_eq!(status_view["storage"]["file_count"], 2);
    assert_eq!(
        status_view["storage"]["trash_bytes"],
        (payload.len() * 2) as i64
    );

    // Restoring brings it back under its original parent.
    let (status, _) = harness
        .json(
            "POST",
            &format!("/api/trash/{folder_id}/restore"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, root) = harness
        .json(
            "GET",
            &format!("/api/files/{ROOT_ID}/children"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(root["items"].as_array().unwrap().len(), 1);

    // Purging is permanent, and the cleanup trigger queues the blobs.
    let (status, _) = harness
        .json(
            "DELETE",
            &format!("/api/files/{folder_id}"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = harness
        .json(
            "DELETE",
            &format!("/api/trash/{folder_id}"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, trash) = harness
        .json("GET", "/api/trash", serde_json::Value::Null)
        .await;
    assert!(trash["items"].as_array().unwrap().is_empty());

    // The copy shares the original's `object_key`, so the two deletions
    // enqueue the *same* object: `object_cleanup` is keyed by key and bumps a
    // generation counter instead of adding a second row. That counter is what
    // protects a concurrent re-enqueue while the worker is mid-delete.
    let (queued, generation): (i64, i64) = harness
        .state
        .db
        .call(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*), COALESCE(MAX(generation),0) FROM object_cleanup",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(revaro_server::db::DbError::Query)
        })
        .await
        .unwrap();
    assert_eq!(
        queued, 1,
        "a shared blob is queued once, not once per reference"
    );
    assert!(generation >= 1, "the queue generation must advance");
}

#[tokio::test]
async fn unauthenticated_and_cross_origin_requests_are_refused() {
    let harness = Harness::start().await;

    // No session: every read is a 401, not a 403 or a redirect.
    let response = harness
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/storage/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A write from another origin is refused by the origin guard.
    let response = harness
        .router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/directories")
                .header("cookie", &harness.cookie)
                .header("origin", "https://evil.example.com")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"parent_id": ROOT_ID, "name": "x"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
