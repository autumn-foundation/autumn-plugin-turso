//! The plugin error type.

/// An error from the Turso plugin.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TursoError {
    /// The configuration is not valid. The message names the key.
    #[error("invalid turso config: {0}")]
    Config(String),

    /// The database file or its directory is not available.
    #[error("cannot open turso database at `{path}`: {source}")]
    Open {
        /// The database path.
        path: String,
        /// The cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The engine returned an error.
    #[error("turso: {0}")]
    Database(#[from] turso::Error),

    /// A migration failed. The plugin rolled it back.
    #[error("turso migration `{id}` failed: {message}")]
    Migration {
        /// The migration id.
        id: String,
        /// The cause.
        message: String,
    },

    /// An applied migration changed after it was applied.
    #[error(
        "turso migration `{id}` changed after it was applied \
         (applied checksum {applied}, current checksum {current}); add a new migration instead"
    )]
    MigrationChecksum {
        /// The migration id.
        id: String,
        /// The checksum in the database.
        applied: String,
        /// The checksum of the current SQL.
        current: String,
    },

    /// Two migrations use the same id.
    #[error("turso migration id `{0}` is used more than once")]
    DuplicateMigration(String),

    /// The database is read-only and migrations are pending.
    #[error("turso database is read-only and these migrations are pending: {}", .0.join(", "))]
    ReadOnlyPending(Vec<String>),

    /// The handle is closed.
    #[error("turso database is closed")]
    Closed,

    /// The plugin is not installed or is disabled.
    #[error("turso plugin is not installed or is disabled")]
    NotInstalled,

    /// The config has `[turso.sync]`, but the crate has no `sync` feature.
    #[error("`[turso.sync]` is set, but the `sync` feature of autumn-plugin-turso is off")]
    SyncFeatureDisabled,

    /// A sync operation is called on a database without sync.
    #[error("turso sync is not configured for this database")]
    SyncNotConfigured,

    /// A sync operation failed.
    #[error("turso sync failed: {0}")]
    Sync(String),
}

/// A result with [`TursoError`].
pub type Result<T, E = TursoError> = std::result::Result<T, E>;
