//! The `Turso` database handle.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::RwLock;
use turso::{Connection, IntoParams, Row};

use crate::config::TursoConfig;
use crate::error::{Result, TursoError};
use crate::migrate::{self, MIGRATIONS_TABLE, Migration};

/// A shared handle to one Turso database.
///
/// Clones share the database. Get it in a handler as an extractor
/// (`db: Turso`), or from state with `state.extension::<Turso>()`.
#[derive(Clone)]
pub struct Turso {
    inner: Arc<Inner>,
}

struct Inner {
    backend: RwLock<Option<Backend>>,
    closed: AtomicBool,
    path: String,
    read_only: bool,
    synced: bool,
    busy_timeout: std::time::Duration,
    counters: Counters,
}

#[derive(Clone)]
enum Backend {
    Local(turso::Database),
    #[cfg(feature = "sync")]
    Synced(turso::sync::Database),
}

#[derive(Default)]
pub struct Counters {
    pub statements: AtomicU64,
    pub errors: AtomicU64,
    pub connections: AtomicU64,
    pub migrations: AtomicU64,
    pub sync_pushes: AtomicU64,
    pub sync_pulls: AtomicU64,
    pub sync_errors: AtomicU64,
}

/// Counter values for one handle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TursoStats {
    /// Statements run through the handle helpers.
    pub statements: u64,
    /// Failed handle calls.
    pub errors: u64,
    /// Connections opened.
    pub connections: u64,
    /// Migrations applied.
    pub migrations_applied: u64,
    /// Successful sync pushes.
    pub sync_pushes: u64,
    /// Successful sync pulls.
    pub sync_pulls: u64,
    /// Failed sync calls.
    pub sync_errors: u64,
}

impl std::fmt::Debug for Turso {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Turso")
            .field("path", &self.inner.path)
            .field("read_only", &self.inner.read_only)
            .field("synced", &self.inner.synced)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl Turso {
    /// Open the database that `config` names.
    ///
    /// This makes missing parent directories of a file path. With sync, it
    /// also pulls when `pull_on_start` is set.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Config`] for a bad config, [`TursoError::Open`]
    /// when the file cannot open, and [`TursoError::Sync`] when the remote
    /// fails.
    pub async fn open(config: &TursoConfig) -> Result<Self> {
        config.validate()?;
        let path = config.path.trim().to_owned();
        if !config.read_only {
            make_parent_dir(&path)?;
        }
        let synced = config.active_sync().is_some();
        let backend = open_backend(config, &path).await?;
        let handle = Self {
            inner: Arc::new(Inner {
                backend: RwLock::new(Some(backend)),
                closed: AtomicBool::new(false),
                path,
                read_only: config.read_only,
                synced,
                busy_timeout: std::time::Duration::from_millis(config.busy_timeout_ms),
                counters: Counters::default(),
            }),
        };
        #[cfg(feature = "sync")]
        if config.active_sync().is_some_and(|sync| sync.pull_on_start) {
            handle.pull().await?;
        }
        Ok(handle)
    }

    /// Open a new connection. Use one connection per task.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Closed`] after [`close`](Self::close), or the
    /// engine error.
    pub async fn connect(&self) -> Result<Connection> {
        let result = self.connect_inner().await;
        self.count(result)
    }

    /// Run one statement and return the number of changed rows.
    ///
    /// # Errors
    ///
    /// Returns the engine error, or [`TursoError::Closed`].
    pub async fn execute(&self, sql: &str, params: impl IntoParams + Send) -> Result<u64> {
        let result = async { Ok(self.statement_conn().await?.execute(sql, params).await?) }.await;
        self.count(result)
    }

    /// Run one or more statements with no parameters.
    ///
    /// # Errors
    ///
    /// Returns the engine error, or [`TursoError::Closed`].
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        let result = async { Ok(self.statement_conn().await?.execute_batch(sql).await?) }.await;
        self.count(result)
    }

