# CLAUDE.md

## Commands

- Build: `cargo build`
- Lint: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test` and `cargo test --features sync`
- Sync tests: `TURSODB_BIN=<path> cargo test --features sync -- --ignored --skip live_round_trip`
- Docs: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`

## Rules

- Use TDD: write a failing test, make it pass, then refactor.
- Write comments and docs in ASD-STE100. Keep them short.
- Never log or show the sync auth token.
- Keep Turso default features off (no global allocator).
- A new sync test that needs a server is `#[ignore = "requires tursodb (TURSODB_BIN)"]`.
  CI runs it in the `sync-server` job.
- Record user-visible changes in `CHANGELOG.md` under `[Unreleased]`.

## Layout

| Path | Content |
| --- | --- |
| `src/plugin.rs` | `TursoPlugin`, startup and shutdown. |
| `src/config.rs` | `[turso]` layers and checks. |
| `src/handle.rs` | `Turso` handle and extractor. |
| `src/migrate.rs` | Migrations. |
| `src/health.rs`, `src/metrics.rs` | Actuator wiring. |
| `tests/` | Integration tests, one file per area. |
| `examples/notes/` | Example app. `tests/example.rs` includes `app.rs`. |
| `docs/plan.md` | Plan and acceptance criteria. |
