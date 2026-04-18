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

impl ProviderKind {
    /// Map rclone's backend `type` field to our domain enum. Everything
    /// that isn't recognised is `None` — treated as "no provider-specific
    /// quirks".
    pub fn from_rclone_type(t: &str) -> Option<Self> {
        match t {
            "dropbox" => Some(Self::Dropbox),
            "drive" => Some(Self::GDrive),
            "pcloud" => Some(Self::PCloud),
            "protondrive" => Some(Self::ProtonDrive),
            // WebDAV-family all use rclone's "webdav" backend; the
            // vendor sub-selector picks Nextcloud / Owncloud / plain
            // WebDAV. Can't tell them apart from `type` alone, so
            // collapse to WebDav — the UI hints apply equally.
            "webdav" => Some(Self::WebDav),
            _ => None,
        }
    }

    /// Interval a fresh remote of this kind should be created with.
    /// Proton Drive rate-limits hard enough that 5 s / 15 s trap the
    /// sync in back-to-back 429 storms; 30 s tested clean.
    pub fn default_interval(self) -> Interval {
        match self {
            ProviderKind::ProtonDrive => Interval::ThirtySeconds,
            _ => Interval::FifteenSeconds,
        }
    }

    /// Intervals shorter than this are flagged in the UI with a ⚠.
    /// Above it, the backend should cope without the warning. `None`
    /// means no warnings for this provider.
    pub fn short_interval_threshold(self) -> Option<Interval> {
        match self {
            ProviderKind::ProtonDrive => Some(Interval::ThirtySeconds),
            _ => None,
        }
    }

    /// Text shown on the tooltip next to the picker when the provider
    /// has short-interval warnings.
    pub fn short_interval_warning(self) -> Option<&'static str> {
        match self {
            ProviderKind::ProtonDrive => Some(
                "Proton Drive rate-limits every per-file revision fetch — intervals shorter than 30 s trip the backoff and starve the sync. 30 s is the tested-clean minimum.",
            ),
            _ => None,
        }
    }

    /// Substrings that, when spotted in rclone's stderr during a pass,
    /// mark the pass as *degraded* — the backend was internally retrying
    /// rate-limits and any `Ok(...)` it returned may reflect partial
    /// data. All substrings must match within the same log line.
    ///
    /// Return an empty slice for backends we don't have a marker set
    /// for yet; those passes can never be flagged degraded and will
    /// run as before.
    pub fn rate_limit_markers(self) -> &'static [&'static [&'static str]] {
        match self {
            // go-proton-api prints `status=429` alongside its package
            // tag on every retry; Proton's own API also surfaces
            // "Too many recent API requests".
            ProviderKind::ProtonDrive => &[
                &["go-proton-api", "status=429"],
                &["go-proton-api", "Too many requests"],
                &["Too many recent API requests"],
            ],
            // Google Drive quota / per-minute limits — the error
            // message pattern rclone surfaces alongside any internal
            // retry warnings.
            ProviderKind::GDrive => &[
                &["rateLimitExceeded"],
                &["userRateLimitExceeded"],
                &["Quota exceeded"],
            ],
            // Dropbox / pCloud / WebDAV etc. don't have a characterised
            // marker set yet. Leaving empty means "never flag as
            // degraded"; we keep the current (pre-backoff) behaviour
            // on them.
            _ => &[],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Remote {
    pub id: RemoteId,
    pub name: String,
    pub policy: SyncPolicy,
    /// Filled in after load by the app layer via `RcloneClient::remote_type`.
    /// Drives provider-specific UI hints (interval warnings, defaults).
    pub provider_kind: Option<ProviderKind>,
    /// Which adapter owns this remote at runtime. New rows default
    /// to `Backend::Rclone`; ProtonDrive remotes added via the
    /// native auth flow are stamped `Backend::NativeProton`.
    pub backend: Backend,
    /// For native-backend remotes, the filesystem path to the
    /// persisted session blob that `celeste-native-sys` reads with
    /// `ProtonDrive_ResumeSession`. `None` for rclone remotes.
    pub session_path: Option<String>,
}

/// Which adapter drives a given remote. Kept as a plain enum rather
/// than a trait object so it can be persisted to the DB as a string
/// and pattern-matched on in the sync scheduler's routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    /// librclone's RPC surface — every backend rclone supports.
    Rclone,
    /// Native ProtonDrive client in `infrastructure::proton`.
    NativeProton,
}

impl Backend {
    /// Parse the DB-stored string form. Unknown strings fall back
    /// to `Rclone` — old rows (or rows from a future schema we
    /// haven't learned about yet) keep working.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "native-proton" => Self::NativeProton,
            _ => Self::Rclone,
        }
    }

    /// The string form stored in the DB.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Rclone => "rclone",
            Self::NativeProton => "native-proton",
        }
    }
}

