// S3 access is limited to SQLite snapshots.
use crate::{AppState, error::ApiError};
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client,
    config::{Builder, Region},
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
};
use axum::{
    Json,
    extract::{Query, Request, State},
    http::StatusCode,
};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::env;
use tokio::io::AsyncWriteExt;

const PREFIX: &str = "revaro-backups/database/";

#[derive(Clone)]
pub struct BackupState {
    client: Client,
    bucket: String,
}
impl BackupState {
    pub async fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let bucket = env::var("S3_BUCKET").unwrap_or_default();
        let enabled = env::var("BACKUP_ENABLED")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| matches!(v.as_str(), "true" | "1" | "TRUE" | "True"))
            .unwrap_or(!bucket.is_empty());
        if !enabled {
            return Ok(None);
        }
        let credentials = Credentials::new(
            env::var("S3_ACCESS_KEY")?,
            env::var("S3_SECRET_KEY")?,
            None,
            None,
            "revaro-database-backup",
        );
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(
                env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
            ))
            .credentials_provider(credentials)
            .load()
            .await;
        let mut config = Builder::from(&shared).force_path_style(
            env::var("S3_PATH_STYLE")
                .is_ok_and(|v| matches!(v.as_str(), "true" | "1" | "TRUE" | "True")),
        );
        if let Ok(endpoint) = env::var("S3_ENDPOINT")
            && !endpoint.is_empty()
        {
            config = config.endpoint_url(endpoint);
        }
        Ok(Some(Self {
            client: Client::from_conf(config.build()),
            bucket,
        }))
    }
}
fn backup(state: &AppState) -> Result<&BackupState, ApiError> {
    state
        .backup
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("S3 database backup is disabled"))
}
fn validate(key: &str) -> Result<(), ApiError> {
    let name = key
        .strip_prefix(PREFIX)
        .and_then(|n| n.strip_prefix("revaro-db-"))
        .and_then(|n| n.strip_suffix(".sqlite"));
    if !name.is_some_and(|n| {
        n.len() == 16
            && n.bytes().enumerate().all(|(i, b)| match i {
                8 => b == b'T',
                15 => b == b'Z',
                _ => b.is_ascii_digit(),
            })
    }) {
        return Err(ApiError::bad_request(
            "only database snapshot keys are allowed",
        ));
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct Key {
    key: String,
}
#[derive(Deserialize)]
pub struct Keys {
    keys: Vec<String>,
}
#[derive(Serialize)]
pub struct BackupRef {
    #[serde(rename = "Key")]
    key: String,
}

pub async fn upload(
    State(state): State<AppState>,
    Query(query): Query<Key>,
    request: Request,
) -> Result<StatusCode, ApiError> {
    validate(&query.key)?;
    let backup = backup(&state)?;
    let work = env::var("APP_WORK_DIR").unwrap_or_else(|_| "/work".into());
    let staging = tempfile::NamedTempFile::new_in(work).map_err(ApiError::internal)?;
    let mut file = tokio::fs::File::from_std(staging.reopen().map_err(ApiError::internal)?);
    let mut body = request.into_body();
    let mut size = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(ApiError::internal)?;
        if let Ok(data) = frame.into_data() {
            size += data.len() as u64;
            if size > 5 * 1024 * 1024 * 1024 {
                return Err(ApiError::bad_request("database snapshot exceeds 5 GiB"));
            }
            file.write_all(&data).await.map_err(ApiError::internal)?;
        }
    }
    file.flush().await.map_err(ApiError::internal)?;
    let body = ByteStream::from_path(staging.path())
        .await
        .map_err(ApiError::internal)?;
    backup
        .client
        .put_object()
        .bucket(&backup.bucket)
        .key(query.key)
        .content_type("application/x-sqlite3")
        .body(body)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<BackupRef>>, ApiError> {
    let backup = backup(&state)?;
    let mut token = None;
    let mut objects = Vec::new();
    loop {
        let page = backup
            .client
            .list_objects_v2()
            .bucket(&backup.bucket)
            .prefix(PREFIX)
            .set_continuation_token(token)
            .send()
            .await
            .map_err(ApiError::upstream)?;
        for item in page.contents() {
            if let Some(key) = item.key()
                && validate(key).is_ok()
            {
                objects.push(BackupRef { key: key.into() });
            }
        }
        if !page.is_truncated().unwrap_or(false) {
            break;
        }
        token = page.next_continuation_token().map(ToOwned::to_owned);
    }
    Ok(Json(objects))
}
pub async fn delete(
    State(state): State<AppState>,
    Json(query): Json<Keys>,
) -> Result<StatusCode, ApiError> {
    let backup = backup(&state)?;
    if query.keys.len() > 1000 {
        return Err(ApiError::bad_request("too many database snapshots"));
    }
    for key in &query.keys {
        validate(key)?;
    }
    if query.keys.is_empty() {
        return Ok(StatusCode::NO_CONTENT);
    }
    let objects = query
        .keys
        .into_iter()
        .map(|key| ObjectIdentifier::builder().key(key).build())
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::internal)?;
    let result = backup
        .client
        .delete_objects()
        .bucket(&backup.bucket)
        .delete(
            Delete::builder()
                .set_objects(Some(objects))
                .build()
                .map_err(ApiError::internal)?,
        )
        .send()
        .await
        .map_err(ApiError::upstream)?;
    if !result.errors().is_empty() {
        return Err(ApiError::upstream("database snapshot deletion failed"));
    }
    Ok(StatusCode::NO_CONTENT)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_database_keys_are_accepted() {
        assert!(validate("revaro-backups/database/revaro-db-20260912T001500Z.sqlite").is_ok());
        for key in [
            "blobs/id",
            "profile/avatar",
            "revaro-backups/database/../../file.sqlite",
            "revaro-backups/database/revaro-db-invalid.sqlite",
        ] {
            assert!(validate(key).is_err());
        }
    }
}
