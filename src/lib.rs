#![doc = include_str!("../README.md")]

mod config;
mod error;
mod handle;
mod health;
mod metrics;
mod migrate;
mod plugin;
mod slot;
#[cfg(all(test, feature = "sync"))]
#[path = "../tests/support/sync_server.rs"]
mod sync_server;

pub use config::{ConfigSources, SyncConfig, TursoConfig};
pub use error::TursoError;
pub use handle::{Turso, TursoStats};
pub use health::TursoHealthIndicator;
pub use metrics::TursoMetricsSource;
pub use migrate::{MIGRATIONS_TABLE, Migration};
pub use plugin::TursoPlugin;

/// The `turso` crate, so the app uses the same version.
pub use turso;