    /// Run a query and collect all rows.
    ///
    /// For large results, use [`connect`](Self::connect) and read rows one
    /// by one.
    ///
    /// # Errors
    ///
    /// Returns the engine error, or [`TursoError::Closed`].
    pub async fn query(&self, sql: &str, params: impl IntoParams + Send) -> Result<Vec<Row>> {
        let result = async {
            let mut rows = self.statement_conn().await?.query(sql, params).await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row);
            }
            Ok(out)
        }
        .await;
        self.count(result)
    }

    /// Run `SELECT 1`. The metrics do not count it, because health probes
    /// call it often.
    ///
    /// # Errors
    ///
    /// Returns the engine error, or [`TursoError::Closed`].
    pub async fn ping(&self) -> Result<()> {
        let conn = self.connect_inner().await?;
        let mut rows = conn.query("SELECT 1", ()).await?;
        while rows.next().await?.is_some() {}
        Ok(())
    }

    /// Apply the pending `migrations` in order. Return how many ran.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::DuplicateMigration`],
    /// [`TursoError::MigrationChecksum`], [`TursoError::ReadOnlyPending`],
    /// [`TursoError::Migration`], [`TursoError::Closed`] or an engine error.
    /// The plugin rolls back a failed migration.
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<usize> {
        let result = self.migrate_inner(migrations).await;
        self.count(result)
    }

    /// The ids of the `migrations` that are not applied.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::DuplicateMigration`],
    /// [`TursoError::MigrationChecksum`] or an engine error.
    pub async fn pending(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        let result = self.pending_inner(migrations).await;
        self.count(result)
    }

    async fn pending_inner(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        migrate::check_list(migrations)?;
        let conn = self.connect_inner().await?;
        let applied = applied_checksums(&conn).await?;
        Ok(migrate::pending(migrations, &applied)?
            .into_iter()
            .map(|migration| migration.id().to_owned())
            .collect())
    }

    /// Close the handle and all its clones. Later calls return
    /// [`TursoError::Closed`]. A second close does nothing.
    ///
    /// Close does not stop work that started before it. A helper call in
    /// progress completes. A connection from [`connect`](Self::connect)
    /// stays open until you drop it.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.backend.write().await.take();
    }

    /// `true` after [`close`](Self::close).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// The database path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    /// `true` when the database has no write access.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.inner.read_only
    }

    /// `true` when the database syncs with Turso Cloud.
    #[must_use]
    pub fn is_synced(&self) -> bool {
        self.inner.synced
    }

    /// The counter values.
    #[must_use]
    pub fn stats(&self) -> TursoStats {
        let c = &self.inner.counters;
        TursoStats {
            statements: c.statements.load(Ordering::Relaxed),
            errors: c.errors.load(Ordering::Relaxed),
            connections: c.connections.load(Ordering::Relaxed),
            migrations_applied: c.migrations.load(Ordering::Relaxed),
            sync_pushes: c.sync_pushes.load(Ordering::Relaxed),
            sync_pulls: c.sync_pulls.load(Ordering::Relaxed),
            sync_errors: c.sync_errors.load(Ordering::Relaxed),
        }
    }

    /// Push local changes to Turso Cloud.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::SyncNotConfigured`] for a local database,
    /// [`TursoError::Closed`], or [`TursoError::Sync`].
    #[cfg(feature = "sync")]
    pub async fn push(&self) -> Result<()> {
        let db = self.synced_backend().await?;
        let result = db
            .push()
            .await
            .map_err(|err| TursoError::Sync(err.to_string()));
        self.count_sync(&result, &self.inner.counters.sync_pushes);
        result
    }

    /// Pull remote changes from Turso Cloud. Return `true` when a change
    /// was applied.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::SyncNotConfigured`] for a local database,
    /// [`TursoError::Closed`], or [`TursoError::Sync`].
    #[cfg(feature = "sync")]
    pub async fn pull(&self) -> Result<bool> {
        let db = self.synced_backend().await?;
        let result = db
            .pull()
            .await
            .map_err(|err| TursoError::Sync(err.to_string()));
        self.count_sync(&result, &self.inner.counters.sync_pulls);
        result
    }

    #[cfg(feature = "sync")]
    async fn synced_backend(&self) -> Result<turso::sync::Database> {
        let backend = self.backend().await?;
        match backend {
            Backend::Synced(db) => Ok(db),
            Backend::Local(_) => Err(TursoError::SyncNotConfigured),
        }
    }

    #[cfg(feature = "sync")]
    fn count_sync<T>(&self, result: &Result<T>, ok_counter: &AtomicU64) {
        let counter = if result.is_ok() {
            ok_counter
        } else {
            &self.inner.counters.sync_errors
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    async fn backend(&self) -> Result<Backend> {
        if self.is_closed() {
            return Err(TursoError::Closed);
        }
        self.inner
            .backend
            .read()
            .await
            .clone()
            .ok_or(TursoError::Closed)
    }

    async fn connect_inner(&self) -> Result<Connection> {
        let backend = self.backend().await?;
        let conn = match backend {
            Backend::Local(db) => db.connect()?,
            #[cfg(feature = "sync")]
            Backend::Synced(db) => db.connect().await?,
        };
        conn.busy_timeout(self.inner.busy_timeout)?;
        self.inner
            .counters
            .connections
            .fetch_add(1, Ordering::Relaxed);
        Ok(conn)
    }

    /// A connection for one helper statement. Counts the statement.
    async fn statement_conn(&self) -> Result<Connection> {
        let conn = self.connect_inner().await?;
        self.inner
            .counters
            .statements
            .fetch_add(1, Ordering::Relaxed);
        Ok(conn)
    }

    async fn migrate_inner(&self, migrations: &[Migration]) -> Result<usize> {
        migrate::check_list(migrations)?;
        let conn = self.connect_inner().await?;
        let applied = applied_checksums(&conn).await?;
        let pending = migrate::pending(migrations, &applied)?;
        if pending.is_empty() {
            return Ok(0);
        }
        if self.inner.read_only {
            return Err(TursoError::ReadOnlyPending(
                pending.iter().map(|m| m.id().to_owned()).collect(),
            ));
        }
        conn.execute(migrate::create_table_sql(), ()).await?;
        let mut applied = 0;
        for migration in &pending {
            if apply_one(&conn, migration).await? {
                applied += 1;
                self.inner
                    .counters
                    .migrations
                    .fetch_add(1, Ordering::Relaxed);
                tracing::info!(id = migration.id(), "turso migration applied");
            }
        }
        Ok(applied)
    }

    fn count<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.inner.counters.errors.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}

async fn open_backend(config: &TursoConfig, path: &str) -> Result<Backend> {
    #[cfg(feature = "sync")]
    if let Some(sync) = config.active_sync() {
        let mut builder = turso::sync::Builder::new_remote(path)
            .with_remote_url(sync.url.clone().unwrap_or_default())
            .with_client_name(concat!("autumn-plugin-turso/", env!("CARGO_PKG_VERSION")));
        if let Some(token) = sync.token() {
            builder = builder.with_auth_token(token.to_owned());
        }
        let db = builder
            .build()
            .await
            .map_err(|err| TursoError::Sync(err.to_string()))?;
        return Ok(Backend::Synced(db));
    }
    let db = turso::Builder::new_local(path)
        .read_only(config.read_only)
        .build()
        .await
        .map_err(|err| TursoError::Open {
            path: path.to_owned(),
            source: Box::new(err),
        })?;
    Ok(Backend::Local(db))
}

fn make_parent_dir(path: &str) -> Result<()> {
    if path == ":memory:" {
        return Ok(());
    }
    match std::path::Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            std::fs::create_dir_all(parent).map_err(|err| TursoError::Open {
                path: path.to_owned(),
                source: Box::new(err),
            })
        }
        _ => Ok(()),
    }
}

