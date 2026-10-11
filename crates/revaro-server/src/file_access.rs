//! Backend-neutral reads. Metadata and the reader describe the same version.
//! Providers can implement AsyncSeek using native remote Range reads; the HTTP
//! layer never needs a local path or a fully buffered object.

use std::{
    fmt::Debug,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, ReadBuf, SeekFrom};

use crate::cache::{CacheLoadError, CacheManager, FILE_BLOCK};
use crate::storage::{LocalStore, StorageError};

const BLOCK_BYTES: u64 = 64 << 10;

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

#[derive(Debug)]
pub struct CachedFileAccess {
    source: Arc<dyn FileAccess>,
    cache: CacheManager,
}

impl CachedFileAccess {
    pub fn new(source: Arc<dyn FileAccess>, cache: CacheManager) -> Self {
        Self { source, cache }
    }
}

impl FileAccess for CachedFileAccess {
    fn open<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FileObject, StorageError>> + Send + 'a>> {
        Box::pin(async move {
            let object = self.source.open(key).await?;
            let identity = revaro_core::keys::sha256_hex(
                format!("{}:{key}:{}:{}", key.len(), object.etag, object.size).as_bytes(),
            );
            Ok(FileObject {
                reader: Box::new(CachedReader {
                    source: Arc::new(tokio::sync::Mutex::new(BlockSource {
                        reader: object.reader,
                        position: None,
                    })),
                    cache: self.cache.clone(),
                    identity,
                    size: object.size,
                    position: 0,
                    block: Bytes::new(),
                    block_start: 0,
                    pending: None,
                }),
                size: object.size,
                etag: object.etag,
            })
        })
    }
}

type BlockRead = Pin<Box<dyn Future<Output = io::Result<(u64, Bytes)>> + Send>>;

struct BlockSource {
    reader: Box<dyn ReadSeek>,
    position: Option<u64>,
}

struct CachedReader {
    source: Arc<tokio::sync::Mutex<BlockSource>>,
    cache: CacheManager,
    identity: String,
    size: u64,
    position: u64,
    block: Bytes,
    block_start: u64,
    pending: Option<BlockRead>,
}

impl AsyncRead for CachedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 || self.position >= self.size {
            return Poll::Ready(Ok(()));
        }
        loop {
            if self.position >= self.block_start
                && self.position - self.block_start < self.block.len() as u64
            {
                let offset = (self.position - self.block_start) as usize;
                let length = output.remaining().min(self.block.len() - offset);
                output.put_slice(&self.block[offset..offset + length]);
                self.position += length as u64;
                return Poll::Ready(Ok(()));
            }
            if self.pending.is_none() {
                let start = self.position / BLOCK_BYTES * BLOCK_BYTES;
                let length = (self.size - start).min(BLOCK_BYTES) as usize;
                let key = format!("{}/{start}", self.identity);
                let source = self.source.clone();
                let cache = self.cache.clone();
                self.pending =
                    Some(Box::pin(async move {
                        let bytes =
                            cache
                                .load(FILE_BLOCK, &key, Duration::ZERO, move || async move {
                                    let mut source = source.lock().await;
                                    if source.position != Some(start) {
                                        source.position = None;
                                        source.reader.seek(SeekFrom::Start(start)).await.map_err(
                                            |error| CacheLoadError::other(error.to_string()),
                                        )?;
                                    }
                                    source.position = None;
                                    let mut bytes = vec![0; length];
                                    source.reader.read_exact(&mut bytes).await.map_err(
                                        |error| CacheLoadError::other(error.to_string()),
                                    )?;
                                    source.position = Some(start + length as u64);
                                    Ok(bytes)
                                })
                                .await
                                .map_err(io::Error::other)?;
                        Ok((start, bytes))
                    }));
            }
            match self
                .pending
                .as_mut()
                .expect("block read was created")
                .as_mut()
                .poll(context)
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => {
                    self.pending = None;
                    let (start, bytes) = result?;
                    self.block_start = start;
                    self.block = bytes;
                }
            }
        }
    }
}

