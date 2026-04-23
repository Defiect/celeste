//! Account-provisioning workflows. Covers:
//!
//! - [`add_webdav_remote`] — Generic WebDAV / Nextcloud / Owncloud (raw
//!   username + password; just a config/create + DB insert).
//! - [`add_oauth_remote`] — Dropbox / Google Drive / pCloud (shells out
//!   to `rclone authorize`, which opens the default browser, runs its
//!   own OAuth callback listener, and prints the token JSON).
//! - [`add_proton_drive_remote`] — Proton Drive (username + password +
//!   optional 2FA, no browser step).

use std::{path::{Path, PathBuf}, process::Command, sync::Arc};

use serde_json::json;

use crate::{
    domain::{
        ports::{RcloneClient, Repository},
        remote::{ProviderKind, RemoteId, SyncPolicy},
    },
    infrastructure::{client_router::ClientRouter, proton::client::NativeProtonClient},
    util,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebDavVendor {
    WebDav,
    Nextcloud,
    Owncloud,
}

impl WebDavVendor {
    fn rclone_vendor(self) -> &'static str {
        match self {
            WebDavVendor::WebDav => "webdav",
            WebDavVendor::Nextcloud => "nextcloud",
            WebDavVendor::Owncloud => "owncloud",
        }
    }
}

/// Add a new WebDAV-family remote: create the rclone config, insert the
/// DB row, and return the new `RemoteId`. Validation (trying to list the
/// root) is the caller's responsibility — mirrors the GTK flow which
/// validates via `can_login` before inserting.
pub fn add_webdav_remote(
    name: &str,
    url: &str,
    user: &str,
    pass: &str,
    vendor: WebDavVendor,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
) -> Result<RemoteId, String> {
    // For Nextcloud/Owncloud the GTK flow reformats the URL to include
    // `/remote.php/dav/files/<user>`; mirror that here so configs the
    // Iced UI creates line up with configs the GTK UI creates.
    let effective_url = match vendor {
        WebDavVendor::Nextcloud | WebDavVendor::Owncloud => {
            let trimmed = url.trim_end_matches('/');
            format!("{trimmed}/remote.php/dav/files/{user}")
        }
        WebDavVendor::WebDav => url.to_owned(),
    };

    let payload = json!({
        "name": name,
        "parameters": {
            "url": effective_url,
            "vendor": vendor.rclone_vendor(),
            "user": user,
            "pass": pass,
        },
        "type": "webdav",
        "opt": { "obscure": true },
    })
    .to_string();

    client.create_config(payload)?;
    util::await_future(repo.insert_remote(name.to_owned()))
        .map_err(|e| e.to_string())
}

/// Proton Drive: username + password + optional TOTP. No browser step.
///
/// Uses the native Proton client (no rclone in the path). Logs in
/// against Proton's API, writes the persistable credential blob to
/// `config_dir/proton-session-<name>.json`, inserts the remote with
/// `backend = native-proton`, and registers the session's UID on the
/// router so the sync engine picks up the native adapter
/// immediately.
pub fn add_proton_drive_remote(
    name: &str,
    username: &str,
    password: &str,
    totp: &str,
    config_dir: &Path,
    repo: &dyn Repository,
    router: &ClientRouter,
) -> Result<RemoteId, String> {
    let params = librclone::proton::LoginParams {
        username: username.to_owned(),
        password: password.to_owned(),
        two_fa: totp.to_owned(),
        mailbox_password: String::new(),
    };
    let cred = librclone::proton::login(&params)?;

    let session_path = proton_session_path(config_dir, name);
    if let Some(parent) = session_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    librclone::proton::save_session(&cred.uid, &session_path)?;

    let id = util::await_future(
        repo.insert_native_proton_remote(name.to_owned(), session_path.display().to_string()),
    )
    .map_err(|e| e.to_string())?;

    // Proton Drive rate-limits short polls; ship the provider-specific
    // default interval so the user doesn't have to discover this the
    // hard way.
    let policy = SyncPolicy {
        interval: ProviderKind::ProtonDrive.default_interval(),
        enabled: true,
    };
    let _ = util::await_future(repo.set_policy(id, policy));

    router.register(
        name.to_owned(),
        Arc::new(NativeProtonClient::new(cred.uid)),
    );
    Ok(id)
}

