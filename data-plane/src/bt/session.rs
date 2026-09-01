#[derive(Deserialize)]
pub struct AddRequest {
    source_type: String,
    source: String,
    selected: Option<Vec<usize>>,
    paused: Option<bool>,
}

pub async fn add(
    State(state): State<AppState>,
    Json(q): Json<AddRequest>,
) -> Result<Json<Value>, ApiError> {
    let add = match q.source_type.as_str() {
        "magnet" => AddTorrent::from_url(q.source),
        "torrent" => AddTorrent::from_bytes(
            base64::engine::general_purpose::STANDARD
                .decode(q.source)
                .map_err(|e| ApiError::bad_request(e.to_string()))?,
        ),
        _ => return Err(ApiError::bad_request("invalid torrent source type")),
    };
    let opts = AddTorrentOptions {
        paused: q.paused.unwrap_or(true),
        only_files: q.selected,
        overwrite: true,
        ..Default::default()
    };
    let response = state
        .bt
        .api
        .api_add_torrent(add, Some(opts))
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(Json(
        serde_json::to_value(response).map_err(ApiError::internal)?,
    ))
}

fn torrent_id(raw: &str) -> Result<TorrentIdOrHash, ApiError> {
    TorrentIdOrHash::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))
}

pub async fn details(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = state
        .bt
        .api
        .api_torrent_details(torrent_id(&id)?)
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(Json(
        serde_json::to_value(result).map_err(ApiError::internal)?,
    ))
}

pub async fn stats(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = state
        .bt
        .api
        .api_stats_v1(torrent_id(&id)?)
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let value = serde_json::to_value(result).map_err(ApiError::internal)?;
    let progress = value
        .get("progress_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let total = value
        .get("total_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let speed = value
        .pointer("/live/download_speed/mbps")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let peers = value
        .pointer("/live/snapshot/peer_stats/live")
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .pointer("/live/snapshot/peer_stats/connected")
                .and_then(Value::as_u64)
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "progress_bytes": progress,
        "total_bytes": total,
        "download_speed": (speed * 1024.0 * 1024.0) as u64,
        "peers": peers,
        "finished": value.get("finished").and_then(Value::as_bool).unwrap_or(progress >= total && total > 0),
    })))
}

#[derive(Deserialize)]
pub struct Selection {
    files: Vec<usize>,
}

pub async fn select(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(q): Json<Selection>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_update_only_files(
            torrent_id(&id)?,
            &q.files.into_iter().collect::<HashSet<_>>(),
        )
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_start(torrent_id(&id)?)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn pause(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_pause(torrent_id(&id)?)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}