impl AsyncSeek for CachedReader {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        self.position = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?,
            SeekFrom::End(offset) => self
                .size
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?,
        };
        self.pending = None;
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.position))
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

    #[derive(Debug)]
    struct CountingFiles {
        reads: Arc<std::sync::atomic::AtomicUsize>,
        version: std::sync::atomic::AtomicUsize,
    }

    struct CountingReader {
        size: u64,
        position: u64,
        value: u8,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl AsyncRead for CountingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let length = output
                .remaining()
                .min((self.size - self.position.min(self.size)) as usize);
            output.initialize_unfilled()[..length].fill(self.value);
            output.advance(length);
            self.position += length as u64;
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncSeek for CountingReader {
        fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
            self.position = match position {
                SeekFrom::Start(position) => position,
                SeekFrom::Current(offset) => self
                    .position
                    .checked_add_signed(offset)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?,
                SeekFrom::End(offset) => self
                    .size
                    .checked_add_signed(offset)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?,
            };
            Ok(())
        }

        fn poll_complete(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<io::Result<u64>> {
            Poll::Ready(Ok(self.position))
        }
    }

    impl FileAccess for CountingFiles {
        fn open<'a>(
            &'a self,
            _key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<FileObject, StorageError>> + Send + 'a>> {
            Box::pin(async move {
                let version = self.version.load(std::sync::atomic::Ordering::Relaxed);
                let size = (8 << 30) + 11;
                Ok(FileObject {
                    reader: Box::new(CountingReader {
                        size,
                        position: 0,
                        value: version as u8,
                        reads: self.reads.clone(),
                    }),
                    size,
                    etag: format!("version-{version}"),
                })
            })
        }
    }

    #[tokio::test]
    async fn aligned_ranges_share_blocks_without_buffering_the_whole_file_and_versions_are_isolated()
     {
        let cache = CacheManager::for_app(
            std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()),
            1 << 20,
            0,
        );
        let source = Arc::new(CountingFiles {
            reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            version: std::sync::atomic::AtomicUsize::new(1),
        });
        let files = CachedFileAccess::new(source.clone(), cache.clone());
        for (start, length, expected_reads) in [
            (0, 10, 1),
            (10, 10, 1),
            (BLOCK_BYTES - 3, 7, 2),
            (8 << 30, 11, 3),
        ] {
            let mut object = files.open("large").await.unwrap();
            object.reader.seek(SeekFrom::Start(start)).await.unwrap();
            let mut data = vec![0; length];
            object.reader.read_exact(&mut data).await.unwrap();
            assert_eq!(data, vec![1; length]);
            assert_eq!(
                source.reads.load(std::sync::atomic::Ordering::Relaxed),
                expected_reads
            );
            assert!(cache.stats().memory_bytes < object.size as i64);
        }
        source
            .version
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let mut object = files.open("large").await.unwrap();
        let mut data = [0; 10];
        object.reader.read_exact(&mut data).await.unwrap();
        assert_eq!(data, [2; 10]);
        assert_eq!(source.reads.load(std::sync::atomic::Ordering::Relaxed), 4);
        assert_eq!(cache.stats().classes[FILE_BLOCK].loads, 4);
        assert_eq!(cache.stats().memory_bytes, (BLOCK_BYTES * 3 + 11) as i64);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_open_readers_share_one_source_read_and_keep_object_keys_separate() {
        let cache = CacheManager::for_app(
            std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()),
            1 << 20,
            0,
        );
        let source = Arc::new(CountingFiles {
            reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            version: std::sync::atomic::AtomicUsize::new(1),
        });
        let files = Arc::new(CachedFileAccess::new(source.clone(), cache.clone()));
        let reads = (0..32).map(|_| {
            let files = files.clone();
            async move {
                let mut object = files.open("shared").await.unwrap();
                let mut data = [0; 10];
                object.reader.read_exact(&mut data).await.unwrap();
                assert_eq!(data, [1; 10]);
            }
        });
        futures_util::future::join_all(reads).await;
        assert_eq!(source.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(cache.stats().classes[FILE_BLOCK].loads, 1);
        let mut other = files.open("other").await.unwrap();
        other.reader.read_exact(&mut [0; 10]).await.unwrap();
        assert_eq!(source.reads.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn cached_reader_supports_end_relative_seeks_eof_and_invalid_offsets() {
        let cache = CacheManager::for_app(
            std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()),
            1 << 20,
            0,
        );
        let files = CachedFileAccess::new(Arc::new(MemoryFiles), cache);
        let mut object = files.open("remote/object").await.unwrap();
        object.reader.seek(SeekFrom::End(-3)).await.unwrap();
        let mut data = Vec::new();
        object.reader.read_to_end(&mut data).await.unwrap();
        assert_eq!(data, b"789");
        object.reader.seek(SeekFrom::Current(-5)).await.unwrap();
        let mut data = [0; 2];
        object.reader.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"56");
        assert_eq!(
            object
                .reader
                .seek(SeekFrom::End(-11))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        object.reader.seek(SeekFrom::Start(20)).await.unwrap();
        assert_eq!(object.reader.read(&mut data).await.unwrap(), 0);
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
