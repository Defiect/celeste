//! One full sync-dir pass: the should_sync gate plus the
//! sync_local_directory / sync_remote_directory calls, stitched into a
//! single function the caller can invoke per-sync-dir.
//!
//! Bundles the `synced_items` scratch buffer so callers don't have to.

use std::{cell::RefCell, path::Path};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::Remote,
        sync::SyncDir,
    },
    services::{should_sync::should_sync, sync_dir_ops},
};

/// What [`run`] ended up doing. Useful for status text decisions on the
/// caller side — e.g. whether to render "Files are synced." unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `should_sync` returned `false`; the sync functions were not called.
    UpToDate,
    /// `should_sync` returned `true` and both sync passes completed.
    Synced,
}

/// Run the should_sync gate and, if it fires, one local + one remote sync
/// pass. Emits `SyncEvent`s through `emit`.
#[allow(clippy::too_many_arguments)]
pub fn run<FE, FO, FD, FC>(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    emit: FE,
    check_open_requests: FO,
    process_deletion_requests: FD,
    is_cancelled: FC,
) -> Outcome
where
    FE: Fn(SyncEvent) + Clone,
    FO: Fn() + Clone,
    FD: Fn() + Clone,
    FC: Fn() -> bool + Clone,
{
    // should_sync pushes its own progress via SyncDirPending as it walks
    // the local tree, the remote tree, and the sync_items table.
    if !should_sync(remote, sync_dir, repo, client, emit.clone()) {
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text: tr::tr!("Files are synced."),
        });
        return Outcome::UpToDate;
    }

    let synced_items: RefCell<Vec<(String, String)>> = RefCell::new(vec![]);

    sync_dir_ops::sync_local_directory(
        Path::new(&sync_dir.local_path),
        remote,
        sync_dir,
        repo,
        client,
        &synced_items,
        emit.clone(),
        check_open_requests.clone(),
        process_deletion_requests.clone(),
        is_cancelled.clone(),
    );
    sync_dir_ops::sync_remote_directory(
        &sync_dir.remote_path,
        remote,
        sync_dir,
        repo,
        client,
        &synced_items,
        emit.clone(),
        check_open_requests,
        process_deletion_requests,
        is_cancelled,
    );

    emit(SyncEvent::SyncDirStatus {
        remote_id: remote.id,
        sync_dir_id: sync_dir.id,
        text: tr::tr!("Files are synced."),
    });
    Outcome::Synced
}
