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
            "APP_OBJECTS_DIR" => Some(scratch.0.join("objects").display().to_string()),
            "APP_CACHES_DIR" => Some(scratch.0.join("work").display().to_string()),
            _ => None,
        })
        .expect("configuration is valid");

        let database = Database::open_in_memory().expect("in-memory database");
        let store = LocalStore::open(config.objects_dir())
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
        let response = self.response(method, uri, body, content_type).await;
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    async fn response(
        &self,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
        content_type: Option<&str>,
    ) -> http::Response<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", &self.cookie)
            .header("origin", "http://localhost:8080");
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        self.router()
            .oneshot(builder.body(Body::from(body.unwrap_or_default())).unwrap())
            .await
            .unwrap()
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

    // Both the original and its copy are visible in the folder.
    let (_, children) = harness
        .json(
            "GET",
            &format!("/api/files/{folder_id}/children"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(children["items"].as_array().unwrap().len(), 2);

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
                .uri("/api/tasks")
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

#[tokio::test]
async fn multipart_upload_commits_a_streamed_sha256_and_rejects_short_completion() {
    let harness = Harness::start().await;
    let size = revaro_core::limits::MULTIPART_UPLOAD_THRESHOLD as usize + 1;
    let payload: Vec<u8> = (0..=u8::MAX).cycle().take(size).collect();
    let expected_hash = revaro_core::keys::sha256_hex(&payload);

    let (status, upload) = harness
        .json(
            "POST",
            "/api/uploads",
            serde_json::json!({
                "parent_id": ROOT_ID,
                "name": "large.bin",
                "size": size,
                "mime_type": "application/octet-stream"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(upload["mode"], "multipart");
    assert_eq!(upload["part_count"], 2);
    let upload_id = upload["upload_id"].as_str().unwrap().to_owned();
    let part_size = upload["part_size"].as_i64().unwrap() as usize;

    let mut parts = Vec::new();
    for (index, part) in payload.chunks(part_size).enumerate() {
        let part_number = index + 1;
        let response = harness
            .response(
                "PUT",
                &format!("/api/uploads/{upload_id}/data/{part_number}"),
                Some(part.to_vec()),
                Some("application/octet-stream"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let etag = response
            .headers()
            .get("etag")
            .expect("each part has an etag")
            .to_str()
            .unwrap()
            .to_owned();
        let (status, _) = harness
            .json(
                "PUT",
                &format!("/api/uploads/{upload_id}/parts/{part_number}"),
                serde_json::json!({
                    "etag": etag,
                    "size": part.len(),
                    "content_hash": ""
                }),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        parts.push(serde_json::json!({
            "part_number": part_number,
            "etag": etag
        }));
    }

    let (status, _) = harness
        .json(
            "POST",
            &format!("/api/uploads/{upload_id}/complete"),
            serde_json::json!({"parts": [parts[0].clone()]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, committed) = harness
        .json(
            "POST",
            &format!("/api/uploads/{upload_id}/complete"),
            serde_json::json!({"parts": parts}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(committed["status"], "ready");
    assert_eq!(committed["content_hash"], expected_hash);
    assert_eq!(committed["hash_algorithm"], "sha256");

    let (status, upload_state) = harness
        .json(
            "GET",
            &format!("/api/uploads/{upload_id}"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(upload_state["status"], "completed");
}

#[tokio::test]
async fn document_versions_restore_with_conflict_protection_and_survive_cleanup() {
    let h = Harness::start().await;
    let (status, file) = h
        .json(
            "POST",
            "/api/documents",
            serde_json::json!({"parent_id":ROOT_ID,"name":"history.md","content":"original"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = file["id"].as_str().unwrap();
    let (_, updated) = h
        .json(
            "PUT",
            &format!("/api/files/{id}/content"),
            serde_json::json!({"content":"second","etag":file["etag"]}),
        )
        .await;
    let (status, versions) = h
        .json(
            "GET",
            &format!("/api/files/{id}/versions"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(versions.as_array().unwrap().len(), 1);
    let v = versions[0]["id"].as_str().unwrap();
    let (status, body) = h
        .json(
            "GET",
            &format!("/api/files/{id}/versions/{v}/content"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "original");
    let (status, _) = h
        .json(
            "POST",
            &format!("/api/files/{id}/versions/{v}/restore"),
            serde_json::json!({"etag":file["etag"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, restored) = h
        .json(
            "POST",
            &format!("/api/files/{id}/versions/{v}/restore"),
            serde_json::json!({"etag":updated["etag"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(restored["etag"], file["etag"]);
    let (_, body) = h
        .json(
            "GET",
            &format!("/api/files/{id}/content"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(body["content"], "original");
    for i in 0..22 {
        let (status, _) = h
            .json(
                "PUT",
                &format!("/api/files/{id}/content"),
                serde_json::json!({"content":format!("edit {i}")}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (_, versions) = h
        .json(
            "GET",
            &format!("/api/files/{id}/versions"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(versions.as_array().unwrap().len(), 20);
    h.state.maintenance.start();
    for _ in 0..100 {
        let count: i64 = h
            .state
            .db
            .call(|c| Ok(c.query_row("SELECT count(*) FROM object_cleanup", [], |r| r.get(0))?))
            .await
            .unwrap();
        if count == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // Retained bytes remain readable after the actual maintenance worker runs.
    for version in versions.as_array().unwrap() {
        let v = version["id"].as_str().unwrap();
        let (s, _) = h
            .json(
                "GET",
                &format!("/api/files/{id}/versions/{v}/content"),
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(s, StatusCode::OK);
    }
    h.state.maintenance.close().await;
}

#[tokio::test]
async fn filename_search_uses_directory_scope_and_root_searches_the_whole_drive() {
    let h = Harness::start().await;
    let mut folders = Vec::new();
    for (name, parent) in [("Inbox", ROOT_ID), ("Elsewhere", ROOT_ID)] {
        let (status, folder) = h
            .json(
                "POST",
                "/api/directories",
                serde_json::json!({
                    "parent_id": parent, "name": name,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        folders.push(folder["id"].as_str().unwrap().to_owned());
    }
    let (_, nested) = h
        .json(
            "POST",
            "/api/directories",
            serde_json::json!({
                "parent_id": folders[0], "name": "Nested",
            }),
        )
        .await;
    let nested_id = nested["id"].as_str().unwrap();
    let mut ids = Vec::new();
    for (name, parent) in [
        ("shared-a.md", ROOT_ID),
        ("shared-b.md", folders[0].as_str()),
        ("shared-c.md", nested_id),
        ("shared-d.md", folders[1].as_str()),
        ("shared-gone.md", folders[0].as_str()),
    ] {
        let (status, file) = h
            .json(
                "POST",
                "/api/documents",
                serde_json::json!({
                    "parent_id": parent, "name": name, "content": "search fixture",
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        ids.push(file["id"].as_str().unwrap().to_owned());
    }
    let (status, _) = h
        .json(
            "DELETE",
            &format!("/api/files/{}", ids[4]),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for query in [
        format!("parent_id={ROOT_ID}&q=shared"),
        "q=shared".to_owned(),
    ] {
        let (status, listing) = h
            .json(
                "GET",
                &format!("/api/files?{query}&limit=2&sort=name"),
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["total"], 4);
        assert_eq!(listing["items"][0]["name"], "shared-a.md");
        assert_eq!(listing["items"][1]["name"], "shared-b.md");
        let (_, next) = h
            .json(
                "GET",
                &format!("/api/files?{query}&limit=2&sort=name&offset=2"),
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(next["total"], 4);
        assert_eq!(next["items"][0]["name"], "shared-c.md");
        assert_eq!(next["items"][1]["name"], "shared-d.md");
    }
    for (parent, expected) in [(&folders[0], "shared-b.md"), (&folders[1], "shared-d.md")] {
        let (status, listing) = h
            .json(
                "GET",
                &format!("/api/files?parent_id={parent}&q=shared"),
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["total"], 1);
        assert_eq!(listing["items"][0]["name"], expected);
    }
    // Clearing the query returns to the root listing, including whitespace-only input.
    for query in ["".to_owned(), format!("parent_id={ROOT_ID}&q=%20%20")] {
        let (status, listing) = h
            .json(
                "GET",
                &format!("/api/files?{query}"),
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["total"], 3);
        assert_eq!(listing["file_count"], 4);
    }
    for parent in ["missing-directory", ids[0].as_str(), ids[4].as_str()] {
        let (status, _) = h
            .json(
                "GET",
                &format!("/api/files?parent_id={parent}&q=shared"),
                serde_json::Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn filename_search_is_literal_sorted_and_paginated_and_shares_expire() {
    let h = Harness::start().await;
    let mut ids = Vec::new();
    for (name, content) in [
        ("zeta.md", "large text"),
        ("alpha.md", "a"),
        ("100%.md", "b"),
    ] {
        let (s, f) = h
            .json(
                "POST",
                "/api/documents",
                serde_json::json!({"parent_id":ROOT_ID,"name":name,"content":content}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        ids.push(f["id"].as_str().unwrap().to_owned());
    }
    let (s, list) = h
        .json(
            "GET",
            &format!("/api/files?parent_id={ROOT_ID}&sort=size&limit=1&offset=2"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 3);
    assert_eq!(list["items"][0]["name"], "zeta.md");
    let (_, list) = h
        .json("GET", "/api/files?q=%25", serde_json::Value::Null)
        .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["name"], "100%.md");
    let (s, _) = h
        .json("GET", "/api/files?limit=201", serde_json::Value::Null)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let id = &ids[0];
    let (s, share) = h
        .json(
            "POST",
            &format!("/api/files/{id}/share"),
            serde_json::json!({"expires_in_seconds":3600}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(share["expires_at"].is_string());
    let path = share["url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://localhost:8080")
        .unwrap();
    let (s, _) = h.request("GET", path, None, None).await;
    assert_eq!(s, StatusCode::OK);
    let id = id.clone();
    h.state
        .db
        .call(move |c| {
            c.execute(
                "UPDATE shares SET expires_at='2000-01-01T00:00:00Z' WHERE file_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, _) = h.request("GET", path, None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, overview) = h.json("GET", "/api/shares", serde_json::Value::Null).await;
    assert_eq!(overview[0]["active"], false);
    let (s, _) = h
        .json(
            "POST",
            &format!("/api/files/{}/share", ids[0]),
            serde_json::json!({"expires_in_seconds":-1}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .json(
            "DELETE",
            &format!("/api/files/{}/share", ids[0]),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn directory_copy_and_zip_preserve_nested_and_empty_folders() {
    let h = Harness::start().await;
    let (_, folder) = h
        .json(
            "POST",
            "/api/directories",
            serde_json::json!({"parent_id":ROOT_ID,"name":"archive"}),
        )
        .await;
    let id = folder["id"].as_str().unwrap();
    let (_, sub) = h
        .json(
            "POST",
            "/api/directories",
            serde_json::json!({"parent_id":id,"name":"empty"}),
        )
        .await;
    let (_, doc) = h
        .json(
            "POST",
            "/api/documents",
            serde_json::json!({"parent_id":id,"name":"hello.md","content":"hello folder"}),
        )
        .await;
    let (s, copy) = h
        .json(
            "POST",
            &format!("/api/files/{id}/copy"),
            serde_json::json!({"parent_id":ROOT_ID}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, list) = h
        .json(
            "GET",
            &format!("/api/files/{}/children", copy["id"].as_str().unwrap()),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert_eq!(list["total_bytes"], 12);
    let (s, _) = h
        .json(
            "POST",
            &format!("/api/files/{id}/copy"),
            serde_json::json!({"parent_id":sub["id"]}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, ticket) = h
        .json(
            "POST",
            "/api/files/batch-download/prepare",
            serde_json::json!({"ids":[id]}),
        )
        .await;
    let response = h
        .response(
            "GET",
            &format!(
                "/api/files/batch-download/{}",
                ticket["token"].as_str().unwrap()
            ),
            None,
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(zip.len(), 3);
    assert!(zip.by_name("archive/empty/").unwrap().is_dir());
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("archive/hello.md").unwrap(), &mut contents)
        .unwrap();
    assert_eq!(contents, "hello folder");
    let (s, _) = h
        .json(
            "DELETE",
            &format!("/api/files/{}", doc["id"].as_str().unwrap()),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn public_download_slots_follow_real_tcp_bodies_and_disconnects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let h = Harness::start().await;
    let (_, file) = h
        .json(
            "POST",
            "/api/documents",
            serde_json::json!({"parent_id":ROOT_ID,"name":"slow.txt","content":"x"}),
        )
        .await;
    let id = file["id"].as_str().unwrap().to_owned();
    let key: String = h
        .state
        .db
        .call({
            let id = id.clone();
            move |c| {
                Ok(
                    c.query_row("SELECT object_key FROM files WHERE id=?1", [id], |r| {
                        r.get(0)
                    })?,
                )
            }
        })
        .await
        .unwrap();
    let path = h.state.store.path_for(&key).unwrap();
    tokio::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .unwrap()
        .set_len(128 << 20)
        .await
        .unwrap();
    h.state
        .db
        .call({
            let id = id.clone();
            move |c| {
                c.execute(
                    "UPDATE files SET size=?2 WHERE id=?1",
                    rusqlite::params![id, 128_i64 << 20],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let (_, share) = h
        .json(
            "POST",
            &format!("/api/files/{id}/share"),
            serde_json::json!({}),
        )
        .await;
    let path = share["url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://localhost:8080")
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = h.router();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut clients = Vec::new();
    for n in 0..9 {
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let byte = tokio::time::timeout(std::time::Duration::from_secs(5), client.read_u8())
                .await
                .unwrap()
                .unwrap();
            header.push(byte);
            assert!(header.len() < 8192);
        }
        let text = String::from_utf8(header).unwrap();
        assert!(
            text.starts_with(if n < 8 {
                "HTTP/1.1 200"
            } else {
                "HTTP/1.1 429"
            }),
            "{text}"
        );
        clients.push(client);
    }
    assert_eq!(h.state.share_slots.available_permits(), 0);
    drop(clients);
    for _ in 0..100 {
        if h.state.share_slots.available_permits() == 8 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(h.state.share_slots.available_permits(), 8);
    server.abort();
}

#[tokio::test]
async fn concurrent_directory_moves_cannot_create_a_cycle() {
    let h = Harness::start().await;
    let (_, a) = h
        .json(
            "POST",
            "/api/directories",
            serde_json::json!({"parent_id":ROOT_ID,"name":"A"}),
        )
        .await;
    let (_, b) = h
        .json(
            "POST",
            "/api/directories",
            serde_json::json!({"parent_id":ROOT_ID,"name":"B"}),
        )
        .await;
    let a_path = format!("/api/files/{}", a["id"].as_str().unwrap());
    let b_path = format!("/api/files/{}", b["id"].as_str().unwrap());
    let (left, right) = tokio::join!(
        h.json("PATCH", &a_path, serde_json::json!({"parent_id":b["id"]})),
        h.json("PATCH", &b_path, serde_json::json!({"parent_id":a["id"]}))
    );
    assert!(matches!(
        (left.0, right.0),
        (StatusCode::OK, StatusCode::BAD_REQUEST) | (StatusCode::BAD_REQUEST, StatusCode::OK)
    ));
}
