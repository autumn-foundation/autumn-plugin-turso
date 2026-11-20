#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use autumn_web::config::{DatabaseConfig, DatabaseFleetConfig};
use autumn_web::db::fleet_replication::FleetReplication;
use autumn_web::db::fleet_tombstones::FleetTombstones;
use autumn_web::db::{RuntimeConnection, create_pool};
use autumn_web::plugin_conformance::CheckStatus;
use autumn_web::plugin_conformance::fleet::{FleetFixture, run_fleet_conformance};
use autumn_web::reexports::diesel_async::pooled_connection::deadpool::Pool;
use autumn_web::replication::{FileDestination, ReplicaDestination, ReplicationSettings};

use super::*;

/// The kit fixture. With `replicated`, generations go to a local directory.
struct Fixture {
    replicated: bool,
}

/// The fixture with no replica.
const PLAIN: Fixture = Fixture { replicated: false };

fn control_pool(dir: &Path) -> Pool<RuntimeConnection> {
    create_pool(&DatabaseConfig {
        url: Some(format!("sqlite://{}", dir.join("control.db").display())),
        ..Default::default()
    })
    .unwrap()
    .unwrap()
}

impl FleetFixture for Fixture {
    type Engine = TursoEngine;

    async fn fleet(
        &self,
        config: DatabaseFleetConfig,
    ) -> Result<DatabaseFleet<TursoEngine>, String> {
        let scratch = Path::new(&config.root)
            .parent()
            .ok_or("the root has no parent")?
            .to_path_buf();
        let engine = TursoEngine::new(TursoConfig::default())
            .resident_bytes(1024)
            .ship_every(Duration::from_hours(1))
            .migrations([Migration::new(
                "0001_kv",
                "CREATE TABLE kv (n INTEGER PRIMARY KEY, v TEXT NOT NULL);",
            )]);
        let fleet = DatabaseFleet::with_engine(&config, engine).map_err(|e| e.to_string())?;
        if self.replicated {
            let destination: Arc<dyn ReplicaDestination> = Arc::new(
                FileDestination::new(scratch.join("replicas")).map_err(|e| e.to_string())?,
            );
            let clock = Arc::new(autumn_web::time::SystemClock);
            let tombstones = Arc::new(FleetTombstones::new(
                control_pool(&scratch),
                Arc::clone(&clock) as _,
            ));
            let replication = Arc::new(
                FleetReplication::new(
                    destination,
                    ReplicationSettings {
                        database_path: PathBuf::new(),
                        root: "app/kit".to_owned(),
                        sync_interval: Duration::from_secs(1),
                        snapshot_interval: Duration::from_hours(1),
                        max_wal_bytes: 4 * 1024 * 1024,
                        retention: Duration::from_hours(24),
                        verify_interval: None,
                    },
                    Duration::from_secs(30),
                    false,
                    clock,
                )
                .with_tombstones(tombstones),
            );
            fleet
                .install_replication(replication)
                .map_err(|_| "replication is installed twice".to_owned())?;
        }
        Ok(fleet)
    }

    async fn restore(
        &self,
        fleet: &DatabaseFleet<TursoEngine>,
        key: &FleetDbKey,
    ) -> Option<Result<(), FleetError>> {
        if self.replicated {
            Some(fleet.restore_latest(key).await)
        } else {
            None
        }
    }

