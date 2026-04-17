//! Decision function: has anything under this sync-dir changed since the
//! last successful sync? Runs before the real sync work and gates it.

use std::{
    fs::{self, File},
    path::Path,
    time::SystemTime,
};

use crate::{
    domain::{
        ports::{RcloneClient, Repository},
        sync::{ListFilter, SyncDirId},
    },
    infrastructure::persistence::models::{RemotesModel, SyncDirsModel},
    util,
};

pub fn should_sync(
    remote: &RemotesModel,
    sync_dir: &SyncDirsModel,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
) -> bool {
    let sync_dir_id = SyncDirId(sync_dir.id);
    let mut should_sync = false;

    // Local file checks.
    let local_glob = format!("{}/**/*", sync_dir.local_path);
    if let Ok(paths) = glob::glob(&local_glob) {
        for maybe_path in paths {
            if let Ok(path) = maybe_path {
                let file = match File::open(&path) {
                    Ok(file) => file,
                    Err(_) => {
                        should_sync = true;
                        break;
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
                    break;
                };

                if current_timestamp != db_timestamp {
                    should_sync = true;
                    break;
                }
            } else {
                should_sync = true;
                break;
            }
        }
    } else {
        // TODO: We should show the user an error instead of trying to sync again.
        should_sync = true;
    }

    // Remote file checks.
    if let Ok(paths) = client.list(&remote.name, &sync_dir.remote_path, true, ListFilter::All) {
        for path in paths {
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
                break;
            };
            let remote_timestamp = path.mod_time.unix_timestamp();

            if remote_timestamp != db_timestamp {
                should_sync = true;
                break;
            }
        }
    } else {
        // TODO: We should show the disconnected icon instead of trying to sync again.
        should_sync = true;
    }

    // DB file checks. This covers files that got deleted locally or on the
    // remote, as those changes wouldn't necessarily be visible above.
    let sync_items = util::await_future(repo.list_sync_items(sync_dir_id)).unwrap_or_default();

    for sync_item in sync_items {
        let local_path_str = sync_item.local_path.display().to_string();
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

        // If the path doesn't exist both locally and on the remote, then we
        // need to delete the DB entry.
        if !Path::new(&sync_item.local_path).exists() && maybe_remote_timestamp.is_none() {
            let _ = util::await_future(repo.delete_sync_item(sync_item.id));
            continue;
        }

        let remote_timestamp = match maybe_remote_timestamp {
            Some(timestamp) => timestamp,
            None => {
                should_sync = true;
                break;
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
                break;
            }
        };

        if local_timestamp != sync_item.last_local_timestamp
            || remote_timestamp != sync_item.last_remote_timestamp
        {
            should_sync = true;
            break;
        }
    }

    should_sync
}
