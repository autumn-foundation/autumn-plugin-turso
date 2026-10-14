//! AC11: secrets never leak.

use autumn_plugin_turso::{SyncConfig, Turso, TursoConfig, TursoHealthIndicator};
use autumn_web::actuator::HealthIndicator;

const TOKEN: &str = "very-secret-token";

#[test]
fn debug_hides_the_auth_token() {
    let config = TursoConfig {
        sync: Some(SyncConfig {
            url: Some("libsql://db.example.turso.io".into()),
            auth_token: Some(TOKEN.into()),
            ..SyncConfig::default()
        }),
        ..TursoConfig::default()
    };
    let text = format!("{config:?} {:?}", config.sync);
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("<redacted>"), "{text}");
}

#[tokio::test]
async fn health_details_show_no_secrets() {
    let config = TursoConfig {
        path: ":memory:".into(),
        ..TursoConfig::default()
    };
    let db = Turso::open(&config).await.expect("open");
    let output = TursoHealthIndicator::for_handle(db).check().await;
    let mut keys: Vec<_> = output.details.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["mode", "path", "read_only"]);
}
