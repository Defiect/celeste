//! Iced application root. The Phase D entry point alongside the existing
//! GTK `launch::launch`. Runs the pure-Rust UI against the already-extracted
//! service layer.

use std::{collections::HashMap, sync::Arc};

use iced::{executor, Application, Command, Element, Settings, Theme};

use crate::{
    domain::{
        ports::{RcloneClient, Repository},
        remote::{Remote, RemoteId},
        sync::SyncDir,
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
}

pub struct CelesteApp {
    repo: Arc<dyn Repository>,
    rclone: Arc<dyn RcloneClient>,
    remotes: Vec<Remote>,
    sync_dirs: HashMap<RemoteId, Vec<SyncDir>>,
    selected: Option<RemoteId>,
    /// Remotes whose sync pass is currently running.
    syncing: std::collections::HashSet<RemoteId>,
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
                // TODO: hook into SyncOrchestrator once it drives the sync.
                Command::none()
            }
            Message::Main(main_page::Msg::AddRemote) => {
                // TODO: invoke the login flow.
                Command::none()
            }
            Message::Remote(remote_page::Msg::Back) => {
                self.selected = None;
                Command::none()
            }
            Message::Remote(remote_page::Msg::RefreshNow(id)) => {
                if self.syncing.contains(&id) {
                    return Command::none();
                }
                self.syncing.insert(id);
                let repo = self.repo.clone();
                let rclone = self.rclone.clone();
                Command::perform(
                    async move {
                        let remote = match repo.find_remote(id).await {
                            Ok(Some(r)) => r,
                            _ => return id,
                        };
                        let sync_dirs = repo.list_sync_dirs(id).await.unwrap_or_default();
                        let repo = repo.clone();
                        let rclone = rclone.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            for sd in sync_dirs {
                                let _ = crate::services::sync_dir_pass::run(
                                    &remote,
                                    &sd,
                                    &*repo,
                                    &*rclone,
                                    |_| {},
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
                .map(|msg| match msg {
                    Message::SyncFinished(id) => Message::SyncFinished(id),
                    other => other,
                })
            }
            Message::SyncStarted(id) => {
                self.syncing.insert(id);
                Command::none()
            }
            Message::SyncFinished(id) => {
                self.syncing.remove(&id);
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
                remote_page::view(remote, dirs).map(Message::Remote)
            }
            None => main_page::view(&self.remotes, self.selected, &self.syncing)
                .map(Message::Main),
        }
    }
}

/// Launch the Iced application. Blocks until the window closes.
pub fn run(repo: Arc<dyn Repository>, rclone: Arc<dyn RcloneClient>) -> iced::Result {
    let settings = Settings::with_flags(Flags { repo, rclone });
    CelesteApp::run(settings)
}
