//! AC2: configuration layers, precedence and validation.

use std::fs;
use std::time::Duration;

use autumn_plugin_turso::{ConfigSources, TursoConfig, TursoError};

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
    assert_eq!(config.busy_timeout(), Duration::from_secs(5));
    assert!(config.migrate_on_start);
    assert!(config.sync.is_none());
}

#[test]
fn no_files_keep_the_base() {
    let dir = dir_with(&[]);
    let base = TursoConfig {
        path: "builder.db".into(),
        ..TursoConfig::default()
    };
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
    let config = TursoConfig {
        path: "  ".into(),
        ..TursoConfig::default()
    };
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
    let config = TursoConfig {
        path: ":memory:".into(),
        ..TursoConfig::default()
    };
    config.validate().expect("valid");
}
