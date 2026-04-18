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
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
    time::{Instant, SystemTime},
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

/// Outcome of a single [`run`] call.
///
/// - `Synced`: the plan was applied in full.
/// - `Aborted`: the pass refused to act (cancelled, list error, or the
///   listing-sanity check refused — the snapshot couldn't be trusted).
/// - `Degraded`: the pass detected provider rate-limiting (e.g. Proton
///   Drive's `status=429` retry warnings in stderr) and skipped the
///   apply step. Drives the scheduler's linear backoff so we stop
///   hammering a distressed API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Synced,
    Aborted,
    Degraded,
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
/// otherwise it's deemed suspect. 2/3 catches the ProtonDrive case where
/// rate-limiting lets `operations/list` return a partial-but-not-empty
/// listing (e.g. ~40-70% of items). Legitimate deletions over 1/3 of the
/// tree in a single pass are rare and simply defer to the next tick —
/// safer than nuking local or remote on a half-listing.
const LISTING_SANITY_NUMERATOR: usize = 2;
const LISTING_SANITY_DENOMINATOR: usize = 3;

pub struct Snapshot {
    pub remote: HashMap<String, RemoteItem>,
    pub local: HashMap<String, LocalEntry>,
    pub db: HashMap<String, SyncItem>,
    /// Remote-key paths whose local walk hit an I/O error (read_dir,
    /// file_type, or filename decoding failed). "Missing from `local`"
    /// under any of these ancestors is *not* a deletion signal — it's
    /// a walk glitch (most often a concurrent writer, e.g. Syncthing
    /// racing Celeste on the same tree). The planner refuses to fire
    /// `DeleteRemote` for anything whose ancestor chain lands here.
    pub walk_unreliable: HashSet<String>,
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
        let (local, walk_unreliable) = walk_local(sync_dir);

        Ok(Snapshot {
            remote,
            local,
            db,
            walk_unreliable,
        })
    }
}

fn walk_local(sync_dir: &SyncDir) -> (HashMap<String, LocalEntry>, HashSet<String>) {
    let root = Path::new(&sync_dir.local_path);
    let mut out: HashMap<String, LocalEntry> = HashMap::new();
    let mut unreliable: HashSet<String> = HashSet::new();
    walk_dir(
        root,
        sync_dir,
        &mut out,
        &mut unreliable,
        &sync_dir.remote_path,
    );
    (out, unreliable)
}

