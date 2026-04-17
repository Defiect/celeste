//! Thin mpsc-channel helper carrying [`SyncEvent`]s from the orchestrator
//! to any UI subscriber (GTK adapter today, Iced `Subscription` in Phase D).

use tokio::sync::mpsc;

use crate::domain::events::SyncEvent;

/// Buffer size for the event channel. 64 is ample for per-remote progress
/// bursts; slow UI consumers will see back-pressure rather than unbounded
/// memory growth.
const CHANNEL_CAPACITY: usize = 64;

pub type EventBus = mpsc::Sender<SyncEvent>;

pub fn event_channel() -> (mpsc::Sender<SyncEvent>, mpsc::Receiver<SyncEvent>) {
    mpsc::channel(CHANNEL_CAPACITY)
}
