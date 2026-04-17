//! Framework-agnostic application services.
//!
//! Services depend on port traits from [`crate::domain::ports`] — never on
//! infrastructure types directly. This lets [`SyncOrchestrator`] and
//! friends be unit-tested against in-memory fakes.

pub mod event_bus;
pub mod orchestrator;
pub mod remote_lifecycle;
pub mod should_sync;
pub mod sync_dir_ops;

pub use event_bus::{EventBus, event_channel};
pub use orchestrator::SyncOrchestrator;