async fn applied_checksums(conn: &Connection) -> Result<HashMap<String, String>> {
    let exists = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [MIGRATIONS_TABLE],
        )
        .await?
        .next()
        .await?
        .is_some();
    let mut applied = HashMap::new();
    if !exists {
        return Ok(applied);
    }
    let mut rows = conn
        .query(format!("SELECT id, checksum FROM {MIGRATIONS_TABLE}"), ())
        .await?;
    while let Some(row) = rows.next().await? {
        applied.insert(row.get::<String>(0)?, row.get::<String>(1)?);
    }
    Ok(applied)
}

/// Apply one migration in one transaction. Return `false` when another
/// process applied it first.
async fn apply_one(conn: &Connection, migration: &Migration) -> Result<bool> {
    let fail = |message: String| TursoError::Migration {
        id: migration.id().to_owned(),
        message,
    };
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| fail(err.to_string()))?;
    let result = apply_in_transaction(conn, migration).await;
    if result.is_err()
        && !conn.is_autocommit().unwrap_or(true)
        && let Err(rollback) = conn.execute("ROLLBACK", ()).await
    {
        tracing::warn!(id = migration.id(), %rollback, "turso migration rollback failed");
    }
    result
}

async fn apply_in_transaction(conn: &Connection, migration: &Migration) -> Result<bool> {
    let fail = |message: String| TursoError::Migration {
        id: migration.id().to_owned(),
        message,
    };
    // Read again under the write lock. Another process can be first.
    let mut rows = conn
        .query(
            format!("SELECT checksum FROM {MIGRATIONS_TABLE} WHERE id = ?1"),
            [migration.id()],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        drop(rows);
        let applied = row.get::<String>(0)?;
        conn.execute("ROLLBACK", ()).await?;
        if applied == migration.checksum() {
            return Ok(false);
        }
        return Err(TursoError::MigrationChecksum {
            id: migration.id().to_owned(),
            applied,
            current: migration.checksum(),
        });
    }
    drop(rows);
    conn.execute_batch(migration.sql())
        .await
        .map_err(|err| fail(err.to_string()))?;
    if conn.is_autocommit()? {
        return Err(fail(
            "the migration ended the transaction; do not use BEGIN, COMMIT or ROLLBACK in a migration"
                .into(),
        ));
    }
    conn.execute(
        format!(
            "INSERT INTO {MIGRATIONS_TABLE} (id, checksum, applied_at) \
             VALUES (?1, ?2, CAST(strftime('%s', 'now') AS INTEGER))"
        ),
        (migration.id().to_owned(), migration.checksum()),
    )
    .await
    .map_err(|err| fail(err.to_string()))?;
    conn.execute("COMMIT", ())
        .await
        .map_err(|err| fail(err.to_string()))?;
    Ok(true)
}

impl axum::extract::FromRequestParts<autumn_web::AppState> for Turso {
    type Rejection = autumn_web::AutumnError;

    fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        state: &autumn_web::AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            state
                .extension::<Self>()
                .map(|handle| (*handle).clone())
                .ok_or_else(|| {
                    autumn_web::AutumnError::internal_server_error(TursoError::NotInstalled)
                }),
        )
    }
}
