//! Unit tests for the new `services::sync` algorithm. Covers:
//!
//! - Snapshot safety brake: abort when the listing is far smaller than
//!   the DB expects (rate-limit / cache-flush guard).
//! - plan() decisions on every (local, remote, db) tuple.
//! - apply() mirror-deletes, race-swallow on upload, conflict emission.

#![cfg(test)]

use std::{fs, path::PathBuf, sync::Mutex};

use super::*;
use crate::{
    domain::{
        events::SyncEvent,
        sync::{SyncDirId, SyncError},
    },
    test_support::{remote, remote_item, sync_dir, touch_mtime, FakeRclone, FakeRepo, TempDir},
};

fn run_full(
    tmp: &TempDir,
    repo: &FakeRepo,
    client: &FakeRclone,
) -> (Outcome, Vec<SyncEvent>) {
    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    let captured: Mutex<Vec<SyncEvent>> = Mutex::new(Vec::new());
    let outcome = run(
        &r,
        &sd,
        repo,
        client,
        |e| captured.lock().unwrap().push(e),
        || false,
        |_| false,
    );
    let events = captured.lock().unwrap().clone();
    (outcome, events)
}

fn errors(events: &[SyncEvent]) -> Vec<SyncError> {
    events
        .iter()
        .filter_map(|e| match e {
            SyncEvent::SyncDirError { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect()
}

/// Snapshot aborts when the listing can't come close to the DB row
/// count. Regression for the 2026-04-17 Google Drive rate-limit cascade.
#[test]
fn snapshot_aborts_when_listing_is_far_smaller_than_db() {
    let tmp = TempDir::new("sync_listing_suspect");
    // Create 10 local files + 10 DB rows. Listing returns 1 item.
    let mut locals: Vec<PathBuf> = Vec::new();
    for i in 0..10 {
        let name = format!("file_{i}.txt");
        let p = tmp.write_file(&name, b"x");
        touch_mtime(&p, 1_700_000_000);
        locals.push(p);
    }
    let repo = FakeRepo::new();
    for (i, p) in locals.iter().enumerate() {
        repo.insert_item(
            SyncDirId(1),
            p.to_str().unwrap(),
            &format!("file_{i}.txt"),
            1_700_000_000,
            1_700_000_000,
        );
    }

    let client = FakeRclone::default();
    client.set_list("", Ok(vec![remote_item("file_0.txt", false, 1_700_000_000)]));

    let (outcome, events) = run_full(&tmp, &repo, &client);

    assert_eq!(outcome, Outcome::Aborted, "must abort when listing is suspect");
    // None of the files should be deleted locally.
    for p in &locals {
        assert!(p.exists(), "file must survive a suspect-listing abort");
    }
    assert_eq!(repo.item_count(), 10, "no DB row may be removed");
    assert!(
        !errors(&events).is_empty(),
        "abort must surface a SyncDirError",
    );
}

/// Snapshot abort path when `client.list` itself errors out (e.g.
/// network loss). Nothing destructive runs.
#[test]
fn snapshot_aborts_when_list_fails() {
    let tmp = TempDir::new("sync_list_err");
    let local = tmp.write_file("a.txt", b"hi");
    touch_mtime(&local, 1_700_000_000);
    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "a.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list("", Err("connection reset".to_owned()));

    let (outcome, _events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Aborted);
    assert!(local.exists());
    assert_eq!(repo.item_count(), 1);
}

/// Everything in sync → no destructive actions, no transfers.
#[test]
fn steady_state_produces_no_actions() {
    let tmp = TempDir::new("sync_steady");
    let local = tmp.write_file("a.txt", b"v1");
    touch_mtime(&local, 1_700_000_000);
    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "a.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, 1_700_000_000)]));

    let (outcome, events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert!(errors(&events).is_empty());
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_local_calls.lock().unwrap().is_empty());
    assert!(client.delete_file_calls.lock().unwrap().is_empty());
    assert!(local.exists());
}

/// Local deleted between syncs → mirror on remote. DB row cleared.
#[test]
fn local_deleted_mirrors_to_remote() {
    let tmp = TempDir::new("sync_local_del");
    let repo = FakeRepo::new();
    let local_path = format!("{}/gone.txt", tmp.as_str());
    repo.insert_item(
        SyncDirId(1),
        &local_path,
        "gone.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list("", Ok(vec![remote_item("gone.txt", false, 1_700_000_000)]));

    let (outcome, _events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert_eq!(client.delete_file_calls.lock().unwrap().len(), 1);
    assert!(!repo.has_item(&local_path, "gone.txt"));
}

/// Remote deleted between syncs → mirror locally. DB row cleared.
#[test]
fn remote_deleted_mirrors_locally() {
    let tmp = TempDir::new("sync_remote_del");
    let local = tmp.write_file("gone.txt", b"v1");
    touch_mtime(&local, 1_700_000_000);
    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "gone.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list("", Ok(vec![]));

    let (outcome, _events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert!(!local.exists());
    assert!(!repo.has_item(local.to_str().unwrap(), "gone.txt"));
}

/// Both sides changed since last sync — emit BothMoreCurrent and
/// refuse to auto-resolve.
#[test]
fn both_sides_changed_emits_conflict() {
    let tmp = TempDir::new("sync_conflict");
    let local = tmp.write_file("a.txt", b"v2");
    touch_mtime(&local, 1_700_000_500);
    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "a.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, 1_700_000_700)]));

    let (outcome, events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert!(
        errors(&events)
            .iter()
            .any(|e| matches!(e, SyncError::BothMoreCurrent(..))),
    );
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_local_calls.lock().unwrap().is_empty());
}

/// Upload that fails with "source gone" after planning — raced with a
/// local delete — is silently swallowed. No DB row written, no error
/// surfaced.
#[test]
fn upload_swallows_source_gone_race() {
    let tmp = TempDir::new("sync_upload_race");
    let local = tmp.write_file("a.txt", b"hi");
    touch_mtime(&local, 1_700_000_000);
    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    client.set_list("", Ok(vec![]));
    // Simulate the race by removing the source before apply() runs —
    // our `!Path::new(local).exists()` early-exit short-circuits the
    // copy call entirely.
    let client_check = &client;
    fs::remove_file(&local).unwrap();
    let (outcome, events) = run_full(&tmp, &repo, client_check);
    assert_eq!(outcome, Outcome::Synced);
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
    assert!(errors(&events).is_empty());
}

/// `is_cancelled` returning true after the snapshot but before apply
/// bails out with `Outcome::Aborted` and no destructive op fires.
#[test]
fn cancellation_between_snapshot_and_apply_stops_the_pass() {
    let tmp = TempDir::new("sync_cancel");
    let repo = FakeRepo::new();
    // Seed a DB row and no local file → plan would normally emit a
    // DeleteRemote, but we cancel first.
    let local_path = format!("{}/doomed.txt", tmp.as_str());
    repo.insert_item(
        SyncDirId(1),
        &local_path,
        "doomed.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list(
        "",
        Ok(vec![remote_item("doomed.txt", false, 1_700_000_000)]),
    );

    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    let captured: Mutex<Vec<SyncEvent>> = Mutex::new(Vec::new());

    // Cancel-on-first-call: the cancel check fires true right after
    // Snapshot::build, before plan/apply.
    let outcome = run(
        &r,
        &sd,
        &repo,
        &client,
        |e| captured.lock().unwrap().push(e),
        || true,
        |_| false,
    );

    assert_eq!(outcome, Outcome::Aborted);
    assert!(
        client.delete_file_calls.lock().unwrap().is_empty(),
        "no remote deletes must fire when cancelled",
    );
    assert!(
        repo.has_item(&local_path, "doomed.txt"),
        "DB row must survive a cancelled pass",
    );
}

/// The rate-limit probe firing during the list step short-circuits the
/// pass to `Outcome::Degraded`. No plan runs, no destructive actions
/// fire, DB rows are preserved.
#[test]
fn rate_limit_probe_short_circuits_to_degraded() {
    let tmp = TempDir::new("sync_rate_limit_probe");
    let local = tmp.write_file("a.txt", b"hi");
    touch_mtime(&local, 1_700_000_000);
    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "a.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    // Listing is fine on its own — it's the probe that flags the pass.
    client.set_list("", Ok(vec![]));

    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    let captured: Mutex<Vec<SyncEvent>> = Mutex::new(Vec::new());
    let outcome = run(
        &r,
        &sd,
        &repo,
        &client,
        |e| captured.lock().unwrap().push(e),
        || false,
        |_| true,
    );

    assert_eq!(outcome, Outcome::Degraded);
    assert!(local.exists(), "local copy must survive degraded pass");
    assert!(
        repo.has_item(local.to_str().unwrap(), "a.txt"),
        "DB row must survive degraded pass",
    );
    assert!(
        client.delete_file_calls.lock().unwrap().is_empty(),
        "no remote deletes may fire on a degraded pass",
    );
}

/// A remote listing that's missing multiple tracked files under the
/// same parent (ProtonDrive rate-limit: partial-but-not-empty result)
/// must not cascade into mirror-delete-locally. Regression for the
/// 2026-04-18 ProtonDrive incident.
#[test]
fn delete_local_skipped_when_parent_listing_has_no_db_siblings() {
    let tmp = TempDir::new("sync_pd_listing_partial");
    // Five tracked siblings under "dir/" — all present locally, all
    // present in the DB, but the listing returns none of them (but
    // still enough root-level items to pass the global 2/3 threshold).
    let mut locals: Vec<PathBuf> = Vec::new();
    for i in 0..5 {
        let p = tmp.write_file(&format!("dir/file_{i}.txt"), b"x");
        touch_mtime(&p, 1_700_000_000);
        locals.push(p);
    }
    // Root-level padding so the global threshold (listing >= 2/3 of db)
    // still passes — isolates the per-parent check.
    let mut roots: Vec<PathBuf> = Vec::new();
    for i in 0..20 {
        let p = tmp.write_file(&format!("root_{i}.txt"), b"r");
        touch_mtime(&p, 1_700_000_000);
        roots.push(p);
    }

    let repo = FakeRepo::new();
    for (i, p) in locals.iter().enumerate() {
        repo.insert_item(
            SyncDirId(1),
            p.to_str().unwrap(),
            &format!("dir/file_{i}.txt"),
            1_700_000_000,
            1_700_000_000,
        );
    }
    for (i, p) in roots.iter().enumerate() {
        repo.insert_item(
            SyncDirId(1),
            p.to_str().unwrap(),
            &format!("root_{i}.txt"),
            1_700_000_000,
            1_700_000_000,
        );
    }

    let client = FakeRclone::default();
    // Listing: all root files present, NONE of the dir/ siblings
    // present (rate-limit returned an empty subfolder).
    let mut listing = Vec::new();
    for i in 0..20 {
        listing.push(remote_item(&format!("root_{i}.txt"), false, 1_700_000_000));
    }
    client.set_list("", Ok(listing));

    let (outcome, _events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced, "global threshold should pass");
    // None of the dir/ files should have been removed locally.
    for p in &locals {
        assert!(
            p.exists(),
            "local file must survive a listing missing all DB siblings: {}",
            p.display(),
        );
    }
    // Their DB rows must stick around for the next pass.
    for (i, p) in locals.iter().enumerate() {
        assert!(
            repo.has_item(p.to_str().unwrap(), &format!("dir/file_{i}.txt")),
            "DB row for dir/file_{i}.txt must survive",
        );
    }
}

/// A legitimate remote-side delete of a single file MUST still
/// propagate locally, even with the new sibling check — the parent
/// listing still shows other DB-tracked siblings.
#[test]
fn delete_local_still_fires_with_visible_sibling() {
    let tmp = TempDir::new("sync_pd_sibling_visible");
    let gone = tmp.write_file("dir/gone.txt", b"x");
    let sibling = tmp.write_file("dir/kept.txt", b"y");
    touch_mtime(&gone, 1_700_000_000);
    touch_mtime(&sibling, 1_700_000_000);

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        gone.to_str().unwrap(),
        "dir/gone.txt",
        1_700_000_000,
        1_700_000_000,
    );
    repo.insert_item(
        SyncDirId(1),
        sibling.to_str().unwrap(),
        "dir/kept.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    // Listing has the sibling but not the deleted file.
    client.set_list(
        "",
        Ok(vec![remote_item("dir/kept.txt", false, 1_700_000_000)]),
    );

    let (outcome, _events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert!(!gone.exists(), "legit delete must mirror locally");
    assert!(sibling.exists(), "unaffected sibling must survive");
    assert!(!repo.has_item(gone.to_str().unwrap(), "dir/gone.txt"));
    assert!(repo.has_item(sibling.to_str().unwrap(), "dir/kept.txt"));
}

/// The symmetric case: a local walk that's missing multiple tracked
/// files under the same parent (concurrent-writer race, permission
/// glitch) must not cascade into mirror-delete-remotely.
#[test]
fn delete_remote_skipped_when_parent_walk_has_no_db_siblings() {
    let tmp = TempDir::new("sync_pd_walk_partial");
    // The DB has five siblings under "dir/" and 20 at root, but the
    // walk sees none of the dir/ siblings (e.g. Syncthing just wiped
    // that subtree mid-pass). The listing still has everything.
    let repo = FakeRepo::new();
    for i in 0..5 {
        let local_path = format!("{}/dir/file_{i}.txt", tmp.as_str());
        repo.insert_item(
            SyncDirId(1),
            &local_path,
            &format!("dir/file_{i}.txt"),
            1_700_000_000,
            1_700_000_000,
        );
    }
    // Root-level padding passes the global threshold.
    let mut roots: Vec<PathBuf> = Vec::new();
    for i in 0..20 {
        let p = tmp.write_file(&format!("root_{i}.txt"), b"r");
        touch_mtime(&p, 1_700_000_000);
        repo.insert_item(
            SyncDirId(1),
            p.to_str().unwrap(),
            &format!("root_{i}.txt"),
            1_700_000_000,
            1_700_000_000,
        );
        roots.push(p);
    }

    let client = FakeRclone::default();
    let mut listing = Vec::new();
    for i in 0..20 {
        listing.push(remote_item(&format!("root_{i}.txt"), false, 1_700_000_000));
    }
    for i in 0..5 {
        listing.push(remote_item(&format!("dir/file_{i}.txt"), false, 1_700_000_000));
    }
    client.set_list("", Ok(listing));

    let (outcome, _events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert!(
        client.delete_file_calls.lock().unwrap().is_empty(),
        "no remote deletes must fire when walk has no DB siblings under the parent",
    );
    assert!(
        client.purge_calls.lock().unwrap().is_empty(),
        "no remote purges must fire either",
    );
    // DB rows under dir/ must survive for the next pass.
    for i in 0..5 {
        let local_path = format!("{}/dir/file_{i}.txt", tmp.as_str());
        assert!(
            repo.has_item(&local_path, &format!("dir/file_{i}.txt")),
            "DB row for dir/file_{i}.txt must survive",
        );
    }
}

/// Editor swap files are filtered from the local walk — never show up
/// as new items.
#[test]
fn editor_swap_files_are_not_seen() {
    let tmp = TempDir::new("sync_swap");
    let real = tmp.write_file("doc.txt", b"v1");
    touch_mtime(&real, 1_700_000_000);
    let swap = tmp.write_file(".doc.txt.kate-swp", b"tmp");
    touch_mtime(&swap, 1_700_000_000);

    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    client.set_list("", Ok(vec![remote_item("doc.txt", false, 1_700_000_000)]));
    // doc.txt is brand new (no DB row); expected action: record it.
    // Stat after upsert returns the same item.
    client.set_stat(
        "doc.txt",
        Ok(Some(remote_item("doc.txt", false, 1_700_000_000))),
    );

    let (outcome, events) = run_full(&tmp, &repo, &client);
    assert_eq!(outcome, Outcome::Synced);
    assert!(errors(&events).is_empty());
    // No upload for the swap file.
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
}
