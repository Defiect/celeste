//! Celeste's combined Go archive — Rust side.
//!
//! Raw bindings live in `ffi` (bindgen-generated at build time from
//! `wrapper.go`'s `//export`s). The crate root also exposes small safe
//! wrappers matching the upstream `librclone` crate's API (initialize,
//! finalize, rpc) so the rest of Celeste can swap its `librclone` dep
//! for this one with no code changes in the rclone path. New surface
//! area (`proton_drive_version` and future native Drive calls) sits
//! alongside.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

pub mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::{ffi::CStr, os::raw::c_char};

/// Initialize the Go runtime, rclone's librclone, and prepare the
/// native Proton client for use. Must be called once at process
/// startup before any other function in this crate.
pub fn initialize() {
    unsafe { ffi::RcloneInitialize() };
}

/// Finalize the Go runtime. Currently triggers a Go GC. Safe to skip
/// at process exit — the OS reclaims everything.
pub fn finalize() {
    unsafe { ffi::RcloneFinalize() };
}

/// Perform a single rclone RPC call. Identical signature to the
/// upstream `librclone::rpc` so existing Celeste code needn't change.
///
/// - `method`: e.g. `operations/list` — see <https://rclone.org/rc/>.
/// - `input`: a serialised JSON object.
/// - returns `Ok(json)` for HTTP 200, otherwise `Err(json)`.
pub fn rpc<S1: Into<String>, S2: Into<String>>(method: S1, input: S2) -> Result<String, String> {
    let mut method_cstr: Vec<c_char> = method
        .into()
        .into_bytes()
        .into_iter()
        .map(|b| b as c_char)
        .collect();
    method_cstr.push(0);
    let mut input_cstr: Vec<c_char> = input
        .into()
        .into_bytes()
        .into_iter()
        .map(|b| b as c_char)
        .collect();
    input_cstr.push(0);

    let result = unsafe { ffi::RcloneRPC(method_cstr.as_mut_ptr(), input_cstr.as_mut_ptr()) };
    let output = unsafe { CStr::from_ptr(result.Output) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::RcloneFreeString(result.Output) };
    if result.Status == 200 {
        Ok(output)
    } else {
        Err(output)
    }
}

/// Phase 1 smoke test for the native ProtonDrive surface. Exercises
/// the cgo boundary to `go-proton-api` without making any network
/// calls. Returns an identity string; later phases add real session /
/// list / upload / download / trash entry points.
pub fn proton_drive_version() -> String {
    let raw = unsafe { ffi::ProtonDrive_Version() };
    let out = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::RcloneFreeString(raw) };
    out
}
