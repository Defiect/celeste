//! Hierarchical sync run-state: Dir → Remote → App.
//!
//! `RunState` is the single enum for all levels. `RemoteState` owns per-dir
//! states and the auth-failure bookkeeping (transitively pausing siblings
//! when one dir needs re-authentication). `AppState` is the top-level holder,
//! providing the coordinator-facing API used by `CelesteApp`.

use std::collections::HashMap;

use super::{remote::RemoteId, sync::SyncDirId};

// ---------------------------------------------------------------------------
// SyncActivity / RunState
// ---------------------------------------------------------------------------

/// What the sync engine is currently doing inside a `Syncing` pass.
/// Carried inside `RunState::Syncing` so the UI can show a structured
/// activity label instead of parsing log strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncActivity {
    Listing,
    Downloading,
    Uploading,
    Deleting,
    Resolving,
}

/// Coarse run-state for a sync directory (or its roll-up at remote/app level).
///
/// Severity ordering (low → high): Waiting < Paused < AuthNeeded < Synced <
/// Syncing < Warning < Error. Roll-up at the remote level takes the maximum
/// over children, then applies the `!enabled` → Paused downgrade (unless a
/// child is already `AuthNeeded`, which overrides the downgrade).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Between scheduled passes or fresh before first sync.
    Waiting,
    /// Explicitly disabled by the user, OR transitively paused while a
    /// sibling dir on the same remote is waiting for re-authentication.
    Paused,
    /// This dir surfaced an auth failure; user must re-authenticate the
    /// parent remote before syncing can resume.
    AuthNeeded,
    /// A pass just completed with no errors.
    Synced,
    /// A sync pass is currently running.
    Syncing(SyncActivity),
    /// Last pass degraded (rate-limited, per-file errors, conflicts).
    /// Sticky — a subsequent clean `Synced` does not clear it.
    Warning,
    /// Last pass aborted (snapshot failure, missing auth, etc.).
    /// Sticky like `Warning`.
    Error,
}

impl RunState {
    fn severity(self) -> u8 {
        match self {
            Self::Waiting => 0,
            Self::Paused => 1,
            Self::AuthNeeded => 2,
            Self::Synced => 3,
            Self::Syncing(_) => 4,
            Self::Warning => 5,
            Self::Error => 6,
        }
    }

    /// Whether `self` should remain instead of being replaced by `next`.
    /// Warning and Error survive a later Synced — a clean apply step does
    /// not erase a per-file error that fired earlier in the same pass.
    fn is_sticky_over(self, next: RunState) -> bool {
        matches!(
            (self, next),
            (RunState::Warning, RunState::Synced) | (RunState::Error, RunState::Synced)
        )
    }
}

// ---------------------------------------------------------------------------
// DirState / RemoteState
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DirState {
    pub state: RunState,
}

/// Per-remote state: owns per-dir run-states and the auth-failure bookkeeping.
///
/// When any dir raises `AuthNeeded`, `RemoteState` snapshots every sibling's
/// state and transitions them to `Paused`. On successful re-authentication,
/// `reauth_complete` restores siblings from the snapshot and returns the
/// formerly-failing dir to `Waiting`.
#[derive(Clone, Debug)]
pub struct RemoteState {
    pub dirs: HashMap<SyncDirId, DirState>,
    /// Mirrors `remote.policy.enabled`. When false, the roll-up returns
    /// `Paused` unless a child dir is in `AuthNeeded` (which takes priority).
    pub enabled: bool,
    /// Snapshot of sibling dir states captured at the moment the first
    /// `AuthNeeded` transition fires. `None` when no auth failure is active.
    pub pause_snapshot: Option<HashMap<SyncDirId, RunState>>,
    /// Consecutive degraded-pass count; drives the linear backoff schedule.
    pub consecutive_degraded: u32,
    /// Scheduler cycles remaining to skip before the next pass attempt.
    pub syncs_to_skip: u32,
}

impl RemoteState {
    pub fn new(enabled: bool) -> Self {
        Self {
            dirs: HashMap::new(),
            enabled,
            pause_snapshot: None,
            consecutive_degraded: 0,
            syncs_to_skip: 0,
        }
    }

