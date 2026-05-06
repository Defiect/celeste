use time::OffsetDateTime;

use super::remote::RemoteId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncDirId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncItemId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncDirExclusionId(pub i32);

/// A user-defined exclusion on a sync_dir. `remote_path` is relative to the
/// sync_dir's own `remote_path` (e.g. `"Videos"` on a sync_dir whose
/// `remote_path` is `"My files"`). The corresponding local subtree is derived
/// at runtime as `sync_dir.local_path + "/" + remote_path`.
#[derive(Clone, Debug)]
pub struct SyncDirExclusion {
    pub id: SyncDirExclusionId,
    pub sync_dir_id: SyncDirId,
    pub remote_path: String,
}

#[derive(Clone, Debug)]
pub struct SyncDir {
    pub id: SyncDirId,
    pub remote_id: RemoteId,
    pub local_path: String,
    pub remote_path: String,
}

#[derive(Clone, Debug)]
pub struct SyncItem {
    pub id: SyncItemId,
    pub sync_dir_id: SyncDirId,
    pub local_path: String,
    pub remote_path: String,
    pub last_local_timestamp: i64,
    pub last_remote_timestamp: i64,
}

#[derive(Clone, Debug)]
pub enum SyncStatus {
    Idle,
    Syncing,
    Ok { at_unix: i64 },
    Error { message: String },
}

/// A single entry on a remote filesystem, as returned by
/// [`crate::domain::ports::BackendClient`] listing / stat calls.
#[derive(Clone, Debug)]
pub struct RemoteItem {
    pub is_dir: bool,
    pub path: String,
    pub name: String,
    pub mod_time: OffsetDateTime,
}

/// Filter for `list` calls — matches rclone's `dirsOnly` / `filesOnly` options.
#[derive(Clone, Copy, Debug)]
pub enum ListFilter {
    All,
    Dirs,
    #[allow(dead_code)]
    Files,
}

/// Errors surfaced to the user for a single sync-dir pass.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SyncError {
    /// Catch-all: `(path, message)`.
    General(String, String),
    /// Local and remote copies both changed since the last sync; user must
    /// pick a winner. `(local_path, remote_path)`.
    BothMoreCurrent(String, String),
}
