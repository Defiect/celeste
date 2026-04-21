//! Shared scaffolding for unit tests across `services/`.
//!
//! - [`TempDir`]: RAII-managed scratch directory under the system temp.
//! - [`FakeRepo`]: in-memory Repository honouring the real CRUD semantics
//!   on sync_items.
//! - [`FakeRclone`]: RcloneClient where every op's outcome is configurable
//!   per-path — the only way to exercise interruption scenarios
//!   (network error, stat cache race) against the sync algorithm without
//!   actually talking to rclone.

#![cfg(test)]
#![allow(dead_code)]

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use time::OffsetDateTime;

use crate::domain::{
    ports::{BoxFuture, RcloneClient, Repository, RepositoryError},
    remote::{Backend, Remote, RemoteId, SyncPolicy},
    sync::{
        ListFilter, RemoteItem, SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId,
        SyncItem, SyncItemId,
    },
};

/// Scratch directory rooted under `$TMPDIR/celeste_test_*`. Dropped
/// automatically; collisions across tests use the process ID + caller tag.
pub struct TempDir {
    pub path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let mut counter = 0u32;
        loop {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "celeste_test_{}_{}_{}",
                tag,
                std::process::id(),
                counter
            ));
            if !p.exists() {
                fs::create_dir_all(&p).expect("create temp dir");
                return Self { path: p };
            }
            counter += 1;
        }
    }

    pub fn write_file(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, contents).unwrap();
        full
    }

    pub fn as_str(&self) -> &str {
        self.path.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

/// In-memory [`Repository`] for tests. Keeps a single sync_items table and
/// exposes a few helper accessors for assertions. Ignores every call that
/// isn't relevant to sync_dir_ops / sync_path (remotes, sync_dirs).
#[derive(Default)]
pub struct FakeRepo {
    pub items: Mutex<Vec<SyncItem>>,
    pub next_id: Mutex<i32>,
    pub sync_dir_exists: Mutex<bool>,
}

impl FakeRepo {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(vec![]),
            next_id: Mutex::new(0),
            sync_dir_exists: Mutex::new(true),
        }
    }

    pub fn insert_item(
        &self,
        sync_dir_id: SyncDirId,
        local_path: &str,
        remote_path: &str,
        local_ts: i64,
        remote_ts: i64,
    ) -> SyncItemId {
        let mut next = self.next_id.lock().unwrap();
        *next += 1;
        let id = SyncItemId(*next);
        self.items.lock().unwrap().push(SyncItem {
            id,
            sync_dir_id,
            local_path: local_path.to_owned(),
            remote_path: remote_path.to_owned(),
            last_local_timestamp: local_ts,
            last_remote_timestamp: remote_ts,
        });
        id
    }

    pub fn item_count(&self) -> usize {
        self.items.lock().unwrap().len()
    }

    pub fn has_item(&self, local_path: &str, remote_path: &str) -> bool {
        self.items
            .lock()
            .unwrap()
            .iter()
            .any(|it| it.local_path == local_path && it.remote_path == remote_path)
    }
}

