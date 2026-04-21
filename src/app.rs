//! Iced application root. The Phase D entry point alongside the existing
//! GTK `launch::launch`. Runs the pure-Rust UI against the already-extracted
//! service layer.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use iced::{executor, subscription, Application, Command, Element, Settings, Subscription, Theme};
use tokio::sync::mpsc;

use std::path::PathBuf;

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::{ProviderKind, Remote, RemoteId},
        sync::{SyncDir, SyncDirExclusion, SyncDirId, SyncError},
    },
    infrastructure::{
        client_router::ClientRouter,
        stderr_capture::{self, CaptureHandle},
    },
    screens::{add_remote, main_page, remote_page, settings},
    services::sync::Outcome,
    theme,
};

/// Messages the root application dispatches. Screen-level messages are
/// wrapped by variants; service results fire their own.
#[derive(Debug, Clone)]
pub enum Message {
    Main(main_page::Msg),
    Remote(remote_page::Msg),
    Settings(settings::Msg),
    AddRemote(add_remote::Msg),
    AddRemoteResult(Result<RemoteId, String>),
    RemotesLoaded(Vec<Remote>),
    SyncDirsLoaded(RemoteId, Vec<SyncDir>),
    AllSyncDirsRefreshed(Vec<SyncDir>),
    ExclusionsLoaded(SyncDirId, Vec<SyncDirExclusion>),
    LocalFilesDeleted,
    PolicySaved,
    SyncStarted(RemoteId),
    SyncFinished(RemoteId, PassVerdict),
    WorkerReady(mpsc::Sender<SyncEvent>),
    SyncEventReceived(SyncEvent),
    Tick,
}

/// Aggregate outcome across every sync_dir of one remote's pass. The
/// scheduler uses this to drive linear backoff on provider rate-limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassVerdict {
    /// Every sync_dir finished cleanly.
    Clean,
    /// At least one sync_dir detected rate-limiting (stderr tap fired).
    /// Scheduler bumps `consecutive_degraded` and skips more cycles.
    Degraded,
    /// Pass aborted for a non-rate-limit reason (cancel, list error,
    /// suspect listing). Backoff counter is left alone.
    Aborted,
}

pub struct CelesteApp {
    repo: Arc<dyn Repository>,
    /// Client router — dispatches RcloneClient calls per-remote.
    /// `Arc<ClientRouter>` rather than `Arc<dyn RcloneClient>` so the
    /// add-/delete-remote paths can register / unregister native
    /// sessions on it; sync code downcasts on the fly (ClientRouter
    /// implements RcloneClient).
    rclone: Arc<ClientRouter>,
    /// User's Celeste config dir — needed so the native Proton
    /// add-remote flow knows where to put session blobs.
    config_dir: PathBuf,
    remotes: Vec<Remote>,
    sync_dirs: HashMap<RemoteId, Vec<SyncDir>>,
    selected: Option<RemoteId>,
    /// Remotes whose sync pass is currently running.
    syncing: std::collections::HashSet<RemoteId>,
    /// Accumulated log lines per sync_dir for the current session.
    /// Each status/pending/error event appends one line; never cleared.
    sync_dir_log: HashMap<SyncDirId, Vec<String>>,
    /// All sync_dirs across every remote, refreshed on navigation changes.
    /// Used to compute auto-exclusions in the UI.
    all_known_sync_dirs: Vec<SyncDir>,
    /// The sync_dir whose exclusion panel is currently open (at most one).
    exclusion_panel: Option<SyncDirId>,
    /// Loaded user-defined exclusions per sync_dir.
    sync_dir_exclusions: HashMap<SyncDirId, Vec<SyncDirExclusion>>,
    /// Draft remote sub-path for the "add exclusion" form per sync_dir.
    draft_exclusion: HashMap<SyncDirId, String>,
    /// Wall-clock timestamp of the last sync completion per remote. Drives
    /// the interval scheduler.
    last_sync_at: HashMap<RemoteId, Instant>,
    /// Remote ids with a refresh request queued while the current pass is
    /// still running — as soon as SyncFinished lands we kick another pass.
    refresh_requested_after: std::collections::HashSet<RemoteId>,
    /// In-progress (local_path, remote_path) inputs for the Add sync_dir form
    /// on each remote page.
    sync_dir_drafts: HashMap<RemoteId, (String, String)>,
    /// In-progress Add Remote form. Some(...) while the screen is shown.
    add_remote_draft: Option<add_remote::Draft>,
    /// Sender handed to us by the subscription worker; sync code clones this
    /// to emit events back into the event loop.
    events_tx: Option<mpsc::Sender<SyncEvent>>,
    /// Per-remote cancel flags. Flipping `true` tells the in-flight
    /// sync pass to bail out between actions — the app sets it when
    /// the user disables a remote (or the app shuts down).
    cancel_flags: HashMap<RemoteId, Arc<AtomicBool>>,
    /// Consecutive degraded (rate-limited) passes per remote. Drives
    /// the linear backoff — reset to 0 on the next clean pass.
    consecutive_degraded: HashMap<RemoteId, u32>,
    /// Remaining sync cycles to skip before attempting another pass on
    /// this remote. Set to `consecutive_degraded` right after a
    /// Degraded verdict, decremented on each tick that would otherwise
    /// have fired a pass.
    syncs_to_skip: HashMap<RemoteId, u32>,
    /// Stderr ring-buffer handle — shared by every sync pass so each
    /// can ask "did any provider rate-limit warning fire since my
    /// pass_start?". Installed once at process startup.
    stderr_capture: CaptureHandle,
}

