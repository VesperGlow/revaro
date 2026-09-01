pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_delete(torrent_id(&id)?)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct StreamQuery {
    start: Option<u64>,
    end: Option<u64>,
}

// stream serves one torrent file as it downloads. Seeking the underlying
// librqbit FileStream moves its read position, which in turn makes librqbit
// prioritize the pieces around the requested byte offset (a ~32 MiB lookahead
// window), so a browser seeking into the middle of a still-downloading file
// gets those pieces first.
pub async fn stream(
    State(state): State<AppState>,
    Path((id, file_id)): Path<(String, usize)>,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let mut file = state
        .bt
        .api
        .api_stream(torrent_id(&id)?, file_id)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let len = file.len();
    if len == 0 {
        let mut response = Body::empty().into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        response
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        return Ok(response);
    }
    let start = q.start.unwrap_or(0);
    if start >= len {
        return Err(ApiError::range_not_satisfiable(
            "invalid torrent stream start",
        ));
    }
    let end = q.end.map(|value| value.min(len - 1)).unwrap_or(len - 1);
    if end < start {
        return Err(ApiError::range_not_satisfiable(
            "invalid torrent stream range",
        ));
    }
    let length = end - start + 1;
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let mut response = Body::from_stream(ReaderStream::new(file.take(length))).into_response();
    *response.status_mut() = if q.start.is_some() || q.end.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if q.start.is_some() || q.end.is_some() {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")).unwrap(),
        );
    }
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use anyhow::Context;
    use librqbit::{
        CreateTorrentOptions, ListenerOptions, Session, SessionOptions, SessionPersistenceConfig,
        create_torrent, spawn_utils::BlockingSpawner,
    };
    use tempfile::TempDir;
    use tokio::{
        io::{AsyncReadExt, AsyncSeekExt},
        time::timeout,
    };

    use super::*;

    #[test]
    fn external_subtitle_sidecars_match_only_their_video() {
        assert!(is_external_subtitle("Movie.zh-Hans.ass"));
        assert!(is_external_subtitle("Movie.srt"));
        assert!(is_external_subtitle("Movie.vtt"));
        assert!(!is_external_subtitle("Movie.mka"));
        assert!(subtitle_matches_video("Movie.mkv", "Movie.ass"));
        assert!(subtitle_matches_video("Movie.mkv", "Movie.zh-Hans.srt"));
        assert!(!subtitle_matches_video("Movie.mkv", "Movie 2.zh-Hans.ass"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn progressive_seek_and_fastresume_survive_restart() -> anyhow::Result<()> {
        timeout(Duration::from_secs(30), async {
            let seed = TempDir::new()?;
            let content: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
            std::fs::write(seed.path().join("fixture.bin"), &content)?;
            let torrent = create_torrent(
                seed.path(),
                CreateTorrentOptions {
                    piece_length: Some(64 << 10),
                    ..Default::default()
                },
                &BlockingSpawner::new(1),
            )
            .await?;
            let torrent_bytes = torrent.as_bytes()?;

            let seeder = Session::new_with_opts(
                seed.path().into(),
                SessionOptions {
                    dht: None,
                    persistence: None,
                    listen: Some(ListenerOptions {
                        listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
            seeder
                .add_torrent(
                    AddTorrent::from_bytes(torrent_bytes.clone()),
                    Some(AddTorrentOptions {
                        paused: false,
                        output_folder: Some(seed.path().to_string_lossy().into_owned()),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .context("missing seeder handle")?
                .wait_until_completed()
                .await?;
            let peer = seeder.listen_addr().context("seeder did not listen")?;

            let client = TempDir::new()?;
            let state = client.path().join("session");
            std::fs::create_dir_all(&state)?;
            let options = || SessionOptions {
                dht: None,
                fastresume: true,
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(state.clone()),
                }),
                concurrent_init_limit: Some(1),
                runtime_worker_threads: Some(2),
                peer_limit: Some(4),
                ..Default::default()
            };
            let downloader = Session::new_with_opts(client.path().into(), options()).await?;
            let handle = downloader
                .add_torrent(
                    AddTorrent::from_bytes(torrent_bytes.clone()),
                    Some(AddTorrentOptions {
                        paused: false,
                        initial_peers: Some(vec![peer]),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .context("missing downloader handle")?;
            handle.wait_until_initialized().await?;
            let offset = content.len() as u64 - (96 << 10);
            let mut stream = handle.clone().stream(0).await?;
            stream.seek(SeekFrom::Start(offset)).await?;
            let mut tail = vec![0; 64 << 10];
            stream.read_exact(&mut tail).await?;
            assert_eq!(tail, content[offset as usize..offset as usize + tail.len()]);
            drop(stream);
            drop(handle);
            downloader.stop().await;
            drop(downloader);

            let restored = Session::new_with_opts(client.path().into(), options()).await?;
            let restored_handle = restored
                .add_torrent(
                    AddTorrent::from_bytes(torrent_bytes),
                    Some(AddTorrentOptions {
                        paused: false,
                        initial_peers: Some(vec![peer]),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .context("missing restored handle")?;
            restored_handle.wait_until_initialized().await?;
            let mut restored_stream = restored_handle.stream(0).await?;
            restored_stream.seek(SeekFrom::Start(offset)).await?;
            let mut restored_tail = vec![0; 64 << 10];
            restored_stream.read_exact(&mut restored_tail).await?;
            assert_eq!(restored_tail, tail);
            restored.stop().await;
            seeder.stop().await;
            anyhow::Ok(())
        })
        .await??;
        Ok(())
    }
}

