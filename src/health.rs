//! The `turso` health indicator.

use std::collections::HashMap;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator};
use futures::future::BoxFuture;
use serde_json::{Value, json};

use crate::handle::Turso;
use crate::slot::{Slot, SlotState};

/// Reports `UP` when `SELECT 1` works, and `DOWN` when the database is
/// closed or the query fails. The plugin registers it as `turso`.
///
/// Details hold `path`, `mode` and `read_only`. They never hold the sync URL
/// or the auth token.
#[derive(Clone)]
pub struct TursoHealthIndicator {
    slot: Slot,
}

impl TursoHealthIndicator {
    /// Make an indicator for one handle. Use this to register a second
    /// database by hand.
    #[must_use]
    pub fn for_handle(handle: Turso) -> Self {
        Self {
            slot: Slot::with_handle(handle),
        }
    }

    pub(crate) const fn from_slot(slot: Slot) -> Self {
        Self { slot }
    }
}

impl HealthIndicator for TursoHealthIndicator {
    fn check(&self) -> BoxFuture<'_, HealthCheckOutput> {
        Box::pin(async move {
            match self.slot.get() {
                SlotState::Pending => down("not started", HashMap::new()),
                SlotState::Disabled => HealthCheckOutput::up()
                    .with_details(HashMap::from([("enabled".to_owned(), json!(false))])),
                SlotState::Open(handle) => {
                    let details = details(&handle);
                    match handle.ping().await {
                        Ok(()) => HealthCheckOutput::up().with_details(details),
                        Err(err) => down(&err.to_string(), details),
                    }
                }
            }
        })
    }
}

fn details(handle: &Turso) -> HashMap<String, Value> {
    let mode = if handle.is_synced() { "sync" } else { "local" };
    HashMap::from([
        ("path".to_owned(), json!(handle.path())),
        ("mode".to_owned(), json!(mode)),
        ("read_only".to_owned(), json!(handle.is_read_only())),
    ])
}

fn down(reason: &str, mut details: HashMap<String, Value>) -> HealthCheckOutput {
    details.insert("error".to_owned(), json!(reason));
    HealthCheckOutput::down().with_details(details)
}
