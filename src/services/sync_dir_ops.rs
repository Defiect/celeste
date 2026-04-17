//! Per-sync-dir sync algorithm. Framework-agnostic: the functions take a
//! `update_status` callback and an `add_error` callback so the caller
//! decides how to render progress and errors (GTK widget mutation in the
//! current launch.rs path, Iced/`SyncEvent` emissions later).

use std::{cell::RefCell, fs, path::Path, time::SystemTime};

use file_lock::{FileLock, FileOptions};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::Remote,
        sync::{ListFilter, RemoteItem, SyncDir},
    },
    util,
};

pub use crate::domain::sync::SyncError;

// sync_dir_ops now lives purely on domain types — no infrastructure leak.

/// Name of the per-sync-dir ignore file (one glob per line).
pub static FILE_IGNORE_NAME: &str = ".sync-exclude.lst";

/// Log a destructive op about to fire so incidents leave a trail. The
/// 2026-04-17 bulk-trash on Google Drive had no logs; every remote/local
/// delete the sync algorithm decides to run now announces itself here.
pub fn log_destructive_op(kind: &str, remote_name: &str, path: &str) {
    eprintln!("sync: DELETE {kind} remote={remote_name} path={path}");
}

/// Editor and toolchain temp-file patterns that should never touch the
/// remote. Syncing these is always wrong: the file lives for seconds,
/// then the editor deletes it, and the sync layer ends up with half-
/// uploaded files, spurious "object not found" errors, and DB records
/// that provoke later `local-delete-mirroring-remote` passes.
pub fn is_editor_temp(name: &str) -> bool {
    // Kate swap.
    if name.ends_with(".kate-swp") {
        return true;
    }
    // Vim swap files: ".foo.swp", ".foo.swo", ".foo.swn".
    if name.starts_with('.')
        && (name.ends_with(".swp") || name.ends_with(".swo") || name.ends_with(".swn"))
    {
        return true;
    }
    // Emacs lock symlinks and autosave.
    if name.starts_with(".#") {
        return true;
    }
    if name.starts_with('#') && name.ends_with('#') {
        return true;
    }
    // Kate / generic backup.
    if name.ends_with('~') {
        return true;
    }
    // GIO/Nautilus copy staging.
    if name.starts_with(".goutputstream-") {
        return true;
    }
    // Browser partial downloads.
    if name.ends_with(".crdownload") || name.ends_with(".part") {
        return true;
    }
    false
}