    async fn write(&self, handle: &Turso, value: &str) -> Result<(), String> {
        handle
            .execute("INSERT INTO kv (v) VALUES (?1)", [value])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn read(&self, handle: &Turso) -> Result<Vec<String>, String> {
        let rows = handle
            .query("SELECT v FROM kv ORDER BY n", ())
            .await
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| row.get::<String>(0).map_err(|e| e.to_string()))
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_passes_the_fleet_conformance_kit() {
    let scratch = tempfile::tempdir().unwrap();
    let report = run_fleet_conformance("turso", &PLAIN, scratch.path()).await;
    assert!(report.passed(), "{}", report.to_text_report());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replicated_engine_passes_every_check() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = Fixture { replicated: true };
    let report = run_fleet_conformance("turso", &fixture, scratch.path()).await;
    assert!(report.passed(), "{}", report.to_text_report());
    for check in &report.checks {
        assert_eq!(
            check.status,
            CheckStatus::Pass,
            "{}: {}",
            check.name,
            check.message
        );
    }
}

fn fleet_config(root: &Path) -> DatabaseFleetConfig {
    let mut table = toml::Table::new();
    table.insert("mode".to_owned(), "tenant".into());
    table.insert("root".to_owned(), root.display().to_string().into());
    table.insert("create_on_demand".to_owned(), true.into());
    toml::Value::Table(table).try_into().unwrap()
}

fn key(fleet: &DatabaseFleet<TursoEngine>, tenant: &str) -> FleetDbKey {
    fleet.key_for(tenant).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_database_snapshots_to_the_same_bytes() {
    let scratch = tempfile::tempdir().unwrap();
    let fleet = PLAIN
        .fleet(fleet_config(&scratch.path().join("fleet")))
        .await
        .unwrap();
    let key = key(&fleet, "acme");
    let db = fleet.open(&key).await.unwrap();
    PLAIN.write(db.handle(), "one").await.unwrap();
    let path = fleet.path_of(&key);
    let snap = |n: &str| {
        let dir = scratch.path().join(n);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let (a, b) = (snap("a"), snap("b"));
    let first = fleet
        .engine()
        .snapshot(db.handle(), &path, &a)
        .await
        .unwrap();
    let second = fleet
        .engine()
        .snapshot(db.handle(), &path, &b)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert!(!wal_of(&first[0]).exists(), "the snapshot leaves no WAL");
    assert_eq!(
        std::fs::read(&first[0]).unwrap(),
        std::fs::read(&second[0]).unwrap()
    );
    let c = snap("c");
    PLAIN.write(db.handle(), "two").await.unwrap();
    let third = fleet
        .engine()
        .snapshot(db.handle(), &path, &c)
        .await
        .unwrap();
    assert_ne!(
        std::fs::read(&first[0]).unwrap(),
        std::fs::read(&third[0]).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_database_snapshots_its_file() {
    let scratch = tempfile::tempdir().unwrap();
    let fleet = PLAIN
        .fleet(fleet_config(&scratch.path().join("fleet")))
        .await
        .unwrap();
    let key = key(&fleet, "acme");
    let db = fleet.open(&key).await.unwrap();
    PLAIN.write(db.handle(), "kept").await.unwrap();
    let handle = db.handle().clone();
    drop(db);
    fleet.close(&key).await;
    assert!(handle.is_closed());
    assert!(!fleet.engine().in_use(&handle));
    let dir = scratch.path().join("copy");
    std::fs::create_dir_all(&dir).unwrap();
    let files = fleet
        .engine()
        .snapshot(&handle, &fleet.path_of(&key), &dir)
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_epoch_stamp_is_the_stamp_of_the_open() {
    let scratch = tempfile::tempdir().unwrap();
    let engine = TursoEngine::new(TursoConfig::default());
    let fleet =
        DatabaseFleet::with_engine(&fleet_config(&scratch.path().join("f")), engine).unwrap();
    let key = key(&fleet, "acme");
    let path = fleet.path_of(&key);
    assert_eq!(fleet.engine().epoch_stamp(&key, &path).unwrap(), None);
    let token = FencingToken::try_from(7_i64).unwrap();
    let db = fleet.engine().open(&key, &path, Some(token)).await.unwrap();
    fleet.engine().close(&db);
    drop(db);
    assert_eq!(fleet.engine().epoch_stamp(&key, &path).unwrap(), Some(7));
    let older = FencingToken::try_from(3_i64).unwrap();
    let err = fleet
        .engine()
        .open(&key, &path, Some(older))
        .await
        .unwrap_err();
    assert!(
        matches!(err, FleetError::NewerEpoch { found: 7, .. }),
        "{err}"
    );
    assert!(fleet.engine().remove(&key, &path).unwrap());
    assert!(!fleet.engine().remove(&key, &path).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_with_no_identity_has_no_stamp() {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("plain.db");
    let plain = Turso::open(&TursoConfig::new(path.display().to_string()))
        .await
        .unwrap();
    plain
        .execute_batch("CREATE TABLE t (x INTEGER)")
        .await
        .unwrap();
    plain.close().await;
    let engine = TursoEngine::new(TursoConfig::default());
    let fleet = DatabaseFleet::with_engine(&fleet_config(scratch.path()), engine).unwrap();
    let key = key(&fleet, "acme");
    let stamp = tokio::task::spawn_blocking({
        let path = path.clone();
        move || TursoEngine::new(TursoConfig::default()).epoch_stamp(&key, &path)
    })
    .await
    .unwrap();
    assert_eq!(stamp.unwrap(), None);
    assert_eq!(fleet.engine().sidecars(&path), vec![wal_of(&path)]);
    assert!(fleet.engine().exists(&path));
    assert!(!fleet.engine().exists(scratch.path()));
}

#[test]
fn the_vacuum_statement_quotes_the_path() {
    assert_eq!(
        vacuum_into_sql(Path::new("/tmp/it's.db")),
        "VACUUM INTO '/tmp/it''s.db'"
    );
}

fn table(text: &str) -> toml::Table {
    toml::from_str(text).unwrap()
}

#[test]
fn the_config_reads_the_fleet_and_the_engine_keys() {
    let config = TursoFleetConfig::from_table(
        "turso_fleet",
        table(
            r#"
            mode = "tenant"
            root = "data/tenants"
            max_resident_bytes = 4096
            ship_every_secs = 5
            resident_bytes = 1024
            [engine]
            busy_timeout_ms = 1234
            [ownership]
            owner = "host-a"
            "#,
        ),
    )
    .unwrap();
    assert_eq!(config.fleet.root, "data/tenants");
    assert_eq!(config.fleet.max_resident_bytes, 4096);
    assert_eq!(config.ship_every, Duration::from_secs(5));
    assert_eq!(config.resident_bytes, 1024);
    assert_eq!(config.engine.busy_timeout_ms, 1234);
    assert_eq!(config.fleet.ownership.unwrap().owner, "host-a");
    let engine = TursoFleetConfig::new(fleet_config(Path::new("x"))).engine();
    assert!(format!("{engine:?}").contains("resident_bytes"));
    assert_eq!(engine.ship_mode().every(), Some(DEFAULT_SHIP_EVERY));
}

#[test]
fn the_config_refuses_bad_values() {
    let cases = [
        (
            "mode = \"tenant\"\nroot = \"r\"\n[engine]\npath = \"x\"",
            "engine.path",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\n[engine]\nread_only = true",
            "engine.read_only",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\nengine = 3",
            "engine` must be a table",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\n[engine]\nbogus = 1",
            "engine",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\nship_every_secs = \"x\"",
            "ship_every_secs",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\nship_every_secs = 0",
            "ship_every_secs",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\nresident_bytes = -1",
            "resident_bytes",
        ),
        (
            "mode = \"tenant\"\nroot = \"r\"\nresident_bytes = 0",
            "resident_bytes",
        ),
        ("mode = \"tenant\"\nroot = \" \"", "root"),
        ("root = \"r\"", "fleet"),
        (
            "mode = \"tenant\"\nroot = \"r\"\n[engine]\nbusy_timeout_ms = \"x\"",
            "engine",
        ),
    ];
    for (text, needle) in cases {
        let err = TursoFleetConfig::from_table("turso_fleet", table(text)).unwrap_err();
        assert!(err.to_string().contains(needle), "{text}: {err}");
    }
}

#[test]
fn variables_set_the_root_and_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("autumn.toml"),
        "[turso_fleet]\nmode = \"tenant\"\nroot = \"from-file\"\n",
    )
    .unwrap();
    let sources = ConfigSources::from_vars(
        "dev",
        vec![
            ("AUTUMN_TURSO_FLEET__ROOT".to_owned(), "from-env".to_owned()),
            (
                "AUTUMN_TURSO_FLEET__OWNERSHIP__OWNER".to_owned(),
                "host-b".to_owned(),
            ),
            (
                "AUTUMN_MANIFEST_DIR".to_owned(),
                dir.path().display().to_string(),
            ),
        ],
        Vec::new(),
        &[],
    );
    let config = TursoFleetConfig::resolve("turso_fleet", &sources).unwrap();
    assert_eq!(config.fleet.root, "from-env");
    assert_eq!(config.fleet.ownership.unwrap().owner, "host-b");
}

#[test]
fn the_plugin_resolves_an_explicit_config() {
    let good = TursoFleetConfig::new(fleet_config(Path::new("data")));
    let explicit = |config: &TursoFleetConfig| {
        TursoFleetPlugin::resolve(
            &FleetSource::Explicit(Box::new(config.clone())),
            None,
            "dev",
        )
    };
    assert!(explicit(&good).is_ok());
    let mut bad = good;
    bad.resident_bytes = 0;
    assert!(explicit(&bad).is_err());
    let plugin = TursoFleetPlugin::default()
        .config_section("tenants")
        .migrations([Migration::new("0001", "SELECT 1")]);
    let text = format!("{plugin:?}");
    assert!(
        text.contains("tenants") && text.contains("migrations: 1"),
        "{text}"
    );
    assert_eq!(plugin.name(), FLEET_PLUGIN_NAME);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_closes_every_database_at_shutdown() {
    let scratch = tempfile::tempdir().unwrap();
    let config = TursoFleetConfig::new(fleet_config(&scratch.path().join("fleet")));
    let state = AppState::for_test();
    let runtime = start(&state, &config, config.engine()).await.unwrap();
    let lease = FleetTurso::for_key(&state, "acme").await.unwrap();
    lease.query("SELECT 1", ()).await.unwrap();
    let handle = (*lease).clone();
    assert_eq!(runtime.fleet.open_keys().len(), 1);
    drop(lease);
    runtime.shutdown().await;
    assert!(handle.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn ownership_needs_a_store() {
    let scratch = tempfile::tempdir().unwrap();
    let mut table = toml::Table::new();
    table.insert("mode".to_owned(), "tenant".into());
    table.insert(
        "root".to_owned(),
        scratch.path().display().to_string().into(),
    );
    let mut ownership = toml::Table::new();
    ownership.insert("owner".to_owned(), "host-a".into());
    table.insert("ownership".to_owned(), ownership.into());
    let config = TursoFleetConfig::new(toml::Value::Table(table).try_into().unwrap());
    let state = AppState::for_test();
    let err = start(&state, &config, config.engine()).await.err().unwrap();
    assert!(err.contains("ownership"), "{err}");
}
