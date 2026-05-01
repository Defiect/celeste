use super::{
    remote::RemoteId,
    sync::{SyncDirId, SyncError},
};

/// Coarse run-state for one sync_dir. Drives the per-card status icon
/// on the remote page; finer per-event detail still comes through as
/// status / pending / error text events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncDirRunState {
    /// A pass is in flight (listing or applying actions).
    Syncing,
    /// The most recent pass finished cleanly with no per-file errors.
    Synced,
    /// The most recent pass surfaced warnings — rate-limit backoff,
    /// per-file errors, or sync conflicts.
    Warning,
    /// The most recent pass aborted before completing (snapshot fail,
    /// missing auth, etc.).
    Error,
}

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
    /// Primary per-sync-dir status text (e.g. "Synchronizing '/foo'…" or
    /// "Files are synced."). Renders on the sync_dir's main row.
    SyncDirStatus {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        text: String,
    },
    /// Secondary per-sync-dir "pending event" text (e.g. "Checking for
    /// changes…" or "Refresh queued…"). Renders on a second line under
    /// the main row, and is cleared once SyncDirStatus advances.
    SyncDirPending {
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
    /// Per-sync-dir coarse run-state transition. Drives the status icon
    /// on the remote page without forcing every consumer to string-match
    /// status text.
    SyncDirStateChanged {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        state: SyncDirRunState,
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
