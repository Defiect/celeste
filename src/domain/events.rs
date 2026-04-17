use super::{
    remote::RemoteId,
    sync::{SyncDirId, SyncError},
};

/// Events the sync services emit. UI adapters (GTK today, Iced later)
/// subscribe and translate into their native render calls.
#[derive(Clone, Debug)]
pub enum SyncEvent {
    /// A remote's full sync pass has begun.
    RemoteStarted {
        remote_id: RemoteId,
    },
    /// A remote's full sync pass finished successfully.
    RemoteCompleted {
        remote_id: RemoteId,
        at_unix: i64,
    },
    /// A remote's sync pass failed at the remote level (not a single file).
    RemoteFailed {
        remote_id: RemoteId,
        message: String,
    },
    /// Transient per-sync-dir status text (e.g. "Checking foo/ for changes...").
    SyncDirStatus {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        text: String,
    },
    /// Per-sync-dir error that should be surfaced in the error list.
    SyncDirError {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        error: SyncError,
    },
    /// Per-file progress (reserved for future streaming). Kept for shape
    /// compatibility with earlier sketches; current callers don't emit it.
    #[allow(dead_code)]
    FileProgress {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        file: String,
        pct: f32,
    },
}

#[derive(Clone, Debug)]
pub enum FsEvent {
    Changed { path: String },
}
