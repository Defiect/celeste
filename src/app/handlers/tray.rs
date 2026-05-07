//! Tray-related message handlers: lifecycle handshake and click actions.

use iced::Command;
use tokio::sync::mpsc;

use crate::infrastructure::tray::{TrayAction, TrayStatus};

use super::super::{CelesteApp, Message};

impl CelesteApp {
    /// Handle [`Message::TrayReady`] — store the sender and push an
    /// initial status snapshot so the icon reflects reality immediately.
    pub(in crate::app) fn handle_tray_ready(
        &mut self,
        tx: mpsc::Sender<TrayStatus>,
    ) -> Command<Message> {
        self.tray_tx = Some(tx);
        // First paint so the icon reflects reality immediately rather
        // than staying on "Loading" until the next state change.
        self.push_tray_status();
        Command::none()
    }

    /// Handle [`Message::TrayClick`] — translate the user-action into
    /// the appropriate window-mode change.
    pub(in crate::app) fn handle_tray_click(action: TrayAction) -> Command<Message> {
        match action {
            TrayAction::Open => {
                // Belt-and-braces: Wayland keeps a minimised xdg-toplevel
                // mapped; X11 honours set_visible. Issue both so either
                // transport restores the window.
                Command::batch([
                    iced::window::change_mode(
                        iced::window::Id::MAIN,
                        iced::window::Mode::Windowed,
                    ),
                    iced::window::minimize(iced::window::Id::MAIN, false),
                    iced::window::gain_focus(iced::window::Id::MAIN),
                ])
            }
            TrayAction::Hide => {
                // `Mode::Hidden` no-ops on Wayland once the surface has
                // been mapped (xdg-toplevel has no unmap request), so
                // `minimize(true)` is the cross-backend fallback. On X11
                // both take effect; on Wayland the minimise wins.
                Command::batch([
                    iced::window::change_mode(
                        iced::window::Id::MAIN,
                        iced::window::Mode::Hidden,
                    ),
                    iced::window::minimize(iced::window::Id::MAIN, true),
                ])
            }
            TrayAction::Quit => {
                // Closing the main window ends the Iced runtime, which
                // returns control to `main` and lets the process exit.
                iced::window::close(iced::window::Id::MAIN)
            }
        }
    }
}
