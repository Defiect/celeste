use std::path::PathBuf;

use time::OffsetDateTime;

use super::remote::RemoteId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncDirId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncItemId(pub i32);

#[derive(Clone, Debug)]
pub struct SyncDir {
    pub id: SyncDirId,
    pub remote_id: RemoteId,
    pub local_path: PathBuf,
    pub remote_path: String,
}

#[derive(Clone, Debug)]
pub struct SyncItem {
    pub id: SyncItemId,
    pub sync_dir_id: SyncDirId,
    pub local_path: PathBuf,
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

#[derive(Clone, Debug)]
pub struct Conflict {
    pub sync_dir_id: SyncDirId,
    pub local_path: PathBuf,
    pub remote_path: String,
}

/// A single entry on a remote filesystem, as returned by
/// [`crate::domain::ports::RcloneClient`] listing / stat calls.
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
