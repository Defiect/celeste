//! Port traits: the contract services use to reach the outside world.
//!
//! Implementations live in `crate::infrastructure`. Services depend only on
//! these traits, so adapter swaps never touch domain or service code.
//!
//! Boxed-future return types are used instead of `async fn in trait` to stay
//! object-safe (`dyn Repository`) without pulling in `async-trait` yet. This
//! is revisited when the orchestrator actually needs `Arc<dyn Port>`s.

use std::{future::Future, pin::Pin};

use super::{
    remote::{Remote, RemoteId, SyncPolicy},
    sync::{ListFilter, RemoteItem, SyncDir, SyncDirId, SyncItem, SyncItemId},
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
pub enum RepositoryError {
    NotFound,
    Other(String),
}

impl std::fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for RepositoryError {}

pub trait Repository: Send + Sync {
    fn list_remotes(&self) -> BoxFuture<'_, Result<Vec<Remote>, RepositoryError>>;
    fn find_remote(&self, id: RemoteId)
        -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>>;
    fn find_remote_by_name(
        &self,
        name: &str,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>>;
    fn insert_remote(
        &self,
        name: String,
    ) -> BoxFuture<'_, Result<RemoteId, RepositoryError>>;
    fn delete_remote(&self, id: RemoteId) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Delete a remote, all of its sync_dirs, and all of their sync_items.
    fn cascade_delete_remote(
        &self,
        id: RemoteId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Delete a sync_dir (by `(local_path, remote_path)`) and all of its
    /// sync_items.
    fn cascade_delete_sync_dir(
        &self,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn set_policy(
        &self,
        id: RemoteId,
        policy: SyncPolicy,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;

    fn list_sync_dirs(
        &self,
        remote: RemoteId,
    ) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>>;
    fn sync_dir_exists(
        &self,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<bool, RepositoryError>>;
    fn insert_sync_dir(
        &self,
        remote: RemoteId,
        local_path: String,
        remote_path: String,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn list_sync_items(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>>;
    fn find_sync_item_by_paths(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>>;
    fn find_sync_item_by_local(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>>;
    fn find_sync_item_by_remote(
        &self,
        sync_dir: SyncDirId,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>>;
    fn delete_sync_item(&self, id: SyncItemId)
        -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn insert_sync_item(
        &self,
        sync_dir: SyncDirId,
        local_path: String,
        remote_path: String,
        last_local_timestamp: i64,
        last_remote_timestamp: i64,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn update_sync_item_timestamps(
        &self,
        id: SyncItemId,
        last_local_timestamp: i64,
        last_remote_timestamp: i64,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn delete_sync_item_by_paths(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
}

pub trait RcloneClient: Send + Sync {
    fn stat(&self, remote: &str, path: &str) -> Result<Option<RemoteItem>, String>;
    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
    ) -> Result<Vec<RemoteItem>, String>;
    fn mkdir(&self, remote: &str, path: &str) -> Result<(), String>;
    fn delete_file(&self, remote: &str, path: &str) -> Result<(), String>;
    fn purge(&self, remote: &str, path: &str) -> Result<(), String>;
    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
    ) -> Result<(), String>;
    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
    ) -> Result<(), String>;
    fn delete_config(&self, remote: &str) -> Result<(), String>;
    /// Create a new rclone config from a JSON body (rclone's
    /// `config/create` RPC payload, including `name`, `type`, `parameters`
    /// and optional `opt`).
    fn create_config(&self, payload_json: String) -> Result<(), String>;
    /// rclone backend type for a configured remote — `"drive"`,
    /// `"dropbox"`, `"protondrive"`, `"webdav"`, etc. Returns `Ok(None)`
    /// when the remote name isn't in rclone's config.
    fn remote_type(&self, remote: &str) -> Result<Option<String>, String>;
}

