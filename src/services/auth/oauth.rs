//! OAuth2 remote provisioning via `rclone authorize` (Dropbox, Google Drive, pCloud).

use std::process::Command;

use serde_json::json;

use crate::domain::{
    ports::{BackendClient, Repository},
    remote::RemoteId,
};
use crate::util;

/// OAuth providers that use `rclone authorize` for token capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Dropbox,
    GDrive,
    PCloud,
}

impl OAuthProvider {
    pub(super) fn rclone_type(self) -> &'static str {
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
    client: &dyn BackendClient,
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

/// Reauthenticate an existing OAuth-backed remote (Dropbox / Google
/// Drive / pCloud). Re-runs `rclone authorize` for fresh tokens, then
/// replaces the rclone config entry under the same name so the new
/// token takes effect. Leaves the DB row untouched — same id, same
/// name, same sync_dirs — so the user's configuration survives the
/// reauth unchanged.
pub fn reauth_oauth_remote(
    name: &str,
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    client: &dyn BackendClient,
) -> Result<(), String> {
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

    // rclone's `config/create` rejects a name that already exists, so
    // drop the stale entry first. Failure to delete is non-fatal — the
    // create step will surface the real error if anything's wrong.
    let _ = client.delete_config(name);
    client.create_config(payload)
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
