//! Account-provisioning workflows. WebDAV-family providers only for now;
//! OAuth providers (Dropbox, Google Drive, pCloud) still go through the
//! GTK login flow in `infrastructure::auth`.

use serde_json::json;

use crate::{
    domain::{
        ports::{RcloneClient, Repository},
        remote::RemoteId,
    },
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
