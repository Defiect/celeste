//! Cross-thread slot the Iced subscription polls for pending fs_watcher
//! events. The watcher thread (started in main.rs) writes into it; the
//! subscription in app.rs drains it every 500 ms and turns each entry
//! into a `Message::FsPathsChanged`.
//!
//! Entries are accumulated per remote_id, with the changed paths merged
//! into a single `Vec<PathBuf>` so bursts from one file save don't
//! produce duplicate targeted syncs.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

static PENDING: OnceLock<Mutex<HashMap<i32, Vec<PathBuf>>>> = OnceLock::new();

fn slot() -> &'static Mutex<HashMap<i32, Vec<PathBuf>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn push(id: i32, paths: Vec<PathBuf>) {
    let mut map = slot().lock().unwrap();
    let entry = map.entry(id).or_default();
    for path in paths {
        if !entry.contains(&path) {
            entry.push(path);
        }
    }
}

pub fn drain() -> Vec<(i32, Vec<PathBuf>)> {
    let mut map = slot().lock().unwrap();
    std::mem::take(&mut *map).into_iter().collect()
}
