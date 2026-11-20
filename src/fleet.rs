//! Turso fleets: one Turso database file for each tenant or slot (feature `fleet`).
//!
//! # Contract
//!
//! [`TursoEngine`] is the Turso engine of an Autumn [`DatabaseFleet`]. Autumn core does the keys,
//! the paths, the opens, the eviction, the memory budget, the ownership and the fence.
//! The engine does the rest:
//!
//! - `open` opens the file with the engine configuration and applies the migrations.
//!   The table `autumn_fleet_identity` holds the key name and the epoch.
//!   One `BEGIN IMMEDIATE` transaction checks and writes them.
//! - `open` refuses a file of another key ([`FleetError::Misplaced`]) and a newer epoch
//!   ([`FleetError::NewerEpoch`]). It never lowers the stamp.
//! - `close` refuses new calls. The database closes when the last connection drops.
//!   `in_use` counts the helper calls (`execute`, `query`, ...). A connection from
//!   [`Turso::connect`] must not live longer than its lease.
//! - `snapshot` writes `VACUUM INTO` to `dir/<file name>`. A closed database opens again for the copy.
//! - `remove` deletes the file and its `-wal` file.
//! - The ship mode is [`ShipMode::Generations`].
//! - [`TursoFleetPlugin`] reads `[turso_fleet]`, makes the fleet and puts it in the app state.
//!   [`FleetTurso`] is the extractor. It opens the database of the request tenant.
//! - With `[turso_fleet.ownership]`, the plugin claims its slot ranges before the first open.
//! - The plugin closes idle databases, and closes all databases at shutdown. This runs in each role:
//!   a role that opens a database must also close it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use autumn_web::app::AppBuilder;
use autumn_web::config::DatabaseFleetConfig;
use autumn_web::db::fleet::{DatabaseFleet, FleetError};
use autumn_web::db::fleet_engine::{FleetEngine, ShipMode};
use autumn_web::db::fleet_ownership::{FleetOwnership, OWNERSHIP_INDICATOR, OwnerId, SlotRange};
use autumn_web::fleet::FleetLease;
use autumn_web::fleet_layout::FleetDbKey;
use autumn_web::lock::FencingToken;
use autumn_web::plugin::Plugin;
use autumn_web::reexports::tokio_util::sync::CancellationToken;
use autumn_web::{AppState, AutumnError};

use crate::config::{ConfigSources, FLEET_ENV_PREFIX, TursoConfig};
use crate::error::TursoError;
use crate::handle::{Turso, vacuum_into_sql};
use crate::migrate::Migration;

/// The plugin name of [`TursoFleetPlugin`] in Autumn diagnostics.
pub const FLEET_PLUGIN_NAME: &str = "autumn-plugin-turso-fleet";

/// The default section of [`TursoFleetConfig`].
pub const DEFAULT_FLEET_SECTION: &str = "turso_fleet";

/// The default interval between two shipped generations.
pub const DEFAULT_SHIP_EVERY: Duration = Duration::from_mins(1);

/// The default memory that the fleet counts for one open database: 16 MiB.
pub const DEFAULT_RESIDENT_BYTES: u64 = 16 * 1024 * 1024;

/// The table of the key name and the epoch.
pub const IDENTITY_TABLE: &str = "autumn_fleet_identity";

/// The Turso engine of an Autumn fleet. See the module contract.
pub struct TursoEngine {
    template: TursoConfig,
    migrations: Vec<Migration>,
    applied: AtomicU64,
    ship_every: Duration,
    resident_bytes: u64,
}

impl TursoEngine {
    /// Makes an engine that opens each file with `template`. The fleet sets the path.
    #[must_use]
    pub const fn new(template: TursoConfig) -> Self {
        Self {
            template,
            migrations: Vec::new(),
            applied: AtomicU64::new(0),
            ship_every: DEFAULT_SHIP_EVERY,
            resident_bytes: DEFAULT_RESIDENT_BYTES,
        }
    }

    /// Sets the interval between two shipped generations.
    #[must_use]
    pub const fn ship_every(mut self, every: Duration) -> Self {
        self.ship_every = every;
        self
    }

