//! AC5: ordered, checksummed, one-time migrations.

use autumn_plugin_turso::{Migration, Turso, TursoConfig, TursoError};

const CREATE: Migration = Migration::new("0001_create_notes", "CREATE TABLE notes (body TEXT);");
const INDEX: Migration = Migration::new(
    "0002_index_notes",
    "CREATE INDEX notes_body ON notes (body);",
);

async fn memory() -> Turso {
    let config = TursoConfig::new(":memory:");
    Turso::open(&config).await.expect("open")
}

async fn applied_ids(db: &Turso) -> Vec<String> {
    db.query("SELECT id FROM _autumn_turso_migrations ORDER BY id", ())
        .await
        .expect("query")
        .iter()
        .map(|row| row.get::<String>(0).expect("id"))
        .collect()
}

#[tokio::test]
async fn migrations_apply_in_order_once() {
    let db = memory().await;
    assert_eq!(db.migrate(&[CREATE, INDEX]).await.expect("first"), 2);
    assert_eq!(db.migrate(&[CREATE, INDEX]).await.expect("second"), 0);
    assert_eq!(
        applied_ids(&db).await,
        ["0001_create_notes", "0002_index_notes"]
    );
    db.execute("INSERT INTO notes VALUES ('x')", ())
        .await
        .expect("table exists");
}

#[tokio::test]
async fn new_migrations_apply_later() {
    let db = memory().await;
    db.migrate(&[CREATE]).await.expect("first");
    assert_eq!(
        db.pending(&[CREATE, INDEX]).await.expect("pending"),
        ["0002_index_notes"]
    );
    assert_eq!(db.migrate(&[CREATE, INDEX]).await.expect("second"), 1);
    assert_eq!(
        db.pending(&[CREATE, INDEX]).await.expect("pending"),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn failed_migration_rolls_back() {
    let db = memory().await;
    let bad = Migration::new(
        "0001_bad",
        "CREATE TABLE half (x INTEGER); INSERT INTO missing VALUES (1);",
    );
    let err = db.migrate(&[bad]).await.expect_err("fails");
    match &err {
        TursoError::Migration { id, .. } => assert_eq!(id, "0001_bad"),
        other => panic!("wrong error: {other:?}"),
    }
    assert!(
        db.query("SELECT * FROM half", ()).await.is_err(),
        "table rolled back"
    );
    assert_eq!(applied_ids(&db).await, Vec::<String>::new());
}

#[tokio::test]
async fn changed_migration_fails_with_checksum_error() {
    let db = memory().await;
    db.migrate(&[CREATE]).await.expect("first");
    let edited = Migration::new(
        "0001_create_notes",
        "CREATE TABLE notes (body TEXT, extra TEXT);",
    );
    let err = db.migrate(&[edited]).await.expect_err("edited");
    assert!(
        matches!(&err, TursoError::MigrationChecksum { id, .. } if id == "0001_create_notes"),
        "{err:?}"
    );
}

#[tokio::test]
async fn duplicate_ids_fail_before_any_change() {
    let db = memory().await;
    let err = db.migrate(&[CREATE, CREATE]).await.expect_err("dup");
    assert!(
        matches!(&err, TursoError::DuplicateMigration(id) if id == "0001_create_notes"),
        "{err:?}"
    );
    assert!(db.query("SELECT * FROM notes", ()).await.is_err());
}

#[tokio::test]
async fn empty_id_is_rejected() {
    let db = memory().await;
    let err = db
        .migrate(&[Migration::new("", "SELECT 1;")])
        .await
        .expect_err("empty id");
    assert!(matches!(err, TursoError::InvalidMigration), "{err:?}");
}

#[test]
fn checksum_is_pinned() {
    // A new algorithm would fail startup on every deployed database.
    let migration = Migration::new("a", "CREATE TABLE notes (body TEXT);");
    assert_eq!(migration.checksum(), "0354e0287578986f");
}

#[test]
fn checksum_ignores_line_endings() {
    let lf = Migration::new("a", "CREATE TABLE a (x);\nCREATE TABLE b (x);\n");
    let crlf = Migration::new("a", "CREATE TABLE a (x);\r\nCREATE TABLE b (x);\r\n");
    assert_eq!(lf.checksum(), crlf.checksum());
}

#[test]
fn owned_equals_static() {
    assert_eq!(Migration::owned("0001_create_notes", CREATE.sql()), CREATE);
}

#[tokio::test]
async fn earlier_migrations_stay_applied_when_a_later_one_fails() {
    let db = memory().await;
    let bad = Migration::new("0002_bad", "INSERT INTO missing VALUES (1);");
    assert!(db.migrate(&[CREATE, bad]).await.is_err());
    assert_eq!(applied_ids(&db).await, ["0001_create_notes"]);
}

#[tokio::test]
async fn migration_that_ends_the_transaction_fails() {
    let db = memory().await;
    let sneaky = Migration::new("0001_sneaky", "CREATE TABLE t2 (x); COMMIT;");
    let err = db.migrate(&[sneaky]).await.expect_err("must fail");
    match &err {
        TursoError::Migration { id, message } => {
            assert_eq!(id, "0001_sneaky");
            assert!(message.contains("BEGIN, COMMIT or ROLLBACK"), "{message}");
        }
        other => panic!("wrong error: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_migrations_on_one_file_both_succeed() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("race.db").to_string_lossy().into_owned();
    let a = Turso::open(&TursoConfig::new(path.clone()))
        .await
        .expect("a");
    let b = Turso::open(&TursoConfig::new(path)).await.expect("b");
    let ta = tokio::spawn({
        let a = a.clone();
        async move { a.migrate(&[CREATE, INDEX]).await }
    });
    let tb = tokio::spawn(async move { b.migrate(&[CREATE, INDEX]).await });
    let (ra, rb) = (ta.await.expect("join a"), tb.await.expect("join b"));
    let total = ra.expect("a migrates") + rb.expect("b migrates");
    assert_eq!(total, 2, "each migration runs once");
    assert_eq!(
        applied_ids(&a).await,
        ["0001_create_notes", "0002_index_notes"]
    );
}

#[test]
fn checksum_is_stable_and_content_based() {
    assert_eq!(CREATE.checksum(), CREATE.checksum());
    assert_ne!(CREATE.checksum(), INDEX.checksum());
    assert_eq!(CREATE.checksum().len(), 16);
    assert_eq!(CREATE.id(), "0001_create_notes");
    assert_eq!(CREATE.sql(), "CREATE TABLE notes (body TEXT);");
}

#[tokio::test]
async fn read_only_with_pending_fails() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("ro.db").to_string_lossy().into_owned();
    let rw = Turso::open(&TursoConfig::new(path.clone()))
        .await
        .expect("rw");
    rw.migrate(&[CREATE]).await.expect("migrate");
    rw.close().await;

    let ro = Turso::open(&TursoConfig::new(path).read_only(true))
        .await
        .expect("ro");
    assert_eq!(ro.migrate(&[CREATE]).await.expect("nothing pending"), 0);
    let err = ro
        .migrate(&[CREATE, INDEX])
        .await
        .expect_err("pending on ro");
    assert!(
        matches!(&err, TursoError::ReadOnlyPending(ids) if ids == &["0002_index_notes".to_owned()]),
        "{err:?}"
    );
}
