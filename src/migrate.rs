//! Ordered, checksummed migrations.
//!
//! The plugin records each applied migration in `_autumn_turso_migrations`.
//! Each migration runs in one transaction. An applied migration must not
//! change: a changed checksum is an error.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::error::{Result, TursoError};

/// The table that records applied migrations.
pub const MIGRATIONS_TABLE: &str = "_autumn_turso_migrations";

/// One schema change. The `id` sets the order and must be unique.
///
/// ```
/// use autumn_plugin_turso::Migration;
///
/// const CREATE_NOTES: Migration =
///     Migration::new("0001_create_notes", "CREATE TABLE notes (body TEXT NOT NULL);");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    id: Cow<'static, str>,
    sql: Cow<'static, str>,
}

impl Migration {
    /// Make a migration from static text.
    #[must_use]
    pub const fn new(id: &'static str, sql: &'static str) -> Self {
        Self {
            id: Cow::Borrowed(id),
            sql: Cow::Borrowed(sql),
        }
    }

    /// Make a migration from owned text, for example from a file.
    #[must_use]
    pub fn owned(id: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            id: Cow::Owned(id.into()),
            sql: Cow::Owned(sql.into()),
        }
    }

    /// The migration id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The SQL text.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// A stable 64-bit FNV-1a checksum of the SQL, as 16 hex digits.
    #[must_use]
    pub fn checksum(&self) -> String {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0100_0000_01b3;
        let hash = self.sql.bytes().fold(OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(PRIME)
        });
        format!("{hash:016x}")
    }
}

/// Check the list before the database changes.
pub fn check_list(migrations: &[Migration]) -> Result<()> {
    let mut seen = HashSet::new();
    for migration in migrations {
        if migration.id().trim().is_empty() {
            return Err(TursoError::Migration {
                id: String::new(),
                message: "the migration id is empty".into(),
            });
        }
        if !seen.insert(migration.id()) {
            return Err(TursoError::DuplicateMigration(migration.id().to_owned()));
        }
    }
    Ok(())
}

/// The ids that are not applied. A changed checksum is an error.
pub fn pending<'a>(
    migrations: &'a [Migration],
    applied: &HashMap<String, String>,
) -> Result<Vec<&'a Migration>> {
    let mut pending = Vec::new();
    for migration in migrations {
        match applied.get(migration.id()) {
            Some(checksum) if *checksum == migration.checksum() => {}
            Some(checksum) => {
                return Err(TursoError::MigrationChecksum {
                    id: migration.id().to_owned(),
                    applied: checksum.clone(),
                    current: migration.checksum(),
                });
            }
            None => pending.push(migration),
        }
    }
    Ok(pending)
}

pub fn create_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {MIGRATIONS_TABLE} (\
         id TEXT PRIMARY KEY NOT NULL, \
         checksum TEXT NOT NULL, \
         applied_at INTEGER NOT NULL)"
    )
}