    /// Aggregate child dir states into a single `RunState` for this remote.
    pub fn roll_up(&self) -> RunState {
        let child_max = self
            .dirs
            .values()
            .map(|d| d.state)
            .max_by_key(|s| s.severity())
            .unwrap_or(RunState::Waiting);

        // AuthNeeded from any child overrides the !enabled downgrade.
        if matches!(child_max, RunState::AuthNeeded) {
            return RunState::AuthNeeded;
        }
        if !self.enabled {
            return RunState::Paused;
        }
        child_max
    }

    /// Transition dir `id` to `next`, respecting the stickiness rule:
    /// Warning/Error survive a later Synced.
    pub fn transition_dir(&mut self, id: SyncDirId, next: RunState) {
        let entry = self
            .dirs
            .entry(id)
            .or_insert(DirState { state: RunState::Waiting });
        if entry.state.is_sticky_over(next) {
            return;
        }
        entry.state = next;
    }

    /// Handle an auth failure on dir `id`.
    /// - Transitions `id` → `AuthNeeded`.
    /// - On the first call, snapshots all sibling states and transitions
    ///   them to `Paused`. Subsequent calls (edge-case: a second dir also
    ///   failing while siblings are already paused) only update `id`.
    pub fn auth_failure_on_dir(&mut self, id: SyncDirId) {
        if self.pause_snapshot.is_none() {
            let snapshot: HashMap<SyncDirId, RunState> = self
                .dirs
                .iter()
                .filter(|entry| *entry.0 != id)
                .map(|entry| (*entry.0, entry.1.state))
                .collect();
            for (did, dir) in self.dirs.iter_mut() {
                if *did != id {
                    dir.state = RunState::Paused;
                }
            }
            self.pause_snapshot = Some(snapshot);
        }
        self.dirs
            .entry(id)
            .or_insert(DirState { state: RunState::Waiting })
            .state = RunState::AuthNeeded;
    }

    /// Re-authentication succeeded. Restore siblings from the pause
    /// snapshot; return all `AuthNeeded` dirs to `Waiting`; clear snapshot.
    pub fn reauth_complete(&mut self) {
        let snapshot = self.pause_snapshot.take();
        for (&id, dir) in self.dirs.iter_mut() {
            if matches!(dir.state, RunState::AuthNeeded) {
                dir.state = RunState::Waiting;
            } else if let Some(prev) = snapshot.as_ref().and_then(|s| s.get(&id)).copied() {
                dir.state = prev;
            }
        }
    }

    /// Record a degraded pass. Increments `consecutive_degraded` and sets
    /// `syncs_to_skip = consecutive_degraded` for linear backoff.
    /// Returns the new `consecutive_degraded` value.
    pub fn on_degraded_pass(&mut self) -> u32 {
        self.consecutive_degraded = self.consecutive_degraded.saturating_add(1);
        self.syncs_to_skip = self.consecutive_degraded;
        self.consecutive_degraded
    }

    /// Record a clean pass. Resets both backoff counters.
    pub fn on_clean_pass(&mut self) {
        self.consecutive_degraded = 0;
        self.syncs_to_skip = 0;
    }

    /// Check whether the scheduler should skip this tick. If so,
    /// decrements `syncs_to_skip` and returns `true`. Returns `false`
    /// when no skip is due.
    pub fn should_skip_and_decrement(&mut self) -> bool {
        if self.syncs_to_skip == 0 {
            return false;
        }
        self.syncs_to_skip -= 1;
        true
    }

    pub fn any_degraded(&self) -> bool {
        self.consecutive_degraded > 0
    }
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application-level sync state. The single source of truth for all
/// per-dir and per-remote run-states, replacing the scattered fields that
/// previously lived directly on `CelesteApp`.
#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub remotes: HashMap<RemoteId, RemoteState>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a `RemoteState` entry for `id` if one does not yet exist.
    /// Also synchronises `enabled` on an existing entry.
    pub fn ensure_remote(&mut self, id: RemoteId, enabled: bool) {
        let rs = self
            .remotes
            .entry(id)
            .or_insert_with(|| RemoteState::new(enabled));
        rs.enabled = enabled;
    }

