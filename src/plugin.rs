//! `TursoPlugin`: wires the database into an Autumn app.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use autumn_web::app::AppBuilder;
use autumn_web::plugin::Plugin;
use autumn_web::plugin_contract::PluginContract;
use autumn_web::{AppState, AutumnError};

use crate::config::{ConfigSources, SyncConfig, TursoConfig};
use crate::error::TursoError;
use crate::handle::Turso;
use crate::health::TursoHealthIndicator;
use crate::metrics::TursoMetricsSource;
use crate::migrate::Migration;
use crate::slot::{Slot, SlotState};

/// The config section and the health and metrics name.
pub const NAME: &str = "turso";

/// Installs a Turso database in an Autumn app.
///
/// At startup the plugin reads `[turso]`, opens the database, applies
/// migrations and puts a [`Turso`] handle in `AppState`. At shutdown it
/// pushes (with sync) and closes the handle.
///
/// ```no_run
/// use autumn_plugin_turso::{Migration, TursoPlugin};
///
/// const NOTES: Migration = Migration::new("0001_notes", "CREATE TABLE notes (body TEXT);");
///
/// # async fn run() {
/// autumn_web::app()
///     .plugin(TursoPlugin::new().migration(NOTES))
///     .run()
///     .await;
/// # }
/// ```
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct TursoPlugin {
    base: TursoConfig,
    migrations: Vec<Migration>,
    sources: Option<ConfigSources>,
}

impl TursoPlugin {
    /// Make a plugin with the default config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `enabled`. The config files and environment can change it.
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.base.enabled = enabled;
        self
    }

    /// Set the database path. The config files and environment can change it.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.base.path = path.into();
        self
    }

    /// Set `read_only`.
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.base.read_only = read_only;
        self
    }

    /// Set the lock wait time.
    pub fn busy_timeout(mut self, timeout: Duration) -> Self {
        self.base.busy_timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self
    }

    /// Set `migrate_on_start`. When `false`, the plugin only logs pending
    /// migrations.
    pub const fn migrate_on_start(mut self, migrate: bool) -> Self {
        self.base.migrate_on_start = migrate;
        self
    }

    /// Set the sync config. It needs the `sync` feature.
    pub fn sync(mut self, sync: SyncConfig) -> Self {
        self.base.sync = Some(sync);
        self
    }

    /// Add one migration. Migrations run in the order you add them.
    pub fn migration(mut self, migration: Migration) -> Self {
        self.migrations.push(migration);
        self
    }

    /// Add migrations.
    pub fn migrations(mut self, migrations: impl IntoIterator<Item = Migration>) -> Self {
        self.migrations.extend(migrations);
        self
    }

    /// Read config from `sources` instead of the process. Use it in tests.
    pub fn config_sources(mut self, sources: ConfigSources) -> Self {
        self.sources = Some(sources);
        self
    }
}

impl Plugin for TursoPlugin {
    fn contract(&self) -> Option<PluginContract> {
        Some(
            PluginContract::new(env!("CARGO_PKG_NAME"))
                .plugin_version(env!("CARGO_PKG_VERSION"))
                .autumn_web("0.8"),
        )
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let slot = Slot::default();
        let running = Running::default();
        let start = Arc::new(Start {
            base: self.base,
            migrations: self.migrations,
            sources: self.sources,
            slot: slot.clone(),
            running: running.clone(),
        });
        app.config_section(NAME)
            .health_indicator(
                NAME,
                Arc::new(TursoHealthIndicator::from_slot(slot.clone())),
            )
            .metrics_source(NAME, Arc::new(TursoMetricsSource::from_slot(slot)))
            .on_startup(move |state| {
                let start = Arc::clone(&start);
                async move {
                    start
                        .run(&state)
                        .await
                        .map_err(AutumnError::internal_server_error)
                }
            })
            .on_shutdown(move || {
                let running = running.clone();
                async move { running.stop().await }
            })
    }
}

struct Start {
    base: TursoConfig,
    migrations: Vec<Migration>,
    sources: Option<ConfigSources>,
    slot: Slot,
    running: Running,
}

impl Start {
    async fn run(&self, state: &AppState) -> Result<(), TursoError> {
        let sources = self
            .sources
            .clone()
            .unwrap_or_else(|| ConfigSources::from_process(state.profile()));
        let config = TursoConfig::resolve(self.base.clone(), &sources)?;
        if !config.enabled {
            tracing::info!("turso plugin is disabled");
            self.slot.set(SlotState::Disabled);
            return Ok(());
        }
        let handle = Turso::open(&config).await?;
        if config.migrate_on_start {
            handle.migrate(&self.migrations).await?;
        } else {
            let pending = handle.pending(&self.migrations).await?;
            if !pending.is_empty() {
                tracing::warn!(
                    ?pending,
                    "turso migrations are pending; migrate_on_start is off"
                );
            }
        }
        self.running.start(&handle, config.sync.as_ref());
        state.insert_extension(handle.clone());
        self.slot.set(SlotState::Open(handle));
        tracing::info!(path = %config.path, "turso database open");
        Ok(())
    }
}

