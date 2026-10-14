//! The notes app. Tests include this file.

use autumn_plugin_turso::turso::{self, Row};
use autumn_plugin_turso::{Migration, Turso, TursoPlugin};
use autumn_web::Route;
use autumn_web::prelude::*;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

/// The database file.
pub const DB_PATH: &str = "data/notes.db";

/// The maximum note length in characters.
pub const MAX_BODY_CHARS: usize = 1000;

/// The schema. Add new migrations at the end. Do not edit old ones.
const MIGRATIONS: [Migration; 1] = [Migration::new(
    "0001_notes",
    "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
)];

/// A stored note.
#[derive(Debug, Serialize)]
pub struct Note {
    id: i64,
    body: String,
}

/// The request body for a new note.
#[derive(Debug, Deserialize)]
pub struct NewNote {
    body: String,
}

/// The plugin with the app migrations.
pub fn plugin() -> TursoPlugin {
    TursoPlugin::new().path(DB_PATH).migrations(MIGRATIONS)
}

/// All app routes.
pub fn routes() -> Vec<Route> {
    routes![list_notes, create_note, get_note, delete_note]
}

#[get("/notes")]
async fn list_notes(db: Turso) -> AutumnResult<Json<Vec<Note>>> {
    let rows = db
        .query("SELECT id, body FROM notes ORDER BY id", ())
        .await?;
    let notes = rows.iter().map(note).collect::<Result<_, _>>()?;
    Ok(Json(notes))
}

#[post("/notes")]
async fn create_note(
    db: Turso,
    Json(new): Json<NewNote>,
) -> AutumnResult<(StatusCode, Json<Note>)> {
    let body = new.body.trim();
    if body.is_empty() || body.chars().count() > MAX_BODY_CHARS {
        return Err(AutumnError::unprocessable_msg(format!(
            "body must have 1 to {MAX_BODY_CHARS} characters"
        )));
    }
    // A bound parameter (`?1`) keeps user text out of the SQL.
    let rows = db
        .query(
            "INSERT INTO notes (body) VALUES (?1) RETURNING id, body",
            [body],
        )
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| AutumnError::internal_server_error_msg("no row"))?;
    Ok((StatusCode::CREATED, Json(note(row)?)))
}

#[get("/notes/{id}")]
async fn get_note(db: Turso, Path(id): Path<i64>) -> AutumnResult<Json<Note>> {
    let rows = db
        .query("SELECT id, body FROM notes WHERE id = ?1", [id])
        .await?;
    let row = rows.first().ok_or_else(|| not_found(id))?;
    Ok(Json(note(row)?))
}

#[delete("/notes/{id}")]
async fn delete_note(db: Turso, Path(id): Path<i64>) -> AutumnResult<StatusCode> {
    match db.execute("DELETE FROM notes WHERE id = ?1", [id]).await? {
        0 => Err(not_found(id)),
        _ => Ok(StatusCode::NO_CONTENT),
    }
}

/// Read one `id, body` row.
fn note(row: &Row) -> turso::Result<Note> {
    Ok(Note {
        id: row.get(0)?,
        body: row.get(1)?,
    })
}

fn not_found(id: i64) -> AutumnError {
    AutumnError::not_found_msg(format!("note {id} not found"))
}
