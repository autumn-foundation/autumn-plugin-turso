# autumn-plugin-turso

A [Turso](https://github.com/tursodatabase/turso) database for
[Autumn](https://crates.io/crates/autumn-web) 0.8. Turso is an embedded,
SQLite-compatible engine. It needs no server. Turso Cloud sync is optional.

## Install

```toml
[dependencies]
autumn-plugin-turso = "0.1"
# For Turso Cloud sync:
# autumn-plugin-turso = { version = "0.1", features = ["sync"] }
```

## Quick start

```rust,no_run
use autumn_plugin_turso::{Migration, Turso, TursoPlugin};
use autumn_web::prelude::*;

const NOTES: Migration = Migration::new("0001_notes", "CREATE TABLE notes (body TEXT);");

#[get("/notes/count")]
async fn count(db: Turso) -> AutumnResult<String> {
    let rows = db.query("SELECT count(*) FROM notes", ()).await?;
    let n: i64 = rows[0].get(0)?;
    Ok(n.to_string())
}

#[autumn_web::main]
async fn main() {
    autumn_web::app()
        .plugin(TursoPlugin::new().migration(NOTES))
        .routes(routes![count])
        .run()
        .await;
}
```

At startup the plugin:

1. Reads `[turso]`.
2. Opens the database. It makes missing parent directories.
3. Applies pending migrations.
4. Puts a `Turso` handle in `AppState`.

Get the handle in a handler with `db: Turso`, or with
`state.extension::<Turso>()`. If the plugin is not installed or is disabled,
the extractor returns `500`.

An app has one Turso database. A second `TursoPlugin` is skipped with a
warning.

## Example

[`examples/notes`](https://github.com/autumn-foundation/autumn-plugin-turso/tree/main/examples/notes) is a JSON notes API. It shows
migrations, the `Turso` extractor, bound parameters and HTTP status codes.

```sh
cargo run --example notes    # Listens on port 3000. Writes data/notes.db.
curl -X POST localhost:3000/notes -H 'content-type: application/json' -d '{"body":"buy milk"}'
curl localhost:3000/notes      # [{"id":1,"body":"buy milk"}]
curl 'localhost:3000/notes?after=100'   # The next page. A page has 100 notes or fewer.
curl localhost:3000/notes/1    # 200, or 404 when the note is missing.
curl -X DELETE localhost:3000/notes/1   # 204, or 404.
```

- The app stores the note text as sent.
- An empty note, or a note longer than 1000 Unicode code points, gives
  `422`. A missing `body` field also gives `422`. Bad JSON or a bad id
  gives `400`.
- Run the example from the repository root. Git ignores `/data`.
- The example has no authentication. Do not deploy it as is. In the `dev`
  profile, Autumn allows CORS from all origins.
- `tests/example.rs` runs the same code.

## Handle

| Call | Result |
| --- | --- |
| `connect()` | A new `turso::Connection`. Use one per task. |
| `execute(sql, params)` | The number of changed rows. |
| `execute_batch(sql)` | Runs one or more statements with no parameters. |
| `query(sql, params)` | All rows as `Vec<turso::Row>`. |
| `ping()` | Runs `SELECT 1`. |
| `migrate(&[Migration])` | The number of applied migrations. |
| `pending(&[Migration])` | The ids that are not applied. |
| `push()`, `pull()` | Sync with Turso Cloud (feature `sync`). |
| `close()` | Closes the handle and all its clones. |

All calls return `Result<_, TursoError>`. After `close`, calls return
`TursoError::Closed`. Close does not stop work that started before it. A
connection from `connect()` stays open until you drop it. The crate re-exports `turso`, so you can use
`autumn_plugin_turso::turso::Value`.

## Configuration

```toml
[turso]
enabled = true            # Open the database at startup.
path = "data/turso.db"    # A file path. ":memory:" gives an in-memory database.
read_only = false
busy_timeout_ms = 5000    # Lock wait time.
migrate_on_start = true   # When false, the plugin only logs pending ids.

[turso.sync]              # Needs the `sync` feature.
enabled = true            # Set false to turn off sync from a higher layer.
url = "libsql://my-db.turso.io"
auth_token = "..."        # Prefer AUTUMN_TURSO__SYNC__AUTH_TOKEN.
pull_on_start = true
push_on_shutdown = true
interval_secs = 0         # Push and pull every N seconds. 0 disables.
```

Layers (lowest first):

1. Defaults.
2. Builder setters, for example `TursoPlugin::new().path("app.db")`.
3. `[turso]` in `autumn.toml`.
4. `[profile.<p>.turso]` in `autumn.toml`.
5. `[turso]` in `autumn-<p>.toml`.
6. Environment variables: `AUTUMN_TURSO__PATH`,
   `AUTUMN_TURSO__SYNC__AUTH_TOKEN`, and so on. Use `__` between levels.
   The plugin also reads them from `.env` files, with the same rules as
   Autumn core. A real environment variable wins.

The plugin finds the profile as Autumn core does: `AUTUMN_ENV`, then
`AUTUMN_PROFILE`, then `--profile`.

An unknown key, an unknown `AUTUMN_TURSO__*` variable or a bad value stops
startup. The message names the key. A `file:` URI as `path` also stops
startup.

These sync settings also stop startup:

- `read_only = true` together with `[turso.sync]`.
- An empty `auth_token`, or one with control characters. The plugin removes
  white space at the ends of the token.
- An `auth_token` with an `http://` URL to a host that is not loopback. Use
  `https://` or `libsql://`. This keeps the token encrypted on the network.

## Migrations

```rust
use autumn_plugin_turso::Migration;

const MIGRATIONS: [Migration; 2] = [
    Migration::new("0001_notes", "CREATE TABLE notes (body TEXT);"),
    Migration::new("0002_notes_index", "CREATE INDEX notes_body ON notes (body);"),
];
```

- Migrations run in the order you add them.
- Each migration runs in one transaction. The plugin rolls back a failed
  migration. Earlier migrations stay applied.
- Do not use `BEGIN`, `COMMIT` or `ROLLBACK` in a migration. A migration that
  ends the transaction fails.
- Two processes can migrate one file at the same time. Each migration runs
  once.
- The checksum reads `\r\n` as `\n`. A checkout with other line endings does
  not change it.
- The table `_autumn_turso_migrations` records each id and checksum.
- Do not edit an applied migration. A changed checksum stops startup.
  Add a new migration.
- A read-only database with pending migrations stops startup.

## Health and metrics

- `/actuator/health` has a `turso` component. It is `UP` when `SELECT 1`
  works. It is `DOWN` when the handle is closed or the query fails. Details
  show `path`, `mode` and `read_only`. They never show the sync URL or token.
  Autumn shows details only when `health.detailed` is on (the `dev` profile
  default).
- `/actuator/prometheus` has `turso_up` and these counters:
  `turso_statements_total`, `turso_errors_total`,
  `turso_connections_opened_total`, `turso_migrations_applied_total`,
  `turso_sync_pushes_total`, `turso_sync_pulls_total`,
  `turso_sync_errors_total`. The counters do not count statements on a
  connection from `connect()`, or the health `ping()`.

## Shutdown

The plugin stops the sync loop, pushes (when `push_on_shutdown` is set) and
closes the handle.

The push is best-effort. It has a 10-second limit. Autumn also gives each
shutdown hook a time limit (`server.shutdown_timeout_secs`, 1 second in the
`dev` profile). When the push does not complete, the data stays in the local
file. The next push or start sends it.

## Features

| Feature | Use |
| --- | --- |
| `sync` | Turso Cloud sync. |
| `fts` | Full-text search in the engine. |

The crate disables all Turso default features (`mimalloc` and `fts`). Thus it
does not set a global allocator. Enable the `fts` feature of this crate to get
full-text search.

Use the re-exported `autumn_plugin_turso::turso`. If you also add `turso` to
your own `Cargo.toml`, set `default-features = false`. If you do not, Cargo
enables `mimalloc` and Turso sets the global allocator.

## Limits

- Use one process per database file. The plugin does not turn on the
  experimental Turso multi-process mode.
- The Turso engine is new. Read the
  [Turso compatibility notes](https://github.com/tursodatabase/turso/blob/main/COMPAT.md)
  before you use it in production.
- This plugin does not give a Diesel pool, a session store or a cache.
- The public API uses `turso` types. A new `turso` minor version (0.x) is a
  breaking change of this crate.
- The plugin makes the database directory with the process umask. On a
  shared host, set a strict umask or make the directory before startup.

## Tests

```sh
cargo test
cargo test --features sync
# Sync tests against a local server:
TURSODB_BIN=$(which tursodb) cargo test --features sync -- --ignored --skip live_round_trip
```

Install `tursodb` with
`cargo install --git https://github.com/tursodatabase/turso --rev 607bff9fd0e547a25a97b3bf06e83bcc5d99d53c turso_cli --locked`
(the `v0.8.3` tag).

## License

Apache-2.0
