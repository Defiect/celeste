//! Filesystem watcher for instant sync.
//!
//! Spawned once at application startup. Queries every enabled remote whose
//! `instant_sync` flag is set, resolves its sync-dir `local_path`s, and
//! watches them recursively with [`notify`]. Each debounced filesystem event
//! inserts the matching remote id into the shared refresh-request set so the
//! main sync loop picks it up on its next tick.
//!
//! Policy changes made while the app runs (toggling `instant_sync`, adding a
//! new remote) require a restart for the watcher to pick them up. A dynamic
//! reconfiguration arrives when the orchestrator extraction lands.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::{
    infrastructure::persistence::models::{
        RemotesColumn, RemotesEntity, SyncDirsColumn, SyncDirsEntity,
    },
    util,
};

/// Debounce window: a single file save fires many notify events (write +
/// close + metadata change). Collapse them into one refresh per remote.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);

pub fn spawn(db: DatabaseConnection, refresh_requests: Arc<Mutex<HashSet<i32>>>) {
    thread::spawn(move || {
        let path_to_remote = match build_watch_map(&db) {
            Some(map) if !map.is_empty() => map,
            _ => return,
        };

        let last_trigger: Arc<Mutex<HashMap<i32, Instant>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let callback_refresh = refresh_requests.clone();
        let callback_map = path_to_remote.clone();
        let callback_last = last_trigger.clone();

        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                let Ok(event) = res else { return };
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    return;
                }

                let matched = matching_remotes(&event.paths, &callback_map);
                if matched.is_empty() {
                    return;
                }

                let now = Instant::now();
                let mut last = callback_last.lock().unwrap();
                let mut pending = callback_refresh.lock().unwrap();
                for remote_id in matched {
                    let fire = last
                        .get(&remote_id)
                        .map(|t| now.duration_since(*t) >= DEBOUNCE_WINDOW)
                        .unwrap_or(true);
                    if fire {
                        pending.insert(remote_id);
                        last.insert(remote_id, now);
                    }
                }
            })
            .expect("failed to create filesystem watcher");

        for path in path_to_remote.keys() {
            if let Err(err) = watcher.watch(path, RecursiveMode::Recursive) {
                eprintln!("fs_watcher: failed to watch {}: {err}", path.display());
            }
        }

        // Hold the watcher alive for the life of the app.
        thread::park();
    });
}

fn build_watch_map(db: &DatabaseConnection) -> Option<HashMap<PathBuf, i32>> {
    let remotes = util::await_future(
        RemotesEntity::find()
            .filter(RemotesColumn::InstantSync.eq(1))
            .filter(RemotesColumn::Enabled.eq(1))
            .all(db),
    )
    .ok()?;

    let mut map: HashMap<PathBuf, i32> = HashMap::new();
    for remote in remotes {
        let sync_dirs = util::await_future(
            SyncDirsEntity::find()
                .filter(SyncDirsColumn::RemoteId.eq(remote.id))
                .all(db),
        )
        .unwrap_or_default();
        for sd in sync_dirs {
            map.insert(PathBuf::from(sd.local_path), remote.id);
        }
    }
    Some(map)
}

fn matching_remotes(
    paths: &[PathBuf],
    path_to_remote: &HashMap<PathBuf, i32>,
) -> HashSet<i32> {
    let mut matched = HashSet::new();
    for path in paths {
        for (watched, remote_id) in path_to_remote {
            if path.starts_with(watched) {
                matched.insert(*remote_id);
            }
        }
    }
    matched
}
