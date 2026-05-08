//! Native ProtonDrive remote provisioning and reauthentication.

use std::{path::{Path, PathBuf}, sync::Arc};

use crate::{
    domain::{
        ports::Repository,
        remote::{ProviderKind, RemoteId, SyncPolicy},
    },
    infrastructure::{client_router::ClientRouter, proton::client::NativeProtonClient},
    util,
};

/// Proton Drive: username + password + optional TOTP. No browser step.
///
/// Uses the native Proton client (no rclone in the path). Logs in
/// against Proton's API, writes the persistable credential blob to
/// `config_dir/proton-session-<name>.json`, inserts the remote with
/// `backend = native-proton`, and registers the session's UID on the
/// router so the sync engine picks up the native adapter immediately.
pub fn add_proton_drive_remote(
    name: &str,
    username: &str,
    password: &str,
    totp: &str,
    config_dir: &Path,
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

    let session_path = proton_session_path(config_dir, name);
    if let Some(parent) = session_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    celeste_go::proton::save_session(&cred.uid, &session_path)?;

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

/// Reauthenticate an existing Proton Drive remote whose session blob
/// has expired (2FA refresh exhausted) or gone missing. Logs in with
/// fresh credentials, overwrites the session file, and swaps the
/// router's disabled stub for a live [`NativeProtonClient`]. Leaves
/// the DB row untouched — same name, same sync_dirs, same exclusions —
/// so the user's configuration survives the reauth unchanged.
pub fn reauth_proton_drive_remote(
    name: &str,
    username: &str,
    password: &str,
    totp: &str,
    config_dir: &Path,
    router: &ClientRouter,
) -> Result<(), String> {
    let params = celeste_go::proton::LoginParams {
        username: username.to_owned(),
        password: password.to_owned(),
        two_fa: totp.to_owned(),
        mailbox_password: String::new(),
    };
    let cred = celeste_go::proton::login(&params)?;

    let session_path = proton_session_path(config_dir, name);
    if let Some(parent) = session_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    celeste_go::proton::save_session(&cred.uid, &session_path)?;

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
pub(super) fn proton_session_path(config_dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    config_dir.join(format!("proton-session-{safe}.json"))
}
