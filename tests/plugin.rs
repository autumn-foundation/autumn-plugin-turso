//! AC1, AC3, AC5, AC7, AC8: the plugin inside an Autumn app.

use autumn_plugin_turso::{ConfigSources, Migration, Turso, TursoPlugin};
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
fn contract_declares_autumn_0_8() {
    let contract = TursoPlugin::new().contract().expect("contract");
    assert_eq!(contract.plugin, "autumn-plugin-turso");
    assert_eq!(contract.autumn_web.as_deref(), Some("0.8"));
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
    let client = TestApp::new().plugin(memory_plugin(&dir)).build();
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
    let text = client.get("/actuator/prometheus").send().await.text();
    for family in [
        "turso_up 1",
        "turso_statements_total",
        "turso_errors_total",
        "turso_connections_opened_total",
        "turso_migrations_applied_total 1",
        "turso_sync_pushes_total",
        "turso_sync_pulls_total",
        "turso_sync_errors_total",
    ] {
        assert!(text.contains(family), "missing {family} in:\n{text}");
    }
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
        assert!(db.query("SELECT * FROM notes", ()).await.is_err());
    });
}
