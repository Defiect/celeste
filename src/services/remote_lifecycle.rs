//! Remove-a-remote / remove-a-sync-dir workflows. UI removal is the caller's
//! problem; these functions handle the DB cascade and rclone-side cleanup
//! via the Repository and RcloneClient ports.

use crate::{
    domain::ports::{RcloneClient, Repository},
    util,
};

/// Cascade-delete one sync_dir (by local + remote path) and all of its
/// sync_items from the DB.
pub fn delete_sync_dir(
    local_path: &str,
    remote_path: &str,
    repo: &dyn Repository,
) -> Result<(), String> {
    util::await_future(repo.cascade_delete_sync_dir(local_path, remote_path))
        .map_err(|e| e.to_string())
}

/// Cascade-delete a remote (by name), its sync_dirs and sync_items, and
/// drop its rclone config.
pub fn delete_remote(
    remote_name: &str,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
) -> Result<(), String> {
    let remote = util::await_future(repo.find_remote_by_name(remote_name))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("remote '{remote_name}' not found"))?;
    util::await_future(repo.cascade_delete_remote(remote.id)).map_err(|e| e.to_string())?;
    client.delete_config(remote_name)
}
