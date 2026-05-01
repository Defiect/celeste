//! Replacement for the old `should_sync` / `sync_dir_ops` / `sync_dir_pass` /
//! `sync_path` pile. One snapshot-based algorithm — no per-item stats, no
//! fs_watcher fast path, no cache-race branches left to recur.
//!
//! Flow:
//!
//! 1. [`Snapshot::build`] fetches the authoritative remote listing
//!    ([`RcloneClient::list`] recursive), walks the local tree, and loads
//!    the DB rows. Bails out on list errors; rate-limit handling lives in
//!    [`run`]'s stderr-tap check around the build call.
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
        events::{SyncDirRunState, SyncEvent},
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
/// - `Aborted`: the pass refused to act (cancelled, or list error).
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
        all_sync_dirs: &[SyncDir],
    ) -> Result<Self, String> {
        // Auto-exclusion is keyed off remote-tree descendancy only: when
        // another sync_dir on the same provider sits inside this one's
        // remote subtree, skip it here so the two passes don't fight.
        // (Local-tree overlaps are blocked at AddSyncDir time, so they
        // can't exist by the time we get here.)
        //
        // Provider listings disagree on shape: librclone's
        // `operations/list` yields paths *relative* to the listed root
        // ("bar/baz.txt"), while the native Proton client builds
        // *absolute* remote paths ("Foo/bar/baz.txt"). Push both forms
        // so the filter matches either way — the extra string only
        // costs one `starts_with` per listed item. The matching local
        // sub-tree is also excluded to keep the walk from picking up a
        // coincidentally-named folder under this sync_dir's root.
        let mut excluded_local_prefixes: Vec<String> = Vec::new();
        let mut excluded_remote_prefixes: Vec<String> = Vec::new();
        for d in all_sync_dirs
            .iter()
            .filter(|d| d.id != sync_dir.id && d.remote_id == sync_dir.remote_id)
        {
            if let Some(rel) = remote_descendant_relative(sync_dir, &d.remote_path) {
                excluded_remote_prefixes
                    .push(absolute_remote_path(sync_dir, &rel));
                excluded_remote_prefixes.push(rel.clone());
                excluded_local_prefixes
                    .push(format!("{}/{}", sync_dir.local_path, rel));
            }
        }

        // User-defined exclusions (stored as remote sub-paths relative to
        // this sync_dir's remote root). Push both the relative and the
        // absolute form for the same provider-shape reason as above; the
        // local form is always absolute and only needs one entry.
        let user_excls = util::await_future(repo.list_exclusions(sync_dir.id))
            .unwrap_or_default();
        for excl in &user_excls {
            excluded_remote_prefixes
                .push(absolute_remote_path(sync_dir, &excl.remote_path));
            excluded_remote_prefixes.push(excl.remote_path.clone());
            excluded_local_prefixes.push(format!("{}/{}", sync_dir.local_path, excl.remote_path));
        }

        // 1. DB (cheap, authoritative for "what we last saw"). Drop any
        //    tracked rows that now fall inside an excluded subtree — they
        //    may linger from before the descendant sync_dir was created.
        let db_rows = util::await_future(repo.list_sync_items(sync_dir.id))
            .unwrap_or_default();
        let db: HashMap<String, SyncItem> = db_rows
            .into_iter()
            .filter(|r| !path_is_excluded(&r.local_path, &excluded_local_prefixes))
            .map(|r| (r.remote_path.clone(), r))
            .collect();

        // 2. Remote listing — single authoritative call. When the
        //    sync_dir's root doesn't exist on the remote (user or a
        //    prior incident trashed it), treat the listing as empty
        //    and mkdir the root so the upload phase can recreate the
        //    tree. Without this, the sync aborts every cycle and the
        //    user has to delete + re-add the sync_dir just to force
        //    a fresh mkdir.
        let remote_items = match client.list(
            &remote.name,
            &sync_dir.remote_path,
            true,
            ListFilter::All,
        ) {
            Ok(items) => items,
            Err(err) if is_directory_missing_error(&err) => {
                eprintln!(
                    "sync: remote dir '{}' missing for remote='{}' ({err}); recreating and proceeding with empty listing.",
                    sync_dir.remote_path, remote.name,
                );
                if !sync_dir.remote_path.is_empty() {
                    let _ = client.mkdir(&remote.name, &sync_dir.remote_path);
                }
                Vec::new()
            }
            Err(err) => return Err(err),
        };

        let remote: HashMap<String, RemoteItem> = remote_items
            .into_iter()
            .filter(|i| !path_is_excluded(&i.path, &excluded_remote_prefixes))
            .map(|i| (i.path.clone(), i))
            .collect();

        // 3. Local walk — skip subtrees managed by descendant sync_dirs.
        let (local, walk_unreliable) = walk_local(sync_dir, &excluded_local_prefixes);

        Ok(Snapshot {
            remote,
            local,
            db,
            walk_unreliable,
        })
    }
}

