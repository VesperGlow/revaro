//! Backend-neutral reads. Metadata and the reader describe the same version.
//! Providers can implement AsyncSeek using native remote Range reads; the HTTP
//! layer never needs a local path or a fully buffered object.

use std::{fmt::Debug, future::Future, pin::Pin};

use tokio::io::{AsyncRead, AsyncSeek};

use crate::storage::{LocalStore, StorageError};

pub trait ReadSeek: AsyncRead + AsyncSeek + Unpin + Send {}
impl<T: AsyncRead + AsyncSeek + Unpin + Send> ReadSeek for T {}

pub struct FileObject {
    pub reader: Box<dyn ReadSeek>,
    pub size: u64,
    /// Unquoted, strong validator stable for the opened representation.
    pub etag: String,
}

pub trait FileAccess: Debug + Send + Sync {
    fn open<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FileObject, StorageError>> + Send + 'a>>;
}

impl FileAccess for LocalStore {
    fn open<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FileObject, StorageError>> + Send + 'a>> {
        Box::pin(async move {
            let object = self.open_object(key).await?;
            Ok(FileObject {
                reader: Box::new(object.file),
                size: object.size.max(0) as u64,
                etag: object.etag,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use std::sync::Arc;

    #[derive(Debug)]
    struct MemoryFiles;

    impl FileAccess for MemoryFiles {
        fn open<'a>(
            &'a self,
            key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<FileObject, StorageError>> + Send + 'a>> {
            Box::pin(async move {
                if key != "remote/object" {
                    return Err(StorageError::NotFound);
                }
                Ok(FileObject {
                    reader: Box::new(std::io::Cursor::new(b"0123456789")),
                    size: 10,
                    etag: "memory-version".into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn original_file_delivery_accepts_a_nonlocal_provider() {
        let root =
            std::env::temp_dir().join(format!("revaro-file-access-{}", uuid::Uuid::new_v4()));
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_CACHES_DIR" => Some(root.join("caches").display().to_string()),
            _ => None,
        })
        .unwrap();
        let store = LocalStore::open(&root).await.unwrap();
        let db = crate::db::Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(db.clone());
        let state = crate::state::AppState::with_file_access(
            Arc::new(config),
            db,
            store,
            auth,
            Arc::new(MemoryFiles),
        );
        let file = revaro_core::model::File {
            name: "remote.txt".into(),
            object_key: "remote/object".into(),
            ..Default::default()
        };
        for (range, status, body) in [
            (None, 200, "0123456789"),
            (Some("bytes=3-7"), 206, "34567"),
            (Some("bytes=-3"), 206, "789"),
        ] {
            let mut headers = http::HeaderMap::new();
            if let Some(range) = range {
                headers.insert(http::header::RANGE, range.parse().unwrap());
            }
            let response =
                crate::file_routes::serve_file(state.clone(), file.clone(), false, headers)
                    .await
                    .unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.headers()[http::header::ETAG], "\"memory-version\"");
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                body
            );
        }
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::RANGE, "bytes=10-".parse().unwrap());
        let response = crate::file_routes::serve_file(state, file, false, headers)
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers()[http::header::CONTENT_RANGE],
            "bytes */10"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
