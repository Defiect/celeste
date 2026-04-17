//! Decision function: has anything under this sync-dir changed since the
//! last successful sync? Runs before the real sync work and gates it.
//!
//! Emits `SyncEvent::SyncDirPending` at key points so the UI can show
//! progress during the slow remote-list step (rclone can take minutes
//! against big Google Drive accounts).

use std::{
    fs::{self, File},
    path::Path,
    time::SystemTime,
};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::Remote,
        sync::{ListFilter, SyncDir},
    },
    util,
};

/// How often to push a progress update while iterating, in items.
const PROGRESS_CHUNK: usize = 25;

pub fn should_sync<FE>(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    emit: FE,
) -> bool
where
    FE: Fn(SyncEvent) + Clone,
{
    let sync_dir_id = sync_dir.id;
    let mut should_sync = false;

    let pending = |text: String| {
        emit(SyncEvent::SyncDirPending {
            remote_id: remote.id,
            sync_dir_id,
            text,
        });
    };

    // Local file checks.
    pending(tr::tr!("Scanning local files…"));
    let local_glob = format!("{}/**/*", sync_dir.local_path);
    let local_paths: Vec<_> = match glob::glob(&local_glob) {
        Ok(paths) => paths.collect(),
        Err(_) => Vec::new(),
    };
    let total_local = local_paths.len();
    if total_local > 0 {
        pending(tr::tr!("Checking 0/{} local files…", total_local));
    }
    'local: for (i, maybe_path) in local_paths.into_iter().enumerate() {
        if i > 0 && (i % PROGRESS_CHUNK == 0 || i + 1 == total_local) {
            pending(tr::tr!(
                "Checking {}/{} local files…",
                i + 1,
                total_local
            ));
        }
        match maybe_path {
            Ok(path) => {
                let file = match File::open(&path) {
                    Ok(file) => file,
                    Err(_) => {
                        should_sync = true;
                        break 'local;
                    }
                };
                let current_timestamp = file
                    .metadata()
                    .unwrap()
                    .modified()
                    .unwrap()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let maybe_db_sync_item = util::await_future(
                    repo.find_sync_item_by_local(sync_dir_id, &path.display().to_string()),
                )
                .unwrap_or(None);

                let db_timestamp: u64 = if let Some(db_sync_item) = maybe_db_sync_item {
                    db_sync_item.last_local_timestamp as u64
                } else {
                    should_sync = true;
                    break 'local;
                };

                if current_timestamp != db_timestamp {
                    should_sync = true;
                    break 'local;
                }
            }
            Err(_) => {
                should_sync = true;
                break 'local;
            }
        }
    }

    // Remote file checks. The list call itself is where Google Drive
    // spends the most wall-clock — no way to show per-item progress
    // there, but the message tells the user why it's waiting.
    pending(tr::tr!("Listing remote (may take a while)…"));
    if let Ok(remote_paths) =
        client.list(&remote.name, &sync_dir.remote_path, true, ListFilter::All)
    {
        let total_remote = remote_paths.len();
        if total_remote > 0 {
            pending(tr::tr!("Checking 0/{} remote files…", total_remote));
        }
        'remote: for (i, path) in remote_paths.into_iter().enumerate() {
            if i > 0 && (i % PROGRESS_CHUNK == 0 || i + 1 == total_remote) {
                pending(tr::tr!(
                    "Checking {}/{} remote files…",
                    i + 1,
                    total_remote
                ));
            }
            let stripped_path = match path.name.contains('/') {
                true => path
                    .name
                    .strip_suffix(&format!("{}/", sync_dir.remote_path))
                    .unwrap()
                    .to_string(),
                false => path.name.clone(),
            };
            let maybe_db_sync_item =
                util::await_future(repo.find_sync_item_by_remote(sync_dir_id, &stripped_path))
                    .unwrap_or(None);
            let db_timestamp: i64 = if let Some(db_sync_item) = maybe_db_sync_item {
                db_sync_item.last_remote_timestamp
            } else {
                should_sync = true;
                break 'remote;
            };
            let remote_timestamp = path.mod_time.unix_timestamp();

            if remote_timestamp != db_timestamp {
                should_sync = true;
                break 'remote;
            }
        }
    } else {
        // TODO: show the disconnected icon instead of trying to sync again.
        should_sync = true;
    }

    // DB file checks. This covers files that got deleted locally or on the
    // remote, as those changes wouldn't necessarily be visible above.
    pending(tr::tr!("Reconciling database…"));
    let sync_items = util::await_future(repo.list_sync_items(sync_dir_id)).unwrap_or_default();
    let total_db = sync_items.len();
    if total_db > 0 {
        pending(tr::tr!("Reconciling 0/{} tracked items…", total_db));
    }

    'db: for (i, sync_item) in sync_items.into_iter().enumerate() {
        if i > 0 && (i % PROGRESS_CHUNK == 0 || i + 1 == total_db) {
            pending(tr::tr!(
                "Reconciling {}/{} tracked items…",
                i + 1,
                total_db
            ));
        }
        let local_path_str = sync_item.local_path.clone();
        let remote_path = if !sync_dir.remote_path.is_empty() {
            format!("{}/{}", sync_dir.remote_path, sync_item.remote_path)
        } else {
            sync_item.remote_path.clone()
        };
        let maybe_remote_timestamp: Option<i64> = client
            .stat(&remote.name, &remote_path)
            .ok()
            .flatten()
            .map(|remote_item| remote_item.mod_time.unix_timestamp());

        // If the path doesn't exist both locally and on the remote, delete
        // the DB entry.
        if !Path::new(&sync_item.local_path).exists() && maybe_remote_timestamp.is_none() {
            let _ = util::await_future(repo.delete_sync_item(sync_item.id));
            continue;
        }

        let remote_timestamp = match maybe_remote_timestamp {
            Some(timestamp) => timestamp,
            None => {
                should_sync = true;
                break 'db;
            }
        };

        let local_timestamp: i64 = match fs::metadata(&local_path_str) {
            Ok(metadata) => metadata
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            Err(_) => {
                should_sync = true;
                break 'db;
            }
        };

        if local_timestamp != sync_item.last_local_timestamp
            || remote_timestamp != sync_item.last_remote_timestamp
        {
            should_sync = true;
            break 'db;
        }
    }

    should_sync
}