/// What shutdown must stop.
#[derive(Clone, Default)]
struct Running(Arc<Mutex<Option<RunningState>>>);

struct RunningState {
    handle: Turso,
    #[cfg(feature = "sync")]
    sync: Option<SyncTask>,
}

impl Running {
    #[cfg_attr(not(feature = "sync"), allow(unused_variables))]
    fn start(&self, handle: &Turso, sync: Option<&SyncConfig>) {
        let state = RunningState {
            handle: handle.clone(),
            #[cfg(feature = "sync")]
            sync: sync.map(|sync| SyncTask::start(handle, sync)),
        };
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(state);
    }

    async fn stop(&self) {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(state) = state else {
            return;
        };
        #[cfg(feature = "sync")]
        if let Some(sync) = state.sync {
            sync.stop(&state.handle).await;
        }
        state.handle.close().await;
        tracing::info!("turso database closed");
    }
}

/// The sync loop and the shutdown push.
#[cfg(feature = "sync")]
struct SyncTask {
    push_on_shutdown: bool,
    stop: Option<tokio::sync::watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(feature = "sync")]
impl SyncTask {
    fn start(handle: &Turso, config: &SyncConfig) -> Self {
        let mut task = Self {
            push_on_shutdown: config.push_on_shutdown,
            stop: None,
            task: None,
        };
        if config.interval_secs > 0 {
            let (tx, rx) = tokio::sync::watch::channel(false);
            let every = Duration::from_secs(config.interval_secs);
            task.stop = Some(tx);
            task.task = Some(tokio::spawn(sync_loop(handle.clone(), every, rx)));
        }
        task
    }

    async fn stop(self, handle: &Turso) {
        if let Some(stop) = self.stop {
            let _ = stop.send(true);
        }
        if let Some(task) = self.task {
            let _ = task.await;
        }
        if self.push_on_shutdown
            && let Err(err) = handle.push().await
        {
            tracing::error!(%err, "turso push at shutdown failed");
        }
    }
}

/// Push and pull every `every`. Log errors and continue.
#[cfg(feature = "sync")]
async fn sync_loop(handle: Turso, every: Duration, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => break,
        }
        if let Err(err) = handle.push().await {
            tracing::warn!(%err, "turso sync push failed");
        }
        if let Err(err) = handle.pull().await {
            tracing::warn!(%err, "turso sync pull failed");
        }
    }
}

#[cfg(all(test, feature = "sync"))]
mod tests {
    use super::*;

    use crate::sync_server;

    fn synced(dir: &tempfile::TempDir, url: &str, interval_secs: u64) -> TursoConfig {
        TursoConfig {
            path: dir.path().join("db").to_string_lossy().into_owned(),
            sync: Some(SyncConfig {
                url: Some(url.to_owned()),
                interval_secs,
                ..SyncConfig::default()
            }),
            ..TursoConfig::default()
        }
    }

    async fn count(db: &Turso) -> i64 {
        let rows = db
            .query("SELECT count(*) FROM notes", ())
            .await
            .expect("count");
        rows[0].get(0).expect("n")
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires tursodb (TURSODB_BIN)"]
    async fn shutdown_pushes_then_closes() {
        let server = sync_server::SyncServer::start();
        let dir = tempfile::tempdir().expect("dir");
        let config = synced(&dir, server.url(), 0);
        let handle = Turso::open(&config).await.expect("open");
        handle
            .execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('x');")
            .await
            .expect("write");
        let running = Running::default();
        running.start(&handle, config.sync.as_ref());
        running.stop().await;
        assert!(handle.is_closed());

        let other_dir = tempfile::tempdir().expect("dir");
        let other = Turso::open(&synced(&other_dir, server.url(), 0))
            .await
            .expect("other");
        assert_eq!(count(&other).await, 1, "shutdown must push");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires tursodb (TURSODB_BIN)"]
    async fn interval_loop_pushes_and_pulls() {
        let server = sync_server::SyncServer::start();
        let dir = tempfile::tempdir().expect("dir");
        let config = synced(&dir, server.url(), 1);
        let handle = Turso::open(&config).await.expect("open");
        handle
            .execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('x');")
            .await
            .expect("write");
        let running = Running::default();
        running.start(&handle, config.sync.as_ref());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while handle.stats().sync_pushes == 0 || handle.stats().sync_pulls < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "loop did not sync: {:?}",
                handle.stats()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        running.stop().await;
        assert!(handle.is_closed());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_without_start_is_a_no_op() {
        Running::default().stop().await;
    }
}
