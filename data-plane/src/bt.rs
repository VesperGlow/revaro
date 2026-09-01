use std::{
    collections::HashSet,
    env,
    io::SeekFrom,
    path::{Component, Path as FsPath, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use librqbit::{
    AddTorrent, AddTorrentOptions, Api, Session, SessionOptions, SessionPersistenceConfig,
    api::TorrentIdOrHash,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{AppState, error::ApiError};

include!("bt/engine.rs");
include!("bt/session.rs");
include!("bt/import.rs");
include!("bt/stream.rs");
