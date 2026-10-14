//! The `[turso]` configuration section.
//!
//! Layers (lowest first):
//!
//! 1. Defaults.
//! 2. Builder setters on [`TursoPlugin`](crate::TursoPlugin).
//! 3. `[turso]` in `autumn.toml`.
//! 4. `[profile.<p>.turso]` in `autumn.toml`.
//! 5. `[turso]` in `autumn-<p>.toml`.
//! 6. `AUTUMN_TURSO__*` environment variables.
//!
//! An unknown key or a bad value is an error.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Result, TursoError};

/// The environment variable prefix for this section.
const ENV_PREFIX: &str = "AUTUMN_TURSO__";

/// The URL schemes that sync accepts.
const SYNC_SCHEMES: [&str; 4] = ["libsql://", "turso://", "https://", "http://"];

/// The resolved `[turso]` configuration.
///
/// Make one with [`TursoConfig::new`] or [`TursoConfig::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TursoConfig {
    /// Open the database at startup. Default: `true`.
    pub enabled: bool,
    /// The database file. `:memory:` gives an in-memory database.
    /// Default: `data/turso.db`.
    pub path: String,
    /// Open the database without write access. Default: `false`.
    pub read_only: bool,
    /// How long a connection waits for a lock, in milliseconds.
    /// Default: `5000`.
    pub busy_timeout_ms: u64,
    /// Apply pending migrations at startup. Default: `true`.
    pub migrate_on_start: bool,
    /// Turso Cloud sync (`[turso.sync]`). Default: off.
    pub sync: Option<SyncConfig>,
}

impl Default for TursoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "data/turso.db".to_owned(),
            read_only: false,
            busy_timeout_ms: 5_000,
            migrate_on_start: true,
            sync: None,
        }
    }
}

/// The `[turso.sync]` configuration. It needs the `sync` feature.
///
/// Make one with [`SyncConfig::new`].
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyncConfig {
    /// The remote database URL (`libsql://`, `turso://`, `https://` or
    /// `http://`).
    pub url: Option<String>,
    /// The auth token. `Debug` output hides it.
    pub auth_token: Option<String>,
    /// Pull remote changes at startup. Default: `true`.
    pub pull_on_start: bool,
    /// Push local changes at shutdown. Default: `true`.
    pub push_on_shutdown: bool,
    /// Push and pull every N seconds. `0` disables the loop. Default: `0`.
    pub interval_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            url: None,
            auth_token: None,
            pull_on_start: true,
            push_on_shutdown: true,
            interval_secs: 0,
        }
    }
}

impl fmt::Debug for SyncConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncConfig")
            .field("url", &self.url)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("pull_on_start", &self.pull_on_start)
            .field("push_on_shutdown", &self.push_on_shutdown)
            .field("interval_secs", &self.interval_secs)
            .finish()
    }
}

