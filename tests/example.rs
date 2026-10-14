//! EX1–EX6: the `notes` example app.

#[path = "../examples/notes/app.rs"]
mod notes;

use autumn_plugin_turso::{ConfigSources, Turso};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

fn client_at(dir: &tempfile::TempDir, path: &str) -> TestClient {
    TestApp::new()
        .plugin(
            notes::plugin()
                .path(path)
                .config_sources(ConfigSources::new(dir.path(), "dev")),
        )
        .routes(notes::routes())
        .build()
}

fn client(dir: &tempfile::TempDir) -> TestClient {
    client_at(dir, ":memory:")
}

async fn create(client: &TestClient, body: &str) -> Value {
    let response = client
        .post("/notes")
        .json(&json!({ "body": body }))
        .send()
        .await;
    response.assert_status(201);
    response.json()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_returns_201_and_the_note() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let note = create(&client, "buy milk").await;
    assert_eq!(note["body"], "buy milk", "{note}");
    assert!(note["id"].as_i64().is_some(), "{note}");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_returns_notes_in_id_order() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    client
        .get("/notes")
        .send()
        .await
        .assert_ok()
        .assert_body_eq("[]");
    let first = create(&client, "one").await;
    let second = create(&client, "two").await;
    let list: Value = client.get("/notes").send().await.json();
    assert_eq!(list, json!([first, second]));
}

#[tokio::test(flavor = "multi_thread")]
async fn get_returns_the_note_or_404() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let note = create(&client, "read me").await;
    let got: Value = client
        .get(&format!("/notes/{}", note["id"]))
        .send()
        .await
        .assert_ok()
        .json();
    assert_eq!(got, note);
    client.get("/notes/999").send().await.assert_status(404);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_returns_204_then_404() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let note = create(&client, "gone soon").await;
    let uri = format!("/notes/{}", note["id"]);
    client
        .delete(&uri)
        .send()
        .await
        .assert_status(204)
        .assert_body_empty();
    client.get(&uri).send().await.assert_status(404);
    client.delete(&uri).send().await.assert_status(404);
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_body_is_422() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let too_long = "x".repeat(notes::MAX_BODY_CHARS + 1);
    for body in ["", "   ", too_long.as_str()] {
        client
            .post("/notes")
            .json(&json!({ "body": body }))
            .send()
            .await
            .assert_status(422);
    }
    client.get("/notes").send().await.assert_body_eq("[]");
}

#[tokio::test(flavor = "multi_thread")]
async fn max_length_body_is_accepted() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    // Count characters, not bytes.
    let body = "é".repeat(notes::MAX_BODY_CHARS);
    let note = create(&client, &body).await;
    assert_eq!(note["body"], body.as_str());
}

#[tokio::test(flavor = "multi_thread")]
async fn sql_text_is_stored_as_data() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let body = "'); DROP TABLE notes; --";
    let note = create(&client, body).await;
    assert_eq!(note["body"], body);
    let list: Value = client.get("/notes").send().await.json();
    assert_eq!(list, json!([note]));
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_survive_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("data").join("notes.db");
    let path = path.to_string_lossy();
    let first = client_at(&dir, &path);
    let note = create(&first, "keep me").await;
    // `TestApp` does not run shutdown hooks. Close the handle here.
    first
        .state()
        .extension::<Turso>()
        .expect("handle")
        .close()
        .await;
    drop(first);

    let second = client_at(&dir, &path);
    let list: Value = second.get("/notes").send().await.json();
    assert_eq!(list, json!([note]));
    // Close before `dir` drops. Windows cannot delete an open file.
    second
        .state()
        .extension::<Turso>()
        .expect("handle")
        .close()
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn health_shows_turso_up() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let body: Value = client.get("/actuator/health").send().await.json();
    assert_eq!(body["components"]["turso"]["status"], "UP", "{body}");
}

#[test]
fn plugin_uses_default_path() {
    let text = format!("{:?}", notes::plugin());
    assert!(text.contains("data/notes.db"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn body_is_stored_as_sent() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    let note = create(&client, "  hi \n").await;
    assert_eq!(note["body"], "  hi \n");
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_ids_are_not_used_again() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    create(&client, "one").await;
    let second = create(&client, "two").await;
    client
        .delete(&format!("/notes/{}", second["id"]))
        .send()
        .await
        .assert_status(204);
    let third = create(&client, "three").await;
    assert!(third["id"].as_i64() > second["id"].as_i64(), "{third}");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_has_a_limit_and_an_after_cursor() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    for n in 0..=notes::MAX_LIST {
        create(&client, &n.to_string()).await;
    }
    let page: Vec<Value> = client.get("/notes").send().await.json();
    let max = usize::try_from(notes::MAX_LIST).expect("fits");
    assert_eq!(page.len(), max);
    let last = &page[max - 1]["id"];
    let rest: Value = client
        .get(&format!("/notes?after={last}"))
        .send()
        .await
        .json();
    assert_eq!(rest.as_array().map(Vec::len), Some(1), "{rest}");
    assert_eq!(rest[0]["body"], notes::MAX_LIST.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_give_4xx() {
    let dir = tempfile::tempdir().expect("dir");
    let client = client(&dir);
    for (uri, status) in [
        ("/notes/abc", 400),
        ("/notes/1.5", 400),
        ("/notes/99999999999999999999", 400),
        ("/notes/-1", 404),
        ("/notes/0", 404),
        ("/notes?after=abc", 400),
    ] {
        client.get(uri).send().await.assert_status(status);
    }
    for body in [json!({}), json!({ "body": null }), json!({ "body": 1 })] {
        client
            .post("/notes")
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
    client
        .post("/notes")
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .assert_status(400);
    client
        .post("/notes")
        .body(r#"{"body":"x"}"#)
        .send()
        .await
        .assert_status(415);
}