    /// Create a `DirState` entry in `Waiting` for `dir_id` inside
    /// remote `remote_id`, if not already present.
    pub fn ensure_dir(&mut self, remote_id: RemoteId, dir_id: SyncDirId) {
        if let Some(rs) = self.remotes.get_mut(&remote_id) {
            rs.dirs
                .entry(dir_id)
                .or_insert(DirState { state: RunState::Waiting });
        }
    }

    /// Transition `dir_id` inside `remote_id` to `next`, respecting the
    /// stickiness rule. Creates the remote entry lazily if absent.
    pub fn transition_dir(&mut self, remote_id: RemoteId, dir_id: SyncDirId, next: RunState) {
        let rs = self
            .remotes
            .entry(remote_id)
            .or_insert_with(|| RemoteState::new(true));
        rs.transition_dir(dir_id, next);
    }

    /// Apply auth-failure semantics to `dir_id` within `remote_id`.
    pub fn auth_failure_on_dir(&mut self, remote_id: RemoteId, dir_id: SyncDirId) {
        if let Some(rs) = self.remotes.get_mut(&remote_id) {
            rs.auth_failure_on_dir(dir_id);
        }
    }

    /// Re-authentication for `remote_id` succeeded: restore all dirs.
    pub fn reauth_complete(&mut self, remote_id: RemoteId) {
        if let Some(rs) = self.remotes.get_mut(&remote_id) {
            rs.reauth_complete();
        }
    }

    /// Update the `enabled` flag for a remote (mirrors policy changes).
    pub fn set_remote_enabled(&mut self, id: RemoteId, enabled: bool) {
        if let Some(rs) = self.remotes.get_mut(&id) {
            rs.enabled = enabled;
        }
    }

    /// Remove all state for a deleted remote.
    pub fn remove_remote(&mut self, id: RemoteId) {
        self.remotes.remove(&id);
    }

    /// Record a degraded pass for `id`. Returns the new
    /// `consecutive_degraded` (for logging).
    pub fn on_degraded_pass(&mut self, id: RemoteId) -> u32 {
        self.remotes
            .entry(id)
            .or_insert_with(|| RemoteState::new(true))
            .on_degraded_pass()
    }

    /// Record a clean pass for `id`.
    pub fn on_clean_pass(&mut self, id: RemoteId) {
        if let Some(rs) = self.remotes.get_mut(&id) {
            rs.on_clean_pass();
        }
    }

    /// Returns `true` and decrements the skip counter if this remote's
    /// scheduler tick should be skipped.
    pub fn should_skip_and_decrement(&mut self, id: RemoteId) -> bool {
        self.remotes
            .get_mut(&id)
            .map_or(false, |rs| rs.should_skip_and_decrement())
    }

    /// `true` when any remote has at least one consecutive-degraded pass.
    pub fn any_degraded(&self) -> bool {
        self.remotes.values().any(|rs| rs.any_degraded())
    }

    /// `true` when any dir in `remote_id` is in the `AuthNeeded` state.
    pub fn needs_reauth(&self, remote_id: RemoteId) -> bool {
        self.remotes.get(&remote_id).map_or(false, |rs| {
            rs.dirs
                .values()
                .any(|d| matches!(d.state, RunState::AuthNeeded))
        })
    }