/// Does `err` look like a "remote directory doesn't exist" failure?
/// Providers word this differently: rclone's GDrive backend bubbles up
/// `error in ListJSON: directory not found`, the native Proton client
/// returns our own `resolve_path` miss, WebDAV can reply with a 404
/// body. Match broadly — a false positive here costs us one speculative
/// mkdir, a miss costs the user a broken sync cycle.
fn is_directory_missing_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("directory not found")
        || lower.contains("not found on remote")
        || lower.contains("no such file or directory")
}

/// Returns the candidate's path *relative to* `ancestor.remote_path` when
/// the candidate is a strict descendant of the ancestor in remote space
/// (or a non-empty path under an empty/root ancestor). Returns `None`
/// otherwise — including when the two are equal or unrelated.
fn remote_descendant_relative(ancestor: &SyncDir, candidate_remote: &str) -> Option<String> {
    if ancestor.remote_path.is_empty() {
        if candidate_remote.is_empty() {
            None
        } else {
            Some(candidate_remote.to_owned())
        }
    } else {
        let sep = format!("{}/", ancestor.remote_path);
        candidate_remote.strip_prefix(&sep).map(str::to_owned)
    }
}

/// Glue an ancestor's remote root onto a relative remote sub-path, producing
/// the absolute form that providers like the native Proton client return.
fn absolute_remote_path(sync_dir: &SyncDir, relative: &str) -> String {
    if sync_dir.remote_path.is_empty() {
        relative.to_owned()
    } else {
        format!("{}/{}", sync_dir.remote_path, relative)
    }
}

/// Returns true when `path` equals one of `excluded_prefixes` or starts
/// with one of them followed by `/`.
fn path_is_excluded(path: &str, excluded_prefixes: &[String]) -> bool {
    excluded_prefixes
        .iter()
        .any(|ex| path == ex || path.starts_with(&format!("{ex}/")))
}

fn walk_local(
    sync_dir: &SyncDir,
    excluded_local_prefixes: &[String],
) -> (HashMap<String, LocalEntry>, HashSet<String>) {
    let root = Path::new(&sync_dir.local_path);
    let mut out: HashMap<String, LocalEntry> = HashMap::new();
    let mut unreliable: HashSet<String> = HashSet::new();
    walk_dir(
        root,
        sync_dir,
        &mut out,
        &mut unreliable,
        &sync_dir.remote_path,
        excluded_local_prefixes,
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
    excluded_local_prefixes: &[String],
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
        if path_is_excluded(local_path_str, excluded_local_prefixes) {
            continue;
        }
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
            walk_dir(&path, sync_dir, out, unreliable, &remote_key, excluded_local_prefixes);
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
    all_sync_dirs: &[SyncDir],
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
    let emit_state = |state: SyncDirRunState| {
        emit(SyncEvent::SyncDirStateChanged {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            state,
        });
    };

    emit_state(SyncDirRunState::Syncing);
    let snapshot_result = Snapshot::build(remote, sync_dir, repo, client, all_sync_dirs);

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
        emit_state(SyncDirRunState::Warning);
        return Outcome::Degraded;
    }

    let snapshot = match snapshot_result {
        Ok(s) => s,
        Err(err) => {
            eprintln!("sync: list failed for {}: {err}", remote.name);
            emit_error(SyncError::General(sync_dir.remote_path.clone(), err));
            emit_status(tr::tr!("Sync failed — will retry next tick."));
            emit_state(SyncDirRunState::Error);
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
        emit_state(SyncDirRunState::Warning);
        return Outcome::Degraded;
    }
    emit_state(SyncDirRunState::Synced);
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
