//! Framework-agnostic application services.
//!
//! Services depend on port traits from [`crate::domain::ports`] — never on
//! infrastructure types directly, which is what makes them unit-testable
//! against in-memory fakes (see `auth_service`'s tests).

pub mod auth_service;
pub mod remote_lifecycle;
pub mod should_sync;
pub mod sync_dir_ops;
pub mod sync_dir_pass;
pub mod sync_path;

#[cfg(test)]
mod should_sync_tests;
#[cfg(test)]
mod sync_dir_ops_tests;
#[cfg(test)]
mod sync_path_tests;