    /// Sets the memory that the fleet counts for one open database.
    #[must_use]
    pub const fn resident_bytes(mut self, bytes: u64) -> Self {
        self.resident_bytes = bytes;
        self
    }

    /// Adds migrations. Each open applies the pending ones, in order.
    #[must_use]
    pub fn migrations(mut self, migrations: impl IntoIterator<Item = Migration>) -> Self {
        self.migrations.extend(migrations);
        self
    }

    fn config_for(&self, path: &Path) -> TursoConfig {
        let mut config = self.template.clone();
        config.path = path.display().to_string();
        config.enabled = true;
        config.read_only = false;
        config.sync = None;
        config
    }
}

impl std::fmt::Debug for TursoEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TursoEngine")
            .field("migrations", &self.migrations.len())
            .field("ship_every", &self.ship_every)
            .field("resident_bytes", &self.resident_bytes)
            .finish_non_exhaustive()
    }
}

fn engine_error(op: &'static str) -> impl FnOnce(TursoError) -> FleetError {
    move |err| FleetError::Io {
        op,
        detail: err.to_string(),
    }
}

fn io_error(op: &'static str) -> impl FnOnce(std::io::Error) -> FleetError {
    move |err| FleetError::Io {
        op,
        detail: err.to_string(),
    }
}

/// The `-wal` file of `path`.
fn wal_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// Reads the stamp: the key name and the epoch.
async fn read_identity(conn: &turso::Connection) -> Result<Option<(String, u64)>, TursoError> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [IDENTITY_TABLE],
        )
        .await?;
    if rows.next().await?.is_none() {
        return Ok(None);
    }
    drop(rows);
    let mut rows = conn
        .query(&format!("SELECT name, epoch FROM {IDENTITY_TABLE}"), ())
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let name: String = row.get(0)?;
    let epoch: i64 = row.get(1)?;
    Ok(Some((name, u64::try_from(epoch).unwrap_or(0))))
}

/// Checks and writes the key and the epoch in one transaction.
async fn stamp(db: &Turso, name: &str, epoch: u64) -> Result<Result<(), FleetError>, TursoError> {
    let conn = db.connect().await?;
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE; CREATE TABLE IF NOT EXISTS {IDENTITY_TABLE} \
         (name TEXT NOT NULL, epoch INTEGER NOT NULL);"
    ))
    .await?;
    let decided = decide(&conn, name, epoch).await;
    let end = if matches!(decided, Ok(Ok(()))) {
        "COMMIT"
    } else {
        "ROLLBACK"
    };
    conn.execute_batch(end).await?;
    decided
}

async fn decide(
    conn: &turso::Connection,
    name: &str,
    epoch: u64,
) -> Result<Result<(), FleetError>, TursoError> {
    let stored = i64::try_from(epoch).unwrap_or(i64::MAX);
    let Some((found, stamp)) = read_identity(conn).await? else {
        conn.execute(
            &format!("INSERT INTO {IDENTITY_TABLE} (name, epoch) VALUES (?1, ?2)"),
            (name, stored),
        )
        .await?;
        return Ok(Ok(()));
    };
    if found != name {
        return Ok(Err(FleetError::Misplaced {
            name: name.to_owned(),
            found,
        }));
    }
    if epoch > 0 && stamp > epoch {
        return Ok(Err(FleetError::NewerEpoch {
            name: name.to_owned(),
            epoch,
            found: stamp,
        }));
    }
    if epoch > stamp {
        conn.execute(&format!("UPDATE {IDENTITY_TABLE} SET epoch = ?1"), [stored])
            .await?;
    }
    Ok(Ok(()))
}

impl FleetEngine for TursoEngine {
    type Handle = Turso;

    async fn open(
        &self,
        key: &FleetDbKey,
        path: &Path,
        epoch: Option<FencingToken>,
    ) -> Result<Turso, FleetError> {
        let db = Turso::open(&self.config_for(path))
            .await
            .map_err(engine_error("open database"))?;
        let name = key.name();
        let epoch = epoch.map_or(0, FencingToken::get);
        let stamped = match stamp(&db, &name, epoch).await {
            Ok(Ok(())) => db
                .migrate(&self.migrations)
                .await
                .map(|applied| {
                    self.applied.fetch_add(applied as u64, Ordering::Relaxed);
                })
                .map_err(engine_error("migrate database")),
            Ok(Err(err)) => Err(err),
            Err(err) => Err(engine_error("stamp database")(err)),
        };
        if let Err(err) = stamped {
            db.close_now();
            return Err(err);
        }
        Ok(db)
    }

