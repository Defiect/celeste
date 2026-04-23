//! StatusNotifier (KDE/freedesktop) tray adapter. Spawns a `ksni` service
//! inside an Iced subscription, translating menu clicks and left-clicks
//! into [`TraySignal`]s and accepting state snapshots back from the app
//! to drive the icon and tooltip.
//!
//! The app never talks to `ksni` directly — it batches
//! [`subscription`] into its own subscription list, hands the returned
//! `Sender<TrayStatus>` to itself on receipt of [`TraySignal::Ready`],
//! and pushes a fresh [`TrayStatus`] whenever its aggregate sync state
//! changes.
//!
//! If the running desktop exposes no StatusNotifier host (plain GNOME
//! without an extension, a session missing a D-Bus broker, …) the
//! service fails to spawn; we log once and keep the subscription alive
//! so the app's `TrayReady`-gated push path is a no-op instead of a
//! back-pressure source.

use std::time::Duration;

use iced::{subscription, Subscription};
use ksni::{
    menu::{StandardItem, TextDirection},
    MenuItem, ToolTip, TrayMethods,
};
use tokio::sync::mpsc;

/// One user-visible action surfaced from the tray. The app maps each
/// of these to a window-lifecycle command.
#[derive(Clone, Copy, Debug)]
pub enum TrayAction {
    /// Show the main window (left-click or "Open Celeste" menu entry).
    Open,
    /// Hide the main window; syncing continues in the background.
    Hide,
    /// Close the main window and exit the process.
    Quit,
}

/// Aggregate sync state the app wants the tray icon and tooltip to
/// reflect. Recomputed by the app after every state change and pushed
/// via the sender handed over in [`TraySignal::Ready`].
#[derive(Clone, Debug)]
pub enum TrayStatus {
    /// Initial state before remotes have been loaded.
    Loading,
    /// No remotes are configured.
    Disconnected,
    /// Every remote is disabled.
    Paused,
    /// At least one remote is actively syncing.
    Syncing { count: usize },
    /// At least one remote has hit provider rate-limiting and is in a
    /// backoff window.
    Warning,
    /// All enabled remotes are idle and up to date.
    Done { last_sync_ago: Option<Duration> },
}

/// Everything the tray subscription emits into the Iced runtime.
#[derive(Debug)]
pub enum TraySignal {
    /// Delivered once at startup. The attached sender is how the app
    /// pushes fresh [`TrayStatus`] snapshots back to the tray.
    Ready(mpsc::Sender<TrayStatus>),
    /// A tray action the user triggered.
    Action(TrayAction),
}

/// Zero-sized marker so the tray subscription carries a different
/// hashed id from the sync-events subscription in `app.rs`.
struct TrayMarker;

/// Build the Iced subscription that owns the ksni service. Batch this
/// alongside the app's existing subscriptions.
pub fn subscription() -> Subscription<TraySignal> {
    subscription::channel(
        std::any::TypeId::of::<TrayMarker>(),
        32,
        |mut output| async move {
            use iced::futures::SinkExt;

            let (click_tx, mut click_rx) = mpsc::channel::<TrayAction>(32);
            let (status_tx, mut status_rx) = mpsc::channel::<TrayStatus>(32);

            let tray = CelesteTray {
                status: TrayStatus::Loading,
                click_tx,
            };

            match tray.spawn().await {
                Ok(handle) => {
                    // Handshake: give the app the status sender so it
                    // can start pushing state updates.
                    let _ = output.send(TraySignal::Ready(status_tx)).await;

                    loop {
                        tokio::select! {
                            Some(action) = click_rx.recv() => {
                                let _ = output.send(TraySignal::Action(action)).await;
                            }
                            Some(new_status) = status_rx.recv() => {
                                let _ = handle.update(|t: &mut CelesteTray| t.status = new_status).await;
                            }
                            else => break,
                        }
                    }
                }
                Err(err) => {
                    eprintln!(
                        "celeste: tray service failed to start ({err}); running without tray icon."
                    );
                    // Deliver Ready anyway so the app's try_send path
                    // stays wired to *something*; drain the receiver
                    // forever so the bounded channel can't backpressure.
                    let _ = output.send(TraySignal::Ready(status_tx)).await;
                    while status_rx.recv().await.is_some() {}
                }
            }

            std::future::pending::<()>().await;
            unreachable!()
        },
    )
}

/// The tray state held inside the ksni service task. Menu callbacks
/// receive `&mut Self`, so the click sender lives here.
struct CelesteTray {
    status: TrayStatus,
    click_tx: mpsc::Sender<TrayAction>,
}

impl ksni::Tray for CelesteTray {
    fn id(&self) -> String {
        "com.hunterwittenborn.Celeste".to_owned()
    }

    fn title(&self) -> String {
        "Celeste".to_owned()
    }

    fn icon_name(&self) -> String {
        icon_for(&self.status).to_owned()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: icon_for(&self.status).to_owned(),
            icon_pixmap: Vec::new(),
            title: "Celeste".to_owned(),
            description: description_for(&self.status),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            MenuItem::Standard(StandardItem {
                label: description_for(&self.status),
                enabled: false,
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Open Celeste".to_owned(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.click_tx.try_send(TrayAction::Open);
                }),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: "Hide window".to_owned(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.click_tx.try_send(TrayAction::Hide);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Quit Celeste".to_owned(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.click_tx.try_send(TrayAction::Quit);
                }),
                ..Default::default()
            }),
        ]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.click_tx.try_send(TrayAction::Open);
    }

    fn text_direction(&self) -> TextDirection {
        TextDirection::LeftToRight
    }
}

fn icon_for(status: &TrayStatus) -> &'static str {
    match status {
        TrayStatus::Loading => {
            "com.hunterwittenborn.Celeste.CelesteTrayLoading-symbolic"
        }
        TrayStatus::Disconnected => {
            "com.hunterwittenborn.Celeste.CelesteTrayDisconnected-symbolic"
        }
        TrayStatus::Paused => "com.hunterwittenborn.Celeste.CelesteTrayPaused-symbolic",
        TrayStatus::Syncing { .. } => {
            "com.hunterwittenborn.Celeste.CelesteTraySyncing-symbolic"
        }
        TrayStatus::Warning => {
            "com.hunterwittenborn.Celeste.CelesteTrayWarning-symbolic"
        }
        TrayStatus::Done { .. } => "com.hunterwittenborn.Celeste.CelesteTrayDone-symbolic",
    }
}

fn description_for(status: &TrayStatus) -> String {
    match status {
        TrayStatus::Loading => "Starting up…".to_owned(),
        TrayStatus::Disconnected => "No remotes configured".to_owned(),
        TrayStatus::Paused => "All remotes are disabled".to_owned(),
        TrayStatus::Syncing { count } => {
            if *count == 1 {
                "Syncing 1 remote…".to_owned()
            } else {
                format!("Syncing {count} remotes…")
            }
        }
        TrayStatus::Warning => "Rate-limited — backing off".to_owned(),
        TrayStatus::Done { last_sync_ago } => match last_sync_ago {
            Some(age) => format!("Up to date — last sync {}", format_ago(*age)),
            None => "Up to date".to_owned(),
        },
    }
}

fn format_ago(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3_600)
    }
}
