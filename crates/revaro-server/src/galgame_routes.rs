//! Same-origin game players and per-request authorized, streaming ZIP reads.
use crate::state::AppState;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    response::Response,
    routing::get,
};
use http::{HeaderMap, Method, StatusCode, header};
use revaro_core::{ApiError, model::File};
use rusqlite::OptionalExtension;
use std::sync::Arc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/g/{token}", get(player))
        .route("/g/{token}/", get(player))
        .route("/g/{token}/manifest", get(manifest))
        .route("/g/{token}/index", get(index))
        .route("/g/{token}/archive", get(archive))
        .route(
            "/g/{token}/{*unknown}",
            get(|| async { ApiError::not_found("game endpoint not found") }),
        )
        .route("/galgame/{*asset}", get(asset))
        .layer(axum::middleware::from_fn(isolation))
}

#[cfg(test)]
fn validate_entry(entry: &str) -> Result<(), ApiError> {
    validate_entry_for_engine("kirikiri2", entry)
}

pub(crate) fn validate_entry_for_engine(engine: &str, entry: &str) -> Result<(), ApiError> {
    if entry.len() > 512
        || entry.contains(['\\', ':', '\0', '?', '#'])
        || entry.chars().any(char::is_control)
        || entry
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || (engine == "kirikiri2" && !entry.to_ascii_lowercase().ends_with(".xp3"))
        || (engine == "siglus"
            && !matches!(
                entry.to_ascii_lowercase().as_str(),
                "scene.pck" | "scene.chs"
            ))
        || !matches!(engine, "kirikiri2" | "siglus")
    {
        return Err(ApiError::bad_request(
            "entry must be a valid relative game path inside the game ZIP",
        ));
    }
    Ok(())
}

struct Game {
    file: File,
    engine: String,
    entry: String,
    key_config: Option<String>,
}

async fn lookup(state: &AppState, token: &str) -> Result<Game, ApiError> {
    if !revaro_core::features::valid_share_slug(token) {
        return Err(ApiError::not_found("game share not found"));
    }
    let token = token.to_owned();
    state.db.call_api(move |c| {
        let row: Option<(String, String, String, Option<String>)> = c.query_row(
            "SELECT file_id,engine,entry,key_config FROM shares WHERE token=?1 AND share_type='galgame' AND engine IN ('kirikiri2','siglus') AND (expires_at IS NULL OR julianday(expires_at)>julianday('now'))",
            [token], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()
            .map_err(|_| ApiError::internal("game share query failed"))?;
        let (id, engine, entry, key_config) = row.ok_or_else(|| ApiError::not_found("game share not found"))?;
        let file = crate::file_routes::lookup_file(c, &id).map_err(|_| ApiError::not_found("game share not found"))?;
        if file.kind != revaro_core::model::FileKind::File || file.status != revaro_core::model::FileStatus::Ready {
            return Err(ApiError::not_found("game share not found"));
        }
        Ok(Game { file, engine, entry, key_config })
    }).await
}

async fn player(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let game = lookup(&state, &token).await?;
    static_response(
        state,
        &format!("{}/index.html", game.engine),
        method,
        headers,
    )
    .await
}

async fn manifest(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let game = lookup(&state, &token).await?;
    // File identity survives link rotation; a replacement object gets separate saves.
    let space = revaro_core::keys::sha256_hex(
        format!("{}:{}", game.file.id, game.file.object_key).as_bytes(),
    );
    Ok(Json(serde_json::json!({
        "engine": game.engine, "entry": game.entry, "name": game.file.name, "key_config": game.key_config,
        "archive": format!("/g/{token}/archive"), "save_space": format!("revaro-{space}")
    })))
}

async fn index(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let game = lookup(&state, &token).await?;
    if game.engine != "siglus" {
        return Err(ApiError::not_found(
            "game index is only available for Siglus shares",
        ));
    }
    let entries = crate::game_entries::archive_index(&state, &game.file).await?;
    Ok(Json(serde_json::json!({
        "engine": game.engine,
        "entry": game.entry,
        "name": game.file.name,
        "save_space": format!("revaro-{}", revaro_core::keys::sha256_hex(format!("{}:{}", game.file.id, game.file.object_key).as_bytes())),
        "entries": entries,
        "key_config": game.key_config,
    })))
}

async fn archive(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let game = lookup(&state, &token).await?;
    if method != Method::HEAD && !headers.contains_key(header::RANGE) {
        return Err(ApiError::bad_request("game resources require HTTP Range"));
    }
    let permit = state
        .share_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("game resource transfers busy"))?;
    let mut response = crate::file_routes::serve_file(state, game.file, false, headers).await?;
    // Invalid units and stale If-Range must never initiate a whole-game download.
    if method != Method::HEAD && response.status() == StatusCode::OK {
        return Err(ApiError::new(412, "Range was not honored; reload the game"));
    }
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        "sandbox; default-src 'none'".parse().unwrap(),
    );
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"multipart/byteranges"))
    {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, "application/zip".parse().unwrap());
    }
    let body = std::mem::replace(response.body_mut(), Body::empty());
    *response.body_mut() = crate::transfer::hold_permit(body, permit);
    Ok(response)
}