    fn close(&self, handle: &Turso) {
        handle.close_now();
    }

    fn in_use(&self, handle: &Turso) -> bool {
        handle.is_busy()
    }

    async fn snapshot(
        &self,
        handle: &Turso,
        path: &Path,
        dir: &Path,
    ) -> Result<Vec<PathBuf>, FleetError> {
        let file = path.file_name().ok_or_else(|| FleetError::Io {
            op: "snapshot database",
            detail: "the path has no file name".to_owned(),
        })?;
        let copy = dir.join(file);
        if handle.is_closed() {
            let db = turso::Builder::new_local(&path.display().to_string())
                .build()
                .await
                .map_err(|err| engine_error("snapshot database")(err.into()))?;
            let conn = db
                .connect()
                .map_err(|err| engine_error("snapshot database")(err.into()))?;
            conn.execute(&vacuum_into_sql(&copy), ())
                .await
                .map_err(|err| engine_error("snapshot database")(err.into()))?;
        } else {
            handle
                .vacuum_into(&copy)
                .await
                .map_err(engine_error("snapshot database"))?;
        }
        // VACUUM INTO leaves an empty WAL beside the copy. The data is in the
        // main file, so the snapshot holds the main file only.
        let wal = wal_of(&copy);
        if std::fs::metadata(&wal).is_ok_and(|meta| meta.len() == 0) {
            std::fs::remove_file(&wal).map_err(io_error("snapshot database"))?;
        }
        Ok(vec![copy])
    }

    fn remove(&self, _key: &FleetDbKey, path: &Path) -> Result<bool, FleetError> {
        let removed = match std::fs::remove_file(path) {
            Ok(()) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => return Err(io_error("delete database")(err)),
        };
        match std::fs::remove_file(wal_of(path)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_error("delete database")(err)),
        }
        Ok(removed)
    }

    fn resident_bytes(&self, _handle: &Turso) -> u64 {
        self.resident_bytes
    }

    fn ship_mode(&self) -> ShipMode {
        ShipMode::Generations {
            every: self.ship_every,
        }
    }

    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn sidecars(&self, path: &Path) -> Vec<PathBuf> {
        vec![wal_of(path)]
    }

    fn migrations_applied(&self) -> u64 {
        self.applied.load(Ordering::Relaxed)
    }

    fn epoch_stamp(&self, _key: &FleetDbKey, path: &Path) -> Result<Option<u64>, FleetError> {
        if !path.is_file() {
            return Ok(None);
        }
        let path = path.display().to_string();
        let read = async {
            let db = turso::Builder::new_local(&path).build().await?;
            let conn = db.connect()?;
            read_identity(&conn).await
        };
        futures::executor::block_on(read)
            .map(|found| found.map(|(_, epoch)| epoch))
            .map_err(engine_error("read epoch stamp"))
    }
}

/// The Turso database of the request tenant, from a [`TursoFleetPlugin`] fleet.
///
/// It derefs to [`Turso`]. The fleet keeps the database open while the value lives.
///
/// The rejection is the fleet error status: `400` with no tenant, `421` when another host owns the
/// slot, and `503` with no fleet.
#[derive(Debug, Clone)]
pub struct FleetTurso(pub FleetLease<TursoEngine>);

impl FleetTurso {
    /// Opens the database of `key` with no request, for example in a job or a task.
    ///
    /// # Errors
    ///
    /// Returns the [`FleetError`] of [`autumn_web::fleet::resolve_key`].
    pub async fn for_key(state: &AppState, key: &str) -> Result<Self, FleetError> {
        autumn_web::fleet::resolve_key::<TursoEngine>(state, key)
            .await
            .map(Self)
    }
}

impl std::ops::Deref for FleetTurso {
    type Target = Turso;

    fn deref(&self) -> &Turso {
        self.0.handle()
    }
}

