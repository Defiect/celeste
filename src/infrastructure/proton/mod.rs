//! Native ProtonDrive client — talks directly to Proton's API via
//! `celeste-native-sys` (the combined Go archive), no rclone in the
//! path. Implements [`crate::domain::ports::RcloneClient`] so the
//! sync engine and auth flow can consume it via the same trait that
//! `LibrcloneClient` satisfies.

pub mod client;