pub struct Flags {
    pub repo: Arc<dyn Repository>,
    pub rclone: Arc<ClientRouter>,
    pub config_dir: PathBuf,
}

impl Application for CelesteApp {
    type Executor = executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = Flags;

    fn new(flags: Flags) -> (Self, Command<Message>) {
        let state = Self {
            repo: flags.repo.clone(),
            rclone: flags.rclone,
            config_dir: flags.config_dir,
            remotes: Vec::new(),
            sync_dirs: HashMap::new(),
            selected: None,
            syncing: std::collections::HashSet::new(),
            sync_dir_log: HashMap::new(),
            all_known_sync_dirs: Vec::new(),
            exclusion_panel: None,
            sync_dir_exclusions: HashMap::new(),
            draft_exclusion: HashMap::new(),
            last_sync_at: HashMap::new(),
            refresh_requested_after: std::collections::HashSet::new(),
            sync_dir_drafts: HashMap::new(),
            add_remote_draft: None,
            events_tx: None,
            cancel_flags: HashMap::new(),
            consecutive_degraded: HashMap::new(),
            syncs_to_skip: HashMap::new(),
            stderr_capture: stderr_capture::handle(),
        };
        let repo = flags.repo;
        let load = Command::perform(
            async move { repo.list_remotes().await.unwrap_or_default() },
            Message::RemotesLoaded,
        );
        (state, load)
    }

    fn title(&self) -> String {
        "Celeste".to_string()
    }

