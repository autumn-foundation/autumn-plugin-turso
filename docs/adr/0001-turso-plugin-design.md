# ADR 0001: Turso plugin design

- Status: accepted
- Date: 2026-10-14

## Context

Autumn 0.8 apps need an embedded database that needs no server and no C
toolchain. Turso (crate `turso` 0.8) is an embedded, SQLite-compatible
engine in Rust. It has optional Turso Cloud sync.

## Decision

1. **Own handle, no Diesel.** Diesel needs the SQLite C API. Turso has a
   different API. The plugin gives a `Turso` handle with small helpers and
   gives raw `turso::Connection` values for all other work.
2. **No pool.** The helpers open one connection per call. Turso has no pool
   API, and a pool adds state that close must clear.
3. **Config in a startup hook.** Autumn reads plugin config after
   `Plugin::build`. The plugin reads `[turso]` itself, with the same layers
   as Autumn core. It calls `config_section("turso")` so strict config
   accepts the section.
4. **Strict config.** Unknown keys and unknown `AUTUMN_TURSO__*` variables
   stop startup.
5. **Migrations as data.** `Migration` is `const` data with an id and SQL.
   A table records id and FNV-1a checksum. One transaction per migration.
6. **Sync is a feature.** `sync` adds HTTP and TLS dependencies. It is off by
   default.
7. **No global allocator.** The plugin turns off Turso's default `mimalloc`
   feature. The app keeps control of the allocator.
8. **Slot for health and metrics.** Health and metrics are registered in
   `build`, before the database opens. They read a shared slot that the
   startup hook fills.

## Consequences

- Apps write SQL. There is no query builder.
- The public API uses `turso` types (`Connection`, `Row`, `Value`,
  `turso::Error`). A new `turso` 0.x minor version is a semver-major release of
  this crate.
- The plugin uses Autumn APIs that are not in Autumn's list of stable plugin
  surfaces (`PLUGIN_SURFACES`): `health_indicator`, `metrics_source`,
  `AppState::insert_extension` and `config::profile_override_file_lookup_names`.
  A change to them in Autumn can break this plugin.
- One database per app. `AppState` holds one `Turso` value, so a second
  plugin instance is skipped.
- Sync tests need a server. CI builds `tursodb` and runs a local sync server.
