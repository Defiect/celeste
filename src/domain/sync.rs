use std::path::PathBuf;

use super::remote::RemoteId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncDirId(pub i32);

#[derive(Clone, Debug)]
pub struct SyncDir {
    pub id: SyncDirId,
    pub remote_id: RemoteId,
    pub local_path: PathBuf,
    pub remote_path: String,
}

#[derive(Clone, Debug)]
pub struct SyncItem {
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
