//! Connects a [`Session`] to a freshly launched, elevated `rustfus-worker` over a private named
//! pipe. Only the platform plumbing lives here; the conversation is in `session.rs`.

use crate::session::{OpenRequest, PROGRESS_INTERVAL, Session};
use rustfus_core::{ErrorCode, OperationError, OperationPlan};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

pub type PipeSession = Session<BufReader<File>, BufWriter<File>>;

const WORKER_FILE_NAME: &str = "rustfus-worker.exe";

fn validate_worker_path(worker_path: &Path) -> Result<(), OperationError> {
    let is_worker = worker_path.is_absolute()
        && worker_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(WORKER_FILE_NAME));
    if !is_worker {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "The worker path must be the absolute path of rustfus-worker.exe",
        ));
    }
    if !worker_path.is_file() {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "rustfus-worker.exe was not found next to the application",
        ));
    }
    Ok(())
}

/// Starts the elevated worker (UAC prompt), accepts its pipe connection and runs the
/// handshake. Blocks until the user answers the prompt.
///
/// # Errors
///
/// Returns the failure reported by the platform layer or by the worker.
#[cfg(windows)]
pub fn open_worker_session(
    worker_path: &Path,
    request: impl FnOnce(String) -> OpenRequest,
) -> Result<(PipeSession, OperationPlan), OperationError> {
    use uuid::Uuid;

    validate_worker_path(worker_path)?;
    let pipe_name = format!(r"\\.\pipe\rustfus-{}", Uuid::new_v4());
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let server = rustfus_windows::NamedPipeServer::create(&pipe_name)?;
    let process_id = rustfus_windows::launch_elevated_worker(worker_path, &pipe_name, &token)?;
    let pipe = server.accept(process_id)?;
    let read_pipe = pipe
        .try_clone()
        .map_err(|error| OperationError::new(ErrorCode::Io, error.to_string()))?;
    Session::open(
        BufReader::new(read_pipe),
        BufWriter::new(pipe),
        request(token),
        PROGRESS_INTERVAL,
    )
}

/// # Errors
///
/// Always `UnsupportedPlatform` outside Windows.
#[cfg(not(windows))]
pub fn open_worker_session(
    worker_path: &Path,
    _request: impl FnOnce(String) -> OpenRequest,
) -> Result<(PipeSession, OperationPlan), OperationError> {
    validate_worker_path(worker_path)?;
    let _ = PROGRESS_INTERVAL;
    Err(OperationError::new(
        ErrorCode::UnsupportedPlatform,
        "Elevated worker sessions are available only on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_or_foreign_worker_paths_are_rejected() {
        for path in ["rustfus-worker.exe", "/tmp/evil.exe", "C:/x/cmd.exe"] {
            let error = validate_worker_path(Path::new(path)).unwrap_err();
            assert_eq!(error.code, ErrorCode::Internal, "{path}");
        }
    }
}
