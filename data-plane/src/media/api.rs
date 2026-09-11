pub async fn probe(
    State(state): State<AppState>,
    Json(q): Json<ProbeRequest>,
) -> Result<Json<ProbeResponse>, ApiError> {
    let _permit = state
        .media_light_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.local.open(&q.key)?;
    let mut guard = CancelOnDrop(cancel.clone());
    let result = task::spawn_blocking(move || probe_blocking(reader, cancel))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::bad_request)?;
    guard.disarm();
    Ok(Json(result))
}

pub async fn thumbnail(
    State(state): State<AppState>,
    Json(q): Json<ThumbnailRequest>,
) -> Result<Response, ApiError> {
    let max_dimension = q.max_dimension.unwrap_or(480).clamp(64, 2048);
    let _permit = state
        .media_light_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.local.open(&q.key)?;
    let mut guard = CancelOnDrop(cancel.clone());
    let attached_picture_only = q.attached_picture_only.unwrap_or(false);
    let jpeg = task::spawn_blocking(move || {
        thumbnail_blocking(reader, max_dimension, attached_picture_only, cancel)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(|error| {
        if error == "media has no attached picture" {
            ApiError::not_found("artwork", error)
        } else {
            ApiError::bad_request(error)
        }
    })?;
    guard.disarm();
    let mut response = Body::from(jpeg).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    Ok(response)
}

pub async fn subtitle(
    State(state): State<AppState>,
    Json(q): Json<SubtitleRequest>,
) -> Result<Response, ApiError> {
    let _permit = state
        .media_light_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.local.open(&q.key)?;
    let mut guard = CancelOnDrop(cancel.clone());
    let converted = task::spawn_blocking(move || {
        subtitle_blocking(reader, q.format.as_deref(), q.stream_index, cancel)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::bad_request)?;
    guard.disarm();
    let mut response = Body::from(converted).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/vtt; charset=utf-8"),
    );
    Ok(response)
}

struct CancelOnDrop(CancellationToken);
impl CancelOnDrop {
    fn disarm(&mut self) {
        self.0 = CancellationToken::new();
    }
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
