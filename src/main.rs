pub mod app;
pub mod domain;
pub mod infrastructure;
pub mod screens;
pub mod services;
#[cfg(test)]
pub mod test_support;
pub mod theme;
pub mod util;
pub mod widgets;

use std::sync::Arc;

use sea_orm::Database;
use serde_json::json;

use crate::{
    app::run as iced_run,
    domain::{
        ports::{BackendClient, Repository},
        remote::Backend,
    },
    infrastructure::{
        client_router::ClientRouter,
        persistence::{
            self,
            migrations::{Migrator, MigratorTrait},
            repository::SeaOrmRepository,
        },
        proton::client::NativeProtonClient,
        rclone::LibrcloneClient,
        stderr_capture,
    },
};

fn main() {
    // Tap stderr before the Go runtime can grab it — that's the
    // only way to catch the `WARN[...] Too many requests` lines rclone's
    // backends emit when they silently retry a 429. Falls back to a no-op
    // if the platform can't hand us a pipe; sync keeps working, we just
    // lose rate-limit detection for the run.
    let _stderr = stderr_capture::install();

    // rclone config file lives next to our SQLite DB in ~/.config/celeste.
    let config_dir = util::get_config_dir();
    std::fs::create_dir_all(&config_dir).expect("failed to create config dir");
    let mut rclone_config = config_dir.clone();
    rclone_config.push("rclone.conf");
    celeste_go::initialize();
    // Prove the combined Go archive loaded — cheap (no network).
    eprintln!("celeste: native-go identity = {}", celeste_go::proton_drive_version());
    celeste_go::rpc(
        "config/setpath",
        json!({ "path": rclone_config }).to_string(),
    )
    .expect("failed to set rclone config path");

    let mut db_path = config_dir.clone();
    db_path.push("data.sqlite");
    if !db_path.exists() {
        std::fs::File::create(&db_path).expect("failed to create db file");
    }
    let db = util::await_future(Database::connect(format!(
        "sqlite://{}",
        db_path.display()
    )))
    .expect("failed to connect to the database");

    if util::await_future(persistence::has_legacy_migrations(&db)) {
        show_legacy_config_popup(&config_dir);
        std::process::exit(0);
    }

    util::await_future(Migrator::up(&db, None))
        .expect("failed to run database migrations");

    let repo: Arc<dyn Repository> = Arc::new(SeaOrmRepository::new(db));

    // Per-remote client router. celeste_go's librclone surface is the default — Celeste's
    // existing rclone-backed remotes keep working unchanged. Each
    // native-backend remote resumes its saved session up front so
    // the UID is registered before the first sync tick fires.
    let default_client: Arc<dyn BackendClient> = Arc::new(LibrcloneClient::new());
    let router = Arc::new(ClientRouter::new(default_client));
    resume_native_sessions(&*repo, &router);
    iced_run(repo, router, config_dir).expect("iced app exited with error");
}

/// Load every remote from the DB, and for those flagged
/// `Backend::NativeProton` resume the saved session (if any), wrap
/// the UID in a [`NativeProtonClient`], and register it on the
/// router keyed by remote name. When the resume fails, register a
/// [`DisabledProtonClient`] instead so the sync engine surfaces a
/// clear "Re-authenticate" message rather than falling through to
/// rclone (which would error with an opaque config-lookup failure).
fn resume_native_sessions(repo: &dyn Repository, router: &ClientRouter) {
    use crate::infrastructure::proton::client::DisabledProtonClient;
    let remotes = util::await_future(repo.list_remotes()).unwrap_or_default();
    for remote in remotes {
        if remote.backend != Backend::NativeProton {
            continue;
        }
        let Some(path) = remote.session_path.as_deref() else {
            let reason = format!(
                "Proton Drive session blob missing for '{}'. Click Reauthenticate on the remote page to log in again.",
                remote.name,
            );
            eprintln!("celeste: {reason}");
            notify_reauth_needed(&remote.name);
            router.register(
                remote.name.clone(),
                Arc::new(DisabledProtonClient::new(reason)),
            );
            continue;
        };
        match celeste_go::proton::resume_session(std::path::Path::new(path)) {
            Ok(cred) => {
                router.register(
                    remote.name.clone(),
                    Arc::new(NativeProtonClient::new(cred.uid)),
                );
                eprintln!(
                    "celeste: native-proton session resumed for '{}'.",
                    remote.name,
                );
            }
            Err(err) => {
                let reason = format!(
                    "Proton Drive session for '{}' could not be resumed ({err}). Click Reauthenticate on the remote page to log in again.",
                    remote.name,
                );
                eprintln!("celeste: {reason}");
                notify_reauth_needed(&remote.name);
                router.register(
                    remote.name.clone(),
                    Arc::new(DisabledProtonClient::new(reason)),
                );
            }
        }
    }
}

/// Best-effort OS notification when a native-proton remote can't resume
/// its session at startup. Silently swallows errors — the remote page
/// banner + button are the authoritative recovery surface; the toast
/// is just there to nudge users who've minimised Celeste to the tray.
fn notify_reauth_needed(remote_name: &str) {
    let _ = notify_rust::Notification::new()
        .summary("Celeste: re-authentication needed")
        .body(&format!(
            "Sync is paused for '{remote_name}'. Open Celeste and click Reauthenticate to log in again.",
        ))
        .appname("Celeste")
        .show();
}

fn show_legacy_config_popup(config_dir: &std::path::Path) {
    use iced::{
        widget::{button, column, text},
        Element, Length, Task, Theme,
    };

    struct LegacyPopup {
        config_dir: String,
    }

    #[derive(Debug, Clone)]
    enum Msg {
        Ack,
    }

    fn legacy_update(_state: &mut LegacyPopup, _msg: Msg) -> Task<Msg> {
        iced::window::latest().and_then(iced::window::close)
    }

    fn legacy_view(state: &LegacyPopup) -> Element<'_, Msg> {
        column![
            text("Outdated Celeste configuration detected").size(20),
            text(format!(
                "The sync algorithm was rewritten and the database schema is no longer compatible.\n\nDelete the following directory and restart Celeste:\n\n  {}",
                state.config_dir,
            ))
            .size(14),
            button(text("Close Celeste")).on_press(Msg::Ack),
        ]
        .spacing(16)
        .padding(24)
        .max_width(560)
        .width(Length::Fill)
        .into()
    }

    fn legacy_theme(_state: &LegacyPopup) -> Theme {
        Theme::Dark
    }

    let config_dir = config_dir.display().to_string();
    let _ = iced::application(
        move || LegacyPopup {
            config_dir: config_dir.clone(),
        },
        legacy_update,
        legacy_view,
    )
    .title("Celeste — outdated configuration")
    .theme(legacy_theme)
    .run();
}