impl TursoConfig {
    /// The default config with this database path.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// Set `enabled`.
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set `read_only`.
    #[must_use]
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Set the lock wait time. The value is stored in milliseconds.
    #[must_use]
    pub fn busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self
    }

    /// Set `migrate_on_start`.
    #[must_use]
    pub const fn migrate_on_start(mut self, migrate: bool) -> Self {
        self.migrate_on_start = migrate;
        self
    }

    /// Set the sync config. It needs the `sync` feature.
    #[must_use]
    pub fn sync(mut self, sync: SyncConfig) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Apply the file and environment layers in `sources` on top of `base`.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Config`] for a file that is not valid TOML, an
    /// unknown key, or a bad value.
    pub fn resolve(base: Self, sources: &ConfigSources) -> Result<Self> {
        let mut config = base;
        let profile = sources.profile.as_str();

        if let Some((path, root)) = sources.read_toml("autumn.toml")? {
            if let Some(section) = root.get("turso") {
                config.apply(parse_patch(section, &path, "turso")?);
            }
            for name in inline_profile_names(profile) {
                let section = root
                    .get("profile")
                    .and_then(|profiles| profiles.get(name))
                    .and_then(|profile| profile.get("turso"));
                if let Some(section) = section {
                    let label = format!("profile.{name}.turso");
                    config.apply(parse_patch(section, &path, &label)?);
                }
            }
        }

        let file_names =
            autumn_web::config::profile_override_file_lookup_names(profile, &sources.profile_input);
        for name in file_names {
            if let Some((path, root)) = sources.read_toml(&format!("autumn-{name}.toml"))? {
                if let Some(section) = root.get("turso") {
                    config.apply(parse_patch(section, &path, "turso")?);
                }
                break;
            }
        }

        config.apply_env(&sources.env)?;
        Ok(config)
    }

    /// Check the values.
    ///
    /// # Errors
    ///
    /// Returns [`TursoError::Config`] for a bad value, and
    /// [`TursoError::SyncFeatureDisabled`] for `[turso.sync]` without the
    /// `sync` feature.
    pub fn validate(&self) -> Result<()> {
        if self.path.trim().is_empty() {
            return Err(TursoError::Config("`turso.path` must not be empty".into()));
        }
        let Some(sync) = &self.sync else {
            return Ok(());
        };
        let url = sync.url.as_deref().map(str::trim).unwrap_or_default();
        if url.is_empty() {
            return Err(TursoError::Config(
                "`turso.sync.url` is required when `[turso.sync]` is set".into(),
            ));
        }
        if !SYNC_SCHEMES.iter().any(|scheme| url.starts_with(scheme)) {
            return Err(TursoError::Config(format!(
                "`turso.sync.url` must start with one of {}; got `{url}`",
                SYNC_SCHEMES.join(", ")
            )));
        }
        if self.read_only {
            return Err(TursoError::Config(
                "`turso.read_only` and `[turso.sync]` cannot be used together".into(),
            ));
        }
        if let Some(token) = &sync.auth_token
            && (token.trim().is_empty() || token.trim().chars().any(char::is_control))
        {
            return Err(TursoError::Config(
                "`turso.sync.auth_token` is empty or has control characters".into(),
            ));
        }
        if sync.token().is_some() && url.starts_with("http://") && !is_loopback(url) {
            return Err(TursoError::Config(
                "`turso.sync.url` uses http:// with an auth token; use https:// or libsql://"
                    .into(),
            ));
        }
        if cfg!(feature = "sync") {
            Ok(())
        } else {
            Err(TursoError::SyncFeatureDisabled)
        }
    }

    fn apply(&mut self, patch: Patch) {
        let Patch {
            enabled,
            path,
            read_only,
            busy_timeout_ms,
            migrate_on_start,
            sync,
        } = patch;
        set(&mut self.enabled, enabled);
        set(&mut self.path, path);
        set(&mut self.read_only, read_only);
        set(&mut self.busy_timeout_ms, busy_timeout_ms);
        set(&mut self.migrate_on_start, migrate_on_start);
        if let Some(patch) = sync {
            self.sync
                .get_or_insert_with(SyncConfig::default)
                .apply(patch);
        }
    }

    fn apply_env(&mut self, env: &BTreeMap<String, String>) -> Result<()> {
        for (key, value) in env {
            let Some(name) = key.strip_prefix(ENV_PREFIX) else {
                continue;
            };
            match name {
                "ENABLED" => self.enabled = parse_env(key, value)?,
                "PATH" => self.path.clone_from(value),
                "READ_ONLY" => self.read_only = parse_env(key, value)?,
                "BUSY_TIMEOUT_MS" => self.busy_timeout_ms = parse_env(key, value)?,
                "MIGRATE_ON_START" => self.migrate_on_start = parse_env(key, value)?,
                _ => self.apply_sync_env(key, name, value)?,
            }
        }
        Ok(())
    }

    fn apply_sync_env(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
        let Some(field) = name.strip_prefix("SYNC__") else {
            return Err(unknown_env(key));
        };
        let sync = self.sync.get_or_insert_with(SyncConfig::default);
        match field {
            "URL" => sync.url = Some(value.to_owned()),
            "AUTH_TOKEN" => sync.auth_token = Some(value.to_owned()),
            "PULL_ON_START" => sync.pull_on_start = parse_env(key, value)?,
            "PUSH_ON_SHUTDOWN" => sync.push_on_shutdown = parse_env(key, value)?,
            "INTERVAL_SECS" => sync.interval_secs = parse_env(key, value)?,
            _ => return Err(unknown_env(key)),
        }
        Ok(())
    }
}

impl SyncConfig {
    /// The default sync config for the remote database at `url`.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            ..Self::default()
        }
    }

    /// Set the auth token.
    #[must_use]
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// The auth token without white space at the ends. `None` when empty.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.auth_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    fn apply(&mut self, patch: SyncPatch) {
        let SyncPatch {
            url,
            auth_token,
            pull_on_start,
            push_on_shutdown,
            interval_secs,
        } = patch;
        if url.is_some() {
            self.url = url;
        }
        if auth_token.is_some() {
            self.auth_token = auth_token;
        }
        set(&mut self.pull_on_start, pull_on_start);
        set(&mut self.push_on_shutdown, push_on_shutdown);
        set(&mut self.interval_secs, interval_secs);
    }
}

/// Where the plugin reads its configuration from.
#[derive(Clone)]
pub struct ConfigSources {
    dirs: Vec<PathBuf>,
    profile: String,
    profile_input: String,
    env: BTreeMap<String, String>,
}

impl ConfigSources {
    /// Read files from `dir` for `profile`. No environment variables apply.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, profile: impl Into<String>) -> Self {
        let profile = profile.into();
        Self {
            dirs: vec![dir.into()],
            profile_input: profile.clone(),
            profile,
            env: BTreeMap::new(),
        }
    }

    /// Read config files from `AUTUMN_MANIFEST_DIR`, then from the current
    /// directory. Read all `AUTUMN_TURSO__*` variables.
    #[must_use]
    pub fn from_process(profile: &str) -> Self {
        let mut dirs = Vec::new();
        if let Some(dir) = std::env::var_os("AUTUMN_MANIFEST_DIR") {
            dirs.push(PathBuf::from(dir));
        }
        dirs.push(PathBuf::from("."));
        let profile_input = ["AUTUMN_ENV", "AUTUMN_PROFILE"]
            .iter()
            .filter_map(|key| std::env::var(key).ok())
            .map(|value| value.trim().to_owned())
            .find(|value| !value.is_empty())
            .unwrap_or_else(|| profile.to_owned());
        let env = std::env::vars()
            .filter(|(key, _)| key.starts_with(ENV_PREFIX))
            .collect();
        Self {
            dirs,
            profile: profile.to_owned(),
            profile_input,
            env,
        }
    }

    /// Add or replace one environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// The active profile.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Find `name` in the first directory that has it and parse it.
    fn read_toml(&self, name: &str) -> Result<Option<(PathBuf, toml::Table)>> {
        let Some(path) = self
            .dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
        else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|err| TursoError::Config(format!("cannot read {}: {err}", path.display())))?;
        let table = text
            .parse::<toml::Table>()
            .map_err(|err| TursoError::Config(format!("{}: {err}", path.display())))?;
        Ok(Some((path, table)))
    }
}

impl fmt::Debug for ConfigSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Values can hold the auth token, so show keys only.
        f.debug_struct("ConfigSources")
            .field("dirs", &self.dirs)
            .field("profile", &self.profile)
            .field("profile_input", &self.profile_input)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    enabled: Option<bool>,
    path: Option<String>,
    read_only: Option<bool>,
    busy_timeout_ms: Option<u64>,
    migrate_on_start: Option<bool>,
    sync: Option<SyncPatch>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncPatch {
    url: Option<String>,
    auth_token: Option<String>,
    pull_on_start: Option<bool>,
    push_on_shutdown: Option<bool>,
    interval_secs: Option<u64>,
}

/// `true` when an `http://` URL names a loopback host.
fn is_loopback(url: &str) -> bool {
    let rest = url.trim_start_matches("http://");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.strip_prefix('[').map_or_else(
        || authority.split(':').next().unwrap_or_default(),
        |v6| v6.split(']').next().unwrap_or_default(),
    );
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn set<T>(slot: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *slot = value;
    }
}

fn parse_patch(section: &toml::Value, file: &Path, label: &str) -> Result<Patch> {
    section
        .clone()
        .try_into()
        .map_err(|err| TursoError::Config(format!("[{label}] in {}: {err}", file.display())))
}

/// Alias first, then canonical, as Autumn core does.
fn inline_profile_names(profile: &str) -> Vec<&str> {
    match profile {
        "prod" => vec!["production", "prod"],
        "dev" => vec!["development", "dev"],
        other => vec![other],
    }
}

fn parse_env<T: EnvValue>(key: &str, value: &str) -> Result<T> {
    T::parse(value.trim()).ok_or_else(|| {
        TursoError::Config(format!(
            "{key} has a bad value `{value}`; expected {}",
            T::EXPECTED
        ))
    })
}

fn unknown_env(key: &str) -> TursoError {
    TursoError::Config(format!("unknown environment variable {key}"))
}

trait EnvValue: Sized {
    const EXPECTED: &'static str;
    fn parse(value: &str) -> Option<Self>;
}

impl EnvValue for bool {
    const EXPECTED: &'static str = "true or false";
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        }
    }
}

impl EnvValue for u64 {
    const EXPECTED: &'static str = "a whole number";
    fn parse(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}
