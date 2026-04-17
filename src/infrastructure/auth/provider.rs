//! Adapter implementation of [`crate::domain::ports::AuthProvider`].
//!
//! The existing `super::login` flow (OAuth2 for Dropbox/GDrive/pCloud,
//! WebDAV basic-auth for Nextcloud/Owncloud/WebDAV, and Proton Drive
//! custom auth) stays as-is — this adapter will expose it through the
//! port once `services::auth_service` needs it.

use crate::domain::ports::AuthProvider;

pub struct OAuthProviders;

impl OAuthProviders {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OAuthProviders {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthProvider for OAuthProviders {}
