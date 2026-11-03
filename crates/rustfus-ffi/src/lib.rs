//! C ABI for the WPF front end. Every function is panic-safe and returns a status code; results
//! travel as UTF-8 JSON strings owned by Rust and released with [`rustfus_string_free`].
//!
//! Status codes: `0` ok, `-1` internal panic, `-2` invalid argument, `-3` operation error
//! (`out_json` holds an `OperationError`), `-4` session closed.

mod json;
mod platform;
mod registry;
mod session;

use platform::PipeSession;
use rustfus_core::{DeviceSnapshot, ErrorCode, InstallAutomation, OperationError};
use serde_json::Value;
use session::{OpenRequest, SessionError};
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

pub const ABI_VERSION: i32 = 5;

const OK: i32 = 0;
const PANIC: i32 = -1;
const INVALID_ARGUMENT: i32 = -2;
const OPERATION_ERROR: i32 = -3;
const CLOSED: i32 = -4;

/// Longest wide string accepted from the UI, in UTF-16 units (the Windows path limit).
const MAX_WIDE_UNITS: usize = 32_768;
/// Imported XML plus JSON escaping. The Rust model separately limits the decoded XML to 2 MiB.
const MAX_UNATTEND_WIDE_UNITS: usize = 4 * 1024 * 1024;

enum Failure {
    InvalidArgument(&'static str),
    Operation(OperationError),
    Closed,
}

impl From<OperationError> for Failure {
    fn from(error: OperationError) -> Self {
        Self::Operation(error)
    }
}

impl From<SessionError> for Failure {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Closed => Self::Closed,
            SessionError::Operation(error) => Self::Operation(error),
        }
    }
}

unsafe fn write_json(out: *mut *mut c_char, value: &Value) {
    if out.is_null() {
        return;
    }
    let text = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned());
    // serde_json never emits NUL bytes, so this cannot fail; fall back to null defensively.
    let raw = CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw);
    // SAFETY: `out` is non-null and, per the ABI contract, points to writable storage.
    unsafe { *out = raw };
}

/// Runs `body`, converting its outcome (or a panic) into a status code and `out_json`.
unsafe fn run(out_json: *mut *mut c_char, body: impl FnOnce() -> Result<Value, Failure>) -> i32 {
    if !out_json.is_null() {
        // SAFETY: non-null, writable per the ABI contract.
        unsafe { *out_json = std::ptr::null_mut() };
    }
    let (code, value) = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(value)) => (OK, value),
        Ok(Err(Failure::Operation(error))) => (OPERATION_ERROR, json::error(&error)),
        Ok(Err(Failure::InvalidArgument(message))) => (
            INVALID_ARGUMENT,
            json::error(&OperationError::new(ErrorCode::Internal, message)),
        ),
        Ok(Err(Failure::Closed)) => (
            CLOSED,
            json::error(&OperationError::new(
                ErrorCode::Io,
                "The media-writer session is closed",
            )),
        ),
        Err(_) => (
            PANIC,
            json::error(&OperationError::new(
                ErrorCode::Internal,
                "Internal error in rustfus-ffi",
            )),
        ),
    };
    // SAFETY: forwarded ABI contract for `out_json`.
    unsafe { write_json(out_json, &value) };
    code
}

unsafe fn wide_arg(ptr: *const u16, what: &'static str) -> Result<String, Failure> {
    // SAFETY: forwarded pointer contract.
    unsafe { wide_arg_limited(ptr, what, MAX_WIDE_UNITS) }
}

unsafe fn wide_arg_limited(
    ptr: *const u16,
    what: &'static str,
    maximum_units: usize,
) -> Result<String, Failure> {
    if ptr.is_null() {
        return Err(Failure::InvalidArgument(what));
    }
    let mut length = 0;
    // SAFETY: the caller guarantees a NUL-terminated buffer; reading stops at the NUL or the cap.
    while length < maximum_units && unsafe { *ptr.add(length) } != 0 {
        length += 1;
    }
    if length == maximum_units {
        return Err(Failure::InvalidArgument(what));
    }
    // SAFETY: `length` units were just read from the buffer.
    let units = unsafe { std::slice::from_raw_parts(ptr, length) };
    String::from_utf16(units).map_err(|_| Failure::InvalidArgument(what))
}

