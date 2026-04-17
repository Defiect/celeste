//! Filesystem watcher for instant sync.
//!
//! Spawned once at application startup. Watches every enabled remote's
//! sync-dir `local_path`s recursively with [`notify`]. A periodic reconciler
//! loop re-reads the DB and adjusts the watch set so toggling `instant_sync`
//! or adding a new remote takes effect without an app restart.
//!
//! Changed paths are accumulated per remote and flushed to the callback
//! every [`FLUSH_INTERVAL`] — this naturally debounces the multi-event
//! burst a single file save produces and lets the app run a
//! path-targeted sync instead of re-examining the whole tree.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    thread,
    time::Duration,
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::{
    infrastructure::persistence::models::{
        RemotesColumn, RemotesEntity, SyncDirsColumn, SyncDirsEntity,
    },
    util,
};

/// Flush interval — also the debounce window. A single file save fires
/// many notify events; they all land in the pending map and leave as one
/// flush.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Re-read the DB and reconcile the watch set every N flushes. 30 × 500ms
/// = 15s, which is often enough for toggling `instant_sync` to feel
/// responsive without hammering SeaORM.
const RECONCILE_EVERY_N_FLUSHES: u32 = 30;

/// Generalised entry point — forwards debounced batches of changed paths
/// per remote through the provided callback. Iced wires this into
/// `pending_fs_events` which the Iced subscription drains each tick.
pub fn spawn_with_callback(
    db: DatabaseConnection,
    on_change: Arc<dyn Fn(i32, Vec<PathBuf>) + Send + Sync>,
) {
    thread::spawn(move || {
        let path_to_remote: Arc<RwLock<HashMap<PathBuf, i32>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let pending: Arc<Mutex<HashMap<i32, HashSet<PathBuf>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let callback_map = path_to_remote.clone();
        let callback_pending = pending.clone();

        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                let Ok(event) = res else { return };
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    return;
                }

                let hits = {
                    let map = callback_map.read().unwrap();
                    hits_for_paths(&event.paths, &map)
                };
                if hits.is_empty() {
                    return;
                }

                let mut pending = callback_pending.lock().unwrap();
                for (remote_id, path) in hits {
                    pending.entry(remote_id).or_default().insert(path);
                }
            })
            .expect("failed to create filesystem watcher");

        let mut flush_count: u32 = 0;
        loop {
            if flush_count % RECONCILE_EVERY_N_FLUSHES == 0 {
                let desired = build_watch_map(&db).unwrap_or_default();
                reconcile_watches(&mut watcher, &path_to_remote, desired);
            }
            flush_count = flush_count.wrapping_add(1);

            thread::sleep(FLUSH_INTERVAL);

            let drained: Vec<(i32, HashSet<PathBuf>)> = {
                let mut p = pending.lock().unwrap();
                std::mem::take(&mut *p).into_iter().collect()
            };
            for (remote_id, paths) in drained {
                on_change(remote_id, paths.into_iter().collect());
            }
        }
    });
}

fn reconcile_watches(
    watcher: &mut RecommendedWatcher,
    path_to_remote: &RwLock<HashMap<PathBuf, i32>>,
    desired: HashMap<PathBuf, i32>,
) {
    let mut map = path_to_remote.write().unwrap();

    let stale: Vec<PathBuf> = map
        .iter()
        .filter(|(p, rid)| desired.get(*p) != Some(rid))
        .map(|(p, _)| p.clone())
        .collect();
    for path in stale {
        if let Err(err) = watcher.unwatch(&path) {
            eprintln!("fs_watcher: failed to unwatch {}: {err}", path.display());
        }
        map.remove(&path);
    }

    for (path, remote_id) in desired {
        if map.contains_key(&path) {
            continue;
        }
        if !Path::new(&path).exists() {
            continue;
        }
        match watcher.watch(&path, RecursiveMode::Recursive) {
            Ok(()) => {
                map.insert(path, remote_id);
            }
            Err(err) => {
                eprintln!("fs_watcher: failed to watch {}: {err}", path.display());
            }
        }
    }
}

fn build_watch_map(db: &DatabaseConnection) -> Option<HashMap<PathBuf, i32>> {
    let remotes = util::await_future(
        RemotesEntity::find()
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

fn hits_for_paths(
    paths: &[PathBuf],
    path_to_remote: &HashMap<PathBuf, i32>,
) -> Vec<(i32, PathBuf)> {
    let mut hits = Vec::new();
    for path in paths {
        for (watched, remote_id) in path_to_remote {
            if path.starts_with(watched) {
                hits.push((*remote_id, path.clone()));
            }
        }
    }
    hits
}
