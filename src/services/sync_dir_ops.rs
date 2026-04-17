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
    let update_ui_progress = |dir: &str| {
        // If this directory no longer exists in the database (i.e. from being
        // deleted from the `sync_dir_deletion_queue`), then do nothing.
        if !sync_dir_still_exists() {
            return;
        }
        let msg = tr::tr!("Checking '{}' for changes...", util::fmt_home(dir));
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text: msg,
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
            } else if let Err(err) =
                client.copy_to_remote(&local_path, &remote.name, &remote_path)
            {
                add_error(SyncError::General(local_path.clone(), err));
                return Err(());
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
            } else if let Err(err) =
                client.copy_to_local(&local_path, &remote.name, &remote_path)
            {
                add_error(SyncError::General(remote_path.clone(), err));
                return Err(());
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
        // If this directory no longer exists in the database (i.e. from being
        // deleted from the `sync_dir_deletion_queue`, do nothing).
        if !sync_dir_still_exists() {
            return;
        }
        let msg = tr::tr!("Checking '{}' on remote for changes...", dir);
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text: msg,
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
        let local_path_string = format!(
            "{}/{}",
            sync_dir.local_path,
            item.path.strip_prefix(&sync_dir.remote_path).unwrap()
        );
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
                    if let Err(err) = client.purge(&remote.name, &remote_path_string) {
                        add_error(SyncError::General(
                            remote_path_string.clone(),
                            err,
                        ));
                        return Err(());
                    }
                }

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
            } else if let Err(err) = client.copy_to_local(
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
                if let Err(err) = client.purge(&remote.name, &remote_path_string) {
                    add_error(SyncError::General(
                        remote_path_string.clone(),
                        err,
                    ));
                    delete_db_entry();
                    continue;
                } else {
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