unsafe fn utf8_arg(ptr: *const c_char, what: &'static str) -> Result<String, Failure> {
    if ptr.is_null() {
        return Err(Failure::InvalidArgument(what));
    }
    // SAFETY: the caller guarantees a NUL-terminated buffer.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| Failure::InvalidArgument(what))
}

/// Borrows a session handle for the duration of one call without consuming the caller's count.
unsafe fn borrow_session(handle: *mut c_void) -> Result<Arc<PipeSession>, Failure> {
    let ptr = handle.cast::<PipeSession>().cast_const();
    if ptr.is_null() {
        return Err(Failure::InvalidArgument("session"));
    }
    // SAFETY: a non-null handle came from `Arc::into_raw` in `rustfus_session_start` and has not
    // been closed (ABI contract); bumping the count keeps it alive while this call runs.
    unsafe {
        Arc::increment_strong_count(ptr);
        Ok(Arc::from_raw(ptr))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rustfus_abi_version() -> i32 {
    ABI_VERSION
}

/// Releases a string returned through an `out_json` parameter. Null is ignored.
///
/// # Safety
///
/// `text` must be null or a pointer previously returned by this library and not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_string_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: allocated by `CString::into_raw` in `write_json`, freed exactly once.
        drop(unsafe { CString::from_raw(text) });
    }
}

fn hash_linux_password(password: &str) -> Result<String, OperationError> {
    use sha_crypt::{PasswordHasher as _, ShaCrypt};

    if password.is_empty() || password.len() > 1024 {
        return Err(OperationError::new(
            ErrorCode::InvalidAutoinstallConfig,
            "The Ubuntu account password must contain 1-1024 UTF-8 bytes",
        ));
    }
    ShaCrypt::SHA512
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| {
            OperationError::new(
                ErrorCode::InvalidAutoinstallConfig,
                format!("Could not hash the Ubuntu account password: {error}"),
            )
        })
}

/// Hashes an Ubuntu account password locally as salted SHA-512 crypt (`$6$`).
///
/// # Safety
///
/// `password` must be NUL-terminated UTF-16; `out_json` as for `rustfus_enumerate_devices`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_hash_linux_password(
    password: *const u16,
    out_json: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded ABI contract.
    unsafe {
        run(out_json, || {
            let password = wide_arg_limited(password, "password", 1024)?;
            let hash = hash_linux_password(&password)?;
            Ok(serde_json::json!({ "hash": hash }))
        })
    }
}

/// Lists eligible USB targets as `{"devices":[...]}`.
///
/// # Safety
///
/// `out_json` must be null or point to writable storage for one pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_enumerate_devices(out_json: *mut *mut c_char) -> i32 {
    // SAFETY: forwarded ABI contract.
    unsafe {
        run(out_json, || {
            let eligible: Vec<DeviceSnapshot> = rustfus_windows::enumerate_usb_devices()?
                .into_iter()
                .filter(DeviceSnapshot::is_eligible_usb_target)
                .collect();
            registry::global().replace_devices(&eligible);
            Ok(json::devices(&eligible))
        })
    }
}

/// Analyzes an ISO and returns its description, including the id used to start a session.
///
/// # Safety
///
/// `path` must be a NUL-terminated UTF-16 string; `out_json` as for `rustfus_enumerate_devices`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_analyze_image(
    path: *const u16,
    out_json: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded ABI contract.
    unsafe {
        run(out_json, || {
            let path = PathBuf::from(wide_arg(path, "path")?);
            let analysis = rustfus_windows::analyze_image(&path)?;
            let value = json::image(&analysis);
            registry::global().remember_image(analysis);
            Ok(value)
        })
    }
}

