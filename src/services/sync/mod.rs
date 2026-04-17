//! Replacement for the old `should_sync` / `sync_dir_ops` / `sync_dir_pass` /
//! `sync_path` pile. One snapshot-based algorithm with a safety brake baked
//! into snapshot construction — no per-item stats, no fs_watcher fast path,
//! no cache-race branches left to recur.
//!
//! Flow:
//!
//! 1. [`Snapshot::build`] fetches the authoritative remote listing
//!    ([`RcloneClient::list`] recursive), walks the local tree, and loads
//!    the DB rows. If the listing looks corrupt (far fewer items than the
//!    DB expects) the whole pass is aborted — that's the main rate-limit
//!    / cache-flush guard.
//!
//! 2. [`plan`] turns the snapshot into a `Vec<Action>` in a pure function.
//!    Every destructive decision is reducible to a `(local, remote, db)`
//!    triple and lands in exactly one branch.
//!
//! 3. [`apply`] executes the actions. Upload failures re-check whether the
//!    source file raced away (user deleted between planning and rclone
//!    reading) and silently skip in that case.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::Path,
    time::SystemTime,
};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::Remote,
        sync::{ListFilter, RemoteItem, SyncDir, SyncError, SyncItem},
    },
    util,
};

#[cfg(test)]
mod tests;

/// Outcome of a single [`run`] call. `Synced` means the plan was applied in
/// full; `Aborted` means the snapshot safety check refused to act and no
/// destructive work ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Synced,
    Aborted,
}

#[derive(Clone, Debug)]
pub struct LocalEntry {
    pub absolute_path: String,
    pub is_dir: bool,
    pub mtime_secs: i64,
}

#[derive(Debug)]
pub enum BuildError {
    /// `client.list` returned `Err(...)`.
    ListFailed(String),
    /// The listing itself looks wrong — far fewer items than the DB says
    /// should be present. Rate-limit / cache-flush territory; refuse to
    /// destroy anything on it.
    ListingSuspect { db_count: usize, list_count: usize },
}

/// Minimum DB row count before the listing-sanity check kicks in. Below
/// this we don't have enough signal to call the listing broken.
const LISTING_SANITY_THRESHOLD: usize = 5;

/// The listing must contain at least this fraction of the DB row count,
/// otherwise it's deemed suspect. 1/3 survives normal transient
/// deletions (user cleaning up a few files) but catches the "API
/// returned almost nothing" rate-limit case.
const LISTING_SANITY_NUMERATOR: usize = 1;
const LISTING_SANITY_DENOMINATOR: usize = 3;

pub struct Snapshot {
    pub remote: HashMap<String, RemoteItem>,
    pub local: HashMap<String, LocalEntry>,
    pub db: HashMap<String, SyncItem>,
}

impl Snapshot {
    pub fn build(
        remote: &Remote,
        sync_dir: &SyncDir,
        repo: &dyn Repository,
        client: &dyn RcloneClient,
    ) -> Result<Self, BuildError> {
        // 1. DB (cheap, authoritative for "what we last saw").
        let db_rows = util::await_future(repo.list_sync_items(sync_dir.id))
            .unwrap_or_default();
        let db: HashMap<String, SyncItem> = db_rows
            .into_iter()
            .map(|r| (r.remote_path.clone(), r))
            .collect();

        // 2. Remote listing — single authoritative call.
        let remote_items = client
            .list(&remote.name, &sync_dir.remote_path, true, ListFilter::All)
            .map_err(BuildError::ListFailed)?;

        // 3. Sanity check. If the listing looks corrupt, abort the whole
        //    pass rather than deleting local files based on junk data.
        if db.len() >= LISTING_SANITY_THRESHOLD
            && remote_items.len() * LISTING_SANITY_DENOMINATOR
                < db.len() * LISTING_SANITY_NUMERATOR
        {
            return Err(BuildError::ListingSuspect {
                db_count: db.len(),
                list_count: remote_items.len(),
            });
        }

        let remote: HashMap<String, RemoteItem> = remote_items
            .into_iter()
            .map(|i| (i.path.clone(), i))
            .collect();

        // 4. Local walk.
        let local = walk_local(sync_dir);

        Ok(Snapshot { remote, local, db })
    }
}

fn walk_local(sync_dir: &SyncDir) -> HashMap<String, LocalEntry> {
    let root = Path::new(&sync_dir.local_path);
    let mut out: HashMap<String, LocalEntry> = HashMap::new();
    walk_dir(root, sync_dir, &mut out);
    out
}