impl axum::extract::FromRequestParts<AppState> for FleetTurso {
    type Rejection = AutumnError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        autumn_web::fleet::resolve::<TursoEngine>(parts, state)
            .await
            .map(Self)
            .map_err(AutumnError::from)
    }
}

/// The configuration of a Turso fleet.
///
/// ```toml
/// [turso_fleet]
/// mode = "tenant"
/// root = "data/tenants"
/// max_resident_bytes = 1073741824
/// ship_every_secs = 60
/// resident_bytes = 16777216
///
/// [turso_fleet.engine]
/// busy_timeout_ms = 2000
///
/// [turso_fleet.ownership]
/// owner = "host-a"
/// ```
///
/// The top keys are the Autumn fleet keys (`[database.fleet]`). `engine` has the `[turso]` keys,
/// except `path`, `read_only` and `sync`. The variables `AUTUMN_TURSO_FLEET__ROOT` and
/// `AUTUMN_TURSO_FLEET__OWNERSHIP__OWNER` set `root` and `ownership.owner`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TursoFleetConfig {
    /// The Autumn fleet keys.
    pub fleet: DatabaseFleetConfig,
    /// The Turso configuration of each database. The fleet sets the path.
    pub engine: TursoConfig,
    /// The interval between two shipped generations.
    pub ship_every: Duration,
    /// The memory that the fleet counts for one open database.
    pub resident_bytes: u64,
}

impl TursoFleetConfig {
    /// Makes a configuration with `fleet`, the default engine and the default limits.
    #[must_use]
    pub fn new(fleet: DatabaseFleetConfig) -> Self {
        Self {
            fleet,
            engine: TursoConfig::default(),
            ship_every: DEFAULT_SHIP_EVERY,
            resident_bytes: DEFAULT_RESIDENT_BYTES,
        }
    }

    /// Reads `[section]` from `sources`.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Config`] if a file is not valid TOML or a value is not valid.
    pub fn resolve(section: &str, sources: &ConfigSources) -> Result<Self, TursoError> {
        let mut table = sources.section_table(section)?;
        if let Some(root) = sources.var(&format!("{FLEET_ENV_PREFIX}ROOT")) {
            table.insert("root".to_owned(), toml::Value::String(root.to_owned()));
        }
        if let Some(owner) = sources.var(&format!("{FLEET_ENV_PREFIX}OWNERSHIP__OWNER")) {
            let ownership = table
                .entry("ownership")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(ownership) = ownership {
                ownership.insert("owner".to_owned(), toml::Value::String(owner.to_owned()));
            }
        }
        Self::from_table(section, table)
    }

    /// Reads a configuration from the TOML table of `[section]`.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Config`] for an unknown key or a value that is not valid.
    pub fn from_table(section: &str, mut table: toml::Table) -> Result<Self, TursoError> {
        let bad = |key: &str, err: &dyn std::fmt::Display| {
            TursoError::Config(format!("[{section}] {key}: {err}"))
        };
        let engine = match table.remove("engine") {
            None => TursoConfig::default(),
            Some(toml::Value::Table(engine)) => {
                for key in ["path", "read_only", "sync"] {
                    if engine.contains_key(key) {
                        return Err(TursoError::Config(format!(
                            "`{section}.engine.{key}` must not be set: the fleet sets it"
                        )));
                    }
                }
                TursoConfig::default().with_table(engine, &format!("{section}.engine"))?
            }
            Some(_) => {
                return Err(TursoError::Config(format!(
                    "`{section}.engine` must be a table"
                )));
            }
        };
        let ship_every = match table.remove("ship_every_secs") {
            None => DEFAULT_SHIP_EVERY,
            Some(value) => Duration::from_secs(
                value
                    .as_integer()
                    .and_then(|v| u64::try_from(v).ok())
                    .ok_or_else(|| bad("ship_every_secs", &"must be a whole number of seconds"))?,
            ),
        };
        let resident_bytes = match table.remove("resident_bytes") {
            None => DEFAULT_RESIDENT_BYTES,
            Some(value) => value
                .as_integer()
                .and_then(|v| u64::try_from(v).ok())
                .ok_or_else(|| bad("resident_bytes", &"must be a whole number"))?,
        };
        let fleet: DatabaseFleetConfig = toml::Value::Table(table)
            .try_into()
            .map_err(|err| bad("fleet", &err))?;
        let config = Self {
            fleet,
            engine,
            ship_every,
            resident_bytes,
        };
        config.validate_section(section)?;
        Ok(config)
    }