impl Repository for FakeRepo {
    fn list_remotes(&self) -> BoxFuture<'_, Result<Vec<Remote>, RepositoryError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn find_remote(
        &self,
        _id: RemoteId,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>> {
        Box::pin(async { Ok(None) })
    }
    fn find_remote_by_name(
        &self,
        _name: &str,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>> {
        Box::pin(async { Ok(None) })
    }
    fn insert_remote(&self, _name: String) -> BoxFuture<'_, Result<RemoteId, RepositoryError>> {
        Box::pin(async { Ok(RemoteId(1)) })
    }
    fn insert_native_proton_remote(
        &self,
        _name: String,
        _session_path: String,
    ) -> BoxFuture<'_, Result<RemoteId, RepositoryError>> {
        Box::pin(async { Ok(RemoteId(1)) })
    }
    fn delete_remote(&self, _id: RemoteId) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn cascade_delete_remote(
        &self,
        _id: RemoteId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn cascade_delete_sync_dir(
        &self,
        _local: &str,
        _remote: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn set_policy(
        &self,
        _id: RemoteId,
        _p: SyncPolicy,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn list_sync_dirs(
        &self,
        _r: RemoteId,
    ) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn list_all_sync_dirs(&self) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn sync_dir_exists(
        &self,
        _l: &str,
        _r: &str,
    ) -> BoxFuture<'_, Result<bool, RepositoryError>> {
        let exists = *self.sync_dir_exists.lock().unwrap();
        Box::pin(async move { Ok(exists) })
    }
    fn insert_sync_dir(
        &self,
        _r: RemoteId,
        _l: String,
        _rp: String,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn list_sync_items(
        &self,
        sd: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>> {
        let items: Vec<SyncItem> = self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|it| it.sync_dir_id == sd)
            .cloned()
            .collect();
        Box::pin(async move { Ok(items) })
    }
    fn find_sync_item_by_paths(
        &self,
        sd: SyncDirId,
        local: &str,
        remote: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
        let found = self
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|it| it.sync_dir_id == sd && it.local_path == local && it.remote_path == remote)
            .cloned();
        Box::pin(async move { Ok(found) })
    }
    fn find_sync_item_by_local(
        &self,
        sd: SyncDirId,
        local: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
        let found = self
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|it| it.sync_dir_id == sd && it.local_path == local)
            .cloned();
        Box::pin(async move { Ok(found) })
    }
    fn find_sync_item_by_remote(
        &self,
        sd: SyncDirId,
        remote: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
        let found = self
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|it| it.sync_dir_id == sd && it.remote_path == remote)
            .cloned();
        Box::pin(async move { Ok(found) })
    }
    fn insert_sync_item(
        &self,
        sd: SyncDirId,
        local: String,
        remote: String,
        lt: i64,
        rt: i64,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        self.insert_item(sd, &local, &remote, lt, rt);
        Box::pin(async { Ok(()) })
    }
    fn update_sync_item_timestamps(
        &self,
        id: SyncItemId,
        lt: i64,
        rt: i64,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|it| it.id == id) {
            item.last_local_timestamp = lt;
            item.last_remote_timestamp = rt;
        }
        Box::pin(async { Ok(()) })
    }
    fn delete_sync_item(
        &self,
        id: SyncItemId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        self.items.lock().unwrap().retain(|it| it.id != id);
        Box::pin(async { Ok(()) })
    }
    fn delete_sync_item_by_paths(
        &self,
        sd: SyncDirId,
        local: &str,
        remote: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        self.items
            .lock()
            .unwrap()
            .retain(|it| !(it.sync_dir_id == sd && it.local_path == local && it.remote_path == remote));
        Box::pin(async { Ok(()) })
    }
    fn list_exclusions(
        &self,
        _sd: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncDirExclusion>, RepositoryError>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn insert_exclusion(
        &self,
        _sd: SyncDirId,
        _remote_path: String,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn delete_exclusion(
        &self,
        _id: SyncDirExclusionId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async { Ok(()) })
    }
    fn delete_sync_items_with_local_prefix(
        &self,
        sd: SyncDirId,
        prefix: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        let prefix = prefix.to_owned();
        let child_prefix = format!("{prefix}/");
        self.items.lock().unwrap().retain(|it| {
            !(it.sync_dir_id == sd
                && (it.local_path == prefix || it.local_path.starts_with(&child_prefix)))
        });
        Box::pin(async { Ok(()) })
    }
}

/// Programmable [`RcloneClient`]. Every op returns whatever the test sets
/// for the matching path (or a catch-all default). Call counts are
/// tracked for regression-style assertions.
pub struct FakeRclone {
    pub stat_map: Mutex<HashMap<String, Result<Option<RemoteItem>, String>>>,
    pub stat_sequence: Mutex<HashMap<String, Vec<Result<Option<RemoteItem>, String>>>>,
    pub list_map: Mutex<HashMap<String, Result<Vec<RemoteItem>, String>>>,
    pub copy_to_remote_result: Mutex<Result<(), String>>,
    pub copy_to_local_result: Mutex<Result<(), String>>,
    pub delete_file_result: Mutex<Result<(), String>>,
    pub purge_result: Mutex<Result<(), String>>,
    pub mkdir_result: Mutex<Result<(), String>>,

    pub stat_calls: Mutex<Vec<String>>,
    pub list_calls: Mutex<Vec<String>>,
    pub copy_to_remote_calls: Mutex<Vec<(String, String)>>,
    pub copy_to_local_calls: Mutex<Vec<(String, String)>>,
    pub delete_file_calls: Mutex<Vec<String>>,
    pub purge_calls: Mutex<Vec<String>>,
    pub mkdir_calls: Mutex<Vec<String>>,
}

impl Default for FakeRclone {
    fn default() -> Self {
        Self {
            stat_map: Mutex::new(HashMap::new()),
            stat_sequence: Mutex::new(HashMap::new()),
            list_map: Mutex::new(HashMap::new()),
            copy_to_remote_result: Mutex::new(Ok(())),
            copy_to_local_result: Mutex::new(Ok(())),
            delete_file_result: Mutex::new(Ok(())),
            purge_result: Mutex::new(Ok(())),
            mkdir_result: Mutex::new(Ok(())),
            stat_calls: Mutex::new(Vec::new()),
            list_calls: Mutex::new(Vec::new()),
            copy_to_remote_calls: Mutex::new(Vec::new()),
            copy_to_local_calls: Mutex::new(Vec::new()),
            delete_file_calls: Mutex::new(Vec::new()),
            purge_calls: Mutex::new(Vec::new()),
            mkdir_calls: Mutex::new(Vec::new()),
        }
    }
}

