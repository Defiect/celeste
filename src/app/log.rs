//! Per-sync_dir log buffer.

use crate::{domain::sync::SyncDirId, screens::remote_page};

use super::CelesteApp;

impl CelesteApp {
    /// Append a line to the per-sync_dir log, drop the oldest entries
    /// once the buffer exceeds [`remote_page::MAX_LOG_LINES`], and
    /// rebuild the matching [`text_editor::Content`] so the read-only
    /// editor renders the trimmed history. The Content is materialised
    /// newest-first so the freshest entry sits on the editor's top line
    /// — iced 0.12's `text_editor` has no scrollbar and no way to pin
    /// the view to the bottom, and the user explicitly asked for
    /// reverse ordering as the fallback.
    pub(in crate::app) fn push_log_line(&mut self, sync_dir_id: SyncDirId, line: String) {
        let lines = self.sync_dir_log_lines.entry(sync_dir_id).or_default();
        lines.push(line);
        let drop = lines.len().saturating_sub(remote_page::MAX_LOG_LINES);
        if drop > 0 {
            lines.drain(..drop);
        }
        let mut joined = String::new();
        for (i, l) in lines.iter().rev().enumerate() {
            if i > 0 {
                joined.push('\n');
            }
            joined.push_str(l);
        }
        self.sync_dir_log_content.insert(
            sync_dir_id,
            iced::widget::text_editor::Content::with_text(&joined),
        );
    }
}
