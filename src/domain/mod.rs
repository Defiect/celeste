//! Framework-agnostic domain types.
//!
//! Everything in this module is pure Rust — no GTK, no SeaORM, no librclone.
//! Infrastructure adapters map between these types and their external
//! representations; services work only with these types.

pub mod commands;
pub mod events;
pub mod remote;
pub mod sync;
