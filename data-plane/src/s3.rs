use std::{
    env,
    io::{self, Read, Seek, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client,
    config::{Builder as S3ConfigBuilder, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier},
};
use axum::{
    Json,
    body::Body,
    extract::{Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::{AppState, error::ApiError};

include!("s3/state.rs");
include!("s3/object.rs");
include!("s3/multipart.rs");