/// Walks `dir`, populating `out` with every entry and recording any I/O
/// failure in `unreliable`. `current_dir_key` is the remote-key path of
/// `dir` itself (empty string for the root when `sync_dir.remote_path`
/// is empty). All errors are logged to stderr so the next incident is
/// traceable without having to reproduce it under a debugger.
fn walk_dir(
    dir: &Path,
    sync_dir: &SyncDir,
    out: &mut HashMap<String, LocalEntry>,
    unreliable: &mut HashSet<String>,
    current_dir_key: &str,
) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(err) => {
            eprintln!(
                "sync: walk read_dir failed for '{}' (key='{current_dir_key}'): {err}; marking subtree unreliable.",
                dir.display(),
            );
            unreliable.insert(current_dir_key.to_owned());
            return;
        }
    };
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                eprintln!(
                    "sync: walk entry iteration failed under '{}' (key='{current_dir_key}'): {err}; marking subtree unreliable.",
                    dir.display(),
                );
                unreliable.insert(current_dir_key.to_owned());
                continue;
            }
        };
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(raw) => {
                eprintln!(
                    "sync: walk file_name non-UTF8 under '{}' (bytes={:?}); marking subtree unreliable.",
                    dir.display(),
                    raw,
                );
                unreliable.insert(current_dir_key.to_owned());
                continue;
            }
        };
        if crate::services::editor_temp::is_editor_temp(&name) {
            continue;
        }
        let remote_key = if current_dir_key.is_empty() {
            name.clone()
        } else {
            format!("{current_dir_key}/{name}")
        };
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(err) => {
                eprintln!(
                    "sync: walk file_type failed for '{}' (key='{remote_key}'): {err}; marking entry unreliable.",
                    entry.path().display(),
                );
                unreliable.insert(remote_key);
                continue;
            }
        };
        let path = entry.path();
        let Some(local_path_str) = path.to_str() else {
            eprintln!(
                "sync: walk path non-UTF8 for '{}' (key='{remote_key}'); marking entry unreliable.",
                path.display(),
            );
            unreliable.insert(remote_key);
            continue;
        };
        let mtime_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.insert(
            remote_key.clone(),
            LocalEntry {
                absolute_path: local_path_str.to_owned(),
                is_dir: file_type.is_dir(),
                mtime_secs,
            },
        );
        if file_type.is_dir() {
            walk_dir(&path, sync_dir, out, unreliable, &remote_key);
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

/// Is at least one DB-tracked sibling of `path` (under the same parent,
/// excluding itself) present in `items`? Returns `true` also when the
/// DB has no other siblings recorded under that parent — that's the
/// legitimate "last file in its parent" case and must not block the
/// delete. Returns `false` only when the DB says siblings should exist
/// but none of them appear in `items`, indicating the listing / walk
/// for that parent is untrustworthy (rate-limit, cache flush, mid-
/// write race).
fn parent_has_tracked_sibling<T>(
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
    !any_expected
}

/// Does `path`, or any of its ancestor directories up to the sync-dir
/// root, appear in `unreliable`? Used to refuse `DeleteRemote` when the
/// local walk reported an I/O error anywhere along the chain — the
/// "local is missing" signal isn't trustworthy under a broken walk,
/// regardless of how healthy the siblings look.
/// Emit a single stderr line summarising the snapshot shape and the
/// planned action counts. Lets the user tell at a glance whether a
/// "DELETE mirror-remote" is one-off (e.g. a manual delete mirroring
/// cleanly) or the start of a cascade they want to interrupt.
fn log_plan_summary(
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

fn ancestor_in_set(path: &str, unreliable: &HashSet<String>) -> bool {
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
        (Some(l), None, Some(_)) => {
            if !parent_has_tracked_sibling(
                remote_path,
                &snapshot.remote,
                db_by_parent,
            ) {
                eprintln!(
                    "sync: SKIP DeleteLocal for '{remote_path}' — listing has no DB-tracked siblings under '{}' (likely rate-limited / cache flush); preserving local copy.",
                    parent_of(remote_path),
                );
                return None;
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
            if !parent_has_tracked_sibling(
                &r.path,
                &snapshot.local,
                db_by_parent,
            ) {
                eprintln!(
                    "sync: SKIP DeleteRemote for '{}' — walk has no DB-tracked siblings under '{}' (likely concurrent-write race); preserving remote copy.",
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
pub fn run<FE, FC, FD>(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
    emit: FE,
    is_cancelled: FC,
    rate_limit_seen_since: FD,
) -> Outcome
where
    FE: Fn(SyncEvent) + Clone,
    FC: Fn() -> bool + Clone,
    FD: Fn(Instant) -> bool + Clone,
{
    let pass_start = Instant::now();
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
    let snapshot_result = Snapshot::build(remote, sync_dir, repo, client);

    // Classify rate-limit *before* we commit to a success/failure path:
    // list failures caused by quota exhaustion still need to route
    // through the Degraded / backoff branch, not be treated as plain
    // network errors that retry at the normal cadence.
    let rate_limited_during_list = rate_limit_seen_since(pass_start);
    if rate_limited_during_list {
        eprintln!(
            "sync: DEGRADED for '{}' — rate-limit warnings observed during listing; skipping this pass for backoff.",
            remote.name,
        );
        emit_error(SyncError::General(
            sync_dir.remote_path.clone(),
            tr::tr!("Rate-limit warnings detected during listing; skipping this pass for backoff."),
        ));
        emit_status(tr::tr!("Sync skipped — provider rate-limited."));
        return Outcome::Degraded;
    }

    let snapshot = match snapshot_result {
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
    log_plan_summary(remote, sync_dir, &snapshot, &actions);
    // Replace the "Listing remote…" pending with a phase-level
    // description of what apply() is about to do. Without this, a
    // multi-hundred-action pass (e.g. first-ever sync of a large
    // remote) leaves the user staring at "Listing…" for as long as
    // the per-action status churn takes to dominate the UI.
    if !actions.is_empty() {
        emit_pending(tr::tr!(
            "Applying {} actions (0 done)…",
            actions.len()
        ));
    }
    apply(
        actions,
        &snapshot,
        remote,
        sync_dir,
        repo,
        client,
        &emit,
        &is_cancelled,
    );

    if is_cancelled() {
        emit_status(tr::tr!("Sync cancelled."));
        return Outcome::Aborted;
    }
    // Post-apply check: even if the pass reached the end cleanly, a
    // rate-limit warning at any point during upload/download means the
    // backend was stressed. Flag Degraded so the scheduler backs off
    // before the next tick — we don't undo the work we already did.
    if rate_limit_seen_since(pass_start) {
        eprintln!(
            "sync: pass for '{}' completed but rate-limit warnings surfaced during apply; flagging Degraded for backoff.",
            remote.name,
        );
        emit_status(tr::tr!(
            "Files are synced — provider rate-limited, backing off next tick."
        ));
        return Outcome::Degraded;
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
    let total = actions.len();
    let emit_status = |text: String| {
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    let emit_pending_local = |text: String| {
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

    for (idx, action) in actions.into_iter().enumerate() {
        if is_cancelled() {
            return;
        }
        // Refresh the phase-pending line roughly every 16 actions so a
        // long apply (e.g. 1800+ downloads on first sync of a large
        // remote) reports progress even when the per-action status is
        // changing faster than the eye can follow. Cheap — one event
        // per batch, same channel as everything else.
        if idx > 0 && idx.is_multiple_of(16) {
            emit_pending_local(tr::tr!(
                "Applying {} actions ({} done)…",
                total,
                idx
            ));
        }
        let pos = idx + 1;
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
                    emit_status(tr::tr!(
                        "[{}/{}] Uploading '{}'…",
                        pos,
                        total,
                        util::fmt_home(&local_path)
                    ));
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
                    emit_status(tr::tr!(
                        "[{}/{}] Downloading '{}'…",
                        pos,
                        total,
                        util::fmt_home(&local_path)
                    ));
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
                    "[{}/{}] Removing '{}' locally…",
                    pos,
                    total,
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
                emit_status(tr::tr!(
                    "[{}/{}] Removing '{}' on remote…",
                    pos,
                    total,
                    remote_path
                ));
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
