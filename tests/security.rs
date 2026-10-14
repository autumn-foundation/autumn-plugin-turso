//! AC11: secrets never leak.

use autumn_plugin_turso::{SyncConfig, Turso, TursoConfig, TursoHealthIndicator};
use autumn_web::actuator::HealthIndicator;

const TOKEN: &str = "very-secret-token";

#[test]
fn debug_hides_the_auth_token() {
    let config = TursoConfig::default()
        .sync(SyncConfig::new("libsql://db.example.turso.io").auth_token(TOKEN));
    let text = format!("{config:?} {:?}", config.sync);
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("<redacted>"), "{text}");
}

#[tokio::test]
async fn health_details_show_no_secrets() {
    let config = TursoConfig::new(":memory:");
    let db = Turso::open(&config).await.expect("open");
    let output = TursoHealthIndicator::for_handle(db).check().await;
    let mut keys: Vec<_> = output.details.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["mode", "path", "read_only"]);
}

#[test]
fn config_sources_debug_hides_env_values() {
    let sources = autumn_plugin_turso::ConfigSources::new(".", "dev")
        .env("AUTUMN_TURSO__SYNC__AUTH_TOKEN", TOKEN);
    let text = format!("{sources:?}");
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("AUTUMN_TURSO__SYNC__AUTH_TOKEN"), "{text}");
}
