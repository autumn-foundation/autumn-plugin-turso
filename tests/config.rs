//! AC2: configuration layers, precedence and validation.

use std::fs;
use std::time::Duration;

use autumn_plugin_turso::{ConfigSources, SyncConfig, TursoConfig, TursoError};

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, body) in files {
        fs::write(dir.path().join(name), body).expect("write file");
    }
    dir
}

#[test]
fn defaults_are_safe() {
    let config = TursoConfig::default();
    assert!(config.enabled);
    assert_eq!(config.path, "data/turso.db");
    assert!(!config.read_only);
    assert_eq!(config.busy_timeout_ms, 5_000);
    assert_eq!(
        config
            .clone()
            .busy_timeout(Duration::from_secs(2))
            .busy_timeout_ms,
        2_000
    );
    assert!(config.migrate_on_start);
    assert!(config.sync.is_none());
}

#[test]
fn no_files_keep_the_base() {
    let dir = dir_with(&[]);
    let base = TursoConfig::new("builder.db");
    let config =
        TursoConfig::resolve(base, &ConfigSources::new(dir.path(), "dev")).expect("resolve");
    assert_eq!(config.path, "builder.db");
}

#[test]
fn layers_apply_in_order() {
    let dir = dir_with(&[
        (
            "autumn.toml",
            r#"
[turso]
path = "base.db"
busy_timeout_ms = 100
read_only = false

[profile.prod.turso]
path = "inline-prod.db"
busy_timeout_ms = 200
"#,
        ),
        (
            "autumn-prod.toml",
            r"
[turso]
busy_timeout_ms = 300
",
        ),
    ]);
    let sources = ConfigSources::new(dir.path(), "prod");
    let config = TursoConfig::resolve(TursoConfig::default(), &sources).expect("resolve");
    assert_eq!(config.path, "inline-prod.db");
    assert_eq!(config.busy_timeout_ms, 300);

    let sources = sources.env("AUTUMN_TURSO__BUSY_TIMEOUT_MS", "400");
    let config = TursoConfig::resolve(TursoConfig::default(), &sources).expect("resolve");
    assert_eq!(config.busy_timeout_ms, 400);
}

#[test]
fn other_profiles_do_not_apply() {
    let dir = dir_with(&[(
        "autumn.toml",
        r#"
[turso]
path = "base.db"

[profile.prod.turso]
path = "prod.db"
"#,
    )]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect("resolve");
    assert_eq!(config.path, "base.db");
}

#[test]
fn production_alias_file_is_read() {
    let dir = dir_with(&[("autumn-production.toml", "[turso]\npath = \"alias.db\"\n")]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "prod"),
    )
    .expect("resolve");
    assert_eq!(config.path, "alias.db");
}

#[test]
fn env_overrides_every_key() {
    let dir = dir_with(&[]);
    let sources = ConfigSources::new(dir.path(), "dev")
        .env("AUTUMN_TURSO__ENABLED", "false")
        .env("AUTUMN_TURSO__PATH", ":memory:")
        .env("AUTUMN_TURSO__READ_ONLY", "true")
        .env("AUTUMN_TURSO__BUSY_TIMEOUT_MS", "7")
        .env("AUTUMN_TURSO__MIGRATE_ON_START", "false")
        .env("AUTUMN_TURSO__SYNC__URL", "libsql://db.example.turso.io")
        .env("AUTUMN_TURSO__SYNC__AUTH_TOKEN", "secret")
        .env("AUTUMN_TURSO__SYNC__PULL_ON_START", "false")
        .env("AUTUMN_TURSO__SYNC__PUSH_ON_SHUTDOWN", "false")
        .env("AUTUMN_TURSO__SYNC__INTERVAL_SECS", "30");
    let config = TursoConfig::resolve(TursoConfig::default(), &sources).expect("resolve");
    assert!(!config.enabled);
    assert_eq!(config.path, ":memory:");
    assert!(config.read_only);
    assert_eq!(config.busy_timeout_ms, 7);
    assert!(!config.migrate_on_start);
    let sync = config.sync.expect("sync");
    assert_eq!(sync.url.as_deref(), Some("libsql://db.example.turso.io"));
    assert_eq!(sync.auth_token.as_deref(), Some("secret"));
    assert!(!sync.pull_on_start);
    assert!(!sync.push_on_shutdown);
    assert_eq!(sync.interval_secs, 30);
}

#[test]
fn unknown_key_fails_with_its_name() {
    let dir = dir_with(&[("autumn.toml", "[turso]\npaht = \"x.db\"\n")]);
    let err = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect_err("unknown key");
    assert!(matches!(err, TursoError::Config(_)), "{err:?}");
    assert!(err.to_string().contains("paht"), "{err}");
}

#[test]
fn unknown_env_key_fails() {
    let dir = dir_with(&[]);
    let sources = ConfigSources::new(dir.path(), "dev").env("AUTUMN_TURSO__PAHT", "x");
    let err = TursoConfig::resolve(TursoConfig::default(), &sources).expect_err("unknown env");
    assert!(err.to_string().contains("AUTUMN_TURSO__PAHT"), "{err}");
}

#[test]
fn bad_env_value_fails_with_key_and_value() {
    let dir = dir_with(&[]);
    let sources = ConfigSources::new(dir.path(), "dev").env("AUTUMN_TURSO__READ_ONLY", "maybe");
    let err = TursoConfig::resolve(TursoConfig::default(), &sources).expect_err("bad bool");
    let text = err.to_string();
    assert!(
        text.contains("AUTUMN_TURSO__READ_ONLY") && text.contains("maybe"),
        "{text}"
    );
}

