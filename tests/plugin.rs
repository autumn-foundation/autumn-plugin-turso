//! AC1, AC3, AC5, AC7, AC8: the plugin inside an Autumn app.

use autumn_plugin_turso::{ConfigSources, Migration, SyncConfig, Turso, TursoError, TursoPlugin};
use autumn_web::plugin::Plugin;
use autumn_web::prelude::*;
use autumn_web::test::TestApp;

const NOTES: Migration = Migration::new("0001_notes", "CREATE TABLE notes (body TEXT NOT NULL);");

#[get("/notes/count")]
async fn count_notes(db: Turso) -> AutumnResult<String> {
    let rows = db.query("SELECT count(*) FROM notes", ()).await?;
    let count: i64 = rows[0].get(0)?;
    Ok(count.to_string())
}

#[get("/notes/add")]
async fn add_note(db: Turso) -> AutumnResult<&'static str> {
    db.execute("INSERT INTO notes (body) VALUES ('hi')", ())
        .await?;
    Ok("ok")
}

fn isolated(dir: &tempfile::TempDir) -> ConfigSources {
    ConfigSources::new(dir.path(), "dev")
}

fn memory_plugin(dir: &tempfile::TempDir) -> TursoPlugin {
    TursoPlugin::new()
        .path(":memory:")
        .config_sources(isolated(dir))
        .migration(NOTES)
}

#[test]
fn contract_declares_autumn_0_9() {
    let contract = TursoPlugin::new().contract().expect("contract");
    assert_eq!(contract.plugin, "autumn-plugin-turso");
    assert_eq!(contract.autumn_web.as_deref(), Some("0.9"));
}

#[test]
fn name_is_the_crate_name() {
    assert_eq!(TursoPlugin::new().name(), "autumn-plugin-turso");
}

#[test]
fn build_declares_config_section_and_registers_once() {
    let app = autumn_web::app().plugin(TursoPlugin::new());
    assert!(app.has_config_section("turso"));
    assert!(app.has_plugin(TursoPlugin::new().name().as_ref()));
}

#[tokio::test(flavor = "multi_thread")]
async fn handle_is_in_state_and_migrations_ran() {
    let dir = tempfile::tempdir().expect("dir");
    let client = TestApp::new().plugin(memory_plugin(&dir)).build();
    let db = client
        .state()
        .extension::<Turso>()
        .expect("handle in state");
    db.execute("INSERT INTO notes (body) VALUES ('x')", ())
        .await
        .expect("migrated table exists");
}

#[tokio::test(flavor = "multi_thread")]
async fn extractor_reaches_the_database() {
    let dir = tempfile::tempdir().expect("dir");
    let client = TestApp::new()
        .plugin(memory_plugin(&dir))
        .routes(routes![count_notes, add_note])
        .build();
    client.get("/notes/add").send().await.assert_ok();
    client.get("/notes/add").send().await.assert_ok();
    client
        .get("/notes/count")
        .send()
        .await
        .assert_ok()
        .assert_body_eq("2");
}

#[tokio::test(flavor = "multi_thread")]
async fn extractor_without_plugin_is_a_500() {
    let client = TestApp::new().routes(routes![count_notes]).build();
    let response = client.get("/notes/count").send().await;
    response.assert_status(500);
}

