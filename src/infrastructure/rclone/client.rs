//! Adapter implementation of [`crate::domain::ports::BackendClient`] for
//! the rclone backend (via the librclone RPC surface).
//!
//! Thin wrapper around [`super::rpc::sync`] — blocking calls underneath,
//! since that's what librclone exposes. Async-ifying is deferred until the
//! orchestrator moves onto a proper tokio runtime.

use crate::domain::{
    ports::BackendClient,
    sync::{ListFilter, RemoteItem},
};

use super::rpc::{self, BackendListFilter, BackendRemoteItem};

#[derive(Clone, Copy)]
pub struct LibrcloneClient;

impl LibrcloneClient {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LibrcloneClient {
    fn default() -> Self {
        Self::new()
    }
}

fn map_item(item: BackendRemoteItem) -> RemoteItem {
    RemoteItem {
        is_dir: item.is_dir,
        path: item.path,
        name: item.name,
        mod_time: item.mod_time,
    }
}

fn map_filter(filter: ListFilter) -> BackendListFilter {
    match filter {
        ListFilter::All => BackendListFilter::All,
        ListFilter::Dirs => BackendListFilter::Dirs,
        ListFilter::Files => BackendListFilter::Files,
    }
}

impl BackendClient for LibrcloneClient {
    fn stat(&self, remote: &str, path: &str) -> Result<Option<RemoteItem>, String> {
        rpc::sync::stat(remote, path)
            .map(|opt| opt.map(map_item))
            .map_err(|err| err.error)
    }

    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
    ) -> Result<Vec<RemoteItem>, String> {
        rpc::sync::list(remote, path, recursive, map_filter(filter))
            .map(|items| items.into_iter().map(map_item).collect())
            .map_err(|err| err.error)
    }

    fn mkdir(&self, remote: &str, path: &str) -> Result<(), String> {
        rpc::sync::mkdir(remote, path).map_err(|err| err.error)
    }

    fn delete_file(&self, remote: &str, path: &str) -> Result<(), String> {
        rpc::sync::delete(remote, path).map_err(|err| err.error)
    }

    fn purge(&self, remote: &str, path: &str) -> Result<(), String> {
        rpc::sync::purge(remote, path).map_err(|err| err.error)
    }

    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
    ) -> Result<(), String> {
        rpc::sync::copy_to_remote(local_path, remote, remote_path).map_err(|err| err.error)
    }

    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
    ) -> Result<(), String> {
        rpc::sync::copy_to_local(local_path, remote, remote_path).map_err(|err| err.error)
    }

    fn delete_config(&self, remote: &str) -> Result<(), String> {
        rpc::sync::delete_config(remote).map_err(|err| err.error)
    }

    fn create_config(&self, payload_json: String) -> Result<(), String> {
        librclone::rpc("config/create", payload_json).map(|_| ())
    }

    fn remote_type(&self, remote: &str) -> Result<Option<String>, String> {
        let payload = serde_json::json!({ "name": remote }).to_string();
        match librclone::rpc("config/get", payload) {
            Ok(body) => {
                let parsed: serde_json::Value =
                    serde_json::from_str(&body).map_err(|e| e.to_string())?;
                // `config/get` returns {} for unknown remotes.
                if parsed.as_object().map(|m| m.is_empty()).unwrap_or(true) {
                    return Ok(None);
                }
                Ok(parsed.get("type").and_then(|v| v.as_str()).map(str::to_owned))
            }
            Err(err) => Err(err),
        }
    }
}