    fn theme(&self) -> Theme {
        theme::celeste_theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        let events = subscription::channel(
            std::any::TypeId::of::<CelesteApp>(),
            128,
            |mut output| async move {
                use iced::futures::SinkExt;
                let (tx, mut rx) = mpsc::channel::<SyncEvent>(128);
                let _ = output.send(Message::WorkerReady(tx)).await;
                while let Some(event) = rx.recv().await {
                    let _ = output.send(Message::SyncEventReceived(event)).await;
                }
                std::future::pending::<()>().await;
                unreachable!()
            },
        );
        // Single ticker at 1 Hz — interval checks are cheap, and the
        // shortest allowed sync cadence is 5 s.
        let ticker = iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick);
        Subscription::batch([events, ticker])
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::RemotesLoaded(mut remotes) => {
                // Ask rclone for each remote's backend type so the
                // scheduler can enforce provider-specific interval
                // floors (see `ProviderKind::min_interval`). A failure
                // here is non-fatal — the remote just loses its
                // provider-specific floor for this session.
                for r in &mut remotes {
                    if let Ok(Some(t)) = self.rclone.remote_type(&r.name) {
                        r.provider_kind = ProviderKind::from_rclone_type(&t);
                    }
                }
                self.remotes = remotes;
                Command::none()
            }
            Message::Main(main_page::Msg::Selected(id)) => {
                self.selected = Some(id);
                let repo = self.repo.clone();
                let repo2 = self.repo.clone();
                Command::batch([
                    Command::perform(
                        async move { repo.list_sync_dirs(id).await.unwrap_or_default() },
                        move |sd| Message::SyncDirsLoaded(id, sd),
                    ),
                    Command::perform(
                        async move { repo2.list_all_sync_dirs().await.unwrap_or_default() },
                        Message::AllSyncDirsRefreshed,
                    ),
                ])
            }
            Message::SyncDirsLoaded(id, sd) => {
                self.sync_dirs.insert(id, sd);
                Command::none()
            }
            Message::AllSyncDirsRefreshed(all) => {
                self.all_known_sync_dirs = all;
                Command::none()
            }
            Message::Main(main_page::Msg::RefreshAll) => {
                let ids: Vec<RemoteId> = self
                    .remotes
                    .iter()
                    .filter(|r| r.policy.enabled && !self.syncing.contains(&r.id))
                    .map(|r| r.id)
                    .collect();
                let cmds: Vec<Command<Message>> =
                    ids.into_iter().map(|id| self.start_sync(id)).collect();
                Command::batch(cmds)
            }
            Message::Main(main_page::Msg::AddRemote) => {
                self.add_remote_draft = Some(add_remote::Draft::default());
                Command::none()
            }
            Message::AddRemote(sub) => {
                let Some(draft) = self.add_remote_draft.as_mut() else {
                    return Command::none();
                };
                match sub {
                    add_remote::Msg::NameChanged(s) => draft.name = s,
                    add_remote::Msg::ProviderChanged(p) => draft.provider = Some(p),
                    add_remote::Msg::UrlChanged(s) => draft.url = s,
                    add_remote::Msg::UserChanged(s) => draft.user = s,
                    add_remote::Msg::PassChanged(s) => draft.pass = s,
                    add_remote::Msg::TotpChanged(s) => draft.totp = s,
                    add_remote::Msg::ClientIdChanged(s) => draft.client_id = s,
                    add_remote::Msg::ClientSecretChanged(s) => draft.client_secret = s,
                    add_remote::Msg::Cancel => {
                        self.add_remote_draft = None;
                        return Command::none();
                    }
                    add_remote::Msg::Submit => {
                        let Some(kind) = draft.provider else {
                            draft.error = Some("Pick a provider first.".to_owned());
                            return Command::none();
                        };
                        if draft.name.trim().is_empty() {
                            draft.error = Some("Name is required.".to_owned());
                            return Command::none();
                        }
                        let name = draft.name.clone();
                        let repo = self.repo.clone();
                        let rclone = self.rclone.clone();
                        draft.error = None;

                        if let Some(vendor) = kind.webdav_vendor() {
                            if draft.url.trim().is_empty()
                                || draft.user.trim().is_empty()
                            {
                                draft.error =
                                    Some("URL and username are required.".to_owned());
                                return Command::none();
                            }
                            let url = draft.url.clone();
                            let user = draft.user.clone();
                            let pass = draft.pass.clone();
                            draft.busy = true;
                            return Command::perform(
                                async move {
                                    tokio::task::spawn_blocking(move || {
                                        crate::services::auth_service::add_webdav_remote(
                                            &name, &url, &user, &pass, vendor, &*repo,
                                            &*rclone,
                                        )
                                    })
                                    .await
                                    .unwrap_or_else(|e| Err(e.to_string()))
                                },
                                Message::AddRemoteResult,
                            );
                        }

                        if kind.is_proton_drive() {
                            if draft.user.trim().is_empty() {
                                draft.error = Some("Username is required.".to_owned());
                                return Command::none();
                            }
                            let user = draft.user.clone();
                            let pass = draft.pass.clone();
                            let totp = draft.totp.clone();
                            let router = self.rclone.clone();
                            let config_dir = self.config_dir.clone();
                            draft.busy = true;
                            return Command::perform(
                                async move {
                                    tokio::task::spawn_blocking(move || {
                                        crate::services::auth_service::add_proton_drive_remote(
                                            &name,
                                            &user,
                                            &pass,
                                            &totp,
                                            &config_dir,
                                            &*repo,
                                            &*router,
                                        )
                                    })
                                    .await
                                    .unwrap_or_else(|e| Err(e.to_string()))
                                },
                                Message::AddRemoteResult,
                            );
                        }

                        if let Some(provider) = kind.oauth_provider() {
                            let client_id = draft.client_id.trim().to_owned();
                            let client_secret = draft.client_secret.trim().to_owned();
                            draft.busy = true;
                            return Command::perform(
                                async move {
                                    tokio::task::spawn_blocking(move || {
                                        let client_id = (!client_id.is_empty())
                                            .then_some(client_id.as_str());
                                        let client_secret = (!client_secret.is_empty())
                                            .then_some(client_secret.as_str());
                                        crate::services::auth_service::add_oauth_remote(
                                            &name,
                                            provider,
                                            client_id,
                                            client_secret,
                                            &*repo,
                                            &*rclone,
                                        )
                                    })
                                    .await
                                    .unwrap_or_else(|e| Err(e.to_string()))
                                },
                                Message::AddRemoteResult,
                            );
                        }
                    }
                }
                Command::none()
            }
            Message::AddRemoteResult(Ok(_id)) => {
                self.add_remote_draft = None;
                let repo = self.repo.clone();
                Command::perform(
                    async move { repo.list_remotes().await.unwrap_or_default() },
                    Message::RemotesLoaded,
                )
            }
            Message::AddRemoteResult(Err(msg)) => {
                if let Some(draft) = self.add_remote_draft.as_mut() {
                    draft.error = Some(msg);
                    draft.busy = false;
                }
                Command::none()
            }
            Message::Remote(remote_page::Msg::Back) => {
                self.selected = None;
                Command::none()
            }
            Message::Remote(remote_page::Msg::RefreshNow(id)) => {
                if self.syncing.contains(&id) {
                    // The current pass is still running — queue a follow-up
                    // so it fires as soon as the current one completes. Leave
                    // a pending-event note on each sync_dir so the user gets
                    // immediate feedback instead of thinking the click was
                    // lost.
                    self.refresh_requested_after.insert(id);
                    if let Some(dirs) = self.sync_dirs.get(&id) {
                        for sd in dirs {
                            self.sync_dir_log
                                .entry(sd.id)
                                .or_default()
                                .push("⟳ Refresh queued — starts after the current pass finishes.".to_owned());
                        }
                    }
                    Command::none()
                } else {
                    self.start_sync(id)
                }
            }
            Message::Remote(remote_page::Msg::DraftLocalPathChanged(s)) => {
                if let Some(id) = self.selected {
                    self.sync_dir_drafts.entry(id).or_default().0 = s;
                }
                Command::none()
            }
            Message::Remote(remote_page::Msg::DraftRemotePathChanged(s)) => {
                if let Some(id) = self.selected {
                    self.sync_dir_drafts.entry(id).or_default().1 = s;
                }
                Command::none()
            }
            Message::Remote(remote_page::Msg::AddSyncDir) => {
                let Some(id) = self.selected else {
                    return Command::none();
                };
                let Some((local, remote)) = self.sync_dir_drafts.get(&id).cloned() else {
                    return Command::none();
                };
                if local.trim().is_empty() || remote.trim().is_empty() {
                    return Command::none();
                }
                let Some(remote_name) =
                    self.remotes.iter().find(|r| r.id == id).map(|r| r.name.clone())
                else {
                    return Command::none();
                };
                // Normalise to match the on-disk contract: the local path is
                // absolute (leading `/`) and has no trailing `/`; the remote
                // path has no leading or trailing `/`. The sync loop assumes
                // this shape when stripping prefixes off listed items.
                let local_norm =
                    format!("/{}", crate::util::strip_slashes(local.trim()));
                let remote_norm = crate::util::strip_slashes(remote.trim());
                // Reject any local path that overlaps an existing sync_dir
                // (descendant or ancestor). Sync_dirs must be local
                // siblings — overlapping local trees would have the engine
                // walking the same files twice with conflicting tracking.
                if let Some(conflict) = self
                    .all_known_sync_dirs
                    .iter()
                    .find(|d| local_paths_overlap(&d.local_path, &local_norm))
                {
                    eprintln!(
                        "AddSyncDir rejected: local path '{}' overlaps existing sync_dir '{}'",
                        local_norm, conflict.local_path,
                    );
                    return Command::none();
                }
                self.sync_dir_drafts.insert(id, (String::new(), String::new()));
                let repo = self.repo.clone();
                let rclone = self.rclone.clone();
                Command::perform(
                    async move {
                        // Auto-create local + remote directory if missing so
                        // the next sync pass doesn't immediately trip
                        // "directory not found" on the listing call.
                        let local_for_mk = local_norm.clone();
                        let remote_for_mk = remote_norm.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let _ = std::fs::create_dir_all(&local_for_mk);
                            let _ = rclone.mkdir(&remote_name, &remote_for_mk);
                        })
                        .await;

                        let _ = repo.insert_sync_dir(id, local_norm, remote_norm).await;
                        id
                    },
                    |id| Message::Main(main_page::Msg::Selected(id)),
                )
            }
            Message::Remote(remote_page::Msg::DeleteSyncDir(local, remote)) => {
                let Some(id) = self.selected else {
                    return Command::none();
                };
                let repo = self.repo.clone();
                Command::perform(
                    async move {
                        let _ = repo.cascade_delete_sync_dir(&local, &remote).await;
                        id
                    },
                    |id| Message::Main(main_page::Msg::Selected(id)),
                )
            }
            Message::Remote(remote_page::Msg::DeleteRemote(id, name)) => {
                self.selected = None;
                self.syncing.remove(&id);
                self.sync_dirs.remove(&id);
                self.last_sync_at.remove(&id);
                self.sync_dir_drafts.remove(&id);
                self.remotes.retain(|r| r.id != id);
                // Drop any native-proton override so the router
                // stops routing its (now-gone) name to a stale
                // session.
                self.rclone.unregister(&name);
                let repo_blocking = self.repo.clone();
                let repo_after = self.repo.clone();
                let rclone = self.rclone.clone();
                Command::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let _ = crate::services::remote_lifecycle::delete_remote(
                                &name,
                                &*repo_blocking,
                                &*rclone,
                            );
                        })
                        .await
                        .ok();
                        repo_after.list_remotes().await.unwrap_or_default()
                    },
                    Message::RemotesLoaded,
                )
            }
            Message::SyncStarted(id) => {
                self.syncing.insert(id);
                Command::none()
            }
            Message::SyncFinished(id, verdict) => {
                self.syncing.remove(&id);
                self.last_sync_at.insert(id, Instant::now());
                match verdict {
                    PassVerdict::Clean => {
                        self.consecutive_degraded.remove(&id);
                        self.syncs_to_skip.remove(&id);
                    }
                    PassVerdict::Degraded => {
                        // Linear backoff: skip N cycles after the N-th
                        // consecutive degraded pass. N=1 the first
                        // time, N=2 the next, and so on — resets the
                        // moment a pass lands clean. Rclone already
                        // does exponential on its side; the linear
                        // layer just stops us hammering.
                        let n = self
                            .consecutive_degraded
                            .entry(id)
                            .and_modify(|c| *c = c.saturating_add(1))
                            .or_insert(1);
                        self.syncs_to_skip.insert(id, *n);
                    }
                    PassVerdict::Aborted => {
                        // Intentionally leave counters as-is: an abort
                        // caused by cancel / suspect listing isn't a
                        // signal the backend is overloaded.
                    }
                }
                // If the user clicked Refresh now while we were already
                // syncing, honour that click now.
                if self.refresh_requested_after.remove(&id) {
                    self.start_sync(id)
                } else {
                    Command::none()
                }
            }
            Message::Tick => {
                // Check each enabled remote; if its interval has elapsed and
                // it's not already syncing, kick off a new pass. Remotes
                // currently inside a backoff window have `syncs_to_skip > 0`
                // — we decrement, stamp last_sync_at, and skip this cycle
                // so the next interval's tick does the same until the
                // counter hits zero.
                let now = Instant::now();
                let mut due: Vec<RemoteId> = Vec::new();
                let remote_ids: Vec<(RemoteId, std::time::Duration)> = self
                    .remotes
                    .iter()
                    .filter(|r| r.policy.enabled && !self.syncing.contains(&r.id))
                    .map(|r| (r.id, r.policy.interval.duration()))
                    .collect();
                for (id, interval) in remote_ids {
                    let elapsed = self
                        .last_sync_at
                        .get(&id)
                        .map(|t| now.duration_since(*t))
                        .unwrap_or(interval);
                    if elapsed < interval {
                        continue;
                    }
                    if let Some(skip) = self.syncs_to_skip.get(&id).copied()
                        && skip > 0
                    {
                        let remaining = skip - 1;
                        if remaining == 0 {
                            self.syncs_to_skip.remove(&id);
                        } else {
                            self.syncs_to_skip.insert(id, remaining);
                        }
                        // Advance the baseline so we wait another full
                        // interval before the next skip decision.
                        self.last_sync_at.insert(id, now);
                        continue;
                    }
                    due.push(id);
                }
                let cmds: Vec<Command<Message>> =
                    due.into_iter().map(|id| self.start_sync(id)).collect();
                Command::batch(cmds)
            }
            Message::WorkerReady(tx) => {
                self.events_tx = Some(tx);
                Command::none()
            }
            Message::SyncEventReceived(event) => {
                match event {
                    SyncEvent::SyncDirStatus {
                        sync_dir_id, text, ..
                    } => {
                        self.sync_dir_log
                            .entry(sync_dir_id)
                            .or_default()
                            .push(text);
                    }
                    SyncEvent::SyncDirPending {
                        sync_dir_id, text, ..
                    } => {
                        self.sync_dir_log
                            .entry(sync_dir_id)
                            .or_default()
                            .push(format!("⟳ {text}"));
                    }
                    SyncEvent::SyncDirError {
                        sync_dir_id, error, ..
                    } => {
                        let line = match &error {
                            SyncError::General(path, msg) => format!("⚠ {path}: {msg}"),
                            SyncError::BothMoreCurrent(local, remote) => {
                                format!("⚠ Conflict: '{local}' vs '{remote}'")
                            }
                        };
                        self.sync_dir_log
                            .entry(sync_dir_id)
                            .or_default()
                            .push(line);
                    }
                    SyncEvent::RemoteStarted { .. }
                    | SyncEvent::RemoteCompleted { .. }
                    | SyncEvent::RemoteFailed { .. }
                    | SyncEvent::FileProgress { .. } => {}
                }
                Command::none()
            }
            Message::Remote(remote_page::Msg::Settings(sub))
            | Message::Settings(sub) => {
                let Some(id) = self.selected else {
                    return Command::none();
                };
                let Some(remote) = self.remotes.iter_mut().find(|r| r.id == id) else {
                    return Command::none();
                };
                let was_enabled = remote.policy.enabled;
                let new_policy = settings::policy_from(&sub, &remote.policy);
                remote.policy = new_policy.clone();
                // If the user just disabled a remote that's currently
                // syncing, trip its cancel flag so the running pass
                // bails out between actions. Re-enabling uses the
                // next scheduler tick — no action here.
                if was_enabled && !new_policy.enabled
                    && let Some(flag) = self.cancel_flags.get(&id)
                {
                    flag.store(true, Ordering::Release);
                    self.refresh_requested_after.remove(&id);
                }
                let repo = self.repo.clone();
                Command::perform(
                    async move {
                        let _ = repo.set_policy(id, new_policy).await;
                    },
                    |_| Message::PolicySaved,
                )
            }
            Message::PolicySaved => Command::none(),

            Message::ExclusionsLoaded(sd_id, excls) => {
                self.sync_dir_exclusions.insert(sd_id, excls);
                Command::none()
            }

            Message::Remote(remote_page::Msg::ToggleExclusions(sd_id)) => {
                if self.exclusion_panel == Some(sd_id) {
                    self.exclusion_panel = None;
                    Command::none()
                } else {
                    self.exclusion_panel = Some(sd_id);
                    let repo = self.repo.clone();
                    Command::perform(
                        async move { repo.list_exclusions(sd_id).await.unwrap_or_default() },
                        move |excls| Message::ExclusionsLoaded(sd_id, excls),
                    )
                }
            }

            Message::Remote(remote_page::Msg::DraftExclusionChanged(sd_id, s)) => {
                self.draft_exclusion.insert(sd_id, s);
                Command::none()
            }

            Message::Remote(remote_page::Msg::AddExclusion(sd_id)) => {
                let raw = self
                    .draft_exclusion
                    .get(&sd_id)
                    .cloned()
                    .unwrap_or_default();
                let path = crate::util::strip_slashes(raw.trim());
                if path.is_empty() {
                    return Command::none();
                }
                self.draft_exclusion.insert(sd_id, String::new());
                let repo = self.repo.clone();
                Command::perform(
                    async move {
                        let _ = repo.insert_exclusion(sd_id, path).await;
                        repo.list_exclusions(sd_id).await.unwrap_or_default()
                    },
                    move |excls| Message::ExclusionsLoaded(sd_id, excls),
                )
            }

            Message::Remote(remote_page::Msg::RemoveExclusion(excl_id, sd_id)) => {
                let repo = self.repo.clone();
                Command::perform(
                    async move {
                        let _ = repo.delete_exclusion(excl_id).await;
                        repo.list_exclusions(sd_id).await.unwrap_or_default()
                    },
                    move |excls| Message::ExclusionsLoaded(sd_id, excls),
                )
            }

            Message::Remote(remote_page::Msg::DeleteLocalFiles(path)) => Command::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let _ = std::fs::remove_dir_all(&path);
                    })
                    .await
                    .ok();
                },
                |_| Message::LocalFilesDeleted,
            ),

            Message::LocalFilesDeleted => Command::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        if let Some(draft) = self.add_remote_draft.as_ref() {
            return add_remote::view(draft).map(Message::AddRemote);
        }

        match self
            .selected
            .and_then(|id| self.remotes.iter().find(|r| r.id == id))
        {
            Some(remote) => {
                let dirs: &[SyncDir] = self
                    .sync_dirs
                    .get(&remote.id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let (draft_local, draft_remote) = self
                    .sync_dir_drafts
                    .get(&remote.id)
                    .map(|(l, r)| (l.as_str(), r.as_str()))
                    .unwrap_or(("", ""));
                let eta = self.next_sync_eta(remote.id);
                remote_page::view(
                    remote,
                    dirs,
                    &self.sync_dir_log,
                    &self.all_known_sync_dirs,
                    self.exclusion_panel,
                    &self.sync_dir_exclusions,
                    &self.draft_exclusion,
                    (draft_local, draft_remote),
                    eta,
                )
                .map(Message::Remote)
            }
            None => main_page::view(&self.remotes, self.selected, &self.syncing)
                .map(Message::Main),
        }
    }
}

