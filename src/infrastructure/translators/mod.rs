//! Per-backend `EventTranslator` implementations.
//!
//! Each submodule classifies the raw error strings and log lines produced by
//! one backend into the unified `BackendEvent` taxonomy. Upper layers import
//! the concrete translator they need rather than using dynamic dispatch, since
//! the translator is known at compile time from the adapter type.

pub mod proton;
pub mod rclone;
