pub async fn presign_put(
    State(state): State<AppState>,
    Query(q): Query<PresignPutQuery>,
) -> Result<Json<UrlResponse>, ApiError> {
    let mut request = s3(&state)
        .presign
        .put_object()
        .bucket(&s3(&state).bucket)
        .key(q.key);
    if let Some(mime) = q.mime {
        request = request.content_type(mime);
    }
    let out = request
        .presigned(expiry(q.expires_seconds)?)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(UrlResponse {
        url: out.uri().to_string(),
    }))
}

pub async fn presign_get(
    State(state): State<AppState>,
    Query(q): Query<PresignGetQuery>,
) -> Result<Json<UrlResponse>, ApiError> {
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if q.inline.unwrap_or(false) {
            "inline"
        } else {
            "attachment"
        },
        utf8_percent_encode(&q.filename, NON_ALPHANUMERIC)
    );
    let mut request = s3(&state)
        .presign
        .get_object()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .response_content_disposition(disposition);
    if let Some(mime) = q.mime {
        request = request.response_content_type(mime);
    }
    let out = request
        .presigned(expiry(q.expires_seconds)?)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(UrlResponse {
        url: out.uri().to_string(),
    }))
}

pub async fn multipart_create(
    State(state): State<AppState>,
    Json(q): Json<MultipartCreate>,
) -> Result<Json<UploadResponse>, ApiError> {
    let mut request = s3(&state)
        .client
        .create_multipart_upload()
        .bucket(&s3(&state).bucket)
        .key(q.key);
    if let Some(mime) = q.mime {
        request = request.content_type(mime);
    }
    let out = request.send().await.map_err(ApiError::upstream)?;
    Ok(Json(UploadResponse {
        upload_id: out.upload_id().unwrap_or_default().to_string(),
    }))
}

pub async fn multipart_part(
    State(state): State<AppState>,
    Query(q): Query<PartQuery>,
) -> Result<Json<UrlResponse>, ApiError> {
    if !(1..=10000).contains(&q.part_number) {
        return Err(ApiError::bad_request(
            "multipart part number is out of range",
        ));
    }
    let out = s3(&state)
        .presign
        .upload_part()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .upload_id(q.upload_id)
        .part_number(q.part_number)
        .presigned(expiry(q.expires_seconds)?)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(UrlResponse {
        url: out.uri().to_string(),
    }))
}

pub async fn multipart_upload(
    State(state): State<AppState>,
    Query(q): Query<UploadPartQuery>,
    request: Request,
) -> Result<Json<ObjectInfo>, ApiError> {
    if !(1..=10000).contains(&q.part_number) || q.size == 0 {
        return Err(ApiError::bad_request("invalid multipart part"));
    }
    let _permit = s3(&state)
        .slots
        .acquire()
        .await
        .map_err(|_| ApiError::cancelled("S3 transfer shutdown"))?;
    let body = request_body_stream(request);
    let out = s3(&state)
        .client
        .upload_part()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .upload_id(q.upload_id)
        .part_number(q.part_number)
        .content_length(q.size as i64)
        .body(body)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(ObjectInfo {
        size: q.size as i64,
        etag: clean_etag(out.e_tag()),
    }))
}

pub async fn multipart_abort(
    State(state): State<AppState>,
    Json(q): Json<MultipartRef>,
) -> Result<StatusCode, ApiError> {
    if !q.upload_id.is_empty() {
        s3(&state)
            .client
            .abort_multipart_upload()
            .bucket(&s3(&state).bucket)
            .key(q.key)
            .upload_id(q.upload_id)
            .send()
            .await
            .map_err(ApiError::upstream)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn multipart_complete(
    State(state): State<AppState>,
    Json(q): Json<CompleteRequest>,
) -> Result<Json<ObjectInfo>, ApiError> {
    if q.parts.is_empty() || q.parts.len() > 10000 {
        return Err(ApiError::bad_request("invalid multipart part count"));
    }
    let parts = q
        .parts
        .into_iter()
        .map(|p| {
            CompletedPart::builder()
                .part_number(p.part_number)
                .e_tag(p.etag)
                .build()
        })
        .collect();
    let upload = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    s3(&state)
        .client
        .complete_multipart_upload()
        .bucket(&s3(&state).bucket)
        .key(&q.key)
        .upload_id(q.upload_id)
        .multipart_upload(upload)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    head(State(state), Query(KeyQuery { key: q.key })).await
}

struct ChannelBody {
    rx: mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>,
}

fn request_body_stream(request: Request) -> ByteStream {
    let (tx, rx) = mpsc::channel(2);
    tokio::spawn(async move {
        let mut body = request.into_body();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data()
                        && tx.send(Ok(data)).await.is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(std::io::Error::other(error))).await;
                    break;
                }
            }
        }
    });
    ByteStream::from_body_1_x(ChannelBody { rx })
}

impl HttpBody for ChannelBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::default()
    }
}