    /// Checks each value.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Config`] that names the first key that is not valid.
    pub fn validate(&self) -> Result<(), TursoError> {
        self.validate_section(DEFAULT_FLEET_SECTION)
    }

    fn validate_section(&self, section: &str) -> Result<(), TursoError> {
        let fail =
            |key: &str, rule: &str| Err(TursoError::Config(format!("`{section}.{key}` {rule}")));
        if self.fleet.root.trim().is_empty() {
            return fail("root", "must be a directory");
        }
        if self.resident_bytes == 0 {
            return fail("resident_bytes", "must be 1 or more");
        }
        if self.ship_every.is_zero() {
            return fail("ship_every_secs", "must be 1 or more");
        }
        if self.engine.read_only || self.engine.active_sync().is_some() {
            return fail(
                "engine",
                "must not be read-only or synced: the fleet writes a stamp and ships the files",
            );
        }
        Ok(())
    }

    /// Makes the engine of this configuration.
    #[must_use]
    pub fn engine(&self) -> TursoEngine {
        TursoEngine::new(self.engine.clone())
            .ship_every(self.ship_every)
            .resident_bytes(self.resident_bytes)
    }
}

enum FleetSource {
    Section(String),
    Explicit(Box<TursoFleetConfig>),
}

/// The running parts of a fleet: the idle sweeper and the ownership loop.
struct FleetRuntime {
    fleet: DatabaseFleet<TursoEngine>,
    stop: CancellationToken,
    maintenance: tokio::task::JoinHandle<()>,
    ownership: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
}

impl FleetRuntime {
    async fn shutdown(self) {
        self.stop.cancel();
        let _ = self.maintenance.await;
        if let Some((stop, handle)) = self.ownership {
            stop.cancel();
            let _ = handle.await;
            if self.fleet.release_ownership().await {
                tracing::info!("the Turso fleet released its slot ranges");
            }
        }
    }
}

/// Makes a fleet of Turso databases and puts it in the app state.
///
/// ```rust,no_run
/// use autumn_plugin_turso::fleet::{FleetTurso, TursoFleetPlugin};
/// use autumn_plugin_turso::Migration;
///
/// #[autumn_web::get("/notes")]
/// async fn notes(db: FleetTurso) -> autumn_web::AutumnResult<String> {
///     let rows = db.query("SELECT count(*) FROM notes", ()).await?;
///     Ok(rows.len().to_string())
/// }
///
/// # async fn run() {
/// autumn_web::app()
///     .plugin(TursoFleetPlugin::new().migrations([Migration::new(
///         "0001_notes",
///         "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
///     )]))
///     .routes(autumn_web::routes![notes])
///     .run()
///     .await;
/// # }
/// ```
#[must_use]
pub struct TursoFleetPlugin {
    source: FleetSource,
    sources: Option<ConfigSources>,
    migrations: Vec<Migration>,
}

impl Default for TursoFleetPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TursoFleetPlugin {
    /// Makes a plugin that reads `[turso_fleet]`.
    pub fn new() -> Self {
        Self {
            source: FleetSource::Section(DEFAULT_FLEET_SECTION.to_owned()),
            sources: None,
            migrations: Vec::new(),
        }
    }

    /// Reads `[section]` instead of `[turso_fleet]`.
    pub fn config_section(mut self, section: impl Into<String>) -> Self {
        self.source = FleetSource::Section(section.into());
        self
    }

    /// Uses `config` and reads no files or variables.
    pub fn config(mut self, config: TursoFleetConfig) -> Self {
        self.source = FleetSource::Explicit(Box::new(config));
        self
    }

    /// Reads config from `sources` instead of the process. Use it in tests.
    pub fn config_sources(mut self, sources: ConfigSources) -> Self {
        self.sources = Some(sources);
        self
    }

