//! Interruption-scenario tests for [`super::sync_dir_ops`] — the
//! branches most likely to destroy data when an operation fails mid-
//! sync (network loss, hibernation, rclone cache race).

#![cfg(test)]

use std::{cell::RefCell, sync::Mutex};

use super::sync_dir_ops::*;
use crate::{
    domain::{
        events::SyncEvent,
        sync::{RemoteItem, SyncDirId, SyncError},
    },
    test_support::{
        remote, remote_item, sync_dir, touch_mtime, FakeRclone, FakeRepo, TempDir,
    },
};

fn run_local_sync(
    tmp: &TempDir,
    repo: &FakeRepo,
    client: &FakeRclone,
    captured_events: &Mutex<Vec<SyncEvent>>,
) {
    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    let synced: RefCell<Vec<(String, String)>> = RefCell::new(vec![]);
    let emit = |event: SyncEvent| {
        captured_events.lock().unwrap().push(event);
    };
    sync_local_directory(
        &tmp.path,
        &r,
        &sd,
        repo,
        client,
        &synced,
        emit,
        || {},
        || {},
        || false,
    );
}

fn errors(events: &Mutex<Vec<SyncEvent>>) -> Vec<SyncError> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SyncEvent::SyncDirError { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect()
}

/// Regression for 2026-04-17: rclone's Google Drive backend returned
/// Ok(None) from `operations/stat` for 17 top-level files right
/// after a delete of an unrelated sibling. sync_local_directory
/// trusted each None and mirrored the "gone on remote" state by
/// deleting the local file + its DB row. The fix does a fresh
/// `list` of the parent when stat says None — if list still sees
/// the file, the branch is aborted and the local copy is preserved.
#[test]
fn list_verify_aborts_delete_when_stat_disagrees_with_list() {
    let tmp = TempDir::new("stat_lies");
    let local = tmp.write_file("important.pdf", b"real data");
    touch_mtime(&local, 1_700_000_000);

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "important.pdf",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat("important.pdf", Ok(None));
    client.set_list("", Ok(vec![remote_item("important.pdf", false, 1_700_000_000)]));

    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    assert!(
        local.exists(),
        "file must not be deleted locally when list confirms it's still on the remote",
    );
    assert!(
        repo.has_item(local.to_str().unwrap(), "important.pdf"),
        "DB row must be preserved when the delete is aborted",
    );
    assert!(
        client.delete_file_calls.lock().unwrap().is_empty(),
        "no remote delete_file should have fired",
    );
}

/// When stat and list both agree the remote file is gone, the
/// branch proceeds: local file deleted, DB row removed.
#[test]
fn list_verify_allows_delete_when_stat_and_list_agree() {
    let tmp = TempDir::new("stat_truthful");
    let local = tmp.write_file("gone.txt", b"doomed");
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
    client.set_stat("gone.txt", Ok(None));
    client.set_list("", Ok(vec![])); // empty listing confirms it's really gone

    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    assert!(!local.exists(), "file must be deleted locally");
    assert!(
        !repo.has_item(local.to_str().unwrap(), "gone.txt"),
        "DB row must be removed",
    );
}

/// When `client.stat` returns Err (network drop, hibernation mid-
/// call), sync_local_directory must record an error and continue,
/// not destroy data.
#[test]
fn stat_error_does_not_delete_local_or_touch_db() {
    let tmp = TempDir::new("stat_err");
    let local = tmp.write_file("keep.pdf", b"still here");
    touch_mtime(&local, 1_700_000_000);

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "keep.pdf",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat("keep.pdf", Err("connection reset".to_owned()));

    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    assert!(local.exists(), "file must survive a stat failure");
    assert_eq!(
        repo.item_count(),
        1,
        "DB row must not be touched on stat failure",
    );
    assert!(
        !errors(&events).is_empty(),
        "a SyncDirError event must be emitted on stat failure",
    );
}

/// Upload interrupted (copy_to_remote returns Err, e.g. network
/// loss mid-transfer). The DB row's timestamps must not advance —
/// otherwise the next sync thinks the upload succeeded and skips it.
#[test]
fn copy_to_remote_error_leaves_db_timestamps_unchanged() {
    let tmp = TempDir::new("upload_err");
    let local = tmp.write_file("doc.txt", b"local v2");
    touch_mtime(&local, 1_700_000_100); // newer than db

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "doc.txt",
        1_700_000_000, // older db local_ts → local is newer → push
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_stat("doc.txt", Ok(Some(remote_item("doc.txt", false, 1_700_000_000))));
    client.set_copy_to_remote(Err("connection reset".to_owned()));

    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    let items = repo.items.lock().unwrap();
    let row = items.iter().find(|it| it.remote_path == "doc.txt").unwrap();
    assert_eq!(
        row.last_local_timestamp, 1_700_000_000,
        "local_ts must not advance on failed upload",
    );
    assert_eq!(
        row.last_remote_timestamp, 1_700_000_000,
        "remote_ts must not advance on failed upload",
    );
    assert!(
        !errors(&events).is_empty(),
        "a SyncDirError event must be emitted on failed upload",
    );
}

/// Download interrupted — same contract: DB must not advance so the
/// next pass can retry.
#[test]
fn copy_to_local_error_leaves_db_timestamps_unchanged() {
    let tmp = TempDir::new("download_err");
    let local = tmp.write_file("doc.txt", b"local v1");
    touch_mtime(&local, 1_700_000_000);

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "doc.txt",
        1_700_000_000,
        1_700_000_000, // old db remote_ts → remote newer → pull
    );

    let client = FakeRclone::default();
    client.set_stat(
        "doc.txt",
        Ok(Some(remote_item("doc.txt", false, 1_700_000_500))),
    );
    client.set_copy_to_local(Err("connection reset".to_owned()));

    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    let items = repo.items.lock().unwrap();
    let row = items.iter().find(|it| it.remote_path == "doc.txt").unwrap();
    assert_eq!(
        row.last_remote_timestamp, 1_700_000_000,
        "remote_ts must not advance on failed download",
    );
    assert!(
        !errors(&events).is_empty(),
        "a SyncDirError event must be emitted on failed download",
    );
}