    /// Return a snapshot of per-dir states for `remote_id`, suitable for
    /// passing to the remote-page view.
    pub fn dir_states(&self, remote_id: RemoteId) -> HashMap<SyncDirId, RunState> {
        self.remotes
            .get(&remote_id)
            .map(|rs| rs.dirs.iter().map(|(&id, d)| (id, d.state)).collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(id: i32) -> SyncDirId {
        SyncDirId(id)
    }

    #[test]
    fn severity_ordering_is_monotone() {
        let states = [
            RunState::Waiting,
            RunState::Paused,
            RunState::AuthNeeded,
            RunState::Synced,
            RunState::Syncing(SyncActivity::Listing),
            RunState::Warning,
            RunState::Error,
        ];
        for w in states.windows(2) {
            assert!(
                w[0].severity() < w[1].severity(),
                "{w:?} — severity not strictly increasing"
            );
        }
    }

    #[test]
    fn warning_and_error_are_sticky_over_synced() {
        assert!(RunState::Warning.is_sticky_over(RunState::Synced));
        assert!(RunState::Error.is_sticky_over(RunState::Synced));
        assert!(!RunState::Syncing(SyncActivity::Listing).is_sticky_over(RunState::Synced));
        assert!(!RunState::Warning.is_sticky_over(RunState::Error));
    }

    #[test]
    fn roll_up_takes_max_child() {
        let mut rs = RemoteState::new(true);
        rs.dirs.insert(dir(1), DirState { state: RunState::Synced });
        rs.dirs.insert(dir(2), DirState { state: RunState::Warning });
        assert_eq!(rs.roll_up(), RunState::Warning);
    }

    #[test]
    fn disabled_remote_rolls_up_to_paused() {
        let mut rs = RemoteState::new(false);
        rs.dirs.insert(dir(1), DirState { state: RunState::Synced });
        assert_eq!(rs.roll_up(), RunState::Paused);
    }

    #[test]
    fn auth_needed_overrides_disabled() {
        let mut rs = RemoteState::new(false);
        rs.dirs.insert(dir(1), DirState { state: RunState::AuthNeeded });
        rs.dirs.insert(dir(2), DirState { state: RunState::Paused });
        assert_eq!(rs.roll_up(), RunState::AuthNeeded);
    }

    #[test]
    fn auth_failure_transitions_and_restores() {
        let mut rs = RemoteState::new(true);
        rs.dirs.insert(dir(1), DirState { state: RunState::Synced });
        rs.dirs.insert(dir(2), DirState { state: RunState::Warning });

        rs.auth_failure_on_dir(dir(1));
        assert_eq!(rs.dirs[&dir(1)].state, RunState::AuthNeeded);
        assert_eq!(rs.dirs[&dir(2)].state, RunState::Paused);
        assert!(rs.pause_snapshot.is_some());

        rs.reauth_complete();
        assert_eq!(rs.dirs[&dir(1)].state, RunState::Waiting);
        assert_eq!(rs.dirs[&dir(2)].state, RunState::Warning);
        assert!(rs.pause_snapshot.is_none());
    }

    #[test]
    fn second_auth_failure_does_not_overwrite_snapshot() {
        let mut rs = RemoteState::new(true);
        rs.dirs.insert(dir(1), DirState { state: RunState::Synced });
        rs.dirs.insert(dir(2), DirState { state: RunState::Synced });
        rs.dirs.insert(dir(3), DirState { state: RunState::Warning });

        rs.auth_failure_on_dir(dir(1));
        // Second failure on dir 2 — snapshot should remain unchanged
        rs.auth_failure_on_dir(dir(2));

        assert_eq!(rs.dirs[&dir(1)].state, RunState::AuthNeeded);
        assert_eq!(rs.dirs[&dir(2)].state, RunState::AuthNeeded);
        // dir(3) should still be Paused (was paused in first snapshot)
        assert_eq!(rs.dirs[&dir(3)].state, RunState::Paused);

        rs.reauth_complete();
        assert_eq!(rs.dirs[&dir(1)].state, RunState::Waiting);
        assert_eq!(rs.dirs[&dir(2)].state, RunState::Waiting);
        // dir(3) restores to Warning (from snapshot captured before second failure)
        assert_eq!(rs.dirs[&dir(3)].state, RunState::Warning);
    }

    #[test]
    fn backoff_counters_increment_and_reset() {
        let mut rs = RemoteState::new(true);
        assert!(!rs.should_skip_and_decrement());

        rs.on_degraded_pass();
        assert_eq!(rs.consecutive_degraded, 1);
        assert!(rs.should_skip_and_decrement()); // skip 1
        assert!(!rs.should_skip_and_decrement()); // 0 left

        rs.on_degraded_pass();
        rs.on_degraded_pass();
        assert_eq!(rs.consecutive_degraded, 3);
        for _ in 0..3 {
            assert!(rs.should_skip_and_decrement());
        }
        assert!(!rs.should_skip_and_decrement());

        rs.on_clean_pass();
        assert_eq!(rs.consecutive_degraded, 0);
        assert!(!rs.should_skip_and_decrement());
    }
}