/// Starts the in-DLL media-writing service, prepares a plan and starts the operation. On success
/// `*out_session` receives the handle and `out_json` the plan.
///
/// # Safety
///
/// String arguments must be NUL-terminated (`device_id`/`image_id` UTF-8, the rest UTF-16);
/// `out_session` and `out_json` must point to writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_session_start(
    device_id: *const c_char,
    image_id: *const c_char,
    volume_label: *const u16,
    verify: i32,
    automation_json: *const u16,
    confirmation: *const u16,
    out_session: *mut *mut c_void,
    out_json: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded ABI contract.
    unsafe {
        if !out_session.is_null() {
            *out_session = std::ptr::null_mut();
        }
        run(out_json, || {
            if out_session.is_null() {
                return Err(Failure::InvalidArgument("out_session"));
            }
            let device_id = utf8_arg(device_id, "device_id")?;
            let image_id = utf8_arg(image_id, "image_id")?;
            let volume_label = wide_arg(volume_label, "volume_label")?;
            let automation_json =
                wide_arg_limited(automation_json, "automation_json", MAX_UNATTEND_WIDE_UNITS)?;
            let automation = if automation_json.is_empty() {
                None
            } else {
                Some(
                    serde_json::from_str::<InstallAutomation>(&automation_json).map_err(
                        |error| {
                            Failure::Operation(OperationError::new(
                                ErrorCode::InvalidAutomationConfig,
                                format!(
                                    "The installation automation configuration is invalid: {error}"
                                ),
                            ))
                        },
                    )?,
                )
            };
            let confirmation = wide_arg(confirmation, "confirmation")?;
            let (device, image) = {
                let registry = registry::global();
                (
                    registry
                        .device(&device_id)
                        .ok_or(Failure::InvalidArgument("unknown device_id"))?,
                    registry
                        .image(&image_id)
                        .ok_or(Failure::InvalidArgument("unknown image_id"))?,
                )
            };
            let (session, plan) = platform::open_worker_session(|token| OpenRequest {
                token,
                device,
                image,
                volume_label,
                verify: verify != 0,
                automation,
                confirmation,
            })?;
            *out_session = Arc::into_raw(Arc::new(session)).cast_mut().cast::<c_void>();
            Ok(json::plan(&plan))
        })
    }
}

/// Blocks until the next event. Call from a dedicated thread.
///
/// # Safety
///
/// `session` must be a live handle from `rustfus_session_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_session_next_event(
    session: *mut c_void,
    out_json: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded ABI contract.
    unsafe {
        run(out_json, || {
            let session = borrow_session(session)?;
            Ok(json::event(&session.next_event()?))
        })
    }
}

/// Asks the media writer to cancel at its next safe boundary.
///
/// # Safety
///
/// `session` must be a live handle from `rustfus_session_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_session_cancel(session: *mut c_void) -> i32 {
    // SAFETY: forwarded ABI contract.
    unsafe {
        run(std::ptr::null_mut(), || {
            borrow_session(session)?.cancel()?;
            Ok(Value::Null)
        })
    }
}