async fn asset(
    State(state): State<Arc<AppState>>,
    Path(asset): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    static_response(state, &asset, method, headers).await
}

async fn static_response(
    state: Arc<AppState>,
    asset: &str,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // No SPA fallback, arbitrary filesystem paths, or game files in this namespace.
    if !matches!(
        asset,
        "bootstrap.js"
            | "kirikiri2/index.html"
            | "kirikiri2/player.js"
            | "kirikiri2/input.js"
            | "kirikiri2/early.js"
            | "kirikiri2/uniforms.js"
            | "kirikiri2/player.css"
            | "kirikiri2/index.js"
            | "kirikiri2/index.wasm"
            | "kirikiri2/vlfs.js"
            | "kirikiri2/assets.zip"
            | "kirikiri2/jszip.min.js"
            | "kirikiri2/ENGINE-LICENSE.txt"
            | "kirikiri2/SOURCE.md"
            | "siglus/index.html"
            | "siglus/player.js"
            | "siglus/fflate.js"
            | "siglus/FFLATE-LICENSE.txt"
            | "siglus/ENGINE-LICENSE.txt"
            | "siglus/pkg/game_launcher.js"
            | "siglus/pkg/game_launcher_bg.wasm"
            | "siglus/pkg/SOURCE_REVISION"
            | "siglus/pkg/SHA256SUMS"
    ) {
        return Err(ApiError::not_found("player asset not found"));
    }
    let path = state.config.web_dir.join("galgame").join(asset);
    if !tokio::fs::metadata(path).await.is_ok_and(|m| m.is_file()) {
        return Err(ApiError::unavailable(
            "Galgame player unavailable; install the player assets and rebuild web",
        ));
    }
    let uri = format!("/galgame/{asset}")
        .parse()
        .expect("allowlisted asset URI");
    Ok(crate::web::serve(State(state), method, headers, uri).await)
}

