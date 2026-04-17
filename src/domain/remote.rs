use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Dropbox,
    GDrive,
    Nextcloud,
    Owncloud,
    PCloud,
    ProtonDrive,
    WebDav,
}

#[derive(Clone, Debug)]
pub struct Remote {
    pub id: RemoteId,
    pub name: String,
    pub policy: SyncPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPolicy {
    pub interval: Duration,
    pub instant_sync: bool,
    pub enabled: bool,
}

impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300),
            instant_sync: false,
            enabled: true,
        }
    }
}