// Returning an [`Err<()>`] means this directory has to stop being synced
// because it was in the deletion queue. Any other error should return an
// [`Ok<()>`].
#[allow(clippy::too_many_arguments)]
pub fn sync_local_directory<FE, FO, FD, FC>(
    local_dir: &Path,
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    synced_items: &RefCell<Vec<(String, String)>>,
    emit: FE,
    check_open_requests: FO,
    process_deletion_requests: FD,
    is_cancelled: FC,
) where
    FE: Fn(SyncEvent) + Clone,
    FO: Fn() + Clone,
    FD: Fn() + Clone,
    FC: Fn() -> bool + Clone,
{
    process_deletion_requests();

    let sync_dir_still_exists = || {
        util::await_future(repo.sync_dir_exists(&sync_dir.local_path, &sync_dir.remote_path))
            .unwrap_or(false)
    };

    let add_error = |err: SyncError| {
        emit(SyncEvent::SyncDirError {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            error: err,
        });
    };

    let dir_string = local_dir.to_str().unwrap().to_owned();
    // Walking files is NOT syncing — it's just checking. Render it as
    // a pending event so the main status line is reserved for real
    // transfer operations.
    let update_ui_progress = |dir: &str| {
        if !sync_dir_still_exists() {
            return;
        }
        emit(SyncEvent::SyncDirPending {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text: tr::tr!("Examining '{}'…", util::fmt_home(dir)),
        });
    };
    // Real rclone transfer helpers — these drive the primary status line.
    let emit_status = |text: String| {
        if !sync_dir_still_exists() {
            return;
        }
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    update_ui_progress(&dir_string);
    let directory = match fs::read_dir(local_dir) {
        Ok(ok_dir) => ok_dir,
        Err(err) => {
            add_error(SyncError::General(dir_string, err.to_string()));
            return;
        }
    };

    // Get the list of ignore globs.
    let ignore_file_string = format!("{}/{}", sync_dir.local_path, FILE_IGNORE_NAME);
    let ignore_file_path = Path::new(&ignore_file_string);
    let ignore_globs = if ignore_file_path.exists() {
        let _lock = FileLock::lock(
            &ignore_file_string,
            true,
            FileOptions::new().write(true).read(true),
        )
        .unwrap();
        let file_content = fs::read_to_string(ignore_file_path).unwrap();
        let mut globs = vec![];

        for line in file_content.lines() {
            if let Ok(pattern) = glob::Pattern::new(line) {
                globs.push(pattern);
            }
        }

        globs
    } else {
        vec![]
    };

    for item in directory {
        // If a close request was sent in, stop syncing this remote so we can
        // quit the application in the 'main loop.
        if is_cancelled() {
            break;
        }

        // Check for open requests.
        check_open_requests();

        // If this directory no longer exists in the database (i.e. from being
        // deleted from the `sync_dir_deletion_queue`), stop processing and return.
        if !sync_dir_still_exists() {
            break;
        }

        if let Err(err) = item {
            add_error(SyncError::General(dir_string.clone(), err.to_string()));
            continue;
        }
        let item = item.unwrap();
        let local_path = item.path().to_str().unwrap().to_owned();

        if let Some(name) = item.file_name().to_str()
            && is_editor_temp(name)
        {
            continue;
        }

        // The path from the root of the remote.
        let remote_path = {
            let local_path_stripped = local_path
                .strip_prefix(&format!("{}/", sync_dir.local_path))
                .unwrap();
            let stripped_path = match local_path_stripped.strip_suffix('/') {
                Some(string) => string,
                None => local_path_stripped,
            };

            if sync_dir.remote_path.is_empty() {
                stripped_path.to_owned()
            } else {
                sync_dir.remote_path.clone() + "/" + stripped_path
            }
        };
        // The above path, with `sync_dir.remote_path` stripped from it.
        let stripped_remote_path =
            if remote_path.contains('/') && sync_dir.remote_path.contains('/') {
                remote_path
                    .strip_prefix(&format!("{}/", sync_dir.remote_path))
                    .unwrap()
                    .to_owned()
            } else {
                remote_path.clone()
            };

        update_ui_progress(&local_path);
        // If this item matches the ignore list, don't sync it.
        if ignore_globs
            .iter()
            .filter(|pattern| pattern.matches(&stripped_remote_path))
            .count()
            > 0
        {
            continue;
        }

        synced_items
            .borrow_mut()
            .push((local_path.clone(), remote_path.clone()));

        let get_local_file_timestamp = || {
            item.metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        let local_utc_timestamp = get_local_file_timestamp();
        let remote_item = match client.stat(&remote.name, &remote_path) {
            Ok(item) => item,
            Err(err) => {
                add_error(SyncError::General(remote_path.clone(), err));
                continue;
            }
        };
        let remote_utc_timestamp = remote_item
            .as_ref()
            .map(|item| item.mod_time.unix_timestamp());
        let db_item = util::await_future(repo.find_sync_item_by_paths(
            sync_dir.id,
            &local_path,
            &remote_path,
        ))
        .unwrap_or(None);

        // Push the item to the remote. Returns the
        // [`crate::infrastructure::rclone::sync::RcloneRemoteItem`] of the item on the remote, or
        // an [`Err<()>`] if an issue occurred (all errors are automatically added
        // via `add_errors`).
        let push_local_to_remote = || -> Result<RemoteItem, ()> {
            let file_type = item.file_type().unwrap();

            if let Some(rclone_item) = &remote_item {
                let same_type = file_type.is_dir() && rclone_item.is_dir;

                if !same_type {
                    log_destructive_op(
                        "purge-on-type-mismatch",
                        &remote.name,
                        &remote_path,
                    );
                    if let Err(err) = client.purge(&remote.name, &remote_path) {
                        add_error(SyncError::General(remote_path.clone(), err));
                        return Err(());
                    }
                }
            }

            if file_type.is_dir() {
                if let Err(err) = client.mkdir(&remote.name, &remote_path) {
                    add_error(SyncError::General(remote_path.clone(), err));
                    return Err(());
                }
                sync_local_directory(
                    &item.path(),
                    remote,
                    sync_dir,
                    repo,
                    client,
                    synced_items,
                    emit.clone(),
                    check_open_requests.clone(),
                    process_deletion_requests.clone(),
                    is_cancelled.clone(),
                );
                update_ui_progress(&local_path);
            } else {
                emit_status(tr::tr!(
                    "Uploading '{}'…",
                    util::fmt_home(&local_path)
                ));
                if let Err(err) =
                    client.copy_to_remote(&local_path, &remote.name, &remote_path)
                {
                    add_error(SyncError::General(local_path.clone(), err));
                    return Err(());
                }
            }

            Ok(client.stat(&remote.name, &remote_path)
                .unwrap()
                .unwrap())
        };
        // Pull the item from the remote.
        let pull_remote_to_local = || -> Result<(), ()> {
            let file_type = item.file_type().unwrap();
            let same_type = file_type.is_dir() && remote_item.as_ref().unwrap().is_dir;

            if !same_type {
                if file_type.is_dir()
                    && let Err(err) = fs::remove_dir_all(item.path())
                {
                    add_error(SyncError::General(local_path.clone(), err.to_string()));
                    return Err(());
                } else if let Err(err) = fs::remove_file(item.path()) {
                    add_error(SyncError::General(local_path.clone(), err.to_string()));
                    return Err(());
                }
            }

            if file_type.is_dir() {
                sync_local_directory(
                    &item.path(),
                    remote,
                    sync_dir,
                    repo,
                    client,
                    synced_items,
                    emit.clone(),
                    check_open_requests.clone(),
                    process_deletion_requests.clone(),
                    is_cancelled.clone(),
                );
                update_ui_progress(&local_path);
            } else {
                emit_status(tr::tr!(
                    "Downloading '{}'…",
                    util::fmt_home(&local_path)
                ));
                if let Err(err) =
                    client.copy_to_local(&local_path, &remote.name, &remote_path)
                {
                    add_error(SyncError::General(remote_path.clone(), err));
                    return Err(());
                }
            }

            Ok(())
        };
        // Delete this item from the database.
        let delete_db_entry = || {
            let _ = util::await_future(repo.delete_sync_item_by_paths(
                sync_dir.id,
                &local_path,
                &remote_path,
            ));
        };

        // If we have a record of the last sync, use that to aid in timestamp
        // checks.
        if let Some(db_model) = db_item {
            let update_db_item = |local_timestamp: i64, remote_timestamp: i64| {
                let _ = util::await_future(repo.update_sync_item_timestamps(
                    db_model.id,
                    local_timestamp,
                    remote_timestamp,
                ));
            };

            // Both items are more current than at the last transaction - we need to
            // let the user decide which to keep.
            if local_utc_timestamp > db_model.last_local_timestamp as u64
                && let Some(remote_timestamp) = remote_utc_timestamp
                && remote_timestamp > db_model.last_remote_timestamp
            {
                // Only add the error if one of the items is not a directory -
                // there's no point in saying both directories are more current, and
                // it's probably because one of the items in the directory got
                // updated anyway.
                if let Some(r_item) = remote_item
                    && (!item.path().is_dir() || !r_item.is_dir)
                {
                    add_error(SyncError::BothMoreCurrent(
                        local_path.clone(),
                        remote_path.clone(),
                    ));
                }
            // The local item is more recent.
            } else if local_utc_timestamp > db_model.last_local_timestamp as u64 {
                if let Ok(rclone_item) = push_local_to_remote() {
                    update_db_item(
                        get_local_file_timestamp() as i64,
                        rclone_item.mod_time.unix_timestamp(),
                    );
                    continue;
                } else {
                    continue;
                }
            // The remote item is more recent.
            } else if let Some(remote_timestamp) = remote_utc_timestamp
                && remote_timestamp > db_model.last_remote_timestamp
            {
                if pull_remote_to_local().is_err() {
                    continue;
                } else {
                    update_db_item(get_local_file_timestamp() as i64, remote_timestamp);
                }
            // The item is missing from the remote, but the last
            // recorded timestamp for the local item is still
            // the same. This means the item got deleted on the
            // server, and we need to reflect such locally.
            } else if remote_item.is_none()
                && local_utc_timestamp == db_model.last_local_timestamp as u64
            {
                // Defensive: rclone's Google Drive backend has been
                // observed returning Ok(None) from operations/stat for
                // files that really are present, right after a mutation
                // elsewhere in the same parent directory. Verify via a
                // fresh list before we mirror the supposed deletion
                // locally — the 2026-04-17 incident trashed 17 top-
                // level files in one pass because of this exact race.
                let (parent, filename) =
                    remote_path.rsplit_once('/').unwrap_or(("", &remote_path));
                let still_on_remote = match client.list(
                    &remote.name,
                    parent,
                    false,
                    ListFilter::All,
                ) {
                    Ok(items) => items.iter().any(|i| i.name == filename),
                    Err(_) => true,
                };
                if still_on_remote {
                    eprintln!(
                        "sync: ABORT local-delete-mirroring-remote remote={} path={} — stat returned None but list shows the file is still present (rclone cache race).",
                        remote.name, remote_path,
                    );
                    continue;
                }
                log_destructive_op(
                    "local-delete-mirroring-remote",
                    &remote.name,
                    &remote_path,
                );
                emit_status(tr::tr!(
                    "Removing '{}' locally…",
                    util::fmt_home(&local_path)
                ));
                if item.path().is_dir() {
                    if let Err(err) = fs::remove_dir_all(&local_path) {
                        add_error(SyncError::General(local_path.clone(), err.to_string()));
                        continue;
                    }
                } else if let Err(err) = fs::remove_file(&local_path) {
                    add_error(SyncError::General(local_path.clone(), err.to_string()));
                    continue;
                }

                delete_db_entry();
                continue;
            // Both the local and remote item remain unchanged -
            // do nothing.
            } else if local_utc_timestamp == db_model.last_local_timestamp as u64
                && let Some(remote_timestamp) = remote_utc_timestamp
                && remote_timestamp == db_model.last_remote_timestamp
            {
                continue;
            // Every possible scenario should have been covered
            // above, so panic if not.
            } else {
                unreachable!();
            }
        // Otherwise just check the local timestamps against
        // those on the remote, and record our new transaction
        // in the database.
        } else {
            // If the timestamp exists, then the remote item did, so check
            // timestamps.
            if let Some(remote_timestamp) = remote_utc_timestamp {
                if local_utc_timestamp > remote_timestamp as u64 {
                    if push_local_to_remote().is_err() {
                        continue;
                    }
                } else if pull_remote_to_local().is_err() {
                    continue;
                }
            // Otherwise the remote item didn't exist, so just
            // sync our local copy.
            } else if push_local_to_remote().is_err() {
                continue;
            }

            // The remote item is now guaranteed to exist, so fetch it.
            let remote_item_safe = match client.stat(&remote.name, &remote_path) {
                Ok(item) => item.unwrap(),
                Err(err) => {
                    add_error(SyncError::General(remote_path.clone(), err));
                    continue;
                }
            };
            match client.stat(&remote.name, &remote_path) {
                Ok(item) => item.unwrap(),
                Err(err) => {
                    add_error(SyncError::General(remote_path.clone(), err));
                    continue;
                }
            };

            // Record the current transaction's timestamps in the database.
            let _ = util::await_future(repo.insert_sync_item(
                sync_dir.id,
                local_path.clone(),
                remote_path.clone(),
                local_utc_timestamp as i64,
                remote_item_safe.mod_time.unix_timestamp(),
            ));
        }
    }
}

// Sync a remote directory. It's implemented as a function because of the same
// logic for `fn sync_local_directory` above.
// - NOTE: `remote_dir` should be: 1. the path with any `/` prefix/suffix
//   removed 2. the full path from the root of the remote server.
#[allow(clippy::too_many_arguments)]
pub fn sync_remote_directory<FE, FO, FD, FC>(
    remote_dir: &str,
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    synced_items: &RefCell<Vec<(String, String)>>,
    emit: FE,
    check_open_requests: FO,
    process_deletion_requests: FD,
    is_cancelled: FC,
) where
    FE: Fn(SyncEvent) + Clone,
    FO: Fn() + Clone,
    FD: Fn() + Clone,
    FC: Fn() -> bool + Clone,
{
    process_deletion_requests();

    let sync_dir_still_exists = || {
        util::await_future(repo.sync_dir_exists(&sync_dir.local_path, &sync_dir.remote_path))
            .unwrap_or(false)
    };

    let add_error = |err: SyncError| {
        emit(SyncEvent::SyncDirError {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            error: err,
        });
    };

    let ignore_file_string = format!("{}/{}", sync_dir.local_path, FILE_IGNORE_NAME);
    let ignore_file_path = Path::new(&ignore_file_string);
    let ignore_globs = if ignore_file_path.exists() {
        let _lock = FileLock::lock(
            ignore_file_path,
            true,
            FileOptions::new().write(true).read(true),
        )
        .unwrap();
        let file_content = fs::read_to_string(ignore_file_path).unwrap();
        let mut globs = vec![];

        for line in file_content.lines() {
            if let Ok(pattern) = glob::Pattern::new(line) {
                globs.push(pattern);
            }
        }

        globs
    } else {
        vec![]
    };
    let update_ui_progress = |dir: &str| {
        if !sync_dir_still_exists() {
            return;
        }
        emit(SyncEvent::SyncDirPending {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text: tr::tr!("Examining '{}' on remote…", dir),
        });
    };
    let emit_status = |text: String| {
        if !sync_dir_still_exists() {
            return;
        }
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    update_ui_progress(remote_dir);
    let items = match client.list(&remote.name, remote_dir, false, ListFilter::All) {
        Ok(ok_items) => ok_items,
        Err(err) => {
            add_error(SyncError::General(remote_dir.to_owned(), err));
            return;
        }
    };

    for item in items {
        // If a close request was sent in, stop syncing this remote so we can quit
        // the application in the 'main loop.
        if is_cancelled() {
            break;
        }

        // Check for open requests.
        check_open_requests();

        // If this directory no longer exists in the database (i.e. from being
        // deleted from the `sync_dir_deletion_queue`), stop processing and return.
        if !sync_dir_still_exists() {
            break;
        }

        if is_editor_temp(&item.name) {
            continue;
        }

        // If this item matches the ignore filter, don't sync it.
        if ignore_globs
            .iter()
            .filter(|pattern| pattern.matches(&item.path))
            .count()
            > 0
        {
            continue;
        }

        let remote_path_string = item.path.clone();
        // rclone returns each item's `path` relative to the remote filesystem
        // root, so it usually starts with sync_dir.remote_path. Fall back to
        // the raw path if it doesn't (e.g., when remote_path was stored with
        // odd normalisation), then trim the separator so we don't end up
        // with "<local>//<file>".
        let relative = item
            .path
            .strip_prefix(&sync_dir.remote_path)
            .unwrap_or(&item.path)
            .trim_start_matches('/');
        let local_path_string = if relative.is_empty() {
            sync_dir.local_path.clone()
        } else {
            format!("{}/{}", sync_dir.local_path, relative)
        };
        update_ui_progress(&remote_path_string);

        // If we've already synced this directory from `fn sync_local_directory`
        // above, don't sync it again.
        if synced_items
            .borrow()
            .contains(&(local_path_string.clone(), remote_path_string.clone()))
        {
            continue;
        }

        let local_path = Path::new(&local_path_string);
        let remote_timestamp = item.mod_time.unix_timestamp();
        let get_local_file_timestamp = || {
            local_path.metadata().ok().map(|metadata| {
                metadata
                    .modified()
                    .unwrap()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            })
        };
        let local_timestamp = get_local_file_timestamp();
        let db_item = util::await_future(repo.find_sync_item_by_paths(
            sync_dir.id,
            &local_path_string,
            &remote_path_string,
        ))
        .unwrap_or(None);

        // Push the item from the local machine to the remote machine. Returns the
        // timestamp of the new file on the remote. Returns the
        // [`crate::infrastructure::rclone::sync::RcloneRemoteItem`] of the item on the remote, or
        // an [`Err<()>`] if an issue occurred (all errors are automatically added
        // via `add_errors`).
        let push_local_to_remote = || {
            if local_path.is_dir() {
                if !item.is_dir {
                    log_destructive_op(
                        "delete-remote-file-on-type-mismatch",
                        &remote.name,
                        &remote_path_string,
                    );
                    if let Err(err) = client.delete_file(&remote.name, &remote_path_string) {
                        add_error(SyncError::General(
                            remote_path_string.clone(),
                            err,
                        ));
                        return Err(());
                    }

                    if let Err(err) = client.mkdir(&remote.name, &remote_path_string) {
                        add_error(SyncError::General(
                            remote_path_string.clone(),
                            err,
                        ));
                        return Err(());
                    }
                }

                sync_remote_directory(
                    &item.path,
                    remote,
                    sync_dir,
                    repo,
                    client,
                    synced_items,
                    emit.clone(),
                    check_open_requests.clone(),
                    process_deletion_requests.clone(),
                    is_cancelled.clone(),
                );
                update_ui_progress(&remote_path_string);
            } else {
                if item.is_dir {
                    log_destructive_op(
                        "purge-remote-dir-on-type-mismatch",
                        &remote.name,
                        &remote_path_string,
                    );
                    if let Err(err) = client.purge(&remote.name, &remote_path_string) {
                        add_error(SyncError::General(
                            remote_path_string.clone(),
                            err,
                        ));
                        return Err(());
                    }
                }
                emit_status(tr::tr!(
                    "Uploading '{}'…",
                    util::fmt_home(&local_path_string)
                ));
                if let Err(err) = client.copy_to_remote(
                    &local_path_string,
                    &remote.name,
                    &remote_path_string,
                ) {
                    add_error(SyncError::General(
                        remote_path_string.clone(),
                        err,
                    ));
                    return Err(());
                }
            }

            Ok(client.stat(&remote.name, &remote_path_string)
                .unwrap()
                .unwrap())
        };

        // Pull the item from the remote to the local machine.
        let pull_remote_to_local = || {
            // Make sure file types match up.
            if local_path.exists() {
                if local_path.is_dir() && !item.is_dir {
                    if let Err(err) = fs::remove_dir_all(local_path) {
                        add_error(SyncError::General(
                            local_path_string.clone(),
                            err.to_string(),
                        ));
                        return Err(());
                    }
                } else if !local_path.is_dir() && item.is_dir {
                    if let Err(err) = fs::remove_file(local_path) {
                        add_error(SyncError::General(
                            local_path_string.clone(),
                            err.to_string(),
                        ));
                        return Err(());
                    }

                    if let Err(err) = fs::create_dir(local_path) {
                        add_error(SyncError::General(
                            local_path_string.clone(),
                            err.to_string(),
                        ));
                        return Err(());
                    }
                }
            }

            if item.is_dir {
                if !local_path.exists()
                    && let Err(err) = fs::create_dir(local_path)
                {
                    add_error(SyncError::General(
                        local_path_string.clone(),
                        err.to_string(),
                    ));
                    return Err(());
                }

                sync_remote_directory(
                    &item.path,
                    remote,
                    sync_dir,
                    repo,
                    client,
                    synced_items,
                    emit.clone(),
                    check_open_requests.clone(),
                    process_deletion_requests.clone(),
                    is_cancelled.clone(),
                );
                update_ui_progress(&remote_path_string);
            } else {
                emit_status(tr::tr!(
                    "Downloading '{}'…",
                    util::fmt_home(&local_path_string)
                ));
                if let Err(err) = client.copy_to_local(
                    &local_path_string,
                    &remote.name,
                    &remote_path_string,
                ) {
                    add_error(SyncError::General(
                        remote_path_string.clone(),
                        err,
                    ));
                    return Err(());
                }
            }

            Ok(())
        };
        // Delete this item from the database.
        let delete_db_entry = || {
            let _ = util::await_future(repo.delete_sync_item_by_paths(
                sync_dir.id,
                &local_path_string,
                &remote_path_string,
            ));
        };

        // If we have a database record, use that in checks.
        if let Some(db_model) = db_item {
            let update_db_item = |local_timestamp: i64, remote_timestamp: i64| {
                let _ = util::await_future(repo.update_sync_item_timestamps(
                    db_model.id,
                    local_timestamp,
                    remote_timestamp,
                ));
            };

            // Both items are more recent.
            if let Some(l_timestamp) = local_timestamp
                && l_timestamp > db_model.last_local_timestamp as u64
                && remote_timestamp > db_model.last_remote_timestamp
            {
                // Only add the error if one of the items is not a directory -
                // there's no point in saying both directories are more current, and
                // it's probably because one of the items in the directory got
                // updated anyway.
                if !local_path.is_dir() || !item.is_dir {
                    add_error(SyncError::BothMoreCurrent(
                        local_path_string.clone(),
                        remote_path_string.clone(),
                    ));
                }
                continue;
            // The local item is more recent.
            } else if let Some(l_timestamp) = local_timestamp
                && l_timestamp > db_model.last_local_timestamp as u64
            {
                if let Ok(rclone_item) = push_local_to_remote() {
                    update_db_item(
                        get_local_file_timestamp().unwrap() as i64,
                        rclone_item.mod_time.unix_timestamp(),
                    );
                    continue;
                } else {
                    continue;
                }

            // The remote item is more recent.
            } else if remote_timestamp > db_model.last_remote_timestamp {
                if pull_remote_to_local().is_err() {
                    continue;
                } else {
                    update_db_item(
                        get_local_file_timestamp().unwrap() as i64,
                        remote_timestamp,
                    );
                }

            // The item is missing locally, but the last
            // recorded timestamp for the remote item is still
            // the same. This means the item got deleted
            // locally, and we need to reflect such on the
            // server.
            } else if !local_path.exists()
                && remote_timestamp == db_model.last_remote_timestamp
            {
                log_destructive_op(
                    "delete-remote-mirroring-local",
                    &remote.name,
                    &remote_path_string,
                );
                emit_status(tr::tr!(
                    "Removing '{}' on remote…",
                    remote_path_string
                ));
                // `purge` is rclone's directory-remove op — asking it to
                // purge a file returns "directory not found" on most
                // backends. Pick the right op for the item type.
                let delete_result = if item.is_dir {
                    client.purge(&remote.name, &remote_path_string)
                } else {
                    client.delete_file(&remote.name, &remote_path_string)
                };
                if let Err(err) = delete_result {
                    add_error(SyncError::General(
                        remote_path_string.clone(),
                        err,
                    ));
                    // Keep the db record so the next pass retries the
                    // delete instead of treating the file as new and
                    // re-downloading it.
                    continue;
                } else {
                    delete_db_entry();
                    continue;
                }

            // Both the local and remote item remain unchanged -
            // do nothing.
            } else if let Some(l_timestamp) = local_timestamp
                && l_timestamp == db_model.last_local_timestamp as u64
                && remote_timestamp == db_model.last_remote_timestamp
            {
                continue;

            // Every possible scenario should have been covered
            // above, so panic if not.
            } else {
                unreachable!();
            }
        // Otherwise just check the local timestamps against
        // those on th remote, and record our new transaction in
        // the database.
        } else {
            // If the local timestamp exists, then compare local and remote
            // timestamps.
            if let Some(l_timestamp) = local_timestamp {
                if l_timestamp > remote_timestamp as u64 {
                    if push_local_to_remote().is_err() {
                        continue;
                    }
                } else if pull_remote_to_local().is_err() {
                    continue;
                }

            // Otherwise the local item didn't exist, so just
            // sync it from the remote.
            } else if pull_remote_to_local().is_err() {
                continue;
            }
        }

        // The local item is now guaranteed to exist. Also fetch the remote's
        // timestamp in case it got updated above.
        let l_timestamp = get_local_file_timestamp().unwrap();
        let r_timestamp = match client.stat(&remote.name, &remote_path_string) {
            Ok(item) => item.unwrap().mod_time.unix_timestamp(),
            Err(err) => {
                add_error(SyncError::General(
                    remote_path_string.clone(),
                    err,
                ));
                continue;
            }
        };

        // Record the current transaction's timestamps in the database.
        let _ = util::await_future(repo.insert_sync_item(
            sync_dir.id,
            local_path_string.clone(),
            remote_path_string.clone(),
            l_timestamp as i64,
            r_timestamp,
        ));
    }
}