async fn isolation(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "require-corp"),
        ("cross-origin-resource-policy", "same-origin"),
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-robots-tag", "noindex, nofollow, noarchive"),
    ] {
        response.headers_mut().insert(
            http::header::HeaderName::from_static(name),
            value.parse().unwrap(),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn setup() -> Arc<AppState> {
        let state = crate::router::tests::test_state().await;
        let hash = crate::auth::token_hash("game-test-session");
        state.db.call(move |c| {
            c.execute("INSERT INTO settings(key,value,updated_at) VALUES('admin_username','admin','2026-01-01T00:00:00Z')", [])?;
            c.execute("INSERT INTO sessions(id,token_hash,created_at,expires_at) VALUES('game-test',?1,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')", [hash])?;
            for id in ["game1", "game2"] {
                c.execute("INSERT INTO files(id,name,kind,object_key,size,mime_type,status,created_at,updated_at) VALUES(?1,?2,'file',?1,10,'application/zip','ready','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')", rusqlite::params![id,format!("{id}.zip")])?;
            }
            Ok(())
        }).await.unwrap();
        state.store.put("game1", b"0123456789").await.unwrap();
        state.store.put("game2", b"abcdefghij").await.unwrap();
        state
    }

    async fn request(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        cookie: Option<&str>,
        range: Option<&str>,
    ) -> Response {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::ORIGIN, "http://localhost:8080");
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        if let Some(range) = range {
            req = req.header(header::RANGE, range);
        }
        if body.is_some() {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        crate::router::build(state.clone())
            .oneshot(
                req.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json(response: Response) -> serde_json::Value {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    async fn create(state: &Arc<AppState>, id: &str, slug: &str) -> serde_json::Value {
        let input = serde_json::json!({"share_type":"galgame","engine":"kirikiri2","entry":"dtcn.xp3","custom_slug":slug});
        let response = request(
            state,
            "POST",
            &format!("/api/files/{id}/share"),
            Some(input),
            Some("revaro_session=game-test-session"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        json(response).await
    }

    #[tokio::test]
    async fn ranges_head_expiry_revocation_and_normal_share_remain_separate() {
        let state = setup().await;
        let status = create(&state, "game1", "play").await;
        assert_eq!(status["url"], "http://localhost:8080/g/play");
        assert_eq!(status["engine"], "kirikiri2");
        let manifest = request(&state, "GET", "/g/play/manifest", None, None, None).await;
        let save_space = json(manifest).await["save_space"].clone();
        let head = request(&state, "HEAD", "/g/play/archive", None, None, None).await;
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(head.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            head.headers()["cross-origin-embedder-policy"],
            "require-corp"
        );
        assert_eq!(head.headers()[header::CACHE_CONTROL], "no-store");
        let etag = head.headers()[header::ETAG].clone();
        assert!(
            head.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
        let range = request(
            &state,
            "GET",
            "/g/play/archive",
            None,
            None,
            Some("bytes=2-5"),
        )
        .await;
        assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(range.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(range.headers()[header::ETAG], etag);
        assert_eq!(
            range.into_body().collect().await.unwrap().to_bytes(),
            "2345"
        );
        let multi = request(
            &state,
            "GET",
            "/g/play/archive",
            None,
            None,
            Some("bytes=0-1,8-9"),
        )
        .await;
        assert_eq!(multi.status(), StatusCode::PARTIAL_CONTENT);
        assert!(
            multi.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("multipart/byteranges; boundary=")
        );
        let multipart = multi.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&multipart).contains("bytes 0-1/10"));
        assert!(String::from_utf8_lossy(&multipart).contains("bytes 8-9/10"));
        let stale = crate::router::build(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/g/play/archive")
                    .header(header::RANGE, "bytes=0-1")
                    .header(header::IF_RANGE, "\"stale\"")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
        let suffix = request(
            &state,
            "GET",
            "/g/play/archive",
            None,
            None,
            Some("bytes=-2"),
        )
        .await;
        assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(suffix.into_body().collect().await.unwrap().to_bytes(), "89");
        assert_eq!(
            request(
                &state,
                "GET",
                "/g/play/archive",
                None,
                None,
                Some("bytes=99-")
            )
            .await
            .status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
        assert_eq!(
            request(&state, "GET", "/g/play/archive", None, None, None)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &state,
                "GET",
                "/g/play/archive",
                None,
                None,
                Some("items=1-2")
            )
            .await
            .status(),
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(
            request(&state, "GET", "/s/play", None, None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        create(&state, "game1", "renewed").await;
        assert_eq!(
            request(&state, "HEAD", "/g/play/archive", None, None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            json(request(&state, "GET", "/g/renewed/manifest", None, None, None).await).await["save_space"],
            save_space
        );
        create(&state, "game2", "other").await;
        assert_ne!(
            json(request(&state, "GET", "/g/other/manifest", None, None, None).await).await["save_space"],
            save_space
        );
        state
            .db
            .call(|c| {
                c.execute(
                    "UPDATE shares SET expires_at='2000-01-01T00:00:00Z' WHERE file_id='game1'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            request(&state, "HEAD", "/g/renewed/archive", None, None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        state
            .db
            .call(|c| {
                c.execute(
                    "UPDATE files SET deleted_at='2026-01-01T00:00:00Z' WHERE id='game2'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            request(&state, "GET", "/g/other/manifest", None, None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(
            !request(&state, "GET", "/healthz", None, None, None)
                .await
                .headers()
                .contains_key("cross-origin-opener-policy")
        );
    }

    #[tokio::test]
    async fn siglus_config_survives_link_rotation_and_clears_explicitly() {
        let state = setup().await;
        let cookie = Some("revaro_session=game-test-session");
        let configuration = "key = [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]";
        let input = serde_json::json!({"share_type":"galgame","engine":"siglus","entry":"scene.chs","custom_slug":"siglus-config","key_config":configuration});
        let created = request(
            &state,
            "POST",
            "/api/files/game1/share",
            Some(input),
            cookie,
            None,
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(json(created).await["key_config"], configuration);
        assert_eq!(
            json(request(&state, "GET", "/g/siglus-config/manifest", None, None, None).await).await
                ["key_config"],
            configuration
        );
        assert_eq!(
            json(request(&state, "GET", "/api/files/game1/share", None, cookie, None).await).await
                ["key_config"],
            configuration
        );
        let shares = json(request(&state, "GET", "/api/shares", None, cookie, None).await).await;
        assert_eq!(shares[0]["key_config"], configuration);

        // Older clients may rotate a link without sending the new optional field.
        let rotate = serde_json::json!({"share_type":"galgame","engine":"siglus","entry":"scene.chs","custom_slug":"siglus-renewed"});
        assert_eq!(
            request(
                &state,
                "POST",
                "/api/files/game1/share",
                Some(rotate.clone()),
                cookie,
                None
            )
            .await
            .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            request(&state, "GET", "/g/siglus-config/manifest", None, None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            json(
                request(
                    &state,
                    "GET",
                    "/g/siglus-renewed/manifest",
                    None,
                    None,
                    None
                )
                .await
            )
            .await["key_config"],
            configuration
        );
        let mut clear = rotate.clone();
        clear["key_config"] = serde_json::json!("");
        assert_eq!(
            request(
                &state,
                "POST",
                "/api/files/game1/share",
                Some(clear),
                cookie,
                None
            )
            .await
            .status(),
            StatusCode::CREATED
        );
        assert!(
            json(
                request(
                    &state,
                    "GET",
                    "/g/siglus-renewed/manifest",
                    None,
                    None,
                    None
                )
                .await
            )
            .await["key_config"]
                .is_null()
        );
        for invalid in ["x".repeat(16385), "bad\0config".to_owned()] {
            let mut input = rotate.clone();
            input["key_config"] = serde_json::json!(invalid);
            assert_eq!(
                request(
                    &state,
                    "POST",
                    "/api/files/game1/share",
                    Some(input),
                    cookie,
                    None
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let wrong_engine = serde_json::json!({"share_type":"galgame","engine":"kirikiri2","entry":"data.xp3","key_config":configuration});
        assert_eq!(
            request(
                &state,
                "POST",
                "/api/files/game1/share",
                Some(wrong_engine),
                cookie,
                None
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn invalid_settings_and_missing_assets_fail_without_spa_fallback() {
        let state = setup().await;
        for entry in [
            "../data.xp3",
            "/data.xp3",
            "a//b.xp3",
            "a\\b.xp3",
            "https://host/a.xp3",
            "script.tjs",
            "",
        ] {
            assert!(validate_entry(entry).is_err(), "{entry}");
        }
        assert!(validate_entry("日本語/data.xp3").is_ok());
        for input in [
            serde_json::json!({"share_type":"galgame","engine":"renpy","entry":"data.xp3"}),
            serde_json::json!({"share_type":"galgame","engine":"kirikiri2"}),
            serde_json::json!({"share_type":"unknown"}),
        ] {
            assert_eq!(
                request(
                    &state,
                    "POST",
                    "/api/files/game1/share",
                    Some(input),
                    Some("revaro_session=game-test-session"),
                    None
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let siglus = request(
            &state,
            "POST",
            "/api/files/game1/share",
            Some(serde_json::json!({"share_type":"galgame","engine":"siglus","entry":"Scene.pck","custom_slug":"siglus"})),
            Some("revaro_session=game-test-session"),
            None,
        ).await;
        assert_eq!(siglus.status(), StatusCode::CREATED);
        let siglus_status = json(siglus).await;
        assert_eq!(siglus_status["engine"], "siglus");
        assert_eq!(
            json(request(&state, "GET", "/g/siglus/manifest", None, None, None).await).await["engine"],
            "siglus"
        );
        assert_eq!(request(&state,"POST","/api/files/game1/share",Some(serde_json::json!({"share_type":"galgame","engine":"kirikiri2","entry":"data.xp3"})),None,None).await.status(),StatusCode::UNAUTHORIZED);
        assert_eq!(
            request(
                &state,
                "GET",
                "/galgame/kirikiri2/missing.js",
                None,
                None,
                None
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                &state,
                "GET",
                "/galgame/kirikiri2/index.wasm",
                None,
                None,
                None
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            request(&state, "GET", "/g/missing/manifest", None, None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