fn walk_dir(
    dir: &Path,
    sync_dir: &SyncDir,
    out: &mut HashMap<String, LocalEntry>,
) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if crate::services::editor_temp::is_editor_temp(&name) {
            continue;
        }
        let Some(local_path_str) = path.to_str() else { continue };
        let relative = match local_path_str
            .strip_prefix(&format!("{}/", sync_dir.local_path))
        {
            Some(r) => r.to_owned(),
            None => continue,
        };
        let remote_key = if sync_dir.remote_path.is_empty() {
            relative
        } else {
            format!("{}/{}", sync_dir.remote_path, relative)
        };
        let mtime_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.insert(
            remote_key,
            LocalEntry {
                absolute_path: local_path_str.to_owned(),
                is_dir: file_type.is_dir(),
                mtime_secs,
            },
        );
        if file_type.is_dir() {
            walk_dir(&path, sync_dir, out);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
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
    Conflict {
        local_path: String,
        remote_path: String,
    },
}

pub fn plan(snapshot: &Snapshot, sync_dir: &SyncDir) -> Vec<Action> {
    let keys: BTreeSet<&String> = snapshot
        .remote
        .keys()
        .chain(snapshot.local.keys())
        .chain(snapshot.db.keys())
        .collect();
    let mut out = Vec::new();
    for key in keys {
        let local = snapshot.local.get(key);
        let remote = snapshot.remote.get(key);
        let db = snapshot.db.get(key);
        if let Some(action) = plan_one(key, local, remote, db, sync_dir) {
            out.push(action);
        }
    }
    // Order: directory creations first (uploads/downloads for dirs before
    // their children), file transfers next, deletes last. Within each
    // group, shorter paths first so parents precede children.
    out.sort_by_key(|a| (phase(a), a_path(a).len(), a_path(a).to_owned()));
    out
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
        // Was tracked, remote gone — mirror delete locally.
        (Some(l), None, Some(_)) => Some(Action::DeleteLocal {
            local_path: l.absolute_path.clone(),
            remote_path: remote_path.to_owned(),
            is_dir: l.is_dir,
        }),
        // Was tracked, local gone — mirror delete remotely.
        (None, Some(r), Some(_)) => Some(Action::DeleteRemote {
            local_path: derive_local_path(&r.path, sync_dir),
            remote_path: r.path.clone(),
            is_dir: r.is_dir,
        }),
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
                (true, true) => {
                    if l.is_dir && r.is_dir {
                        None
                    } else {
                        Some(Action::Conflict {
                            local_path: l.absolute_path.clone(),
                            remote_path: remote_path.to_owned(),
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

/// Entry point. Builds the snapshot, plans, applies. `is_cancelled`
/// is polled at the start of each destructive action so the pass can
/// bail out promptly when the user disables the remote or shuts down
/// the app — in-flight rclone calls still run to completion (we can't
/// interrupt `copy_to_remote` cleanly), but nothing new fires.
pub fn run<FE, FC>(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    emit: FE,
    is_cancelled: FC,
) -> Outcome
where
    FE: Fn(SyncEvent) + Clone,
    FC: Fn() -> bool + Clone,
{
    let emit_pending = |text: String| {
        emit(SyncEvent::SyncDirPending {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    let emit_error = |error: SyncError| {
        emit(SyncEvent::SyncDirError {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            error,
        });
    };
    let emit_status = |text: String| {
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };

    emit_pending(tr::tr!("Listing remote (may take a while)…"));
    let snapshot = match Snapshot::build(remote, sync_dir, repo, client) {
        Ok(s) => s,
        Err(BuildError::ListFailed(err)) => {
            eprintln!("sync: list failed for {}: {err}", remote.name);
            emit_error(SyncError::General(sync_dir.remote_path.clone(), err));
            emit_status(tr::tr!("Sync failed — will retry next tick."));
            return Outcome::Aborted;
        }
        Err(BuildError::ListingSuspect {
            db_count,
            list_count,
        }) => {
            eprintln!(
                "sync: ABORT full pass — listing returned {list_count} of {db_count} expected items, parent listing is untrustworthy (likely rate limit or cache flush).",
            );
            emit_error(SyncError::General(
                sync_dir.remote_path.clone(),
                tr::tr!(
                    "Remote listing looked corrupt ({} of {} expected items); refusing to act.",
                    list_count,
                    db_count
                ),
            ));
            emit_status(tr::tr!("Sync skipped — remote listing looked corrupt."));
            return Outcome::Aborted;
        }
    };

    if is_cancelled() {
        emit_status(tr::tr!("Sync cancelled."));
        return Outcome::Aborted;
    }
    let actions = plan(&snapshot, sync_dir);
    apply(actions, &snapshot, remote, sync_dir, repo, client, &emit, &is_cancelled);

    if is_cancelled() {
        emit_status(tr::tr!("Sync cancelled."));
        return Outcome::Aborted;
    }
    emit_status(tr::tr!("Files are synced."));
    Outcome::Synced
}

fn apply<FE, FC>(
    actions: Vec<Action>,
    snapshot: &Snapshot,
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    emit: &FE,
    is_cancelled: &FC,
) where
    FE: Fn(SyncEvent) + Clone,
    FC: Fn() -> bool + Clone,
{
    let emit_status = |text: String| {
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    let emit_error = |error: SyncError| {
        emit(SyncEvent::SyncDirError {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            error,
        });
    };

    for action in actions {
        if is_cancelled() {
            return;
        }
        match action {
            Action::Upload {
                local_path,
                remote_path,
                is_dir,
            } => {
                if !Path::new(&local_path).exists() {
                    // Raced with a local delete between planning and now.
                    continue;
                }
                if is_dir {
                    if let Err(err) = client.mkdir(&remote.name, &remote_path) {
                        emit_error(SyncError::General(remote_path.clone(), err));
                        continue;
                    }
                } else {
                    emit_status(tr::tr!("Uploading '{}'…", util::fmt_home(&local_path)));
                    if let Err(err) =
                        client.copy_to_remote(&local_path, &remote.name, &remote_path)
                    {
                        if !Path::new(&local_path).exists() {
                            eprintln!(
                                "sync: copy_to_remote raced with local delete for '{local_path}' — swallowing '{err}'.",
                            );
                            continue;
                        }
                        emit_error(SyncError::General(local_path.clone(), err));
                        continue;
                    }
                }
                record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name);
            }
            Action::Download {
                local_path,
                remote_path,
                is_dir,
            } => {
                if is_dir {
                    if !Path::new(&local_path).exists()
                        && let Err(err) = fs::create_dir_all(&local_path)
                    {
                        emit_error(SyncError::General(local_path.clone(), err.to_string()));
                        continue;
                    }
                } else {
                    if let Some(parent) = Path::new(&local_path).parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    emit_status(tr::tr!("Downloading '{}'…", util::fmt_home(&local_path)));
                    if let Err(err) =
                        client.copy_to_local(&local_path, &remote.name, &remote_path)
                    {
                        emit_error(SyncError::General(remote_path.clone(), err));
                        continue;
                    }
                }
                record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name);
            }
            Action::DeleteLocal {
                local_path,
                remote_path,
                is_dir,
            } => {
                eprintln!(
                    "sync: DELETE mirror-local remote={} path={}",
                    remote.name, remote_path
                );
                emit_status(tr::tr!(
                    "Removing '{}' locally…",
                    util::fmt_home(&local_path)
                ));
                let res = if is_dir {
                    fs::remove_dir_all(&local_path)
                } else {
                    fs::remove_file(&local_path)
                };
                if let Err(err) = res {
                    emit_error(SyncError::General(local_path.clone(), err.to_string()));
                    continue;
                }
                let _ = util::await_future(repo.delete_sync_item_by_paths(
                    sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::DeleteRemote {
                local_path,
                remote_path,
                is_dir,
            } => {
                eprintln!(
                    "sync: DELETE mirror-remote remote={} path={}",
                    remote.name, remote_path
                );
                emit_status(tr::tr!("Removing '{}' on remote…", remote_path));
                let res = if is_dir {
                    client.purge(&remote.name, &remote_path)
                } else {
                    client.delete_file(&remote.name, &remote_path)
                };
                if let Err(err) = res {
                    emit_error(SyncError::General(remote_path.clone(), err));
                    continue;
                }
                let _ = util::await_future(repo.delete_sync_item_by_paths(
                    sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::ClearDbRow {
                local_path,
                remote_path,
            } => {
                let _ = util::await_future(repo.delete_sync_item_by_paths(
                    sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::Conflict {
                local_path,
                remote_path,
            } => {
                emit_error(SyncError::BothMoreCurrent(local_path, remote_path));
            }
        }
    }
    let _ = snapshot;
}

fn record_upsert(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn RcloneClient,
    remote_name: &str,
) {
    let Some(local_ts) = local_timestamp(Path::new(local_path)) else {
        return;
    };
    let Some(rstat) = client.stat(remote_name, remote_path).ok().flatten() else {
        return;
    };
    let remote_ts = rstat.mod_time.unix_timestamp();
    if let Some(existing) = util::await_future(
        repo.find_sync_item_by_paths(sync_dir.id, local_path, remote_path),
    )
    .unwrap_or(None)
    {
        let _ = util::await_future(repo.update_sync_item_timestamps(
            existing.id,
            local_ts as i64,
            remote_ts,
        ));
    } else {
        let _ = util::await_future(repo.insert_sync_item(
            sync_dir.id,
            local_path.to_owned(),
            remote_path.to_owned(),
            local_ts as i64,
            remote_ts,
        ));
    }
}

fn local_timestamp(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
