//! SQLite access, ported from the Go `internal/database` package.
//!
//! The compatibility contract lives here:
//!
//! * the same pragmas (`foreign_keys=1`, `busy_timeout=5000`, `journal_mode=WAL`),
//! * the same `schema_migrations(version, applied_at)` bookkeeping,
//! * the same migration bodies, applied in the same order, each in one
//!   transaction,
//! * the same file permissions (`0700` on the directory, `0600` on the file).
//!
//! The migration SQL lives next to this module and is included at compile time,
//! so the Rust binary has one immutable schema source and does not depend on
//! the retired Go tree.
//!
//! ## Concurrency
//!
//! SQLite has a single writer. `Database` is a small pool of connections: WAL
//! lets readers proceed while a writer holds the write lock, and the pool bound
//! keeps write contention from turning into a thundering herd. Every call runs
//! on a blocking thread, because `rusqlite` is synchronous and must never stall
//! the async runtime.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use revaro_core::ApiError;
use rusqlite::Connection;

/// Number of connections kept open, matching the Go pool bound.
const POOL_SIZE: usize = 4;

/// How long SQLite waits for a competing writer before returning `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A migration embedded into the binary.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Numeric version recorded in `schema_migrations`.
    pub version: i64,
    /// File name, used in error messages and by the drift test.
    pub name: &'static str,
    /// The SQL body.
    pub sql: &'static str,
}

/// Every migration, in application order.
///
/// The `version` values match the numeric prefix of each file name, which is
/// how the original server derived them.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "001_local_product.sql",
        sql: include_str!("../migrations/001_local_product.sql"),
    },
    Migration {
        version: 2,
        name: "002_file_cleanup.sql",
        sql: include_str!("../migrations/002_file_cleanup.sql"),
    },
    Migration {
        version: 3,
        name: "003_remove_subtitles.sql",
        sql: include_str!("../migrations/003_remove_subtitles.sql"),
    },
];

/// Failure modes of opening, migrating or querying the database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The data directory could not be created or secured.
    #[error("could not prepare the data directory: {0}")]
    Directory(#[source] std::io::Error),
    /// The database could not be opened or configured.
    #[error("could not open the database: {0}")]
    Open(#[source] rusqlite::Error),
    /// A migration failed; the transaction was rolled back.
    #[error("could not apply migration {name}: {source}")]
    Migration {
        /// File name of the failing migration.
        name: &'static str,
        /// Underlying SQLite error.
        #[source]
        source: rusqlite::Error,
    },
    /// A statement failed.
    #[error("database error: {0}")]
    Query(#[from] rusqlite::Error),
    /// The blocking worker panicked or was cancelled.
    #[error("database worker failed: {0}")]
    Worker(String),
    /// The pool could not hand out a connection.
    #[error("database is unavailable: {0}")]
    Unavailable(String),
}

impl DbError {
    /// True when the failure is a constraint violation.
    ///
    /// Handlers use this to answer `409` for duplicate names instead of `500`,
    /// which is what the Go `isConflict` helper did by matching on the error
    /// text.
    #[must_use]
    pub fn is_constraint_violation(&self) -> bool {
        match self {
            DbError::Query(rusqlite::Error::SqliteFailure(error, _))
            | DbError::Open(rusqlite::Error::SqliteFailure(error, _)) => {
                error.code == rusqlite::ErrorCode::ConstraintViolation
            }
            _ => false,
        }
    }

    /// True when the failure means "no such row", the equivalent of Go's
    /// `sql.ErrNoRows`.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, DbError::Query(rusqlite::Error::QueryReturnedNoRows))
    }
}

/// Where a pool's connections come from.
#[derive(Debug, Clone)]
enum Source {
    /// A file on disk; every connection opens the same database.
    File(PathBuf),
    /// A private in-memory database, restricted to a single connection because
    /// each `:memory:` connection would otherwise get its own database.
    Memory,
}

#[derive(Debug, Default)]
struct PoolState {
    idle: Vec<Connection>,
    /// Connections checked out right now, plus the idle ones.
    total: usize,
    /// Set when the pool can no longer produce connections.
    poisoned: Option<String>,
}

/// A bounded pool of SQLite connections.
#[derive(Debug)]
struct Pool {
    source: Source,
    max_size: usize,
    state: Mutex<PoolState>,
    available: Condvar,
}

impl Pool {
    fn new(source: Source) -> Self {
        let max_size = match source {
            Source::Memory => 1,
            Source::File(_) => POOL_SIZE,
        };
        Self {
            source,
            max_size,
            state: Mutex::new(PoolState::default()),
            available: Condvar::new(),
        }
    }

    /// Check out a connection, creating one if the pool has room.
    fn acquire(self: &Arc<Self>) -> Result<PooledConnection, DbError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(connection) = state.idle.pop() {
                return Ok(PooledConnection {
                    pool: Arc::clone(self),
                    connection: Some(connection),
                });
            }
            if let Some(reason) = &state.poisoned {
                return Err(DbError::Unavailable(reason.clone()));
            }
            if state.total < self.max_size {
                state.total += 1;
                drop(state);
                match self.open_connection() {
                    Ok(connection) => {
                        return Ok(PooledConnection {
                            pool: Arc::clone(self),
                            connection: Some(connection),
                        });
                    }
                    Err(error) => {
                        // Give the slot back so a later attempt can retry, and
                        // wake one waiter so it does not sleep forever.
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.total -= 1;
                        state.poisoned = Some(error.to_string());
                        self.available.notify_one();
                        return Err(error);
                    }
                }
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Return a connection to the pool.
    fn release(&self, connection: Connection) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.idle.push(connection);
        self.available.notify_one();
    }

    fn open_connection(&self) -> Result<Connection, DbError> {
        match &self.source {
            Source::Memory => {
                let connection = Connection::open_in_memory().map_err(DbError::Open)?;
                configure(&connection)?;
                Ok(connection)
            }
            Source::File(path) => {
                let connection = Connection::open(path).map_err(DbError::Open)?;
                configure(&connection)?;
                Ok(connection)
            }
        }
    }
}

/// Apply the pragmas every connection needs.
fn configure(connection: &Connection) -> Result<(), DbError> {
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(DbError::Open)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys=ON;\nPRAGMA journal_mode=WAL;\nPRAGMA synchronous=NORMAL;",
        )
        .map_err(DbError::Open)?;
    Ok(())
}

