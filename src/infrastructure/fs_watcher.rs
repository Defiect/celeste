//! Filesystem watcher for instant sync.
//!
//! Spawned once at application startup. Watches every enabled remote's
//! sync-dir `local_path`s recursively with [`notify`]. A periodic reconciler
//! loop re-reads the DB and adjusts the watch set so toggling `instant_sync`
//! or adding a new remote takes effect without an app restart.
//!
//! Each debounced filesystem event is forwarded through the supplied
//! callback. The caller decides what to do with it — the Iced app only fires
//! a sync when the remote's `instant_sync` policy is on, so the watcher can
//! safely watch every enabled remote without honouring the toggle itself.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
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

/// How often the reconciler wakes up to reconcile the watch set against the
/// current DB state.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

pub fn spawn(db: DatabaseConnection, refresh_requests: Arc<Mutex<HashSet<i32>>>) {
    let on_change: Arc<dyn Fn(i32) + Send + Sync> = Arc::new(move |remote_id: i32| {
        refresh_requests.lock().unwrap().insert(remote_id);
    });
    spawn_with_callback(db, on_change);
}

/// Generalised entry point — forwards each debounced filesystem event as a
/// remote_id through the provided callback. The GTK shell wraps that into
/// REFRESH_REQUESTS (via [`spawn`]); Iced pushes it onto its Message channel.
pub fn spawn_with_callback(
    db: DatabaseConnection,
    on_change: Arc<dyn Fn(i32) + Send + Sync>,
) {
    thread::spawn(move || {
        let path_to_remote: Arc<RwLock<HashMap<PathBuf, i32>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let last_trigger: Arc<Mutex<HashMap<i32, Instant>>> =
            Arc::new(Mutex::new(HashMap::new()));

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

                let matched = {
                    let map = callback_map.read().unwrap();
                    matching_remotes(&event.paths, &map)
                };
                if matched.is_empty() {
                    return;
                }

                let now = Instant::now();
                let mut last = callback_last.lock().unwrap();
                for remote_id in matched {
                    let fire = last
                        .get(&remote_id)
                        .map(|t| now.duration_since(*t) >= DEBOUNCE_WINDOW)
                        .unwrap_or(true);
                    if fire {
                        on_change(remote_id);
                        last.insert(remote_id, now);
                    }
                }
            })
            .expect("failed to create filesystem watcher");

        loop {
            let desired = build_watch_map(&db).unwrap_or_default();
            reconcile_watches(&mut watcher, &path_to_remote, desired);
            thread::sleep(RECONCILE_INTERVAL);
        }
    });
}

fn reconcile_watches(
    watcher: &mut RecommendedWatcher,
    path_to_remote: &RwLock<HashMap<PathBuf, i32>>,
    desired: HashMap<PathBuf, i32>,
) {
    let mut map = path_to_remote.write().unwrap();

    // Unwatch paths that are gone or re-pointed to a different remote.
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

    // Watch newly desired paths.
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
