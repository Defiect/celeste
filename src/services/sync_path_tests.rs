//! Interruption-scenario tests for the fs_watcher-driven fast path
//! [`super::sync_path::sync_single_path`]. Covers the conditions a
//! shaky network or mid-save hibernation can leave the sync algorithm
//! in.

#![cfg(test)]

use std::sync::Mutex;

use super::sync_path::sync_single_path;
use crate::{
    domain::{
        events::SyncEvent,
        sync::{SyncDirId, SyncError},
    },
    test_support::{
        remote, remote_item, sync_dir, touch_mtime, FakeRclone, FakeRepo, TempDir,
    },
};

fn run(
    path: &std::path::Path,
    tmp: &TempDir,
    repo: &FakeRepo,
    client: &FakeRclone,
) -> Vec<SyncEvent> {
    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    let captured: Mutex<Vec<SyncEvent>> = Mutex::new(Vec::new());
    sync_single_path(path, &r, &sd, repo, client, |e| {
        captured.lock().unwrap().push(e)
    });
    captured.lock().unwrap().clone()
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

/// Fresh local file, nothing on remote, no DB row — normal upload.
/// Smoke test to anchor the other tests.
#[test]
fn new_local_file_uploads_and_inserts_db_row() {
    let tmp = TempDir::new("spath_new");
    let local = tmp.write_file("hello.txt", b"hi");
    touch_mtime(&local, 1_700_000_000);

    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    // First stat: file doesn't exist on remote → (None, None) branch
    // uploads. Second stat (from record_insert) returns the new
    // remote item so the DB row gets the correct mod_time.
    client.set_stat_sequence(
        "hello.txt",
        vec![
            Ok(None),
            Ok(Some(remote_item("hello.txt", false, 1_700_000_100))),
        ],
    );

    let _ = run(&local, &tmp, &repo, &client);

    assert_eq!(client.copy_to_remote_calls.lock().unwrap().len(), 1);
    assert!(repo.has_item(local.to_str().unwrap(), "hello.txt"));
}

/// Upload interrupted (network drop). No DB row should be inserted
/// — otherwise the next pass thinks the file is already synced.
#[test]
fn upload_failure_keeps_db_empty() {
    let tmp = TempDir::new("spath_upload_err");
    let local = tmp.write_file("hello.txt", b"hi");
    touch_mtime(&local, 1_700_000_000);

    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    client.set_copy_to_remote(Err("connection reset".to_owned()));

    let events = run(&local, &tmp, &repo, &client);

    assert_eq!(client.copy_to_remote_calls.lock().unwrap().len(), 1);
    assert_eq!(
        repo.item_count(),
        0,
        "no DB row must be created when upload fails",
    );
    assert!(
        !errors(&events).is_empty(),
        "a SyncDirError event must be emitted on upload failure",
    );
}

/// Local deleted, remote still has the file, timestamps match DB —
/// the normal user-driven delete. Propagates to the remote via
/// delete_file and clears the DB row.
#[test]
fn local_gone_remote_matches_db_deletes_on_remote() {
    let tmp = TempDir::new("spath_delete_ok");
    // No local file.

    let repo = FakeRepo::new();
    let local_path = format!("{}/Text File.txt", tmp.as_str());
    repo.insert_item(
        SyncDirId(1),
        &local_path,
        "Text File.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat(
        "Text File.txt",
        Ok(Some(remote_item("Text File.txt", false, 1_700_000_000))),
    );

    let _ = run(
        std::path::Path::new(&local_path),
        &tmp,
        &repo,
        &client,
    );

    assert_eq!(client.delete_file_calls.lock().unwrap().len(), 1);
    assert!(
        !repo.has_item(&local_path, "Text File.txt"),
        "DB row must be cleared after successful remote delete",
    );
}

/// Delete interrupted (network drop during delete_file). DB row
/// must be preserved so the next pass can retry; otherwise the
/// remote file comes back down on the next sync.
#[test]
fn delete_interrupted_preserves_db_record() {
    let tmp = TempDir::new("spath_delete_err");
    let repo = FakeRepo::new();
    let local_path = format!("{}/doomed.txt", tmp.as_str());
    repo.insert_item(
        SyncDirId(1),
        &local_path,
        "doomed.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat(
        "doomed.txt",
        Ok(Some(remote_item("doomed.txt", false, 1_700_000_000))),
    );
    client.set_delete_file(Err("connection reset".to_owned()));

    let events = run(
        std::path::Path::new(&local_path),
        &tmp,
        &repo,
        &client,
    );

    assert!(
        repo.has_item(&local_path, "doomed.txt"),
        "DB row must be preserved when remote delete fails",
    );
    assert!(
        !errors(&events).is_empty(),
        "a SyncDirError event must be emitted on remote delete failure",
    );
}

/// Local gone, but remote moved ahead (mtime doesn't match DB).
/// This is ambiguous — could be the user deleting locally AND
/// someone else editing on the remote. sync_single_path must NOT
/// delete on remote; let the periodic scheduler's conflict logic
/// handle it.
#[test]
fn local_gone_remote_moved_ahead_is_not_propagated() {
    let tmp = TempDir::new("spath_conflict");
    let repo = FakeRepo::new();
    let local_path = format!("{}/shared.txt", tmp.as_str());
    repo.insert_item(
        SyncDirId(1),
        &local_path,
        "shared.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat(
        "shared.txt",
        Ok(Some(remote_item("shared.txt", false, 1_700_000_500))), // newer
    );

    let _ = run(
        std::path::Path::new(&local_path),
        &tmp,
        &repo,
        &client,
    );

    assert!(
        client.delete_file_calls.lock().unwrap().is_empty(),
        "no remote delete when remote has moved ahead since last sync",
    );
    assert!(
        repo.has_item(&local_path, "shared.txt"),
        "DB row must be preserved so periodic pass can resolve the conflict",
    );
}

/// Editor swap files fire fs_watcher events but must never be
/// touched by the fast-path syncer — uploading one is guaranteed
/// to race the editor's deletion and produce bogus 'object not
/// found' errors.
#[test]
fn editor_swap_files_are_skipped() {
    let tmp = TempDir::new("spath_swap");
    let swap = tmp.write_file(".hello.txt.kate-swp", b"tmp");
    touch_mtime(&swap, 1_700_000_000);

    let repo = FakeRepo::new();
    let client = FakeRclone::default();

    let _ = run(&swap, &tmp, &repo, &client);

    assert!(client.stat_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
    assert_eq!(repo.item_count(), 0);
}

/// Conflict path: both sides changed since last sync. Must emit
/// BothMoreCurrent and not auto-resolve.
#[test]
fn both_sides_newer_than_db_emits_conflict() {
    let tmp = TempDir::new("spath_both_newer");
    let local = tmp.write_file("shared.txt", b"v2");
    touch_mtime(&local, 1_700_000_500); // local newer than db

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "shared.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat(
        "shared.txt",
        Ok(Some(remote_item("shared.txt", false, 1_700_000_700))), // remote newer than db too
    );

    let events = run(&local, &tmp, &repo, &client);

    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_local_calls.lock().unwrap().is_empty());
    assert!(
        errors(&events)
            .iter()
            .any(|e| matches!(e, SyncError::BothMoreCurrent(..))),
        "a BothMoreCurrent SyncError must be emitted",
    );
}