/// True when two local paths overlap — equal, or one is a strict
/// descendant of the other. Used to reject AddSyncDir requests so all
/// sync_dirs stay on disjoint subtrees.
fn local_paths_overlap(a: &str, b: &str) -> bool {
    a == b || b.starts_with(&format!("{a}/")) || a.starts_with(&format!("{b}/"))
}

impl CelesteApp {
    /// Spawn a sync pass for one remote. No-op if already syncing. Marks the
    /// remote as in-flight so the sidebar shows "(syncing…)" and returns
    /// a Command that will deliver `SyncFinished(id)` when the blocking
    /// task completes.
    fn start_sync(&mut self, id: RemoteId) -> Command<Message> {
        if self.syncing.contains(&id) {
            return Command::none();
        }
        // Fresh cancel flag for this pass. Reusing the existing Arc
        // lets any stored reference remain wired up (we flip-flop the
        // bool rather than swap the Arc).
        let flag = self
            .cancel_flags
            .entry(id)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        flag.store(false, Ordering::Release);
        self.syncing.insert(id);
        let repo = self.repo.clone();
        let rclone = self.rclone.clone();
        let events_tx = self.events_tx.clone();
        let stderr_capture = self.stderr_capture.clone();
        Command::perform(
            async move {
                let remote = match repo.find_remote(id).await {
                    Ok(Some(r)) => r,
                    _ => return (id, PassVerdict::Aborted),
                };
                let sync_dirs = repo.list_sync_dirs(id).await.unwrap_or_default();
                let all_sync_dirs = repo.list_all_sync_dirs().await.unwrap_or_default();
                let verdict = tokio::task::spawn_blocking(move || {
                    let emit = move |event: SyncEvent| {
                        if let Some(tx) = &events_tx {
                            let _ = tx.blocking_send(event);
                        }
                    };
                    let is_cancelled = {
                        let f = flag.clone();
                        move || f.load(Ordering::Acquire)
                    };
                    // The stderr probe: any line containing *all* of a
                    // provider's marker substrings, received on or
                    // after `since`, flips the pass to Degraded. The
                    // marker table lives on `ProviderKind`; unknown
                    // providers get an empty table and never degrade.
                    let markers: &'static [&'static [&'static str]] =
                        remote.provider_kind.map_or(&[], |k| k.rate_limit_markers());
                    let stderr_for_probe = stderr_capture.clone();
                    let rate_limit_seen_since = move |since: Instant| -> bool {
                        if markers.is_empty() {
                            return false;
                        }
                        stderr_for_probe
                            .any_line_since(since, |line| {
                                markers
                                    .iter()
                                    .any(|m| m.iter().all(|needle| line.contains(needle)))
                            })
                    };
                    let mut any_degraded = false;
                    let mut any_error = false;
                    let mut any_synced = false;
                    for sd in sync_dirs {
                        if is_cancelled() {
                            break;
                        }
                        match crate::services::sync::run(
                            &remote,
                            &sd,
                            &*repo,
                            &*rclone,
                            &all_sync_dirs,
                            emit.clone(),
                            is_cancelled.clone(),
                            rate_limit_seen_since.clone(),
                        ) {
                            Outcome::Synced => any_synced = true,
                            Outcome::Degraded => any_degraded = true,
                            Outcome::Aborted => any_error = true,
                        }
                    }
                    if any_degraded {
                        PassVerdict::Degraded
                    } else if any_synced {
                        PassVerdict::Clean
                    } else if any_error {
                        PassVerdict::Aborted
                    } else {
                        // No sync_dirs to run (or everything cancelled
                        // before the first). Treat as clean-ish — no
                        // reason to accrue backoff.
                        PassVerdict::Clean
                    }
                })
                .await
                .unwrap_or(PassVerdict::Aborted);
                (id, verdict)
            },
            |(id, v)| Message::SyncFinished(id, v),
        )
    }

    /// Time until the scheduler will next attempt this remote, plus a
    /// flag telling the caller whether the remote is currently in a
    /// backoff window (next attempt will be a skip, not a real pass).
    /// Returns `None` when the remote is disabled.
    pub fn next_sync_eta(&self, id: RemoteId) -> Option<(std::time::Duration, bool)> {
        let remote = self.remotes.iter().find(|r| r.id == id)?;
        if !remote.policy.enabled {
            return None;
        }
        let interval = remote.policy.interval.duration();
        let now = Instant::now();
        let base_remaining = match self.last_sync_at.get(&id) {
            Some(t) => {
                let elapsed = now.duration_since(*t);
                interval.saturating_sub(elapsed)
            }
            None => std::time::Duration::ZERO,
        };
        let in_backoff = self
            .syncs_to_skip
            .get(&id)
            .copied()
            .unwrap_or(0)
            > 0;
        Some((base_remaining, in_backoff))
    }
}

/// Launch the Iced application. Blocks until the window closes.
pub fn run(
    repo: Arc<dyn Repository>,
    rclone: Arc<ClientRouter>,
    config_dir: PathBuf,
) -> iced::Result {
    let mut settings = Settings::with_flags(Flags {
        repo,
        rclone,
        config_dir,
    });
    settings.fonts = fallback_fonts();
    // Bias iced's default glyph lookup to the sans-serif family so
    // cosmic-text's fallback layer resolves against the fonts we just
    // loaded instead of a bare built-in. Without this the ⚠ and
    // anything beyond basic Latin still falls through to tofu.
    settings.default_font = iced::Font {
        family: iced::font::Family::Name("Noto Sans"),
        ..iced::Font::DEFAULT
    };
    CelesteApp::run(settings)
}

/// Discover fallback fonts via fontconfig at startup and hand them to
/// iced as `Settings::fonts`. iced 0.12's bundled default only covers
/// Latin — without fallbacks, anything past ASCII (emoji, ⚠, Cyrillic,
/// CJK, …) silently drops from the render.
///
/// Queries cover three tiers of glyph coverage:
/// - emoji (color, e.g. Noto Color Emoji) for actual emoji;
/// - a dedicated symbols font for ⚠ / arrows / checkmarks;
/// - a general-purpose sans-serif for wide script coverage;
/// - a monospace for the rare places that want it.
///
/// Failures (no `fc-match`, missing fonts, unreadable files) degrade
/// gracefully — the app still runs, just without the extra coverage.
/// We log each load/miss to stderr so the first "I see boxes" report
/// is traceable.
fn fallback_fonts() -> Vec<std::borrow::Cow<'static, [u8]>> {
    [
        "Noto Color Emoji",
        "Noto Sans Symbols 2",
        "Noto Sans",
        "sans-serif",
        "emoji",
        "monospace",
    ]
    .iter()
    .filter_map(|q| fc_match_read(q))
    .map(std::borrow::Cow::Owned)
    .collect()
}

fn fc_match_read(pattern: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}"])
        .arg(pattern)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    std::fs::read(path).ok()
}
