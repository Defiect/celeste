//! Sync_dir-level handlers: list load, add/delete, exclusion CRUD,
//! per-remote settings (interval + enabled toggle), and the read-only
//! log editor's scroll/select pass-through.

use std::sync::atomic::Ordering;

use iced::{widget::text_editor, Command};

use crate::{
    domain::{
        ports::BackendClient,
        remote::RemoteId,
        sync::{SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId},
    },
    screens::{main_page, settings},
};

use super::super::{local_paths_overlap, CelesteApp, Message};

impl CelesteApp {
    /// Handle [`Message::SyncDirsLoaded`] — pre-populate empty log
    /// content + state machine entries for every dir on this remote.
    pub(in crate::app) fn handle_sync_dirs_loaded(
        &mut self,
        id: RemoteId,
        sd: Vec<SyncDir>,
    ) -> Command<Message> {
        // Pre-populate an empty `text_editor::Content` for every
        // sync_dir so the read-only editor renders even when the
        // engine hasn't emitted a single event for it yet — the
        // editor needs a `&Content` to draw against.
        for d in &sd {
            self.sync_dir_log_content
                .entry(d.id)
                .or_insert_with(iced::widget::text_editor::Content::new);
            self.sync_state.ensure_dir(id, d.id);
        }
        self.sync_dirs.insert(id, sd);
        Command::none()
    }

    /// Handle [`Message::AllSyncDirsRefreshed`] — replace the global
    /// auto-exclusion lookup table.
    pub(in crate::app) fn handle_all_sync_dirs_refreshed(
        &mut self,
        all: Vec<SyncDir>,
    ) -> Command<Message> {
        self.all_known_sync_dirs = all;
        Command::none()
    }

    /// Handle [`remote_page::Msg::DraftLocalPathChanged`] / [`DraftRemotePathChanged`].
    pub(in crate::app) fn handle_draft_local_path_changed(&mut self, s: String) -> Command<Message> {
        if let Some(id) = self.selected {
            self.sync_dir_drafts.entry(id).or_default().0 = s;
        }
        Command::none()
    }

    pub(in crate::app) fn handle_draft_remote_path_changed(&mut self, s: String) -> Command<Message> {
        if let Some(id) = self.selected {
            self.sync_dir_drafts.entry(id).or_default().1 = s;
        }
        Command::none()
    }

    /// Handle [`remote_page::Msg::AddSyncDir`] — normalise input,
    /// reject overlapping local paths, auto-create the directories,
    /// and insert the DB row.
    pub(in crate::app) fn handle_add_sync_dir(&mut self) -> Command<Message> {
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
        let local_norm = format!("/{}", crate::util::strip_slashes(local.trim()));
        let remote_norm = crate::util::strip_slashes(remote.trim());
        // Reject any local path that overlaps an existing sync_dir
        // (descendant or ancestor). Sync_dirs must be local siblings
        // — overlapping local trees would have the engine walking the
        // same files twice with conflicting tracking.
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
                    let _ = rclone.mkdir(
                        &remote_name,
                        &remote_for_mk,
                        &crate::domain::ports::cancel_never(),
                    );
                })
                .await;

                let _ = repo.insert_sync_dir(id, local_norm, remote_norm).await;
                id
            },
            |id| Message::Main(main_page::Msg::Selected(id)),
        )
    }

    /// Handle [`remote_page::Msg::DeleteSyncDir`].
    pub(in crate::app) fn handle_delete_sync_dir(
        &mut self,
        local: String,
        remote: String,
    ) -> Command<Message> {
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

    /// Handle [`remote_page::Msg::Settings`] / [`Message::Settings`] —
    /// update the in-memory policy, sync the enabled flag to the state
    /// machine, cancel an in-flight pass when the user disables, and
    /// persist asynchronously.
    pub(in crate::app) fn handle_settings(&mut self, sub: settings::Msg) -> Command<Message> {
        let Some(id) = self.selected else {
            return Command::none();
        };
        let Some(remote) = self.remotes.iter_mut().find(|r| r.id == id) else {
            return Command::none();
        };
        let was_enabled = remote.policy.enabled;
        let new_policy = settings::policy_from(&sub, &remote.policy);
        remote.policy = new_policy.clone();
        self.sync_state.set_remote_enabled(id, new_policy.enabled);
        // If the user just disabled a remote that's currently syncing,
        // trip its cancel flag so the running pass bails out between
        // actions. Re-enabling uses the next scheduler tick — no
        // action here.
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

    /// Handle [`Message::ExclusionsLoaded`].
    pub(in crate::app) fn handle_exclusions_loaded(
        &mut self,
        sd_id: SyncDirId,
        excls: Vec<SyncDirExclusion>,
    ) -> Command<Message> {
        self.sync_dir_exclusions.insert(sd_id, excls);
        Command::none()
    }

    /// Handle [`remote_page::Msg::ToggleExclusions`] — toggle the panel
    /// open/closed; loads the exclusions list when opening.
    pub(in crate::app) fn handle_toggle_exclusions(&mut self, sd_id: SyncDirId) -> Command<Message> {
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

    pub(in crate::app) fn handle_draft_exclusion_changed(
        &mut self,
        sd_id: SyncDirId,
        s: String,
    ) -> Command<Message> {
        self.draft_exclusion.insert(sd_id, s);
        Command::none()
    }

    pub(in crate::app) fn handle_add_exclusion(&mut self, sd_id: SyncDirId) -> Command<Message> {
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

    pub(in crate::app) fn handle_remove_exclusion(
        &mut self,
        excl_id: SyncDirExclusionId,
        sd_id: SyncDirId,
    ) -> Command<Message> {
        let repo = self.repo.clone();
        Command::perform(
            async move {
                let _ = repo.delete_exclusion(excl_id).await;
                repo.list_exclusions(sd_id).await.unwrap_or_default()
            },
            move |excls| Message::ExclusionsLoaded(sd_id, excls),
        )
    }

    /// Handle [`remote_page::Msg::LogEditorAction`] — drop edit
    /// actions, but feed scroll / select / click / drag through so the
    /// user can still navigate the history pane.
    pub(in crate::app) fn handle_log_editor_action(
        &mut self,
        sd_id: SyncDirId,
        action: text_editor::Action,
    ) -> Command<Message> {
        if !action.is_edit()
            && let Some(content) = self.sync_dir_log_content.get_mut(&sd_id)
        {
            content.perform(action);
        }
        Command::none()
    }
}
