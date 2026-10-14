//! The shared slot that the plugin fills at startup.

use std::sync::{Arc, RwLock};

use crate::handle::Turso;

/// The plugin state that health and metrics read.
#[derive(Clone, Default)]
pub struct Slot(Arc<RwLock<SlotState>>);

#[derive(Clone, Default)]
pub enum SlotState {
    /// Startup did not run yet.
    #[default]
    Pending,
    /// `enabled = false`.
    Disabled,
    /// The database is open.
    Open(Turso),
}

impl Slot {
    pub fn with_handle(handle: Turso) -> Self {
        Self(Arc::new(RwLock::new(SlotState::Open(handle))))
    }

    pub fn set(&self, state: SlotState) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }

    pub fn get(&self) -> SlotState {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
