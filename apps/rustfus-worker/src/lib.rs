//! Privileged media-writing service linked into `rustfus_ffi.dll`.
//!
//! The WPF front end runs as administrator and starts this service on a background thread. The
//! existing authenticated protocol remains the boundary between UI orchestration and destructive
//! disk operations, but no separate worker executable is launched or shipped.

#[cfg(any(windows, test))]
mod service;

#[cfg(windows)]
pub use service::run_pipe;
