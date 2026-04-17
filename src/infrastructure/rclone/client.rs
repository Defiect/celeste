//! Adapter implementation of [`crate::domain::ports::RcloneClient`].
//!
//! Operations get added as the orchestrator extraction identifies which
//! librclone RPC calls actually need to cross the port boundary. The raw
//! RPC helpers stay in `super::rpc` and are reused here.

use crate::domain::ports::RcloneClient;

pub struct LibrcloneClient;

impl LibrcloneClient {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LibrcloneClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RcloneClient for LibrcloneClient {}
