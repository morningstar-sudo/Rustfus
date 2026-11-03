//! Connects a [`Session`] to the media-writing service linked into this DLL. The service runs on a
//! background thread and retains the authenticated protocol boundary without launching a second
//! process.

use crate::session::{OpenRequest, PROGRESS_INTERVAL, Session};
use rustfus_core::{ErrorCode, OperationError, OperationPlan};
use std::fs::File;
use std::io::{BufReader, BufWriter};

pub type PipeSession = Session<BufReader<File>, BufWriter<File>>;

/// Starts the in-DLL service thread, accepts its private pipe connection and runs the handshake.
///
/// # Errors
///
/// Returns the failure reported by the platform layer or by the worker.
#[cfg(windows)]
pub fn open_worker_session(
    request: impl FnOnce(String) -> OpenRequest,
) -> Result<(PipeSession, OperationPlan), OperationError> {
    use uuid::Uuid;

    let pipe_name = format!(r"\\.\pipe\rustfus-{}", Uuid::new_v4());
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let server = rustfus_windows::NamedPipeServer::create(&pipe_name)?;
    let service_pipe = pipe_name.clone();
    let service_token = token.clone();
    std::thread::Builder::new()
        .name("rustfus-media-writer".to_owned())
        .spawn(move || {
            let result = rustfus_windows::connect_named_pipe(&service_pipe)
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
                .and_then(|pipe| rustfus_worker::run_pipe(pipe, service_token));
            if let Err(error) = result {
                eprintln!("Rustfus media writer stopped: {error}");
            }
        })
        .map_err(|error| OperationError::new(ErrorCode::Io, error.to_string()))?;
    let pipe = server.accept(std::process::id())?;
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
    _request: impl FnOnce(String) -> OpenRequest,
) -> Result<(PipeSession, OperationPlan), OperationError> {
    let _ = PROGRESS_INTERVAL;
    Err(OperationError::new(
        ErrorCode::UnsupportedPlatform,
        "Worker sessions are available only on Windows",
    ))
}
