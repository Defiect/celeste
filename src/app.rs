//! Iced application root. The Phase D entry point alongside the existing
//! GTK `launch::launch`. Runs the pure-Rust UI against the already-extracted
//! service layer.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use iced::{executor, subscription, Application, Command, Element, Settings, Subscription, Theme};
use tokio::sync::mpsc;

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::{ProviderKind, Remote, RemoteId},
        sync::{SyncDir, SyncDirId, SyncError},
    },
    screens::{add_remote, main_page, remote_page, settings},
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
    PolicySaved,
    SyncStarted(RemoteId),
    SyncFinished(RemoteId),
    WorkerReady(mpsc::Sender<SyncEvent>),
    SyncEventReceived(SyncEvent),
    Tick,
}

pub struct CelesteApp {
    repo: Arc<dyn Repository>,
    rclone: Arc<dyn RcloneClient>,
    remotes: Vec<Remote>,
    sync_dirs: HashMap<RemoteId, Vec<SyncDir>>,
    selected: Option<RemoteId>,
    /// Remotes whose sync pass is currently running.
    syncing: std::collections::HashSet<RemoteId>,
    /// Latest status text per sync_dir — populated from SyncDirStatus events.
    sync_dir_status: HashMap<SyncDirId, String>,
    /// Latest "pending event" text per sync_dir — e.g. "Checking for
    /// changes…" during should_sync or "Refresh queued…" while another
    /// pass is in flight. Rendered under the main status on a second line.
    sync_dir_pending: HashMap<SyncDirId, String>,
    /// Errors accumulated for each sync_dir since its last refresh.
    sync_dir_errors: HashMap<SyncDirId, Vec<SyncError>>,
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
}

pub struct Flags {
    pub repo: Arc<dyn Repository>,
    pub rclone: Arc<dyn RcloneClient>,
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
            remotes: Vec::new(),
            sync_dirs: HashMap::new(),
            selected: None,
            syncing: std::collections::HashSet::new(),
            sync_dir_status: HashMap::new(),
            sync_dir_pending: HashMap::new(),
            sync_dir_errors: HashMap::new(),
            last_sync_at: HashMap::new(),
            refresh_requested_after: std::collections::HashSet::new(),
            sync_dir_drafts: HashMap::new(),
            add_remote_draft: None,
            events_tx: None,
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
                Command::perform(
                    async move { repo.list_sync_dirs(id).await.unwrap_or_default() },
                    move |sd| Message::SyncDirsLoaded(id, sd),
                )
            }
            Message::SyncDirsLoaded(id, sd) => {
                self.sync_dirs.insert(id, sd);
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
                            draft.busy = true;
                            return Command::perform(
                                async move {
                                    tokio::task::spawn_blocking(move || {
                                        crate::services::auth_service::add_proton_drive_remote(
                                            &name, &user, &pass, &totp, &*repo, &*rclone,
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
                            self.sync_dir_pending.insert(
                                sd.id,
                                "Refresh queued — starts after the current pass finishes."
                                    .to_owned(),
                            );
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
                // Normalise to match the on-disk contract: the local path is
                // absolute (leading `/`) and has no trailing `/`; the remote
                // path has no leading or trailing `/`. The sync loop assumes
                // this shape when stripping prefixes off listed items.
                let local_norm =
                    format!("/{}", crate::util::strip_slashes(local.trim()));
                let remote_norm = crate::util::strip_slashes(remote.trim());
                self.sync_dir_drafts.insert(id, (String::new(), String::new()));
                let repo = self.repo.clone();
                Command::perform(
                    async move {
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
            Message::SyncFinished(id) => {
                self.syncing.remove(&id);
                self.last_sync_at.insert(id, Instant::now());
                // Clear lingering "Synchronizing '/foo'…" strings left on
                // each sync_dir row — the pass is done, those are stale.
                if let Some(dirs) = self.sync_dirs.get(&id) {
                    for sd in dirs {
                        self.sync_dir_status.remove(&sd.id);
                        self.sync_dir_pending.remove(&sd.id);
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
                // it's not already syncing, kick off a new pass.
                let now = Instant::now();
                let due: Vec<RemoteId> = self
                    .remotes
                    .iter()
                    .filter(|r| {
                        r.policy.enabled
                            && !self.syncing.contains(&r.id)
                            && self
                                .last_sync_at
                                .get(&r.id)
                                .map(|t| now.duration_since(*t) >= r.policy.interval.duration())
                                .unwrap_or(true)
                    })
                    .map(|r| r.id)
                    .collect();
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
                        // A primary status supersedes any pending-event
                        // note.
                        self.sync_dir_pending.remove(&sync_dir_id);
                        self.sync_dir_status.insert(sync_dir_id, text);
                    }
                    SyncEvent::SyncDirPending {
                        sync_dir_id, text, ..
                    } => {
                        self.sync_dir_pending.insert(sync_dir_id, text);
                    }
                    SyncEvent::SyncDirError {
                        sync_dir_id, error, ..
                    } => {
                        self.sync_dir_errors
                            .entry(sync_dir_id)
                            .or_default()
                            .push(error);
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
                let new_policy = settings::policy_from(&sub, &remote.policy);
                remote.policy = new_policy.clone();
                let repo = self.repo.clone();
                Command::perform(
                    async move {
                        let _ = repo.set_policy(id, new_policy).await;
                    },
                    |_| Message::PolicySaved,
                )
            }
            Message::PolicySaved => Command::none(),
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
                remote_page::view(
                    remote,
                    dirs,
                    &self.sync_dir_status,
                    &self.sync_dir_pending,
                    &self.sync_dir_errors,
                    (draft_local, draft_remote),
                )
                .map(Message::Remote)
            }
            None => main_page::view(&self.remotes, self.selected, &self.syncing)
                .map(Message::Main),
        }
    }
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
        // Clear previous errors for all sync_dirs of this remote before the
        // new pass starts populating them.
        if let Some(dirs) = self.sync_dirs.get(&id) {
            for sd in dirs {
                self.sync_dir_errors.remove(&sd.id);
            }
        }
        self.syncing.insert(id);
        let repo = self.repo.clone();
        let rclone = self.rclone.clone();
        let events_tx = self.events_tx.clone();
        Command::perform(
            async move {
                let remote = match repo.find_remote(id).await {
                    Ok(Some(r)) => r,
                    _ => return id,
                };
                let sync_dirs = repo.list_sync_dirs(id).await.unwrap_or_default();
                let _ = tokio::task::spawn_blocking(move || {
                    let emit = move |event: SyncEvent| {
                        if let Some(tx) = &events_tx {
                            let _ = tx.blocking_send(event);
                        }
                    };
                    for sd in sync_dirs {
                        let _ = crate::services::sync::run(
                            &remote,
                            &sd,
                            &*repo,
                            &*rclone,
                            emit.clone(),
                        );
                    }
                })
                .await;
                id
            },
            Message::SyncFinished,
        )
    }
}

/// Launch the Iced application. Blocks until the window closes.
pub fn run(repo: Arc<dyn Repository>, rclone: Arc<dyn RcloneClient>) -> iced::Result {
    let mut settings = Settings::with_flags(Flags { repo, rclone });
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
