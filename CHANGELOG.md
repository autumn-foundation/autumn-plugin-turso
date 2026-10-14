# Changelog

## [Unreleased]

### Added

- `TursoPlugin` for Autumn 0.8. It opens a Turso database at startup and
  closes it at shutdown.
- `[turso]` config with profile and `AUTUMN_TURSO__*` layers. Unknown keys
  stop startup.
- `Turso` handle in `AppState`, also an Axum extractor.
- Ordered, checksummed migrations.
- `turso` health indicator and `turso_*` metrics.
- Optional Turso Cloud sync (feature `sync`): pull at start, push at
  shutdown, timed loop.