/// Create-then-delete race: rclone's copy_to_remote fails with "no
/// such file or directory" because the user deleted the source file
/// between our `path.exists()` check and rclone actually reading it.
/// `push_tolerating_source_race` detects that the source is gone and
/// reports `SourceRacedAway` — callers treat it as a silent no-op
/// because the incoming fs_watcher Remove event (with no DB row) will
/// correctly do nothing.
#[test]
fn push_tolerating_source_race_swallows_when_source_is_gone() {
    use super::sync_path::{push_tolerating_source_race, PushOutcome};

    let tmp = TempDir::new("spath_race");
    // No file on disk → path.exists() is false.
    let missing = tmp.path.join("ghost.txt");

    let client = FakeRclone::default();
    client.set_copy_to_remote(Err(
        "failed to open source object: no such file or directory".to_owned(),
    ));

    let outcome = push_tolerating_source_race(
        &client,
        missing.to_str().unwrap(),
        "TestRemote",
        "ghost.txt",
    );
    assert!(
        matches!(outcome, PushOutcome::SourceRacedAway),
        "source-gone must produce SourceRacedAway, got Failed/Done instead",
    );
}

/// When copy_to_remote fails but the source IS still on disk, the
/// error is real and surfaces. Guards against the swallow path
/// hiding genuine upload failures (permission denied, network
/// error with the file still intact, etc.).
#[test]
fn push_tolerating_source_race_surfaces_real_errors() {
    use super::sync_path::{push_tolerating_source_race, PushOutcome};

    let tmp = TempDir::new("spath_real_err");
    let local = tmp.write_file("stable.txt", b"still here");

    let client = FakeRclone::default();
    client.set_copy_to_remote(Err("permission denied".to_owned()));

    let outcome = push_tolerating_source_race(
        &client,
        local.to_str().unwrap(),
        "TestRemote",
        "stable.txt",
    );
    assert!(
        matches!(outcome, PushOutcome::Failed(_)),
        "real errors (source still present) must not be swallowed",
    );
}

/// Paths outside the configured sync_dir must be rejected. Guards
/// against fs_watcher misfires that might leak paths from a
/// sibling directory.
#[test]
fn path_outside_sync_dir_is_ignored() {
    let tmp = TempDir::new("spath_outside");
    let outside_dir = TempDir::new("spath_outside_sibling");
    let outside = outside_dir.write_file("foreign.txt", b"not ours");

    let repo = FakeRepo::new();
    let client = FakeRclone::default();

    let _ = run(&outside, &tmp, &repo, &client);

    assert!(client.stat_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
}