/// Per-remote sync cadence. Fixed to a handful of discrete choices —
/// any other value coming out of the DB is rounded to the nearest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPolicy {
    pub interval: Interval,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interval {
    FiveSeconds,
    FifteenSeconds,
    ThirtySeconds,
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    ThirtyMinutes,
    OneHour,
}

impl Interval {
    pub const ALL: [Interval; 8] = [
        Interval::FiveSeconds,
        Interval::FifteenSeconds,
        Interval::ThirtySeconds,
        Interval::OneMinute,
        Interval::FiveMinutes,
        Interval::FifteenMinutes,
        Interval::ThirtyMinutes,
        Interval::OneHour,
    ];

    pub fn duration(self) -> Duration {
        Duration::from_secs(self.seconds())
    }

    pub fn seconds(self) -> u64 {
        match self {
            Interval::FiveSeconds => 5,
            Interval::FifteenSeconds => 15,
            Interval::ThirtySeconds => 30,
            Interval::OneMinute => 60,
            Interval::FiveMinutes => 300,
            Interval::FifteenMinutes => 900,
            Interval::ThirtyMinutes => 1_800,
            Interval::OneHour => 3_600,
        }
    }

    /// Map an arbitrary seconds value to the closest supported choice.
    pub fn from_seconds(secs: u64) -> Self {
        Self::ALL
            .into_iter()
            .min_by_key(|i| i.seconds().abs_diff(secs))
            .unwrap_or(Interval::FifteenSeconds)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_with(kind: Option<ProviderKind>, picked: Interval) -> Remote {
        Remote {
            id: RemoteId(1),
            name: "Remote".to_owned(),
            policy: SyncPolicy {
                interval: picked,
                enabled: true,
            },
            provider_kind: kind,
            backend: Backend::Rclone,
            session_path: None,
        }
    }

    #[test]
    fn proton_drive_defaults_to_thirty_seconds() {
        assert_eq!(
            ProviderKind::ProtonDrive.default_interval(),
            Interval::ThirtySeconds,
        );
    }

    #[test]
    fn other_providers_default_to_fifteen_seconds() {
        assert_eq!(
            ProviderKind::GDrive.default_interval(),
            Interval::FifteenSeconds,
        );
        assert_eq!(
            ProviderKind::WebDav.default_interval(),
            Interval::FifteenSeconds,
        );
    }

    #[test]
    fn short_interval_threshold_only_proton() {
        assert_eq!(
            ProviderKind::ProtonDrive.short_interval_threshold(),
            Some(Interval::ThirtySeconds),
        );
        assert_eq!(ProviderKind::GDrive.short_interval_threshold(), None);
        assert_eq!(ProviderKind::Dropbox.short_interval_threshold(), None);
    }

    #[test]
    fn user_picking_still_survives_through_the_policy() {
        // No clamping anywhere: whatever the user picks is what we use.
        let r = remote_with(Some(ProviderKind::ProtonDrive), Interval::FiveSeconds);
        assert_eq!(r.policy.interval.duration(), Duration::from_secs(5));
    }

    #[test]
    fn rclone_type_mapping_covers_known_backends() {
        assert_eq!(
            ProviderKind::from_rclone_type("protondrive"),
            Some(ProviderKind::ProtonDrive)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("drive"),
            Some(ProviderKind::GDrive)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("dropbox"),
            Some(ProviderKind::Dropbox)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("webdav"),
            Some(ProviderKind::WebDav)
        );
        assert_eq!(ProviderKind::from_rclone_type("bogus"), None);
    }
}
