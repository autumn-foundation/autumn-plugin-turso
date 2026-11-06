# Changelog

## [Unreleased]

## [0.3.0] - 2026-11-06

### Changed

- **Breaking:** Support `autumn-web` 0.10. The plugin now needs `autumn-web`
  0.10 and Rust 1.97. Apps on `autumn-web` 0.9 stay on 0.2.0.

## [0.2.0] - 2026-10-31

### Changed

- Support `autumn-web` 0.9. The plugin now needs `autumn-web` 0.9.

### Added

- `TursoPlugin` for Autumn 0.8. It opens a Turso database at startup and
  closes it at shutdown.
- `[turso]` config with profile and `AUTUMN_TURSO__*` layers. Unknown keys
  stop startup.
- `Turso` handle in `AppState`, also an Axum extractor.
- Ordered, checksummed migrations.
- `turso` health indicator and `turso_*` metrics.
- Optional Turso Cloud sync (feature `sync`): pull at start, push at
  shutdown, timed loop. `sync.enabled` turns it off from a higher layer.
- `AUTUMN_TURSO__*` values from `.env` files, and the `--profile` flag.
- Safe concurrent migrations on one file. Line endings do not change the
  checksum.
- A sync auth token over plain `http://` to a host that is not loopback
  stops startup.
- `notes` example: a JSON notes API. Run it with
  `cargo run --example notes`.
