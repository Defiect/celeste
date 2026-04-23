//! Router that dispatches [`RcloneClient`] calls to the appropriate
//! adapter per-remote. Holds a default (rclone-backed) client plus a
//! name-keyed override map that points native-backend remotes at a
//! `NativeProtonClient`. Sync code keeps talking to
//! `Arc<dyn RcloneClient>` — the router is transparent.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use crate::domain::{
    ports::RcloneClient,
    sync::{ListFilter, RemoteItem},
};

pub struct ClientRouter {
    default: Arc<dyn RcloneClient>,
    overrides: RwLock<HashMap<String, Arc<dyn RcloneClient>>>,
    /// Subset of overrides that are placeholder adapters for native
    /// remotes which failed to resume at startup (session expired,
    /// blob missing, etc.). The UI reads this to decide whether to
    /// surface a "Re-authenticate" banner on the remote page.
    disabled_native: RwLock<HashSet<String>>,
}

impl ClientRouter {
    /// Create a router whose fallback is `default`. No overrides
    /// registered initially — call [`register`] for each native
    /// remote at startup.
    pub fn new(default: Arc<dyn RcloneClient>) -> Self {
        Self {
            default,
            overrides: RwLock::new(HashMap::new()),
            disabled_native: RwLock::new(HashSet::new()),
        }
    }

    /// Register a per-remote override. Subsequent calls with that
    /// `remote` name route to `client` instead of the default.
    /// Replaces any existing override for the same name and clears
    /// any "disabled" marker (e.g. after a successful re-auth).
    pub fn register(&self, remote: String, client: Arc<dyn RcloneClient>) {
        self.disabled_native.write().unwrap().remove(&remote);
        self.overrides.write().unwrap().insert(remote, client);
    }

    /// Register a placeholder override flagged as "disabled" — the
    /// caller supplies a stub that returns an error explaining how
    /// to recover. Keeps sync calls off the default rclone client so
    /// the user sees the recovery hint instead of a rclone config
    /// lookup failure.
    pub fn register_disabled_native(
        &self,
        remote: String,
        client: Arc<dyn RcloneClient>,
    ) {
        self.overrides
            .write()
            .unwrap()
            .insert(remote.clone(), client);
        self.disabled_native.write().unwrap().insert(remote);
    }

    /// Returns `true` when `remote` is a native-backend remote whose
    /// session isn't currently usable. The remote page reads this to
    /// show the "Re-authenticate" banner.
    pub fn is_disabled_native(&self, remote: &str) -> bool {
        self.disabled_native.read().unwrap().contains(remote)
    }

    /// Drop an override — future calls with that name fall back to
    /// the default. No-op if the name isn't registered.
    pub fn unregister(&self, remote: &str) {
        self.overrides.write().unwrap().remove(remote);
        self.disabled_native.write().unwrap().remove(remote);
    }

    /// Resolve which client should handle `remote`. Cheap read-lock
    /// hot path; the override map only mutates at app startup +
    /// add/remove remote.
    fn pick(&self, remote: &str) -> Arc<dyn RcloneClient> {
        if let Some(client) = self.overrides.read().unwrap().get(remote) {
            return client.clone();
        }
        self.default.clone()
    }
}

impl RcloneClient for ClientRouter {
    fn stat(&self, remote: &str, path: &str) -> Result<Option<RemoteItem>, String> {
        self.pick(remote).stat(remote, path)
    }
    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
    ) -> Result<Vec<RemoteItem>, String> {
        self.pick(remote).list(remote, path, recursive, filter)
    }
    fn mkdir(&self, remote: &str, path: &str) -> Result<(), String> {
        self.pick(remote).mkdir(remote, path)
    }
    fn delete_file(&self, remote: &str, path: &str) -> Result<(), String> {
        self.pick(remote).delete_file(remote, path)
    }
    fn purge(&self, remote: &str, path: &str) -> Result<(), String> {
        self.pick(remote).purge(remote, path)
    }
    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
    ) -> Result<(), String> {
        self.pick(remote).copy_to_remote(local_path, remote, remote_path)
    }
    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
    ) -> Result<(), String> {
        self.pick(remote).copy_to_local(local_path, remote, remote_path)
    }
    fn delete_config(&self, remote: &str) -> Result<(), String> {
        self.pick(remote).delete_config(remote)
    }
    fn create_config(&self, payload_json: String) -> Result<(), String> {
        // There's no remote name to route on here; create_config is
        // only called from the rclone-side add-remote flow. Always
        // route to the default (librclone) — native-backend adds
        // bypass this method entirely.
        self.default.create_config(payload_json)
    }
    fn remote_type(&self, remote: &str) -> Result<Option<String>, String> {
        self.pick(remote).remote_type(remote)
    }
}