/// Sends `Shutdown` and releases the handle. The handle must not be used afterwards; a
/// concurrent `rustfus_session_next_event` finishes with status `-4` once the service exits.
///
/// # Safety
///
/// `session` must be null or a live handle from `rustfus_session_start`, closed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rustfus_session_close(session: *mut c_void) {
    let ptr = session.cast::<PipeSession>().cast_const();
    if ptr.is_null() {
        return;
    }
    let _ = catch_unwind(|| {
        // SAFETY: consumes the count created by `Arc::into_raw` in `rustfus_session_start`.
        let session = unsafe { Arc::from_raw(ptr) };
        session.shutdown();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(body: impl FnOnce() -> Result<Value, Failure>) -> (i32, Value) {
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `out` is valid writable storage.
        let code = unsafe { run(&raw mut out, body) };
        assert!(!out.is_null());
        // SAFETY: `out` was produced by `write_json` and is freed once below.
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        unsafe { rustfus_string_free(out) };
        (code, serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn abi_version_is_five() {
        assert_eq!(rustfus_abi_version(), 5);
    }

    #[test]
    fn ok_value_is_returned_as_json() {
        let (code, value) = call(|| Ok(serde_json::json!({"a": 1})));
        assert_eq!(code, OK);
        assert_eq!(value["a"], 1);
    }

    #[test]
    fn ubuntu_passwords_are_salted_sha512_crypt_hashes() {
        use sha_crypt::{PasswordVerifier as _, ShaCrypt};

        let first = hash_linux_password("correct horse battery staple").unwrap();
        let second = hash_linux_password("correct horse battery staple").unwrap();
        assert!(first.starts_with("$6$"));
        assert_ne!(first, second);
        let parsed = sha_crypt::PasswordHash::new(&first).unwrap();
        ShaCrypt::SHA512
            .verify_password(b"correct horse battery staple", &parsed)
            .unwrap();
    }

    #[test]
    fn failures_map_to_their_status_codes() {
        let (code, value) = call(|| Err(Failure::InvalidArgument("bad")));
        assert_eq!(
            (code, value["message"].as_str()),
            (INVALID_ARGUMENT, Some("bad"))
        );
        let (code, value) = call(|| {
            Err(Failure::Operation(OperationError::new(
                ErrorCode::DeviceChanged,
                "x",
            )))
        });
        assert_eq!(
            (code, value["code"].as_str()),
            (OPERATION_ERROR, Some("device_changed"))
        );
        assert_eq!(call(|| Err(Failure::Closed)).0, CLOSED);
    }

    #[test]
    fn panics_never_cross_the_boundary() {
        let (code, value) = call(|| panic!("boom"));
        assert_eq!(code, PANIC);
        assert_eq!(value["code"], "internal");
    }

    #[test]
    fn wide_arguments_are_validated() {
        let ok: Vec<u16> = "RUSTFUS\0".encode_utf16().collect();
        // SAFETY: NUL-terminated buffer.
        assert_eq!(
            unsafe { wide_arg(ok.as_ptr(), "x") }.ok().as_deref(),
            Some("RUSTFUS")
        );
        // SAFETY: null is explicitly handled.
        assert!(unsafe { wide_arg(std::ptr::null(), "x") }.is_err());
        let lone_surrogate = [0xD800_u16, 0];
        // SAFETY: NUL-terminated buffer.
        assert!(unsafe { wide_arg(lone_surrogate.as_ptr(), "x") }.is_err());
    }

    #[test]
    fn unknown_ids_are_rejected_before_any_service_is_started() {
        let id = CString::new("nope").unwrap();
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (label, unattend, confirmation) = (wide("RUSTFUS"), wide(""), wide("ERASE"));
        let mut session: *mut c_void = std::ptr::null_mut();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: all pointers are valid for the duration of the call.
        let code = unsafe {
            rustfus_session_start(
                id.as_ptr(),
                id.as_ptr(),
                label.as_ptr(),
                1,
                unattend.as_ptr(),
                confirmation.as_ptr(),
                &raw mut session,
                &raw mut out,
            )
        };
        assert_eq!(code, INVALID_ARGUMENT);
        assert!(session.is_null());
        // SAFETY: freed once.
        unsafe { rustfus_string_free(out) };
    }

    #[test]
    fn null_session_handles_are_invalid_arguments() {
        // SAFETY: null handles are explicitly rejected.
        assert_eq!(
            unsafe { rustfus_session_cancel(std::ptr::null_mut()) },
            INVALID_ARGUMENT
        );
        // SAFETY: null is ignored.
        unsafe { rustfus_session_close(std::ptr::null_mut()) };
    }
}
