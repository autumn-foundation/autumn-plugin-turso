//! AC3, AC4, AC6, AC9: the `Turso` handle.

use autumn_plugin_turso::turso::Value;
use autumn_plugin_turso::{Turso, TursoConfig, TursoError};

fn memory() -> TursoConfig {
    TursoConfig {
        path: ":memory:".into(),
        ..TursoConfig::default()
    }
}

const fn assert_handle_traits<T: Clone + Send + Sync + 'static>() {}

#[test]
fn handle_is_clone_send_sync() {
    assert_handle_traits::<Turso>();
}

#[tokio::test]
async fn execute_and_query_round_trip() {
    let db = Turso::open(&memory()).await.expect("open");
    db.execute_batch("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);")
        .await
        .expect("create");
    let changed = db
        .execute("INSERT INTO notes (body) VALUES (?1)", ["hello"])
        .await
        .expect("insert");
    assert_eq!(changed, 1);

    let rows = db
        .query("SELECT id, body FROM notes WHERE body = ?1", ["hello"])
        .await
        .expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get_value(1).expect("col"),
        Value::Text("hello".into())
    );
}

#[tokio::test]
async fn connections_share_one_database() {
    let db = Turso::open(&memory()).await.expect("open");
    let a = db.connect().await.expect("connect a");
    a.execute("CREATE TABLE t (x INTEGER)", ())
        .await
        .expect("create");
    a.execute("INSERT INTO t VALUES (1)", ())
        .await
        .expect("insert");
    let rows = db.query("SELECT x FROM t", ()).await.expect("query");
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn clones_share_state() {
    let db = Turso::open(&memory()).await.expect("open");
    let other = db.clone();
    db.execute_batch("CREATE TABLE t (x INTEGER);")
        .await
        .expect("create");
    other
        .execute("INSERT INTO t VALUES (?1)", [7])
        .await
        .expect("insert");
    assert_eq!(db.query("SELECT x FROM t", ()).await.expect("q").len(), 1);
}

#[tokio::test]
async fn file_data_survives_reopen() {
    let dir = tempfile::tempdir().expect("dir");
    let config = TursoConfig {
        path: dir.path().join("app.db").to_string_lossy().into_owned(),
        ..TursoConfig::default()
    };
    {
        let db = Turso::open(&config).await.expect("open");
        db.execute_batch("CREATE TABLE t (x TEXT); INSERT INTO t VALUES ('kept');")
            .await
            .expect("write");
        db.close().await;
    }
    let db = Turso::open(&config).await.expect("reopen");
    let rows = db.query("SELECT x FROM t", ()).await.expect("query");
    assert_eq!(
        rows[0].get_value(0).expect("col"),
        Value::Text("kept".into())
    );
}

#[tokio::test]
async fn open_creates_missing_parent_dirs() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("nested/deeper/app.db");
    let config = TursoConfig {
        path: path.to_string_lossy().into_owned(),
        ..TursoConfig::default()
    };
    let db = Turso::open(&config).await.expect("open");
    db.ping().await.expect("ping");
    assert!(path.parent().expect("parent").is_dir());
}

#[tokio::test]
async fn read_only_rejects_writes() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("ro.db").to_string_lossy().into_owned();
    let rw = TursoConfig {
        path: path.clone(),
        ..TursoConfig::default()
    };
    let db = Turso::open(&rw).await.expect("open rw");
    db.execute_batch("CREATE TABLE t (x INTEGER);")
        .await
        .expect("create");
    db.close().await;

    let ro = TursoConfig {
        path,
        read_only: true,
        ..TursoConfig::default()
    };
    let db = Turso::open(&ro).await.expect("open ro");
    assert!(db.is_read_only());
    db.query("SELECT x FROM t", ()).await.expect("read works");
    let err = db
        .execute("INSERT INTO t VALUES (1)", ())
        .await
        .expect_err("write fails");
    assert!(matches!(err, TursoError::Database(_)), "{err:?}");
}

#[tokio::test]
async fn bad_sql_is_a_typed_error() {
    let db = Turso::open(&memory()).await.expect("open");
    let err = db.execute("NOT SQL", ()).await.expect_err("bad sql");
    assert!(matches!(err, TursoError::Database(_)), "{err:?}");
    let err = db
        .query("SELECT * FROM missing", ())
        .await
        .expect_err("no table");
    assert!(matches!(err, TursoError::Database(_)), "{err:?}");
}

#[tokio::test]
async fn invalid_config_does_not_open() {
    let config = TursoConfig {
        path: String::new(),
        ..TursoConfig::default()
    };
    let err = Turso::open(&config).await.expect_err("invalid");
    assert!(matches!(err, TursoError::Config(_)), "{err:?}");
}

#[tokio::test]
async fn calls_after_close_return_closed() {
    let db = Turso::open(&memory()).await.expect("open");
    let clone = db.clone();
    db.close().await;
    assert!(clone.is_closed());
    assert!(matches!(clone.ping().await, Err(TursoError::Closed)));
    assert!(matches!(clone.connect().await, Err(TursoError::Closed)));
    assert!(matches!(
        clone.execute("SELECT 1", ()).await,
        Err(TursoError::Closed)
    ));
    assert!(matches!(
        clone.execute_batch("SELECT 1").await,
        Err(TursoError::Closed)
    ));
    assert!(matches!(
        clone.query("SELECT 1", ()).await,
        Err(TursoError::Closed)
    ));
    // A second close is a no-op.
    clone.close().await;
}

#[tokio::test]
async fn handle_reports_its_settings() {
    let db = Turso::open(&memory()).await.expect("open");
    assert_eq!(db.path(), ":memory:");
    assert!(!db.is_read_only());
    assert!(!db.is_synced());
}

#[test]
fn handle_works_on_a_current_thread_runtime() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let db = Turso::open(&memory()).await.expect("open");
        db.ping().await.expect("ping");
    });
}

#[tokio::test]
async fn handle_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let db = Turso::open(&memory()).await.expect("open");
    let fut = db.execute("SELECT 1", ());
    assert_send(&fut);
    let _ = fut.await;
    let fut = db.query("SELECT 1", ());
    assert_send(&fut);
    let _ = fut.await;
}