impl FakeRclone {
    pub fn set_stat(&self, path: &str, resp: Result<Option<RemoteItem>, String>) {
        self.stat_map.lock().unwrap().insert(path.to_owned(), resp);
    }
    /// Queue an ordered sequence of stat responses for a path. Each
    /// call consumes one entry; once exhausted, falls back to
    /// `stat_map` / `Ok(None)`. Useful for testing flows that stat
    /// the same path before and after a mutation.
    pub fn set_stat_sequence(
        &self,
        path: &str,
        responses: Vec<Result<Option<RemoteItem>, String>>,
    ) {
        self.stat_sequence
            .lock()
            .unwrap()
            .insert(path.to_owned(), responses);
    }
    pub fn set_list(&self, path: &str, resp: Result<Vec<RemoteItem>, String>) {
        self.list_map.lock().unwrap().insert(path.to_owned(), resp);
    }
    pub fn set_copy_to_remote(&self, resp: Result<(), String>) {
        *self.copy_to_remote_result.lock().unwrap() = resp;
    }
    pub fn set_copy_to_local(&self, resp: Result<(), String>) {
        *self.copy_to_local_result.lock().unwrap() = resp;
    }
    pub fn set_delete_file(&self, resp: Result<(), String>) {
        *self.delete_file_result.lock().unwrap() = resp;
    }
    pub fn set_purge(&self, resp: Result<(), String>) {
        *self.purge_result.lock().unwrap() = resp;
    }
}

impl RcloneClient for FakeRclone {
    fn stat(&self, _remote: &str, path: &str) -> Result<Option<RemoteItem>, String> {
        self.stat_calls.lock().unwrap().push(path.to_owned());
        // Sequence wins if present: pop the next response.
        if let Some(seq) = self.stat_sequence.lock().unwrap().get_mut(path)
            && !seq.is_empty()
        {
            return seq.remove(0);
        }
        match self.stat_map.lock().unwrap().get(path) {
            Some(r) => r.clone(),
            None => Ok(None),
        }
    }
    fn list(
        &self,
        _remote: &str,
        path: &str,
        _recursive: bool,
        _filter: ListFilter,
    ) -> Result<Vec<RemoteItem>, String> {
        self.list_calls.lock().unwrap().push(path.to_owned());
        match self.list_map.lock().unwrap().get(path) {
            Some(r) => r.clone(),
            None => Ok(vec![]),
        }
    }
    fn mkdir(&self, _remote: &str, path: &str) -> Result<(), String> {
        self.mkdir_calls.lock().unwrap().push(path.to_owned());
        self.mkdir_result.lock().unwrap().clone()
    }
    fn delete_file(&self, _remote: &str, path: &str) -> Result<(), String> {
        self.delete_file_calls.lock().unwrap().push(path.to_owned());
        self.delete_file_result.lock().unwrap().clone()
    }
    fn purge(&self, _remote: &str, path: &str) -> Result<(), String> {
        self.purge_calls.lock().unwrap().push(path.to_owned());
        self.purge_result.lock().unwrap().clone()
    }
    fn copy_to_remote(
        &self,
        local: &str,
        _remote: &str,
        path: &str,
    ) -> Result<(), String> {
        self.copy_to_remote_calls
            .lock()
            .unwrap()
            .push((local.to_owned(), path.to_owned()));
        self.copy_to_remote_result.lock().unwrap().clone()
    }
    fn copy_to_local(
        &self,
        local: &str,
        _remote: &str,
        path: &str,
    ) -> Result<(), String> {
        self.copy_to_local_calls
            .lock()
            .unwrap()
            .push((local.to_owned(), path.to_owned()));
        self.copy_to_local_result.lock().unwrap().clone()
    }
    fn delete_config(&self, _remote: &str) -> Result<(), String> {
        Ok(())
    }
    fn create_config(&self, _payload: String) -> Result<(), String> {
        Ok(())
    }
    fn remote_type(&self, _remote: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// Build a test Remote with the given id and name.
pub fn remote(id: i32, name: &str) -> Remote {
    Remote {
        id: RemoteId(id),
        name: name.to_owned(),
        policy: SyncPolicy::default(),
        provider_kind: None,
        backend: Backend::Rclone,
        session_path: None,
    }
}

/// Build a test SyncDir. `local_path` is typically the TempDir path.
pub fn sync_dir(id: i32, remote_id: i32, local_path: &str, remote_path: &str) -> SyncDir {
    SyncDir {
        id: SyncDirId(id),
        remote_id: RemoteId(remote_id),
        local_path: local_path.to_owned(),
        remote_path: remote_path.to_owned(),
    }
}

/// Build a RemoteItem at the given path with the given Unix timestamp.
pub fn remote_item(path: &str, is_dir: bool, unix_ts: i64) -> RemoteItem {
    let name = path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
    RemoteItem {
        is_dir,
        path: path.to_owned(),
        name: name.to_owned(),
        mod_time: OffsetDateTime::from_unix_timestamp(unix_ts).unwrap(),
    }
}

/// Set the mtime of a local file to `unix_ts` so tests can control the
/// local_utc_timestamp the sync algorithm reads.
pub fn touch_mtime(path: &Path, unix_ts: i64) {
    let _ = filetime_set(path, unix_ts);
}

fn filetime_set(path: &Path, unix_ts: i64) -> std::io::Result<()> {
    use std::fs::File;
    use std::time::{Duration, UNIX_EPOCH};
    let file = File::open(path)?;
    let time = UNIX_EPOCH + Duration::from_secs(unix_ts as u64);
    file.set_modified(time)?;
    Ok(())
}
