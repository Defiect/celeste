//! [`SyncOrchestrator`] — owns per-remote sync tasks and exposes the
//! small surface the UI needs: start, refresh, shutdown.
//!
//! This is a scaffold. Per-remote `tokio` task spawning arrives in Phase B
//! (parallel sync) and the actual sync algorithm is extracted from
//! `launch.rs` in the remaining Phase A commits.

use std::{collections::HashMap, sync::Arc};

use crate::domain::{
    ports::{RcloneClient, Repository},
    remote::RemoteId,
};

use super::event_bus::EventBus;

/// Handle to a single remote's scheduler task. Populated once per-remote
/// tokio tasks are spawned in Phase B.
#[allow(dead_code)]
struct RemoteTaskHandle {
    // cmd_tx: mpsc::Sender<SchedulerCommand>,
    // join: JoinHandle<()>,
}

pub struct SyncOrchestrator {
    repo: Arc<dyn Repository>,
    rclone: Arc<dyn RcloneClient>,
    events_tx: EventBus,
    #[allow(dead_code)]
    tasks: HashMap<RemoteId, RemoteTaskHandle>,
}

impl SyncOrchestrator {
    pub fn new(
        repo: Arc<dyn Repository>,
        rclone: Arc<dyn RcloneClient>,
        events_tx: EventBus,
    ) -> Self {
        Self {
            repo,
            rclone,
            events_tx,
            tasks: HashMap::new(),
        }
    }

    /// Access to the ports — used by the GTK adapter during the transitional
    /// phase where `launch.rs` still runs the sync loop itself.
    pub fn repo(&self) -> &Arc<dyn Repository> {
        &self.repo
    }

    pub fn rclone(&self) -> &Arc<dyn RcloneClient> {
        &self.rclone
    }

    pub fn events_tx(&self) -> &EventBus {
        &self.events_tx
    }
}
