//! Decision tests for the [`super::should_sync::should_sync`] gate.
//! Covers the common "nothing changed" fast path plus the 'new-item'
//! and 'timestamp-drifted' cases a slipped / interrupted prior sync
//! leaves behind.

#![cfg(test)]

use super::should_sync::should_sync;
use crate::{
    domain::sync::SyncDirId,
    test_support::{
        remote, remote_item, sync_dir, touch_mtime, FakeRclone, FakeRepo, TempDir,
    },
};

fn check(tmp: &TempDir, repo: &FakeRepo, client: &FakeRclone) -> bool {
    let r = remote(1, "TestRemote");
    let sd = sync_dir(1, 1, tmp.as_str(), "");
    should_sync(&r, &sd, repo, client, |_| {})
}

/// Fully in-sync state: local, remote, and DB all match. No pass
/// needed. This is the hot path and must stay cheap.
#[test]
fn returns_false_when_everything_matches() {
    let tmp = TempDir::new("ss_insync");
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
    client.set_list(
        "",
        Ok(vec![remote_item("a.txt", false, 1_700_000_000)]),
    );

    assert!(!check(&tmp, &repo, &client));
}

/// A new local file appears (no DB row) → should_sync must fire
/// so sync_dir_ops can upload it.
#[test]
fn returns_true_when_local_has_a_new_file() {
    let tmp = TempDir::new("ss_new_local");
    let local = tmp.write_file("fresh.txt", b"v1");
    touch_mtime(&local, 1_700_000_000);

    let repo = FakeRepo::new();
    let client = FakeRclone::default();

    assert!(check(&tmp, &repo, &client));
}

/// A new remote file appears (no DB row) → should_sync must fire
/// so sync_dir_ops can pull it.
#[test]
fn returns_true_when_remote_has_a_new_file() {
    let tmp = TempDir::new("ss_new_remote");
    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    client.set_list(
        "",
        Ok(vec![remote_item("fresh.txt", false, 1_700_000_000)]),
    );

    assert!(check(&tmp, &repo, &client));
}

/// A DB row whose local file has gone missing (e.g. user deleted
/// between syncs) must trigger a pass so the deletion can be
/// propagated to the remote.
#[test]
fn returns_true_when_tracked_local_file_is_missing() {
    let tmp = TempDir::new("ss_missing_local");
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
    client.set_list(
        "",
        Ok(vec![remote_item("gone.txt", false, 1_700_000_000)]),
    );

    assert!(check(&tmp, &repo, &client));
}

/// Editor swap files must not wake the sync algorithm up. Before
/// the `is_editor_temp` filter, each Kate save briefly created a
/// `.foo.kate-swp` that had no DB row → infinite should_sync=true.
#[test]
fn editor_swap_files_do_not_force_a_sync() {
    let tmp = TempDir::new("ss_swap");
    let real = tmp.write_file("doc.txt", b"v1");
    touch_mtime(&real, 1_700_000_000);
    let swap = tmp.write_file(".doc.txt.kate-swp", b"tmp");
    touch_mtime(&swap, 1_700_000_000);

    let repo = FakeRepo::new();
    repo.insert_item(
        SyncDirId(1),
        real.to_str().unwrap(),
        "doc.txt",
        1_700_000_000,
        1_700_000_000,
    );

    let client = FakeRclone::default();
    client.set_list(
        "",
        Ok(vec![remote_item("doc.txt", false, 1_700_000_000)]),
    );

    assert!(
        !check(&tmp, &repo, &client),
        "should_sync must not fire purely because of an editor swap file",
    );
}

/// Network error mid-list: should_sync returns true and the
/// sync_dir_ops downstream will surface the failure. The point is
/// we don't silently stay "in sync" against a failing API.
#[test]
fn returns_true_when_remote_list_fails() {
    let tmp = TempDir::new("ss_list_err");
    let repo = FakeRepo::new();
    let client = FakeRclone::default();
    client.set_list("", Err("connection reset".to_owned()));

    assert!(check(&tmp, &repo, &client));
}
