//! Native ProtonDrive remote provisioning and reauthentication.

use std::{path::Path, sync::Arc};

use crate::{
    domain::{
        ports::Repository,
        remote::{ProviderKind, RemoteId, SyncPolicy},
    },
    infrastructure::{client_router::ClientRouter, proton::client::NativeProtonClient},
    services::secrets,
    util,
};

/// Marker stored in the `remotes.session_path` column for
/// keyring-backed remotes. The column is preserved as a "session
/// configured" flag — its actual contents are no longer a path, since
/// the credential blob lives in the OS keyring under the remote's name.
const KEYRING_SESSION_MARKER: &str = "keyring";

/// Proton Drive: username + password + optional TOTP. No browser step.
///
/// Uses the native Proton client (no rclone in the path). Logs in
/// against Proton's API, persists the reusable credential blob into
/// the OS keyring under `Celeste Keys / proton-session-<name>`,
/// inserts the remote with `backend = native-proton`, and registers
/// the session's UID on the router so the sync engine picks up the
/// native adapter immediately.
pub fn add_proton_drive_remote(
    name: &str,
    username: &str,
    password: &str,
    totp: &str,
    repo: &dyn Repository,
    router: &ClientRouter,
) -> Result<RemoteId, String> {
    let params = celeste_go::proton::LoginParams {
        username: username.to_owned(),
        password: password.to_owned(),
        two_fa: totp.to_owned(),
        mailbox_password: String::new(),
    };
    let cred = celeste_go::proton::login(&params)?;

    persist_session_to_keyring(name, &cred.uid)?;

    let id = util::await_future(repo.insert_native_proton_remote(
        name.to_owned(),
        KEYRING_SESSION_MARKER.to_owned(),
    ))
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

/// Reauthenticate an existing Proton Drive remote whose session blob
/// has expired (2FA refresh exhausted) or gone missing. Logs in with
/// fresh credentials, overwrites the keyring entry, and swaps the
/// router's disabled stub for a live [`NativeProtonClient`]. Leaves
/// the DB row untouched — same name, same sync_dirs, same exclusions —
/// so the user's configuration survives the reauth unchanged.
pub fn reauth_proton_drive_remote(
    name: &str,
    username: &str,
    password: &str,
    totp: &str,
    router: &ClientRouter,
) -> Result<(), String> {
    let params = celeste_go::proton::LoginParams {
        username: username.to_owned(),
        password: password.to_owned(),
        two_fa: totp.to_owned(),
        mailbox_password: String::new(),
    };
    let cred = celeste_go::proton::login(&params)?;

    persist_session_to_keyring(name, &cred.uid)?;

    router.register(
        name.to_owned(),
        Arc::new(NativeProtonClient::new(cred.uid)),
    );
    Ok(())
}

/// Resume a previously saved session for `remote_name` from the OS
/// keyring. Returns `Ok(None)` when no entry exists (the caller
/// surfaces a "reauth required" message); other errors propagate.
pub fn resume_session_from_keyring(
    remote_name: &str,
) -> Result<Option<celeste_go::proton::ReusableCredential>, String> {
    let Some(blob) = secrets::load(&secrets::proton_account(remote_name))? else {
        return Ok(None);
    };
    let tmp = TempBlob::write(&blob)?;
    let cred = celeste_go::proton::resume_session(tmp.path())?;
    Ok(Some(cred))
}

/// Drop the keyring entry for `remote_name`. Idempotent — missing
/// entries are not an error.
pub fn forget_session(remote_name: &str) -> Result<(), String> {
    secrets::delete(&secrets::proton_account(remote_name))
}

/// Ask the Go side to serialise the live session for `uid` into a
/// tempfile, slurp the JSON back out, and stash it in the keyring.
/// The tempfile is deleted on success or failure.
fn persist_session_to_keyring(remote_name: &str, uid: &str) -> Result<(), String> {
    let tmp = TempBlob::reserve()?;
    celeste_go::proton::save_session(uid, tmp.path())?;
    let blob = std::fs::read_to_string(tmp.path())
        .map_err(|e| format!("reading proton session tempfile: {e}"))?;
    secrets::store(&secrets::proton_account(remote_name), &blob)?;
    Ok(())
}

/// Tempfile under `$XDG_RUNTIME_DIR` (falling back to the system temp
/// dir) used as a hand-off between the Go FFI's path-based session API
/// and the keyring-backed blob storage. Created with 0600; deleted on
/// drop so a crash in between leaves at most a momentary on-disk copy.
struct TempBlob {
    path: std::path::PathBuf,
}

impl TempBlob {
    /// Reserve a path without creating the file (Go's `SaveSession`
    /// writes it).
    fn reserve() -> Result<Self, String> {
        let path = tempfile_path("save");
        Ok(Self { path })
    }

    /// Create the file with `contents` at 0600 so the Go side's
    /// `ResumeSession` can read it back.
    fn write(contents: &str) -> Result<Self, String> {
        use std::io::Write;
        let path = tempfile_path("resume");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&path)
            .map_err(|e| format!("creating proton session tempfile: {e}"))?;
        file.write_all(contents.as_bytes())
            .map_err(|e| format!("writing proton session tempfile: {e}"))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempBlob {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn tempfile_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("celeste-proton-{tag}-{pid}-{nanos}.json"))
}
