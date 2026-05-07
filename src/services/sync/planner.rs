//! Diff a [`Snapshot`] (remote / local / db tuple) into a `Vec<Action>`
//! the applier executes. Pure function — no I/O, no logging beyond a
//! single one-line summary on the way out.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::domain::{
    remote::Remote,
    sync::{RemoteItem, SyncDir, SyncItem},
};

use super::snapshot::{LocalEntry, Snapshot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Upload {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    Download {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    DeleteLocal {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    DeleteRemote {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    ClearDbRow {
        local_path: String,
        remote_path: String,
    },
    /// Reserved for the case where a future planner version refuses to
    /// pick a side on simultaneous local+remote drift. Today's planner
    /// always picks newer-mtime, so this is unreachable — but the apply
    /// + summary code keep the arm so reintroducing it stays a one-line
    /// change.
    #[allow(dead_code)]
    Conflict {
        local_path: String,
        remote_path: String,
    },
}

pub(super) fn plan(snapshot: &Snapshot, sync_dir: &SyncDir) -> Vec<Action> {
    let keys: BTreeSet<&String> = snapshot
        .remote
        .keys()
        .chain(snapshot.local.keys())
        .chain(snapshot.db.keys())
        .collect();
    let db_by_parent = group_db_keys_by_parent(&snapshot.db);
    let mut out = Vec::new();
    for key in keys {
        let local = snapshot.local.get(key);
        let remote = snapshot.remote.get(key);
        let db = snapshot.db.get(key);
        if let Some(action) = plan_one(
            key,
            local,
            remote,
            db,
            sync_dir,
            snapshot,
            &db_by_parent,
        ) {
            out.push(action);
        }
    }
    // Order: directory creations first (uploads/downloads for dirs before
    // their children), file transfers next, deletes last. Within each
    // group, shorter paths first so parents precede children.
    out.sort_by_key(|a| (phase(a), a_path(a).len(), a_path(a).to_owned()));
    out
}

fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

fn group_db_keys_by_parent(
    db: &HashMap<String, SyncItem>,
) -> HashMap<&str, Vec<&str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for key in db.keys() {
        out.entry(parent_of(key)).or_default().push(key.as_str());
    }
    out
}

/// Did the listing / walk successfully enumerate `path`'s parent?
/// Used to distinguish a legitimate "parent was emptied" from a
/// rate-limit / cache-flush / concurrent-write glitch that returned
/// partial data.
///
/// Returns `true` when any of the following holds:
///   1. The DB has no other tracked siblings under that parent — this
///      is the legitimate "last file in its parent" case.
///   2. At least one DB-tracked sibling of `path` is present in
///      `items`. Proof that the listing / walk reached the parent.
///   3. `items` contains *any* entry directly under the same parent
///      — even untracked ones. A non-empty enumeration of the parent
///      is proof the call succeeded; the DB-tracked ones are genuinely
///      gone. This is the mass-replace / bulk-delete case where none
///      of the old DB-tracked items survive but new content arrived.
///
/// Returns `false` only when the DB says siblings should exist, none
/// of them appear in `items`, and the listing / walk shows nothing
/// at all under that parent — the dangerous "we saw zero where we
/// expected many" pattern.
fn parent_is_verified<T>(
    path: &str,
    items: &HashMap<String, T>,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> bool {
    let parent = parent_of(path);
    let siblings = match db_by_parent.get(parent) {
        Some(v) => v,
        None => return true,
    };
    let mut any_expected = false;
    for sibling in siblings {
        if *sibling == path {
            continue;
        }
        any_expected = true;
        if items.contains_key(*sibling) {
            return true;
        }
    }
    if !any_expected {
        return true;
    }
    items.keys().any(|k| parent_of(k) == parent)
}

/// Emit a single stderr line summarising the snapshot shape and the
/// planned action counts. Lets the user tell at a glance whether a
/// "DELETE mirror-remote" is one-off (e.g. a manual delete mirroring
/// cleanly) or the start of a cascade they want to interrupt.
pub(super) fn log_plan_summary(
    remote: &Remote,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    actions: &[Action],
) {
    let (mut uploads, mut downloads) = (0usize, 0usize);
    let (mut delete_local, mut delete_remote) = (0usize, 0usize);
    let (mut conflicts, mut clear_rows) = (0usize, 0usize);
    for a in actions {
        match a {
            Action::Upload { .. } => uploads += 1,
            Action::Download { .. } => downloads += 1,
            Action::DeleteLocal { .. } => delete_local += 1,
            Action::DeleteRemote { .. } => delete_remote += 1,
            Action::Conflict { .. } => conflicts += 1,
            Action::ClearDbRow { .. } => clear_rows += 1,
        }
    }
    eprintln!(
        "sync: plan for remote='{}' dir='{}' — snapshot(db={}, listing={}, walk={}, walk_unreliable={}); actions(upload={}, download={}, delete_local={}, delete_remote={}, conflict={}, clear_db_row={}).",
        remote.name,
        sync_dir.remote_path,
        snapshot.db.len(),
        snapshot.remote.len(),
        snapshot.local.len(),
        snapshot.walk_unreliable.len(),
        uploads,
        downloads,
        delete_local,
        delete_remote,
        conflicts,
        clear_rows,
    );
}

/// Does `path`, or any of its ancestor directories up to the sync-dir
/// root, appear in `unreliable`? Used to refuse `DeleteRemote` when the
/// local walk reported an I/O error anywhere along the chain — the
/// "local is missing" signal isn't trustworthy under a broken walk,
/// regardless of how healthy the siblings look.
pub(super) fn ancestor_in_set(path: &str, unreliable: &HashSet<String>) -> bool {
    if unreliable.is_empty() {
        return false;
    }
    if unreliable.contains(path) {
        return true;
    }
    let mut p = path;
    loop {
        let parent = parent_of(p);
        if unreliable.contains(parent) {
            return true;
        }
        if parent == p {
            return false;
        }
        p = parent;
    }
}

fn phase(a: &Action) -> u8 {
    match a {
        Action::Upload { is_dir: true, .. } | Action::Download { is_dir: true, .. } => 0,
        Action::Upload { .. } | Action::Download { .. } => 1,
        Action::Conflict { .. } => 2,
        Action::ClearDbRow { .. } => 3,
        Action::DeleteLocal { .. } | Action::DeleteRemote { .. } => 4,
    }
}

fn a_path(a: &Action) -> &str {
    match a {
        Action::Upload { remote_path, .. }
        | Action::Download { remote_path, .. }
        | Action::DeleteLocal { remote_path, .. }
        | Action::DeleteRemote { remote_path, .. }
        | Action::ClearDbRow { remote_path, .. }
        | Action::Conflict { remote_path, .. } => remote_path,
    }
}

fn plan_one(
    remote_path: &str,
    local: Option<&LocalEntry>,
    remote: Option<&RemoteItem>,
    db: Option<&SyncItem>,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> Option<Action> {
    match (local, remote, db) {
        (None, None, None) => None,
        // Stale DB: both sides gone. Clean the row.
        (None, None, Some(_)) => Some(Action::ClearDbRow {
            local_path: db_local_path(remote_path, sync_dir),
            remote_path: remote_path.to_owned(),
        }),
        // New local, never seen — upload.
        (Some(l), None, None) => Some(Action::Upload {
            local_path: l.absolute_path.clone(),
            remote_path: remote_path.to_owned(),
            is_dir: l.is_dir,
        }),
        // New remote, never seen — download.
        (None, Some(r), None) => Some(Action::Download {
            local_path: derive_local_path(&r.path, sync_dir),
            remote_path: r.path.clone(),
            is_dir: r.is_dir,
        }),
        // Both present, never tracked — compare timestamps, upload if
        // local is strictly newer, else download. Equal timestamps: just
        // record the DB row via an upload (no-op transfer but aligns DB).
        (Some(l), Some(r), None) => {
            if l.is_dir && r.is_dir {
                // Dirs: nothing to transfer, will be recorded when a
                // child lands.
                return None;
            }
            if l.mtime_secs > r.mod_time.unix_timestamp() {
                Some(Action::Upload {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: l.is_dir,
                })
            } else {
                Some(Action::Download {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: r.is_dir,
                })
            }
        }
        // Was tracked, remote gone — mirror delete locally. Require a
        // DB-tracked sibling of this item to also appear in the listing;
        // otherwise the listing for this parent is untrustworthy (rate-
        // limit / cache flush) and we refuse to destroy the local copy.
        // Equivalent of the GoogleDrive-era sibling verification in
        // 6117026, adapted to the snapshot algorithm.
        (Some(l), None, Some(db)) => {
            if !parent_is_verified(
                remote_path,
                &snapshot.remote,
                db_by_parent,
            ) {
                eprintln!(
                    "sync: SKIP DeleteLocal for '{remote_path}' — listing enumerated no entries under '{}' (likely rate-limited / cache flush); preserving local copy.",
                    parent_of(remote_path),
                );
                return None;
            }
            // If the local file has been modified since the last successful
            // sync, the missing-remote is more consistent with a failed
            // upload that rclone cleaned up (e.g. hash mismatch on transfer)
            // than with an intentional remote deletion. Re-upload rather
            // than destroy the newer local copy.
            if !l.is_dir && l.mtime_secs > db.last_local_timestamp {
                eprintln!(
                    "sync: SWAP DeleteLocal → Upload for '{remote_path}' — local mtime {} newer than last synced {} (likely failed upload cleanup); retrying upload.",
                    l.mtime_secs, db.last_local_timestamp,
                );
                return Some(Action::Upload {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: l.is_dir,
                });
            }
            Some(Action::DeleteLocal {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                is_dir: l.is_dir,
            })
        }
        // Was tracked, local gone — mirror delete remotely. Two
        // guards before we fire anything destructive:
        //   1. Ancestor reliability. If the local walk reported an
        //      I/O error anywhere in this path's chain, the absence
        //      isn't a deletion signal — it's a walk glitch (most
        //      commonly a concurrent writer like Syncthing racing
        //      Celeste mid-readdir). Refuse.
        //   2. Sibling presence. Weaker backstop for paths whose
        //      ancestors look healthy but whose parent holds other
        //      DB-tracked rows that didn't make the walk either.
        (None, Some(r), Some(_)) => {
            if ancestor_in_set(&r.path, &snapshot.walk_unreliable) {
                eprintln!(
                    "sync: SKIP DeleteRemote for '{}' — local walk reported an error on this path or an ancestor; preserving remote copy.",
                    r.path,
                );
                return None;
            }
            if !parent_is_verified(
                &r.path,
                &snapshot.local,
                db_by_parent,
            ) {
                eprintln!(
                    "sync: SKIP DeleteRemote for '{}' — walk found no entries under '{}' (likely concurrent-write race); preserving remote copy.",
                    r.path,
                    parent_of(&r.path),
                );
                return None;
            }
            Some(Action::DeleteRemote {
                local_path: derive_local_path(&r.path, sync_dir),
                remote_path: r.path.clone(),
                is_dir: r.is_dir,
            })
        }
        // Full triple — compare timestamps against the recorded values.
        (Some(l), Some(r), Some(db)) => {
            let local_changed = l.mtime_secs > db.last_local_timestamp;
            let remote_changed =
                r.mod_time.unix_timestamp() > db.last_remote_timestamp;
            match (local_changed, remote_changed) {
                (false, false) => None,
                (true, false) => Some(Action::Upload {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: l.is_dir,
                }),
                (false, true) => Some(Action::Download {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: r.is_dir,
                }),
                // Both sides drifted since the last recorded sync.
                // Don't stall on a Conflict — pick the side with the
                // newer current mtime and propagate it over the other.
                // Ties go to local on the assumption that the user is
                // the active editor (Celeste's typical workloads —
                // game saves, dotfiles, notes — are local-driven).
                // Two dirs against each other still no-op since the
                // contents land via their children.
                (true, true) => {
                    if l.is_dir && r.is_dir {
                        None
                    } else if l.mtime_secs >= r.mod_time.unix_timestamp() {
                        Some(Action::Upload {
                            local_path: l.absolute_path.clone(),
                            remote_path: remote_path.to_owned(),
                            is_dir: l.is_dir,
                        })
                    } else {
                        Some(Action::Download {
                            local_path: l.absolute_path.clone(),
                            remote_path: remote_path.to_owned(),
                            is_dir: r.is_dir,
                        })
                    }
                }
            }
        }
    }
}

fn derive_local_path(remote_path: &str, sync_dir: &SyncDir) -> String {
    let relative = if sync_dir.remote_path.is_empty() {
        remote_path.to_owned()
    } else {
        remote_path
            .strip_prefix(&format!("{}/", sync_dir.remote_path))
            .unwrap_or(remote_path)
            .to_owned()
    };
    if relative.is_empty() {
        sync_dir.local_path.clone()
    } else {
        format!("{}/{}", sync_dir.local_path, relative)
    }
}

fn db_local_path(remote_path: &str, sync_dir: &SyncDir) -> String {
    derive_local_path(remote_path, sync_dir)
}