#[tokio::test(flavor = "multi_thread")]
async fn health_is_up_then_down_after_close() {
    let dir = tempfile::tempdir().expect("dir");
    // Autumn 0.9 caches health results for 1 s. The test reads twice.
    let mut config = autumn_web::config::AutumnConfig::default();
    config.health.cache_ttl_ms = 0;
    let client = TestApp::new()
        .config(config)
        .plugin(memory_plugin(&dir))
        .build();
    let body: serde_json::Value = client.get("/actuator/health").send().await.json();
    assert_eq!(body["components"]["turso"]["status"], "UP", "{body}");

    client
        .state()
        .extension::<Turso>()
        .expect("handle")
        .close()
        .await;
    let body: serde_json::Value = client.get("/actuator/health").send().await.json();
    assert_eq!(body["components"]["turso"]["status"], "DOWN", "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_are_exported() {
    let dir = tempfile::tempdir().expect("dir");
    let client = TestApp::new()
        .plugin(memory_plugin(&dir))
        .routes(routes![add_note])
        .build();
    client.get("/notes/add").send().await.assert_ok();
    // A health probe must not change the statement count.
    client.get("/actuator/health").send().await;
    let text = client.get("/actuator/prometheus").send().await.text();
    let lines: Vec<&str> = text.lines().collect();
    for sample in [
        "turso_up 1",
        "turso_statements_total 1",
        "turso_errors_total 0",
        "turso_migrations_applied_total 1",
        "turso_sync_pushes_total 0",
        "turso_sync_pulls_total 0",
        "turso_sync_errors_total 0",
    ] {
        assert!(lines.contains(&sample), "missing `{sample}` in:\n{text}");
    }
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("turso_connections_opened_total ")),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_plugin_opens_nothing_and_stays_healthy() {
    let dir = tempfile::tempdir().expect("dir");
    let client = TestApp::new()
        .plugin(memory_plugin(&dir).enabled(false))
        .build();
    assert!(client.state().extension::<Turso>().is_none());
    let body: serde_json::Value = client.get("/actuator/health").send().await.json();
    assert_eq!(body["components"]["turso"]["status"], "UP", "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn file_config_overrides_builder() {
    let dir = tempfile::tempdir().expect("dir");
    let db_path = dir.path().join("from-toml.db");
    std::fs::write(
        dir.path().join("autumn.toml"),
        format!("[turso]\npath = {:?}\n", db_path.to_string_lossy()),
    )
    .expect("write toml");
    let client = TestApp::new().plugin(memory_plugin(&dir)).build();
    let db = client.state().extension::<Turso>().expect("handle");
    assert_eq!(db.path(), db_path.to_string_lossy());
    assert!(db_path.exists());
}

#[test]
fn bad_config_fails_startup() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("autumn.toml"), "[turso]\nbogus = 1\n").expect("write");
    let result = std::panic::catch_unwind(|| {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(async { TestApp::new().plugin(memory_plugin(&dir)).build() });
    });
    let payload = result.expect_err("startup must fail on an unknown key");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("bogus"), "{message}");
}

#[test]
fn migrate_on_start_false_leaves_schema_alone() {
    let dir = tempfile::tempdir().expect("dir");
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let client = TestApp::new()
            .plugin(memory_plugin(&dir).migrate_on_start(false))
            .build();
        let db = client.state().extension::<Turso>().expect("handle");
        assert_eq!(db.pending(&[NOTES]).await.expect("pending"), ["0001_notes"]);
        let err = db
            .query("SELECT * FROM notes", ())
            .await
            .expect_err("no table");
        assert!(matches!(err, TursoError::Database(_)), "{err:?}");
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn extractor_after_close_is_a_500() {
    let dir = tempfile::tempdir().expect("dir");
    let client = TestApp::new()
        .plugin(memory_plugin(&dir))
        .routes(routes![count_notes])
        .build();
    client
        .state()
        .extension::<Turso>()
        .expect("handle")
        .close()
        .await;
    client.get("/notes/count").send().await.assert_status(500);
}

#[tokio::test(flavor = "multi_thread")]
async fn extractor_when_disabled_is_a_500_and_metrics_show_down() {
    let dir = tempfile::tempdir().expect("dir");
    let client = TestApp::new()
        .plugin(memory_plugin(&dir).enabled(false))
        .routes(routes![count_notes])
        .build();
    client.get("/notes/count").send().await.assert_status(500);
    let text = client.get("/actuator/prometheus").send().await.text();
    assert!(text.lines().any(|line| line == "turso_up 0"), "{text}");
}

#[test]
fn read_only_with_pending_migrations_fails_startup() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("ro.db");
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let db = Turso::open(&autumn_plugin_turso::TursoConfig::new(
            path.to_string_lossy(),
        ))
        .await
        .expect("create file");
        db.close().await;
    });
    let plugin = TursoPlugin::new()
        .path(path.to_string_lossy())
        .read_only(true)
        .config_sources(isolated(&dir))
        .migration(NOTES);
    let result = std::panic::catch_unwind(|| {
        rt.block_on(async { TestApp::new().plugin(plugin).build() });
    });
    let payload = result.expect_err("startup must fail");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("0001_notes"), "{message}");
}

#[test]
fn plugin_debug_hides_the_token() {
    let plugin = TursoPlugin::new()
        .sync(SyncConfig::new("libsql://db.example.turso.io").auth_token("plugin-secret"));
    let text = format!("{plugin:?}");
    assert!(!text.contains("plugin-secret"), "{text}");
}
