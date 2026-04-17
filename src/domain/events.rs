use std::path::PathBuf;

use super::remote::RemoteId;

#[derive(Clone, Debug)]
pub enum SyncEvent {
    Started {
        remote_id: RemoteId,
    },
    Progress {
        remote_id: RemoteId,
        file: String,
        pct: f32,
    },
    Completed {
        remote_id: RemoteId,
        at_unix: i64,
    },
    Failed {
        remote_id: RemoteId,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub enum FsEvent {
    Changed { path: PathBuf },
}
