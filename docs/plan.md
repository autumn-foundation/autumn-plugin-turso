# Plan: TursoDB plugin for Autumn

## Goal

Give an Autumn 0.8 app an embedded, SQLite-compatible database with no
external service. The engine is [Turso](https://github.com/tursodatabase/turso)
(crate `turso` 0.8). Turso Cloud sync is optional.

## 1. Brainstorming

Ideas. We keep the marked ones (✓) for v0.1.

- ✓ `TursoPlugin` with `::new()` and fluent setters.
- ✓ A `[turso]` config section with profile and environment layers.
- ✓ A `Turso` handle in `AppState`. It is also a handler extractor.
- ✓ Helper calls: `connect`, `execute`, `execute_batch`, `query`, `ping`.
- ✓ Ordered, checksummed migrations that run at startup.
- ✓ A health indicator and a metrics source.
- ✓ Optional Turso Cloud sync: pull at start, push at stop, a timed loop.
- ✓ Read-only mode.
- ✓ Re-export the `turso` crate so the app uses one version.
- Diesel pool provider on Turso. Not possible: Diesel needs the SQLite C API.
- Session store and cache backends. Later.
- At-rest encryption. Turso marks it experimental. Later.
- A connection pool. Not necessary: a Turso connection is cheap.

## 2. Reverse brainstorming

Question: "How can we make this plugin fail?" Each answer gives a control.

| How to make it fail | Control |
| --- | --- |
| Add a global allocator that the app does not want. | Disable Turso default features (`mimalloc`). |
| Leak the auth token in logs or health output. | `Debug` hides the token. Health shows no URL or token. |
| Send the token over plain `http://`. | Reject a token with an `http://` URL that is not loopback. |
| A typo in `[turso]` is ignored. | Unknown keys fail startup. `config_section("turso")` keeps strict mode. |
| Someone edits an applied migration. | Store a checksum. A changed checksum fails startup. |
| Two migrations use one id. | Fail startup and name the id. |
| A migration fails halfway. | Run each migration in one transaction. |
| A read-only database has pending migrations. | Fail startup with a clear message. |
| A handler runs before startup or after shutdown. | Return `TursoError::Closed`. The extractor returns 500 with a message. |
| Health says `UP` when the database is closed. | Health runs `SELECT 1` and reports `DOWN` on error or close. |
| Shutdown loses local writes that are not pushed. | Push at shutdown when sync is on (default). |
| A sync failure at runtime stops the app. | The loop logs the error, counts it, and continues. |
| Disabling the plugin breaks health. | Health reports `UP` with `enabled: false`. |

## 3. Six thinking hats

- **White (facts).** `turso` 0.8.3 is async. It has no pool API.
  Sync is behind the `sync` feature. Autumn reads plugin config after
  `Plugin::build`, so the plugin resolves its config in a startup hook.
  `TestApp` runs plugin startup hooks but not shutdown hooks.
- **Red (feelings).** Users want to add the plugin with one line. A default
  file under `data/` feels safe. Unclear errors make users angry, so messages
  name the key and the value.
- **Black (risks).** Turso is young; its API can change. We pin `0.8` and wrap
  the API. Sync tests need a server. CI builds `tursodb` and runs a local
  sync server. One `#[ignore]` test also runs against Turso Cloud.
- **Yellow (benefits).** No Diesel, no C toolchain, no server. Fast local
  tests on `:memory:`. Same SQL dialect as SQLite.
- **Green (ideas).** A `Turso` extractor. A `Migration` list as `const` data.
  Metrics that count statements, errors and sync calls.
- **Blue (process).** Red, green, refactor for each criterion. Then a
  multi-angle review. Then map each criterion to its evidence.

## 4. Acceptance criteria

- **AC1 — Plugin.** `TursoPlugin` implements `autumn_web::plugin::Plugin`.
  `app().plugin(TursoPlugin::new())` installs it. `contract()` declares
  `autumn_web = "0.8"`. The plugin declares the `turso` config section.
- **AC2 — Configuration.** `[turso]` sets `enabled`, `path`, `read_only`,
  `busy_timeout_ms` and `migrate_on_start`. With `sync`, `[turso.sync]` sets
  `url`, `auth_token`, `pull_on_start`, `push_on_shutdown` and
  `interval_secs`. Precedence (lowest first): defaults, builder setters,
  `autumn.toml`, `[profile.<p>.turso]`, `autumn-<p>.toml`,
  `AUTUMN_TURSO__*`. An unknown key or a bad value fails startup.
- **AC3 — Handle.** At startup the plugin opens the database and puts a
  `Turso` handle in `AppState`. The handle is `Clone + Send + Sync`. It has
  `connect`, `execute`, `execute_batch`, `query`, `ping` and `close`. It is
  also an Axum extractor.
- **AC4 — Durability.** Data in a file database survives a close and a
  reopen.
- **AC5 — Migrations.** Migrations run in order, one transaction each, and
  run once. A changed checksum or a duplicate id fails startup. A read-only
  database with pending migrations fails startup.
- **AC6 — Errors.** All failures return a typed `TursoError`. Nothing
  panics on bad input.
- **AC7 — Health.** A `turso` indicator reports `UP` with details, and
  `DOWN` when the database is closed or a ping fails.
- **AC8 — Metrics.** A metrics source exports `turso_*` families.
- **AC9 — Lifecycle.** Shutdown pushes (with sync) and closes the handle.
  Calls after close return `TursoError::Closed`.
- **AC10 — Sync (feature `sync`).** The plugin opens a synced database,
  pulls at start, pushes at shutdown, and runs a timed push and pull loop.
  A sync config without `url` fails startup.
- **AC11 — Security.** `Debug` output and health details never show the auth
  token.
- **AC12 — Quality gates.** CI runs fmt, clippy (pedantic, nursery,
  `-D warnings`), tests on Linux, macOS and Windows, rustdoc with
  `-D warnings`, and an MSRV check.
- **AC13 — Documentation.** README, CHANGELOG, `CLAUDE.md`, an ADR, and
  rustdoc on all public items. Text uses ASD-STE100 and is short.

## Out of scope (v0.1)

Diesel integration, session store, cache backend, at-rest encryption,
partial sync.