/// Type-mismatch purge interrupted. If a local file replaced a
/// remote dir and rclone's purge RPC fails mid-call, the upload
/// must not go ahead against a stale structure. The first-call
/// failure should abort the push, leaving the DB row untouched.
#[test]
fn purge_error_on_type_mismatch_aborts_push() {
    let tmp = TempDir::new("purge_err");
    let local = tmp.write_file("data", b"now a file");
    touch_mtime(&local, 1_700_000_500);

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        local.to_str().unwrap(),
        "data",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    // Remote 'data' is currently a directory — type mismatch with
    // the new local file.
    client.set_stat("data", Ok(Some(remote_item("data", true, 1_700_000_000))));
    client.set_purge(Err("connection reset".to_owned()));

    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    let items = repo.items.lock().unwrap();
    let row = items.iter().find(|it| it.remote_path == "data").unwrap();
    assert_eq!(
        row.last_local_timestamp, 1_700_000_000,
        "DB row must not advance when purge fails",
    );
    assert!(
        client.copy_to_remote_calls.lock().unwrap().is_empty(),
        "upload must not run after a failed purge",
    );
}

/// Editor swap files must be skipped entirely — no stat, no upload,
/// no db write.
#[test]
fn editor_swap_files_are_ignored() {
    let tmp = TempDir::new("editor_swap");
    let swap = tmp.write_file(".doc.txt.kate-swp", b"temp");
    touch_mtime(&swap, 1_700_000_000);

    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    let events = Mutex::new(Vec::new());
    run_local_sync(&tmp, &repo, &client, &events);

    assert!(
        client.stat_calls.lock().unwrap().is_empty(),
        "stat must not be called for editor swap files",
    );
    assert!(
        client.copy_to_remote_calls.lock().unwrap().is_empty(),
        "upload must not run for editor swap files",
    );
    assert_eq!(
        repo.item_count(),
        0,
        "no DB row should be inserted for editor swap files",
    );
}

/// Regression for `is_editor_temp` so the next person to add a
/// pattern doesn't accidentally drop one.
#[test]
fn editor_temp_pattern_matches_common_offenders() {
    assert!(is_editor_temp(".foo.kate-swp"));
    assert!(is_editor_temp(".bar.swp"));
    assert!(is_editor_temp(".bar.swo"));
    assert!(is_editor_temp(".bar.swn"));
    assert!(is_editor_temp(".#baz"));
    assert!(is_editor_temp("#baz#"));
    assert!(is_editor_temp("baz~"));
    assert!(is_editor_temp(".goutputstream-abc"));
    assert!(is_editor_temp("file.crdownload"));
    assert!(is_editor_temp("file.part"));

    assert!(!is_editor_temp("real-file.txt"));
    assert!(!is_editor_temp("swapfile")); // not .swp
    assert!(!is_editor_temp(".hidden")); // dotfiles are real files
}

fn run_remote_sync(
    tmp: &TempDir,
    repo: &FakeRepo,
    client: &FakeRclone,
    remote_items_in_root: Vec<RemoteItem>,
) -> Vec<SyncEvent> {
    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    let synced: RefCell<Vec<(String, String)>> = RefCell::new(vec![]);
    let captured: Mutex<Vec<SyncEvent>> = Mutex::new(Vec::new());
    client.set_list("", Ok(remote_items_in_root));
    sync_remote_directory(
        "",
        &r,
        &sd,
        repo,
        client,
        &synced,
        |event| captured.lock().unwrap().push(event),
        || {},
        || {},
        || false,
    );
    let events = captured.lock().unwrap().clone();
    events
}

/// Remote delete interrupted — local file is already gone, DB
/// record exists, remote delete_file fails. The DB record MUST be
/// preserved so the next pass retries, rather than the remote file
/// coming back on us (that was the 2026-04-17 "files re-download"
/// bug before the delete_file / db-keep fix).
#[test]
fn delete_file_error_on_remote_preserves_db_record() {
    let tmp = TempDir::new("remote_del_err");
    // No local file — simulates user deletion already happened.

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
    client.set_delete_file(Err("network error".to_owned()));

    let remote_items = vec![remote_item("Text File.txt", false, 1_700_000_000)];
    let _ = run_remote_sync(&tmp, &repo, &client, remote_items);

    assert!(
        repo.has_item(&local_path, "Text File.txt"),
        "DB record must be preserved when remote delete fails so the next pass retries",
    );
    assert_eq!(
        client.delete_file_calls.lock().unwrap().len(),
        1,
        "delete_file must have been attempted once",
    );
}

/// Remote delete succeeded — DB record gets removed as part of the
/// same branch, so the next periodic pass doesn't see a ghost row.
#[test]
fn successful_remote_delete_clears_db_record() {
    let tmp = TempDir::new("remote_del_ok");

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
    let remote_items = vec![remote_item("Text File.txt", false, 1_700_000_000)];
    let _ = run_remote_sync(&tmp, &repo, &client, remote_items);

    assert!(
        !repo.has_item(&local_path, "Text File.txt"),
        "DB record must be cleared after a successful remote delete",
    );
    assert_eq!(client.delete_file_calls.lock().unwrap().len(), 1);
}