    /// Adds migrations. Each database open applies the pending ones, in order.
    pub fn migrations(mut self, migrations: impl IntoIterator<Item = Migration>) -> Self {
        self.migrations.extend(migrations);
        self
    }

    fn resolve(
        source: &FleetSource,
        sources: Option<&ConfigSources>,
        profile: &str,
    ) -> Result<TursoFleetConfig, TursoError> {
        match source {
            FleetSource::Section(section) => sources.map_or_else(
                || TursoFleetConfig::resolve(section, &ConfigSources::from_process(profile)),
                |sources| TursoFleetConfig::resolve(section, sources),
            ),
            FleetSource::Explicit(config) => {
                config.validate()?;
                Ok((**config).clone())
            }
        }
    }
}

impl std::fmt::Debug for TursoFleetPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match &self.source {
            FleetSource::Section(section) => section.as_str(),
            FleetSource::Explicit(_) => "(explicit)",
        };
        f.debug_struct("TursoFleetPlugin")
            .field("config", &source)
            .field("migrations", &self.migrations.len())
            .finish_non_exhaustive()
    }
}

/// Starts the fleet: ownership first, then the idle sweeper.
async fn start(
    state: &AppState,
    config: &TursoFleetConfig,
    engine: TursoEngine,
) -> Result<FleetRuntime, String> {
    let fleet = DatabaseFleet::with_engine(&config.fleet, engine).map_err(|e| e.to_string())?;
    let ownership = match &config.fleet.ownership {
        None => None,
        Some(settings) => {
            let store = autumn_web::fleet::ownership_store(state)
                .ok_or("fleet ownership needs a control database or a fleet_ownership_store")?;
            store.ensure_schema().await.map_err(|e| e.to_string())?;
            let ranges = settings
                .ranges()?
                .into_iter()
                .map(|(start, end)| SlotRange::new(start, end))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            let owner = OwnerId::new(settings.owner.as_str()).map_err(|e| e.to_string())?;
            let ownership = Arc::new(
                FleetOwnership::new(
                    store,
                    owner,
                    ranges,
                    Duration::from_secs(settings.lease_secs),
                )
                .map_err(|e| e.to_string())?,
            );
            fleet
                .start_ownership(Arc::clone(&ownership))
                .await
                .map_err(|e| e.to_string())?;
            if let Err(err) = state.health_indicator_registry().register(
                OWNERSHIP_INDICATOR,
                autumn_web::actuator::IndicatorGroup::HealthOnly,
                ownership.indicator(),
            ) {
                tracing::warn!("{err}");
            }
            let stop = CancellationToken::new();
            let handle = fleet.spawn_ownership(stop.clone());
            Some((stop, handle))
        }
    };
    let stop = CancellationToken::new();
    let maintenance = fleet.spawn_maintenance(stop.clone());
    state.insert_extension(fleet.clone());
    Ok(FleetRuntime {
        fleet,
        stop,
        maintenance,
        ownership,
    })
}

impl Plugin for TursoFleetPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(FLEET_PLUGIN_NAME)
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let Self {
            source,
            sources,
            migrations,
        } = self;
        let source = Arc::new(source);
        let runtime: Arc<Mutex<Option<FleetRuntime>>> = Arc::default();
        let on_start = Arc::clone(&runtime);
        app.on_startup(move |state| {
            let source = Arc::clone(&source);
            let sources = sources.clone();
            let runtime = Arc::clone(&on_start);
            let migrations = migrations.clone();
            async move {
                let fail = |err: &dyn std::fmt::Display| {
                    AutumnError::internal_server_error_msg(format!("{FLEET_PLUGIN_NAME}: {err}"))
                };
                let config = Self::resolve(&source, sources.as_ref(), state.profile())
                    .map_err(|err| fail(&err))?;
                let engine = config.engine().migrations(migrations);
                let started = start(&state, &config, engine)
                    .await
                    .map_err(|err| fail(&err))?;
                *runtime.lock().unwrap_or_else(PoisonError::into_inner) = Some(started);
                tracing::info!("the Turso fleet is ready");
                Ok(())
            }
        })
        .on_shutdown(move || {
            let runtime = runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            async move {
                if let Some(runtime) = runtime {
                    runtime.shutdown().await;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
