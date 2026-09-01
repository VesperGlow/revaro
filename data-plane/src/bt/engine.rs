fn is_external_subtitle(name: &str) -> bool {
    matches!(
        FsPath::new(name)
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| v.to_ascii_lowercase())
            .as_deref(),
        Some("ass" | "ssa" | "srt" | "vtt")
    )
}

fn subtitle_matches_video(video: &str, subtitle: &str) -> bool {
    let video_stem = FsPath::new(video)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    let subtitle_stem = FsPath::new(subtitle)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    if subtitle_stem.eq_ignore_ascii_case(video_stem) {
        return true;
    }
    if subtitle_stem.len() <= video_stem.len()
        || !subtitle_stem[..video_stem.len()].eq_ignore_ascii_case(video_stem)
    {
        return false;
    }
    let suffix = &subtitle_stem[video_stem.len()..];
    let Some(delimiter) = suffix.chars().next() else {
        return false;
    };
    if !matches!(delimiter, '.' | ' ' | '_' | '-' | '[' | '(') {
        return false;
    }
    !suffix[delimiter.len_utf8()..]
        .trim_start()
        .starts_with(|value: char| value.is_ascii_digit())
}

async fn canonical_torrent_file(root: &FsPath, file: &Value) -> Result<PathBuf, ApiError> {
    let components = file
        .get("components")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::internal("torrent file path is missing"))?;
    let mut path = root.to_owned();
    for component in components {
        let value = component
            .as_str()
            .ok_or_else(|| ApiError::internal("torrent file path is invalid"))?;
        let parsed = FsPath::new(value);
        if value.is_empty()
            || value.contains(['/', '\\', '\0'])
            || parsed
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(ApiError::bad_request("unsafe torrent file path"));
        }
        path.push(value);
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(ApiError::internal)?;
    if !canonical.starts_with(root) {
        return Err(ApiError::bad_request("torrent file escapes download root"));
    }
    Ok(canonical)
}

#[derive(Clone)]
pub struct BtState {
    api: Api,
}

impl BtState {
    pub async fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let work_root = PathBuf::from(env::var("APP_WORK_DIR")?);
        let web_root = work_root.join("revaro-bt-web");
        if tokio::fs::try_exists(&web_root).await? {
            tokio::fs::remove_dir_all(&web_root).await?;
        }
        tokio::fs::create_dir_all(&web_root).await?;
        let root = work_root.join("revaro-bt");
        tokio::fs::create_dir_all(&root).await?;
        let blocklist = root.join("private-addresses.blocklist");
        tokio::fs::write(
            &blocklist,
            concat!(
                "reserved:0.0.0.0-0.255.255.255\n",
                "private:10.0.0.0-10.255.255.255\n",
                "shared:100.64.0.0-100.127.255.255\n",
                "loopback:127.0.0.0-127.255.255.255\n",
                "linklocal:169.254.0.0-169.254.255.255\n",
                "private:172.16.0.0-172.31.255.255\n",
                "private:192.168.0.0-192.168.255.255\n",
                "benchmark:198.18.0.0-198.19.255.255\n",
                "documentation:192.0.2.0-192.0.2.255\n",
                "documentation:198.51.100.0-198.51.100.255\n",
                "documentation:203.0.113.0-203.0.113.255\n",
                "multicast:224.0.0.0-255.255.255.255\n",
                "loopback6:::0-::1\n",
                "private6:fc00::-fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n",
                "linklocal6:fe80::-febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n",
                "multicast6:ff00::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n",
            ),
        )
        .await?;
        let persistence = root.join("session");
        tokio::fs::create_dir_all(&persistence).await?;
        let session = Session::new_with_opts(
            root,
            SessionOptions {
                fastresume: true,
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(persistence),
                }),
                concurrent_init_limit: Some(2),
                runtime_worker_threads: Some(2),
                peer_limit: Some(40),
                disable_local_service_discovery: true,
                blocklist_url: Some(format!("file://{}", blocklist.to_string_lossy())),
                ..Default::default()
            },
        )
        .await?;
        Ok(Self {
            api: Api::new(session, None),
        })
    }
    pub async fn stop(&self) {
        self.api.session().stop().await;
    }
}

