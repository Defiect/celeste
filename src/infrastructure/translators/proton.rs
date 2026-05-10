//! Translator for the native ProtonDrive backend (go-proton-api via FFI).
//!
//! Proton returns structured error codes in the form `Code=NNNN`. The
//! canonical codes we handle:
//!   - `Code=401`   — auth / session expired
//!   - `Code=10013` — invalid refresh token (server force-revoked the
//!                    session; only the user re-entering credentials can
//!                    recover it)
//!   - `Code=2500`  — already exists (mkdir idempotence, also "already exists"
//!                    substring from the Go bridge)
//!   - `Code=429`   — rate limited (Proton reports this directly, unlike
//!                    rclone which hides 429s in retry loops)

use crate::domain::backend_events::{BackendEvent, EventTranslator, Operation};

pub struct ProtonTranslator;

impl EventTranslator for ProtonTranslator {
    fn classify(&self, op: Operation, msg: &str) -> BackendEvent {
        let lower = msg.to_ascii_lowercase();

        if msg.contains("Code=401")
            || msg.contains("Code=10013")
            || lower.contains("unauthenticated")
            || lower.contains("unauthorized")
            || lower.contains("invalid access token")
            || lower.contains("invalid refresh token")
            || lower.contains("session expired")
            // go-proton-api wraps a server-driven session revocation as
            // "failed to refresh auth, de-auth: …". The "de-auth" marker
            // is unique to that path; matching it covers future Code=
            // values the API may add for the same condition.
            || lower.contains("de-auth")
        {
            return BackendEvent::AuthExpired;
        }

        if msg.contains("Code=429") || lower.contains("too many requests") {
            return BackendEvent::RateLimited;
        }

        // Folder-already-exists from the Go bridge (create_folder returns
        // Code=2500 for duplicate names on Proton's API).
        if op == Operation::Mkdir
            && (msg.contains("Code=2500") || lower.contains("already exists"))
        {
            return BackendEvent::AlreadyExists;
        }

        if lower.contains("not found") || msg.contains("Code=2501") {
            return BackendEvent::NotFound;
        }

        if lower.contains("connection reset")
            || lower.contains("broken pipe")
            || lower.contains("eof")
            || lower.contains("timeout")
        {
            return BackendEvent::TransientNetwork;
        }

        BackendEvent::Other(msg.to_owned())
    }
}
