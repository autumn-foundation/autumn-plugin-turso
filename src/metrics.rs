//! The `turso_*` metric families.

use autumn_web::actuator::{MetricFamily, MetricKind, MetricSample, MetricsSource};

use crate::handle::{Turso, TursoStats};
use crate::slot::{Slot, SlotState};

/// Exports `turso_*` families to `/actuator/prometheus`. The plugin
/// registers it as `turso`.
///
/// Counters count calls through the handle helpers and migrations. They do
/// not count statements on a connection from [`Turso::connect`].
#[derive(Clone)]
pub struct TursoMetricsSource {
    slot: Slot,
}

impl TursoMetricsSource {
    /// Make a source for one handle.
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

impl MetricsSource for TursoMetricsSource {
    fn collect(&self) -> Vec<MetricFamily> {
        let (up, stats) = match self.slot.get() {
            SlotState::Open(handle) => (!handle.is_closed(), handle.stats()),
            SlotState::Pending | SlotState::Disabled => (false, TursoStats::default()),
        };
        let counters = [
            (
                "turso_statements_total",
                "Statements run through the handle.",
                stats.statements,
            ),
            ("turso_errors_total", "Failed handle calls.", stats.errors),
            (
                "turso_connections_opened_total",
                "Connections opened.",
                stats.connections,
            ),
            (
                "turso_migrations_applied_total",
                "Migrations applied.",
                stats.migrations_applied,
            ),
            (
                "turso_sync_pushes_total",
                "Successful sync pushes.",
                stats.sync_pushes,
            ),
            (
                "turso_sync_pulls_total",
                "Successful sync pulls.",
                stats.sync_pulls,
            ),
            (
                "turso_sync_errors_total",
                "Failed sync calls.",
                stats.sync_errors,
            ),
        ];
        let mut families = vec![family(
            "turso_up",
            "1 when the database is open.",
            MetricKind::Gauge,
            u64::from(up),
        )];
        families.extend(
            counters
                .into_iter()
                .map(|(name, help, value)| family(name, help, MetricKind::Counter, value)),
        );
        families
    }
}

#[allow(clippy::cast_precision_loss)] // Counters stay far below 2^53.
fn family(name: &str, help: &str, kind: MetricKind, value: u64) -> MetricFamily {
    MetricFamily {
        name: name.to_owned(),
        help: help.to_owned(),
        kind,
        samples: vec![MetricSample {
            labels: Vec::new(),
            value: value as f64,
        }],
    }
}
