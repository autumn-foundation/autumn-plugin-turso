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
    let rows = db
        .query("SELECT count(*) FROM notes", ())
        .await
        .map_err(AutumnError::internal_server_error)?;
    let n: i64 = rows[0].get(0).map_err(AutumnError::internal_server_error)?;
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
`state.extension::<Turso>()`. If the plugin is not installed, the extractor
returns `500`.

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
| `close()` | Closes the handle for all clones. |

All calls return `Result<_, TursoError>`. After `close`, calls return
`TursoError::Closed`. The crate re-exports `turso`, so you can use
`autumn_plugin_turso::turso::Value`.

## Configuration

```toml
[turso]
enabled = true            # Open the database at startup.
path = "data/turso.db"    # ":memory:" gives an in-memory database.
read_only = false
busy_timeout_ms = 5000    # Lock wait time.
migrate_on_start = true   # When false, the plugin only logs pending ids.

[turso.sync]              # Needs the `sync` feature.
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

An unknown key, an unknown `AUTUMN_TURSO__*` variable or a bad value stops
startup. The message names the key.

## Migrations

```rust
use autumn_plugin_turso::Migration;

const MIGRATIONS: [Migration; 2] = [
    Migration::new("0001_notes", "CREATE TABLE notes (body TEXT);"),
    Migration::new("0002_notes_index", "CREATE INDEX notes_body ON notes (body);"),
];
```

- Migrations run in the order you add them.
- Each migration runs in one transaction. A failure rolls it back.
- The table `_autumn_turso_migrations` records each id and checksum.
- Do not edit an applied migration. A changed checksum stops startup.
  Add a new migration.
- A read-only database with pending migrations stops startup.

## Health and metrics

- `/actuator/health` has a `turso` component. It is `UP` when `SELECT 1`
  works. It is `DOWN` when the handle is closed or the query fails. Details
  show `path`, `mode` and `read_only`. They never show the sync URL or token.
- `/actuator/prometheus` has `turso_up` and these counters:
  `turso_statements_total`, `turso_errors_total`,
  `turso_connections_opened_total`, `turso_migrations_applied_total`,
  `turso_sync_pushes_total`, `turso_sync_pulls_total`,
  `turso_sync_errors_total`. Statements on a connection from `connect()`
  are not counted.

## Shutdown

The plugin stops the sync loop, pushes (when `push_on_shutdown` is set) and
closes the handle.

## Features

| Feature | Use |
| --- | --- |
| `sync` | Turso Cloud sync. |
| `fts` | Full-text search in the engine. |

The crate turns off the Turso `mimalloc` feature. It does not set a global
allocator.

## Limits

- Use one process per database file. The plugin does not turn on the
  experimental Turso multi-process mode.
- The Turso engine is young. Read the
  [Turso compatibility notes](https://github.com/tursodatabase/turso/blob/main/COMPAT.md)
  before you use it in production.
- This plugin does not give a Diesel pool, a session store or a cache.

## Tests

```sh
cargo test
cargo test --features sync
# Sync tests against a local server:
TURSODB_BIN=$(which tursodb) cargo test --features sync -- --ignored --skip live_round_trip
```

Install `tursodb` with
`cargo install --git https://github.com/tursodatabase/turso --tag v0.8.3 turso_cli`.

## License

Apache-2.0