/// Re-authenticate an existing Proton Drive remote whose session blob
/// has expired (2FA refresh exhausted) or gone missing. Logs in with
/// fresh credentials, overwrites the session file, and swaps the
/// router's disabled stub for a live [`NativeProtonClient`]. Leaves
/// the DB row untouched — same name, same sync_dirs, same exclusions —
/// so the user's configuration survives the re-auth unchanged.
pub fn reauth_proton_drive_remote(
    name: &str,
    username: &str,
    password: &str,
    totp: &str,
    config_dir: &Path,
    router: &ClientRouter,
) -> Result<(), String> {
    let params = librclone::proton::LoginParams {
        username: username.to_owned(),
        password: password.to_owned(),
        two_fa: totp.to_owned(),
        mailbox_password: String::new(),
    };
    let cred = librclone::proton::login(&params)?;

    let session_path = proton_session_path(config_dir, name);
    if let Some(parent) = session_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    librclone::proton::save_session(&cred.uid, &session_path)?;

    router.register(
        name.to_owned(),
        Arc::new(NativeProtonClient::new(cred.uid)),
    );
    Ok(())
}

/// Where the native-backed Proton session blob lives for a remote
/// named `name`. Kept in one place so the add / resume / delete flows
/// agree on the location. Sanitises the name so unusual characters
/// don't hit the filesystem.
fn proton_session_path(config_dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    config_dir.join(format!("proton-session-{safe}.json"))
}

/// OAuth providers that use `rclone authorize` for token capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Dropbox,
    GDrive,
    PCloud,
}

impl OAuthProvider {
    fn rclone_type(self) -> &'static str {
        match self {
            OAuthProvider::Dropbox => "dropbox",
            OAuthProvider::GDrive => "drive",
            OAuthProvider::PCloud => "pcloud",
        }
    }
}

/// OAuth2 via `rclone authorize`. Blocks until the user finishes the
/// browser flow (or closes it); meant to run on a blocking tokio task.
/// `client_id`/`client_secret` override rclone's built-in defaults.
pub fn add_oauth_remote(
    name: &str,
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    repo: &dyn Repository,
    client: &dyn RcloneClient,
) -> Result<RemoteId, String> {
    let token = run_rclone_authorize(provider, client_id, client_secret)?;

    let payload = json!({
        "name": name,
        "parameters": {
            "client_id": client_id.unwrap_or_default(),
            "client_secret": client_secret.unwrap_or_default(),
            "token": token,
            "config_refresh_token": false,
        },
        "type": provider.rclone_type(),
    })
    .to_string();

    client.create_config(payload)?;
    util::await_future(repo.insert_remote(name.to_owned()))
        .map_err(|e| e.to_string())
}

