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

/// Per-remote sync cadence. Fixed to the two choices `5s` and `15s` —
/// any other value coming out of the DB is clamped to the closer one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPolicy {
    pub interval: Interval,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interval {
    FiveSeconds,
    FifteenSeconds,
}

impl Interval {
    pub fn duration(self) -> Duration {
        Duration::from_secs(self.seconds())
    }

    pub fn seconds(self) -> u64 {
        match self {
            Interval::FiveSeconds => 5,
            Interval::FifteenSeconds => 15,
        }
    }

    pub fn from_seconds(secs: u64) -> Self {
        if secs <= 9 {
            Interval::FiveSeconds
        } else {
            Interval::FifteenSeconds
        }
    }
}

impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            interval: Interval::FifteenSeconds,
            enabled: true,
        }
    }
}
