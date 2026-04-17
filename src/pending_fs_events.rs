//! Cross-thread slot the Iced subscription polls for pending fs_watcher
//! events. The watcher thread (started in main.rs) writes into it; the
//! subscription in app.rs drains it every 500 ms and turns each id into a
//! `Message::FsEvent`.

use std::sync::{Mutex, OnceLock};

static PENDING: OnceLock<Mutex<Vec<i32>>> = OnceLock::new();

fn slot() -> &'static Mutex<Vec<i32>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn push(id: i32) {
    slot().lock().unwrap().push(id);
}

pub fn drain() -> Vec<i32> {
    std::mem::take(&mut *slot().lock().unwrap())
}