#[test]
fn bad_toml_type_fails() {
    let dir = dir_with(&[("autumn.toml", "[turso]\nbusy_timeout_ms = \"slow\"\n")]);
    let err = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect_err("bad type");
    assert!(matches!(err, TursoError::Config(_)), "{err:?}");
}

#[test]
fn broken_toml_file_fails_with_its_path() {
    let dir = dir_with(&[("autumn.toml", "[turso\n")]);
    let err = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect_err("broken toml");
    assert!(err.to_string().contains("autumn.toml"), "{err}");
}

#[test]
fn empty_path_is_invalid() {
    let config = TursoConfig::new("  ");
    let err = config.validate().expect_err("empty path");
    assert!(err.to_string().contains("path"), "{err}");
}

#[test]
fn sync_without_url_is_invalid() {
    let dir = dir_with(&[("autumn.toml", "[turso.sync]\nauth_token = \"t\"\n")]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect("resolve");
    let err = config.validate().expect_err("no url");
    assert!(err.to_string().contains("url"), "{err}");
}

#[test]
fn sync_with_read_only_is_invalid() {
    let dir = dir_with(&[(
        "autumn.toml",
        "[turso]\nread_only = true\n[turso.sync]\nurl = \"https://x\"\n",
    )]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect("resolve");
    assert!(config.validate().is_err());
}

#[cfg(not(feature = "sync"))]
#[test]
fn sync_section_needs_the_sync_feature() {
    let dir = dir_with(&[("autumn.toml", "[turso.sync]\nurl = \"https://x\"\n")]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect("resolve");
    let err = config.validate().expect_err("feature off");
    assert!(matches!(err, TursoError::SyncFeatureDisabled), "{err:?}");
}

#[test]
fn memory_database_is_valid() {
    let config = TursoConfig::new(":memory:");
    config.validate().expect("valid");
}

#[test]
fn builders_set_fields() {
    let config = TursoConfig::new("x.db")
        .enabled(false)
        .read_only(true)
        .migrate_on_start(false)
        .sync(SyncConfig::new("https://db.example").auth_token(" t\n"));
    assert_eq!(config.path, "x.db");
    assert!(!config.enabled && config.read_only && !config.migrate_on_start);
    let sync = config.sync.expect("sync");
    assert_eq!(sync.url.as_deref(), Some("https://db.example"));
    assert_eq!(sync.token(), Some("t"), "token is trimmed");
}

#[test]
fn token_over_plain_http_is_rejected() {
    let config =
        TursoConfig::new("x.db").sync(SyncConfig::new("http://db.example.com").auth_token("t"));
    let err = config.validate().expect_err("cleartext token");
    assert!(err.to_string().contains("http://"), "{err}");
}

#[test]
fn plain_http_is_allowed_on_loopback_or_without_token() {
    for url in [
        "http://127.0.0.1:8080",
        "http://localhost:8080/db",
        "http://[::1]:8080",
    ] {
        let config = TursoConfig::new("x.db").sync(SyncConfig::new(url).auth_token("t"));
        assert!(
            !matches!(config.validate(), Err(TursoError::Config(_))),
            "{url} must be allowed"
        );
    }
    let config = TursoConfig::new("x.db").sync(SyncConfig::new("http://db.example.com"));
    assert!(!matches!(config.validate(), Err(TursoError::Config(_))));
}

#[test]
fn blank_or_control_token_is_rejected() {
    for token in ["", "  ", "ab\u{7}c"] {
        let config =
            TursoConfig::new("x.db").sync(SyncConfig::new("https://db.example").auth_token(token));
        let err = config.validate().expect_err("bad token");
        assert!(err.to_string().contains("auth_token"), "{token:?}: {err}");
    }
}

#[test]
fn file_uri_path_is_rejected() {
    let err = TursoConfig::new("file:app.db?mode=ro")
        .validate()
        .expect_err("uri");
    assert!(err.to_string().contains("file:"), "{err}");
}

#[test]
fn sync_enabled_false_turns_sync_off() {
    let dir = dir_with(&[("autumn.toml", "[turso.sync]\nurl = \"https://x\"\n")]);
    let sources = ConfigSources::new(dir.path(), "dev").env("AUTUMN_TURSO__SYNC__ENABLED", "false");
    let config = TursoConfig::resolve(TursoConfig::default(), &sources).expect("resolve");
    assert!(config.active_sync().is_none());
    config.validate().expect("valid without sync");
}

#[test]
fn inline_alias_and_canonical_profiles_apply_canonical_last() {
    let dir = dir_with(&[(
        "autumn.toml",
        r#"
[profile.production.turso]
path = "alias.db"
busy_timeout_ms = 1

[profile.prod.turso]
path = "canonical.db"
"#,
    )]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "prod"),
    )
    .expect("resolve");
    assert_eq!(config.path, "canonical.db");
    assert_eq!(config.busy_timeout_ms, 1, "alias still applies");
}

#[test]
fn development_alias_section_applies_to_dev() {
    let dir = dir_with(&[(
        "autumn.toml",
        "[profile.development.turso]\npath = \"dev-alias.db\"\n",
    )]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "dev"),
    )
    .expect("resolve");
    assert_eq!(config.path, "dev-alias.db");
}

#[test]
fn only_the_first_profile_file_is_read() {
    let dir = dir_with(&[
        ("autumn-prod.toml", "[turso]\npath = \"prod.db\"\n"),
        (
            "autumn-production.toml",
            "[turso]\npath = \"production.db\"\n",
        ),
    ]);
    let config = TursoConfig::resolve(
        TursoConfig::default(),
        &ConfigSources::new(dir.path(), "prod"),
    )
    .expect("resolve");
    assert_eq!(config.path, "prod.db");
}