/// A checked-out connection that returns itself to the pool when dropped.
#[derive(Debug)]
pub struct PooledConnection {
    pool: Arc<Pool>,
    connection: Option<Connection>,
}

impl std::ops::Deref for PooledConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("connection is present until drop")
    }
}

impl std::ops::DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_mut()
            .expect("connection is present until drop")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.pool.release(connection);
        }
    }
}

/// The application database handle.
///
/// Cheap to clone; every clone shares the same pool.
#[derive(Debug, Clone)]
pub struct Database {
    pool: Arc<Pool>,
    path: PathBuf,
}

impl Database {
    /// Open (creating if necessary) the database at `path` and migrate it.
    ///
    /// # Errors
    /// Returns a [`DbError`] when the directory cannot be secured, the database
    /// cannot be opened, or a migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            prepare_directory(parent)?;
        }
        let database = Self {
            pool: Arc::new(Pool::new(Source::File(path.clone()))),
            path: path.clone(),
        };
        {
            let mut connection = database.acquire()?;
            database.migrate(&mut connection)?;
        }
        secure_file(&path)?;
        Ok(database)
    }

    /// Open a private in-memory database and migrate it. For tests.
    ///
    /// # Errors
    /// Returns a [`DbError`] when a migration fails.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let database = Self {
            pool: Arc::new(Pool::new(Source::Memory)),
            path: PathBuf::from(":memory:"),
        };
        {
            let mut connection = database.acquire()?;
            database.migrate(&mut connection)?;
        }
        Ok(database)
    }

    /// The database file path, or `:memory:`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Check out a connection for synchronous use (startup, tests).
    ///
    /// # Errors
    /// Returns a [`DbError`] when the pool cannot produce a connection.
    pub fn acquire(&self) -> Result<PooledConnection, DbError> {
        self.pool.acquire()
    }

    /// Run `operation` on a blocking thread with a pooled connection.
    ///
    /// # Errors
    /// Propagates the operation's error, or reports a worker failure if the
    /// blocking task could not run.
    pub async fn call<T, F>(&self, operation: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
        T: Send + 'static,
    {
        let pool = Arc::clone(&self.pool);
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.acquire()?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| DbError::Worker(error.to_string()))?
    }

    /// Run `operation` on a blocking thread, allowing it to fail with a
    /// client-visible [`ApiError`] as well as an internal database error.
    ///
    /// Handlers constantly need to distinguish "no such row" (a `404`) from "the
    /// query broke" (a `500`) *inside* the closure, where the connection is in
    /// scope. Returning [`ApiError`] from the closure keeps that decision next
    /// to the query, instead of reconstructing it afterwards from an error kind
    /// and re-deriving the right message.
    ///
    /// # Errors
    /// Propagates the operation's error, or reports a generic database failure
    /// if a connection could not be acquired or the worker could not run.
    pub async fn call_api<T, F>(&self, operation: F) -> Result<T, ApiError>
    where
        F: FnOnce(&mut Connection) -> Result<T, ApiError> + Send + 'static,
        T: Send + 'static,
    {
        let pool = Arc::clone(&self.pool);
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.acquire().map_err(|error| {
                tracing::error!(%error, "could not acquire a database connection");
                ApiError::internal("database error")
            })?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "database worker failed");
            ApiError::internal("database error")
        })?
    }

    /// Create the bookkeeping table and apply every migration that is missing.
    fn migrate(&self, connection: &mut Connection) -> Result<(), DbError> {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (\
                     version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
            )
            .map_err(|source| DbError::Migration {
                name: "schema_migrations",
                source,
            })?;

        for migration in MIGRATIONS {
            let applied: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                    [migration.version],
                    |row| row.get(0),
                )
                .map_err(|source| DbError::Migration {
                    name: migration.name,
                    source,
                })?;
            if applied > 0 {
                continue;
            }
            let transaction = connection
                .transaction()
                .map_err(|source| DbError::Migration {
                    name: migration.name,
                    source,
                })?;
            transaction
                .execute_batch(migration.sql)
                .map_err(|source| DbError::Migration {
                    name: migration.name,
                    source,
                })?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES(?1, ?2)",
                    rusqlite::params![
                        migration.version,
                        revaro_core::Timestamp::now().to_rfc3339()
                    ],
                )
                .map_err(|source| DbError::Migration {
                    name: migration.name,
                    source,
                })?;
            transaction.commit().map_err(|source| DbError::Migration {
                name: migration.name,
                source,
            })?;
        }
        Ok(())
    }

    /// Confirm the database answers a trivial query.
    ///
    /// # Errors
    /// Returns a [`DbError`] when the query fails.
    pub fn ping(&self) -> Result<(), DbError> {
        let connection = self.acquire()?;
        connection.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }
}

