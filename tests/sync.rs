//! AC10: Turso Cloud sync (feature `sync`).
#![cfg(feature = "sync")]

use autumn_plugin_turso::turso::Value;
use autumn_plugin_turso::{SyncConfig, Turso, TursoConfig, TursoError};

#[path = "support/sync_server.rs"]
mod sync_server;

use sync_server::SyncServer;

fn synced(dir: &tempfile::TempDir, url: &str, token: Option<String>) -> TursoConfig {
    let mut sync = SyncConfig::new(url);
    sync.auth_token = token;
    TursoConfig::new(dir.path().join("synced.db").to_string_lossy()).sync(sync)
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_remote_fails_open() {
    let dir = tempfile::tempdir().expect("dir");
    // Port 9 (discard) on loopback refuses connections.
    let err = Turso::open(&synced(&dir, "http://127.0.0.1:9", None))
        .await
        .expect_err("no server");
    assert!(matches!(err, TursoError::Sync(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn push_and_pull_need_sync() {
    let db = Turso::open(&TursoConfig::new(":memory:"))
        .await
        .expect("open");
    assert!(matches!(
        db.push().await,
        Err(TursoError::SyncNotConfigured)
    ));
    assert!(matches!(
        db.pull().await,
        Err(TursoError::SyncNotConfigured)
    ));
}

/// Live test. Set `TURSO_TEST_URL` and `TURSO_TEST_TOKEN` to run it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a Turso Cloud database (TURSO_TEST_URL, TURSO_TEST_TOKEN)"]
async fn live_round_trip() {
    let url = std::env::var("TURSO_TEST_URL").expect("TURSO_TEST_URL");
    let token = std::env::var("TURSO_TEST_TOKEN").ok();
    let dir = tempfile::tempdir().expect("dir");
    let db = Turso::open(&synced(&dir, &url, token)).await.expect("open");
    assert!(db.is_synced());
    db.execute_batch("CREATE TABLE IF NOT EXISTS autumn_turso_probe (x INTEGER);")
        .await
        .expect("create");
    db.execute("INSERT INTO autumn_turso_probe VALUES (1)", ())
        .await
        .expect("insert");
    db.push().await.expect("push");
    db.pull().await.expect("pull");
    assert!(db.stats().sync_pushes >= 1);
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
async fn local_server_push_then_pull_on_start() {
    let server = SyncServer::start();
    let writer_dir = tempfile::tempdir().expect("dir");
    let writer = Turso::open(&synced(&writer_dir, server.url(), None))
        .await
        .expect("open writer");
    assert!(writer.is_synced());
    writer
        .execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('a');")
        .await
        .expect("write");
    writer.push().await.expect("push");
    assert_eq!(writer.stats().sync_pushes, 1);
    // `pull_on_start` ran once in `open`.
    assert_eq!(writer.stats().sync_pulls, 1);

    let reader_dir = tempfile::tempdir().expect("dir");
    let reader = Turso::open(&synced(&reader_dir, server.url(), None))
        .await
        .expect("open reader");
    assert_eq!(count(&reader).await, 1);
    let rows = reader.query("SELECT body FROM notes", ()).await.expect("q");
    assert_eq!(rows[0].get_value(0).expect("v"), Value::Text("a".into()));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires tursodb (TURSODB_BIN)"]
async fn local_server_pull_reports_changes() {
    let server = SyncServer::start();
    let a_dir = tempfile::tempdir().expect("dir");
    let b_dir = tempfile::tempdir().expect("dir");
    let a = Turso::open(&synced(&a_dir, server.url(), None))
        .await
        .expect("a");
    a.execute_batch("CREATE TABLE notes (body TEXT);")
        .await
        .expect("create");
    a.push().await.expect("push schema");

    let b = Turso::open(&synced(&b_dir, server.url(), None))
        .await
        .expect("b");
    assert_eq!(count(&b).await, 0);
    a.execute("INSERT INTO notes VALUES ('late')", ())
        .await
        .expect("insert");
    a.push().await.expect("push row");
    assert!(b.pull().await.expect("pull"), "pull must report a change");
    assert_eq!(count(&b).await, 1);
    assert!(!b.pull().await.expect("pull again"), "no new change");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires tursodb (TURSODB_BIN)"]
async fn local_server_migrations_sync() {
    use autumn_plugin_turso::Migration;
    const NOTES: Migration = Migration::new("0001_notes", "CREATE TABLE notes (body TEXT);");
    let server = SyncServer::start();
    let a_dir = tempfile::tempdir().expect("dir");
    let a = Turso::open(&synced(&a_dir, server.url(), None))
        .await
        .expect("a");
    assert_eq!(a.migrate(&[NOTES]).await.expect("migrate"), 1);
    a.push().await.expect("push");

    let b_dir = tempfile::tempdir().expect("dir");
    let b = Turso::open(&synced(&b_dir, server.url(), None))
        .await
        .expect("b");
    assert_eq!(b.migrate(&[NOTES]).await.expect("already applied"), 0);
}