fn run_rclone_authorize(
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> Result<String, String> {
    let mut cmd = Command::new("rclone");
    cmd.arg("authorize").arg(provider.rclone_type());
    if let (Some(id), Some(secret)) = (client_id, client_secret) {
        if !id.is_empty() && !secret.is_empty() {
            cmd.arg(id).arg(secret);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("couldn't run `rclone authorize`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "rclone authorize exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    extract_token(&String::from_utf8_lossy(&output.stdout))
}

/// Pull the token out of rclone's `authorize` stdout. rclone prints
/// a banner around the token (either a JSON object or a base-hex blob)
/// so we scan for the first line that looks like one.
fn extract_token(stdout: &str) -> Result<String, String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            return Ok(trimmed.to_owned());
        }
    }
    Err(format!(
        "couldn't parse rclone authorize output (expected a JSON token line). Raw:\n{}",
        stdout.trim()
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::domain::{
        ports::{BoxFuture, Repository, RepositoryError},
        remote::{Remote, RemoteId, SyncPolicy},
        sync::{
            ListFilter, RemoteItem, SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId,
            SyncItem, SyncItemId,
        },
    };

    #[derive(Default)]
    struct FakeRepo {
        inserted: Mutex<Vec<String>>,
        next_id: Mutex<i32>,
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
        fn insert_remote(
            &self,
            name: String,
        ) -> BoxFuture<'_, Result<RemoteId, RepositoryError>> {
            let mut inserted = self.inserted.lock().unwrap();
            inserted.push(name);
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            let id = RemoteId(*next);
            Box::pin(async move { Ok(id) })
        }
        fn insert_native_proton_remote(
            &self,
            name: String,
            _session_path: String,
        ) -> BoxFuture<'_, Result<RemoteId, RepositoryError>> {
            let mut inserted = self.inserted.lock().unwrap();
            inserted.push(name);
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            let id = RemoteId(*next);
            Box::pin(async move { Ok(id) })
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
        fn sync_dir_exists(
            &self,
            _l: &str,
            _r: &str,
        ) -> BoxFuture<'_, Result<bool, RepositoryError>> {
            Box::pin(async { Ok(false) })
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
            _sd: SyncDirId,
        ) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn find_sync_item_by_paths(
            &self,
            _sd: SyncDirId,
            _l: &str,
            _r: &str,
        ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
            Box::pin(async { Ok(None) })
        }
        fn find_sync_item_by_local(
            &self,
            _sd: SyncDirId,
            _l: &str,
        ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
            Box::pin(async { Ok(None) })
        }
        fn find_sync_item_by_remote(
            &self,
            _sd: SyncDirId,
            _r: &str,
        ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
            Box::pin(async { Ok(None) })
        }
        fn insert_sync_item(
            &self,
            _sd: SyncDirId,
            _l: String,
            _r: String,
            _lt: i64,
            _rt: i64,
        ) -> BoxFuture<'_, Result<(), RepositoryError>> {
            Box::pin(async { Ok(()) })
        }
        fn update_sync_item_timestamps(
            &self,
            _id: SyncItemId,
            _lt: i64,
            _rt: i64,
        ) -> BoxFuture<'_, Result<(), RepositoryError>> {
            Box::pin(async { Ok(()) })
        }
        fn delete_sync_item(
            &self,
            _id: SyncItemId,
        ) -> BoxFuture<'_, Result<(), RepositoryError>> {
            Box::pin(async { Ok(()) })
        }
        fn delete_sync_item_by_paths(
            &self,
            _sd: SyncDirId,
            _l: &str,
            _r: &str,
        ) -> BoxFuture<'_, Result<(), RepositoryError>> {
            Box::pin(async { Ok(()) })
        }
        fn list_all_sync_dirs(
            &self,
        ) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn delete_sync_items_with_local_prefix(
            &self,
            _sd: SyncDirId,
            _prefix: &str,
        ) -> BoxFuture<'_, Result<(), RepositoryError>> {
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
    }

    #[derive(Default)]
    struct FakeRclone {
        created: Mutex<Vec<String>>,
    }

    impl crate::domain::ports::RcloneClient for FakeRclone {
        fn stat(&self, _r: &str, _p: &str) -> Result<Option<RemoteItem>, String> {
            Ok(None)
        }
        fn list(
            &self,
            _r: &str,
            _p: &str,
            _rec: bool,
            _f: ListFilter,
        ) -> Result<Vec<RemoteItem>, String> {
            Ok(vec![])
        }
        fn mkdir(&self, _r: &str, _p: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete_file(&self, _r: &str, _p: &str) -> Result<(), String> {
            Ok(())
        }
        fn purge(&self, _r: &str, _p: &str) -> Result<(), String> {
            Ok(())
        }
        fn copy_to_remote(&self, _l: &str, _r: &str, _rp: &str) -> Result<(), String> {
            Ok(())
        }
        fn copy_to_local(&self, _l: &str, _r: &str, _rp: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete_config(&self, _r: &str) -> Result<(), String> {
            Ok(())
        }
        fn create_config(&self, payload: String) -> Result<(), String> {
            self.created.lock().unwrap().push(payload);
            Ok(())
        }
        fn remote_type(&self, _r: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    #[test]
    fn webdav_creates_rclone_config_and_inserts_row() {
        let repo = FakeRepo::default();
        let client = FakeRclone::default();

        let id = add_webdav_remote(
            "Home NAS",
            "https://nas.example.org/webdav",
            "alex",
            "hunter2",
            WebDavVendor::WebDav,
            &repo,
            &client,
        )
        .expect("add should succeed against fakes");

        assert_eq!(id.0, 1);
        assert_eq!(repo.inserted.lock().unwrap().as_slice(), &["Home NAS".to_owned()]);
        let created = client.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert!(created[0].contains("\"vendor\":\"webdav\""));
        assert!(created[0].contains("\"user\":\"alex\""));
    }

    #[test]
    fn nextcloud_reformats_the_url() {
        let repo = FakeRepo::default();
        let client = FakeRclone::default();

        add_webdav_remote(
            "Work Nextcloud",
            "https://cloud.example.org",
            "alex",
            "hunter2",
            WebDavVendor::Nextcloud,
            &repo,
            &client,
        )
        .unwrap();

        let created = client.created.lock().unwrap();
        assert!(
            created[0].contains("/remote.php/dav/files/alex"),
            "expected Nextcloud URL to be rewritten, got: {}",
            created[0]
        );
    }
}