/// Create the data directory and restrict it to the owner.
fn prepare_directory(directory: &Path) -> Result<(), DbError> {
    std::fs::create_dir_all(directory).map_err(DbError::Directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(DbError::Directory)?;
    }
    Ok(())
}

/// Restrict the database file to the owner. The WAL sidecars inherit this.
fn secure_file(path: &Path) -> Result<(), DbError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(DbError::Directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use revaro_core::ids::ROOT_ID;

    #[test]
    fn migrations_are_ordered_and_named_consistently() {
        let mut previous = 0;
        for migration in MIGRATIONS {
            assert!(migration.version > previous, "versions must ascend");
            previous = migration.version;
            let prefix = migration.name.split('_').next().unwrap();
            assert_eq!(
                prefix.parse::<i64>().unwrap(),
                migration.version,
                "{} version prefix must match",
                migration.name
            );
            assert!(!migration.sql.trim().is_empty());
        }
    }

    #[test]
    fn opening_creates_the_expected_schema() {
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        let tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for expected in [
            "directory_stats",
            "files",
            "media_metadata",
            "media_progress",
            "object_cleanup",
            "schema_migrations",
            "sessions",
            "settings",
            "shares",
            "task_files",
            "tasks",
            "upload_parts",
            "uploads",
        ] {
            assert!(
                tables.iter().any(|name| name == expected),
                "missing table {expected}"
            );
        }
    }

    #[test]
    fn upgrading_existing_media_metadata_preserves_the_record() {
        let directory =
            std::env::temp_dir().join(format!("revaro-upgrade-{}", crate::ids::new_id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("revaro.db");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
                )
                .unwrap();
            for migration in &MIGRATIONS[..2] {
                connection.execute_batch(migration.sql).unwrap();
                connection
                    .execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES(?1, '2024-01-01T00:00:00Z')",
                        [migration.version],
                    )
                    .unwrap();
            }
            connection
                .execute(
                    "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) \
                     VALUES('f1',?1,'movie.mp4','file','blobs/f1',3,'ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                    [ROOT_ID],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO media_metadata(file_id,duration_ms,container,chapters_json,analyzed_at,subtitles_json) \
                     VALUES('f1',1234,'mp4','[]','2024-01-01T00:00:00Z','[{\"id\":\"old\"}]')",
                    [],
                )
                .unwrap();
        }

        let database = Database::open(&path).unwrap();
        let connection = database.acquire().unwrap();
        let media: (i64, String) = connection
            .query_row(
                "SELECT duration_ms, container FROM media_metadata WHERE file_id='f1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(media, (1234, "mp4".to_owned()));
        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(media_metadata)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!columns.iter().any(|column| column == "subtitles_json"));
        drop(connection);
        drop(database);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_root_directory_is_seeded() {
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        let (name, kind): (String, String) = connection
            .query_row(
                "SELECT name, kind FROM files WHERE id = ?1",
                [ROOT_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "");
        assert_eq!(kind, "directory");
    }

    #[test]
    fn migrations_are_recorded_once() {
        let database = Database::open_in_memory().unwrap();
        {
            let connection = database.acquire().unwrap();
            let count: i64 = connection
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, MIGRATIONS.len() as i64);
        }
        // Re-opening applies nothing new.
        let connection = database.acquire().unwrap();
        let mut statement = connection
            .prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
            .unwrap();
        let rows: Vec<(i64, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows.len(), MIGRATIONS.len());
        for (index, (version, applied_at)) in rows.iter().enumerate() {
            assert_eq!(*version, MIGRATIONS[index].version);
            assert!(
                revaro_core::Timestamp::parse(applied_at).is_ok(),
                "{applied_at}"
            );
        }
    }

    #[test]
    fn the_cleanup_trigger_queues_deleted_blobs() {
        // 002_file_cleanup.sql is the migration most likely to be silently lost,
        // so its observable effect is asserted directly.
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        connection
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) \
                 VALUES('f1',?1,'a.bin','file','blobs/f1',3,'ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                [ROOT_ID],
            )
            .unwrap();
        connection
            .execute("DELETE FROM files WHERE id='f1'", [])
            .unwrap();
        let (key, reason, generation): (String, String, i64) = connection
            .query_row(
                "SELECT object_key, reason, generation FROM object_cleanup",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(key, "blobs/f1");
        assert_eq!(reason, "file deleted");
        assert_eq!(generation, 0);
    }

    #[test]
    fn directory_stats_triggers_maintain_ancestor_totals() {
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        connection
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
                 VALUES('d1',?1,'sub','directory','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                [ROOT_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) \
                 VALUES('f1','d1','a.bin','file','blobs/f1',7,'ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let (files, bytes): (i64, i64) = connection
            .query_row(
                "SELECT file_count, total_bytes FROM directory_stats WHERE directory_id='d1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((files, bytes), (1, 7));
        // The root aggregates its subtree too.
        let (root_files, root_bytes): (i64, i64) = connection
            .query_row(
                "SELECT file_count, total_bytes FROM directory_stats WHERE directory_id=?1",
                [ROOT_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((root_files, root_bytes), (1, 7));
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        let result = connection.execute(
            "INSERT INTO shares(file_id,token,created_at) VALUES('missing','t','2024-01-01T00:00:00Z')",
            [],
        );
        assert!(
            result.is_err(),
            "a share for a missing file must be refused"
        );
    }

    #[test]
    fn constraint_violations_are_recognized() {
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        connection
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
                 VALUES('d1',?1,'sub','directory','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                [ROOT_ID],
            )
            .unwrap();
        let error = connection
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
                 VALUES('d2',?1,'sub','directory','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                [ROOT_ID],
            )
            .unwrap_err();
        let error = DbError::Query(error);
        assert!(error.is_constraint_violation(), "{error}");
    }

    #[test]
    fn opening_a_file_database_persists_and_secures_it() {
        let directory = std::env::temp_dir().join(format!("revaro-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let path = directory.join("revaro.db");
        {
            let database = Database::open(&path).unwrap();
            assert_eq!(database.path(), path.as_path());
            database.ping().unwrap();
        }
        assert!(path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600, "database file must not be world readable");
            let dir_mode = std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700, "data directory must not be world readable");
        }
        // Reopening finds the migrations already applied.
        let reopened = Database::open(&path).unwrap();
        let connection = reopened.acquire().unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn call_runs_off_the_async_runtime() {
        let database = Database::open_in_memory().unwrap();
        let root: String = database
            .call(|connection| {
                connection
                    .query_row("SELECT id FROM files", [], |row| row.get(0))
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(root, ROOT_ID);
    }

    #[tokio::test]
    async fn call_supports_transactions() {
        let database = Database::open_in_memory().unwrap();
        database
            .call(|connection| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
                     VALUES('d1',?1,'sub','directory','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z')",
                    [ROOT_ID],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let count: i64 = database
            .call(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM files WHERE id='d1'", [], |row| {
                        row.get(0)
                    })
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn concurrent_calls_share_the_pool() {
        let database = Database::open_in_memory().unwrap();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let database = database.clone();
            handles.push(tokio::spawn(async move {
                database
                    .call(|connection| {
                        connection
                            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                            .map_err(DbError::Query)
                    })
                    .await
            }));
        }
        for handle in handles {
            assert_eq!(handle.await.unwrap().unwrap(), 1);
        }
    }

    #[test]
    fn not_found_errors_are_recognized() {
        let database = Database::open_in_memory().unwrap();
        let connection = database.acquire().unwrap();
        let error = connection
            .query_row("SELECT id FROM files WHERE id='nope'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap_err();
        assert!(DbError::Query(error).is_not_found());
    }
}
