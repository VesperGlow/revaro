fn s3(state: &AppState) -> &S3State {
    &state.s3
}

fn expiry(seconds: u64) -> Result<PresigningConfig, ApiError> {
    if seconds == 0 || seconds > 7 * 24 * 3600 {
        return Err(ApiError::bad_request("invalid presign expiry"));
    }
    PresigningConfig::expires_in(Duration::from_secs(seconds))
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

fn clean_etag(value: Option<&str>) -> String {
    value.unwrap_or_default().trim_matches('"').to_string()
}

pub async fn ping(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    s3(&state).ping().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn head(
    State(state): State<AppState>,
    Query(q): Query<KeyQuery>,
) -> Result<Json<ObjectInfo>, ApiError> {
    let out = s3(&state)
        .client
        .head_object()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(ObjectInfo {
        size: out.content_length().unwrap_or_default(),
        etag: clean_etag(out.e_tag()),
    }))
}

pub async fn get_range(
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<Response, ApiError> {
    if q.end.is_some_and(|end| end < q.start.unwrap_or(0)) {
        return Err(ApiError::bad_request("invalid byte range"));
    }
    let mut request = s3(&state)
        .client
        .get_object()
        .bucket(&s3(&state).bucket)
        .key(q.key);
    if let Some(start) = q.start {
        request = request.range(match q.end {
            Some(end) => format!("bytes={start}-{end}"),
            None => format!("bytes={start}-"),
        });
    }
    let out = request.send().await.map_err(ApiError::upstream)?;
    let length = out.content_length();
    let content_range = out.content_range().map(str::to_string);
    let etag = out.e_tag().map(str::to_string);
    let mut response =
        Body::from_stream(ReaderStream::new(out.body.into_async_read())).into_response();
    *response.status_mut() = if q.start.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    if let Some(length) = length {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).unwrap(),
        );
    }
    if let Some(range) = content_range
        .as_deref()
        .and_then(|v| HeaderValue::from_str(v).ok())
    {
        response.headers_mut().insert(header::CONTENT_RANGE, range);
    }
    if let Some(etag) = etag.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    Ok(response)
}

pub async fn put_stream(
    State(state): State<AppState>,
    Query(q): Query<PutQuery>,
    request: Request,
) -> Result<Json<ObjectInfo>, ApiError> {
    let size = q
        .size
        .ok_or_else(|| ApiError::bad_request("size is required"))?;
    let body = request_body_stream(request);
    let mut put = s3(&state)
        .client
        .put_object()
        .bucket(&s3(&state).bucket)
        .key(&q.key)
        .content_length(size as i64)
        .body(body);
    if let Some(mime) = q.mime {
        put = put.content_type(mime);
    }
    if q.immutable.unwrap_or(false) {
        put = put.if_none_match("*");
    }
    let out = put.send().await.map_err(ApiError::upstream)?;
    Ok(Json(ObjectInfo {
        size: size as i64,
        etag: clean_etag(out.e_tag()),
    }))
}

pub async fn put_blob(
    State(state): State<AppState>,
    Query(q): Query<PutQuery>,
    request: Request,
) -> Result<Json<ObjectInfo>, ApiError> {
    if q.size.is_some_and(|size| size < 8 << 20) {
        return put_stream(State(state), Query(q), request).await;
    }
    let reader = tokio_util::io::StreamReader::new(
        request
            .into_body()
            .into_data_stream()
            .map(|result| result.map_err(io::Error::other)),
    );
    Ok(Json(
        state
            .s3
            .store_reader(&q.key, q.mime.as_deref(), reader, q.size)
            .await?,
    ))
}

async fn upload_buffer(
    state: &S3State,
    key: &str,
    upload_id: &str,
    parts: &mut Vec<CompletedPart>,
    data: bytes::Bytes,
) -> Result<u64, ApiError> {
    if parts.len() >= 10_000 {
        return Err(ApiError::payload_too_large(
            "S3 multipart part limit exceeded",
        ));
    }
    let number = parts.len() as i32 + 1;
    let size = data.len() as i64;
    let out = state
        .client
        .upload_part()
        .bucket(&state.bucket)
        .key(key)
        .upload_id(upload_id)
        .part_number(number)
        .content_length(size)
        .body(ByteStream::from(data))
        .send()
        .await
        .map_err(ApiError::upstream)?;
    parts.push(
        CompletedPart::builder()
            .part_number(number)
            .e_tag(clean_etag(out.e_tag()))
            .build(),
    );
    Ok(size as u64)
}

pub async fn delete_one(
    State(state): State<AppState>,
    Query(q): Query<KeyQuery>,
) -> Result<StatusCode, ApiError> {
    s3(&state)
        .client
        .delete_object()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_many(
    State(state): State<AppState>,
    Json(q): Json<DeleteRequest>,
) -> Result<StatusCode, ApiError> {
    if q.keys.len() > MAX_DELETE_KEYS {
        return Err(ApiError::payload_too_large(
            "delete batch exceeds 1000 keys",
        ));
    }
    if q.keys.is_empty() {
        return Ok(StatusCode::NO_CONTENT);
    }
    let objects = q
        .keys
        .into_iter()
        .map(|key| ObjectIdentifier::builder().key(key).build())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let delete = Delete::builder()
        .set_objects(Some(objects))
        .quiet(true)
        .build()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let out = s3(&state)
        .client
        .delete_objects()
        .bucket(&s3(&state).bucket)
        .delete(delete)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    if let Some(error) = out.errors().first() {
        return Err(ApiError::upstream(format!(
            "{}: {}",
            error.key().unwrap_or_default(),
            error.message().unwrap_or_default()
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let out = s3(&state)
        .client
        .list_objects_v2()
        .bucket(&s3(&state).bucket)
        .prefix(q.prefix)
        .set_continuation_token(q.continuation)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let objects = out
        .contents()
        .iter()
        .map(|o| ObjectRef {
            key: o.key().unwrap_or_default().to_string(),
            size: o.size().unwrap_or_default(),
            last_modified_unix_ms: o
                .last_modified()
                .map(|d| d.to_millis().unwrap_or_default())
                .unwrap_or_default(),
        })
        .collect();
    Ok(Json(ListResponse {
        objects,
        continuation: out.next_continuation_token().map(str::to_string),
    }))
}


