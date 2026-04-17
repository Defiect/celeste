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
    let outcome = run(&r, &sd, repo, client, |e| captured.lock().unwrap().push(e));
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
