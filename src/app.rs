//! Iced application root. The Phase D entry point alongside the existing
//! GTK `launch::launch`. Runs the pure-Rust UI against the already-extracted
//! service layer.

use std::{collections::HashMap, sync::Arc};

use iced::{executor, subscription, Application, Command, Element, Settings, Subscription, Theme};
use tokio::sync::mpsc;

use crate::{
    domain::{
        events::SyncEvent,
        ports::{RcloneClient, Repository},
        remote::{Remote, RemoteId},
        sync::{SyncDir, SyncDirId, SyncError},
    },
    screens::{main_page, remote_page, settings},
    theme,
};

/// Messages the root application dispatches. Screen-level messages are
/// wrapped by variants; service results fire their own.
#[derive(Debug, Clone)]
pub enum Message {
    Main(main_page::Msg),
    Remote(remote_page::Msg),
    Settings(settings::Msg),
    RemotesLoaded(Vec<Remote>),
    SyncDirsLoaded(RemoteId, Vec<SyncDir>),
    PolicySaved,
    SyncStarted(RemoteId),
    SyncFinished(RemoteId),
    WorkerReady(mpsc::Sender<SyncEvent>),
    SyncEventReceived(SyncEvent),
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
    /// Errors accumulated for each sync_dir since its last refresh.
    sync_dir_errors: HashMap<SyncDirId, Vec<SyncError>>,
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
            sync_dir_errors: HashMap::new(),
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
        subscription::channel(std::any::TypeId::of::<CelesteApp>(), 128, |mut output| async move {
            use iced::futures::SinkExt;
            let (tx, mut rx) = mpsc::channel::<SyncEvent>(128);
            let _ = output.send(Message::WorkerReady(tx)).await;
            while let Some(event) = rx.recv().await {
                let _ = output.send(Message::SyncEventReceived(event)).await;
            }
            std::future::pending::<()>().await;
            unreachable!()
        })
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::RemotesLoaded(remotes) => {
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
                // TODO: invoke the login flow.
                Command::none()
            }
            Message::Remote(remote_page::Msg::Back) => {
                self.selected = None;
                Command::none()
            }
            Message::Remote(remote_page::Msg::RefreshNow(id)) => self.start_sync(id),
            Message::SyncStarted(id) => {
                self.syncing.insert(id);
                Command::none()
            }
            Message::SyncFinished(id) => {
                self.syncing.remove(&id);
                Command::none()
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
                        self.sync_dir_status.insert(sync_dir_id, text);
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
                remote_page::view(
                    remote,
                    dirs,
                    &self.sync_dir_status,
                    &self.sync_dir_errors,
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
                        let _ = crate::services::sync_dir_pass::run(
                            &remote,
                            &sd,
                            &*repo,
                            &*rclone,
                            emit.clone(),
                            || {},
                            || {},
                            || false,
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
    let settings = Settings::with_flags(Flags { repo, rclone });
    CelesteApp::run(settings)
}
