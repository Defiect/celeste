//! Path-targeted sync: sync exactly one local path (file) against its
//! remote counterpart, skipping the whole-tree walks that `sync_dir_ops`
//! does. Invoked from the fs_watcher fast path so a single file save
//! doesn't cause every file in the sync_dir to be re-examined.
//!
//! The periodic scheduler still runs the full walks — this path is a
//! best-effort shortcut for the common "one file changed" case. Anything
//! this function can't handle cleanly (directory operations, ambiguous
//! conflicts) is left for the next periodic pass to reconcile.

use std::{fs, path::Path, time::SystemTime};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::Remote,
        sync::{SyncDir, SyncError},
    },
    services::sync_dir_ops::{is_editor_temp, log_destructive_op},
    util,
};

/// Sync a single local path. The path must sit inside `sync_dir.local_path`.
/// Directory paths are ignored — their child events already cover the files,
/// and the periodic scheduler handles empty-dir and recursive-delete cases.
pub fn sync_single_path<FE>(
    path: &Path,
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    emit: FE,
) where
    FE: Fn(SyncEvent) + Clone,
{
    let Some(local_path) = path.to_str().map(str::to_owned) else {
        return;
    };

    // Skip editor swap/temp files: they churn faster than we can upload
    // and only produce `object not found` errors + stale DB rows.
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && is_editor_temp(name)
    {
        return;
    }

    // Reject paths outside this sync_dir.
    let prefix = format!("{}/", sync_dir.local_path);
    let rel = if local_path == sync_dir.local_path {
        ""
    } else if let Some(r) = local_path.strip_prefix(&prefix) {
        r
    } else {
        return;
    };

    let remote_path = if sync_dir.remote_path.is_empty() {
        rel.to_owned()
    } else if rel.is_empty() {
        sync_dir.remote_path.clone()
    } else {
        format!("{}/{}", sync_dir.remote_path, rel)
    };

    let add_error = |err: SyncError| {
        emit(SyncEvent::SyncDirError {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            error: err,
        });
    };
    let emit_status = |text: String| {
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };

    let db_item = util::await_future(
        repo.find_sync_item_by_paths(sync_dir.id, &local_path, &remote_path),
    )
    .unwrap_or(None);

    if path.exists() {
        // Directories are handled by the periodic scheduler; any file
        // inside will surface its own event.
        if path.is_dir() {
            return;
        }
        let local_ts = match local_timestamp(path) {
            Some(ts) => ts,
            None => return,
        };
        let remote_stat = match client.stat(&remote.name, &remote_path) {
            Ok(item) => item,
            Err(err) => {
                add_error(SyncError::General(remote_path.clone(), err));
                return;
            }
        };

        match (db_item, remote_stat) {
            (None, None) => {
                emit_status(tr::tr!("Uploading '{}'…", util::fmt_home(&local_path)));
                if let Err(err) =
                    client.copy_to_remote(&local_path, &remote.name, &remote_path)
                {
                    add_error(SyncError::General(local_path.clone(), err));
                    return;
                }
                record_insert(repo, sync_dir, &local_path, &remote_path, client, &remote.name);
            }
            (None, Some(rstat)) => {
                let remote_ts = rstat.mod_time.unix_timestamp();
                if local_ts as i64 > remote_ts {
                    emit_status(tr::tr!("Uploading '{}'…", util::fmt_home(&local_path)));
                    if let Err(err) =
                        client.copy_to_remote(&local_path, &remote.name, &remote_path)
                    {
                        add_error(SyncError::General(local_path.clone(), err));
                        return;
                    }
                    record_insert(
                        repo,
                        sync_dir,
                        &local_path,
                        &remote_path,
                        client,
                        &remote.name,
                    );
                } else if local_ts as i64 != remote_ts {
                    emit_status(tr::tr!("Downloading '{}'…", util::fmt_home(&local_path)));
                    if let Err(err) =
                        client.copy_to_local(&local_path, &remote.name, &remote_path)
                    {
                        add_error(SyncError::General(remote_path.clone(), err));
                        return;
                    }
                    record_insert(
                        repo,
                        sync_dir,
                        &local_path,
                        &remote_path,
                        client,
                        &remote.name,
                    );
                }
            }
            (Some(db), None) => {
                // Remote vanished since last sync. If the local side is
                // unchanged, the user's intent is closer to "push back",
                // but that means re-uploading a file that was deleted on
                // the remote — and we can't tell which was intentional.
                // Fall back to the periodic scheduler.
                let _ = db;
            }
            (Some(db), Some(rstat)) => {
                let remote_ts = rstat.mod_time.unix_timestamp();
                let local_changed = local_ts as i64 > db.last_local_timestamp;
                let remote_changed = remote_ts > db.last_remote_timestamp;
                if local_changed && remote_changed {
                    add_error(SyncError::BothMoreCurrent(
                        local_path.clone(),
                        remote_path.clone(),
                    ));
                } else if local_changed {
                    emit_status(tr::tr!("Uploading '{}'…", util::fmt_home(&local_path)));
                    if let Err(err) =
                        client.copy_to_remote(&local_path, &remote.name, &remote_path)
                    {
                        add_error(SyncError::General(local_path.clone(), err));
                        return;
                    }
                    let new_remote_ts = client
                        .stat(&remote.name, &remote_path)
                        .ok()
                        .flatten()
                        .map(|r| r.mod_time.unix_timestamp())
                        .unwrap_or(remote_ts);
                    let _ = util::await_future(repo.update_sync_item_timestamps(
                        db.id,
                        local_ts as i64,
                        new_remote_ts,
                    ));
                } else if remote_changed {
                    emit_status(tr::tr!(
                        "Downloading '{}'…",
                        util::fmt_home(&local_path)
                    ));
                    if let Err(err) =
                        client.copy_to_local(&local_path, &remote.name, &remote_path)
                    {
                        add_error(SyncError::General(remote_path.clone(), err));
                        return;
                    }
                    let new_local_ts = local_timestamp(path).unwrap_or(local_ts);
                    let _ = util::await_future(repo.update_sync_item_timestamps(
                        db.id,
                        new_local_ts as i64,
                        remote_ts,
                    ));
                }
                // else nothing changed relative to db — notify fired on
                // a touch that didn't actually modify the file.
            }
        }
    } else {
        // Local path is gone. If we have a db record and the remote is
        // still in the last-known state, mirror the deletion to the
        // remote. Otherwise leave it to the periodic scheduler.
        let Some(db) = db_item else { return };
        let rstat = match client.stat(&remote.name, &remote_path) {
            Ok(item) => item,
            Err(err) => {
                add_error(SyncError::General(remote_path.clone(), err));
                return;
            }
        };
        match rstat {
            None => {
                let _ = util::await_future(repo.delete_sync_item(db.id));
            }
            Some(r) if r.mod_time.unix_timestamp() == db.last_remote_timestamp => {
                log_destructive_op(
                    "sync_single_path/local-gone-remote-matches-db",
                    &remote.name,
                    &remote_path,
                );
                emit_status(tr::tr!("Removing '{}' on remote…", remote_path));
                let res = if r.is_dir {
                    client.purge(&remote.name, &remote_path)
                } else {
                    client.delete_file(&remote.name, &remote_path)
                };
                match res {
                    Ok(()) => {
                        let _ = util::await_future(repo.delete_sync_item_by_paths(
                            sync_dir.id,
                            &local_path,
                            &remote_path,
                        ));
                    }
                    Err(err) => {
                        add_error(SyncError::General(remote_path.clone(), err));
                    }
                }
            }
            Some(_) => {
                // Remote moved ahead since last sync — periodic pass can
                // decide whether to re-download or treat as conflict.
            }
        }
    }
}

fn local_timestamp(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn record_insert(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn RcloneClient,
    remote_name: &str,
) {
    let Some(local_ts) = local_timestamp(Path::new(local_path)) else {
        return;
    };
    let Some(rstat) = client.stat(remote_name, remote_path).ok().flatten() else {
        return;
    };
    let _ = util::await_future(repo.insert_sync_item(
        sync_dir.id,
        local_path.to_owned(),
        remote_path.to_owned(),
        local_ts as i64,
        rstat.mod_time.unix_timestamp(),
    ));
}
