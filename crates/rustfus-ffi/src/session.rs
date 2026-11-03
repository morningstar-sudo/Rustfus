//! Client side of the UI/worker protocol, generic over the transport so the whole conversation is
//! testable without Windows, UAC or a real worker.

use rustfus_core::{
    ErrorCode, Fingerprint, ImageAnalysis, InstallAutomation, OperationError, OperationEvent,
    OperationPhase, OperationPlan,
};
use rustfus_protocol::{
    Envelope, ProtocolError, WorkerRequest, WorkerResponse, read_frame, write_frame,
};
use std::io::{self, Read, Write};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Minimum spacing between two `Progress` events of the same phase handed to the UI.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(75);

#[derive(Debug, PartialEq, Eq)]
pub enum SessionError {
    /// The worker closed the pipe, normally after `Shutdown` or when the operation ended.
    Closed,
    Operation(OperationError),
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub struct OpenRequest {
    pub token: String,
    pub device: Fingerprint,
    pub image: ImageAnalysis,
    pub volume_label: String,
    pub verify: bool,
    pub automation: Option<InstallAutomation>,
    /// Sent verbatim: the worker, not the UI, decides whether the phrase is acceptable.
    pub confirmation: String,
}

pub struct Session<R, W> {
    plan_id: Uuid,
    reader: Mutex<R>,
    writer: Mutex<W>,
    progress_interval: Duration,
    last_progress: Mutex<Option<(OperationPhase, Instant)>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn io_error(error: &ProtocolError) -> OperationError {
    OperationError::new(ErrorCode::Io, format!("Worker connection failed: {error}"))
}

fn unexpected(response: WorkerResponse) -> OperationError {
    match response {
        WorkerResponse::Error { error } => error,
        other => OperationError::new(
            ErrorCode::Internal,
            format!("Unexpected worker response: {other:?}"),
        ),
    }
}

fn session_error(error: ProtocolError) -> SessionError {
    match &error {
        ProtocolError::Io(io_error)
            if matches!(
                io_error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
            ) =>
        {
            SessionError::Closed
        }
        _ => SessionError::Operation(io_error(&error)),
    }
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn exchange<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    message: WorkerRequest,
) -> Result<WorkerResponse, OperationError> {
    let request_id = Uuid::new_v4();
    write_frame(writer, &Envelope::new(request_id, message)).map_err(|e| io_error(&e))?;
    loop {
        let response: Envelope<WorkerResponse> = read_frame(reader).map_err(|e| io_error(&e))?;
        if response.request_id == request_id {
            return Ok(response.message);
        }
    }
}

impl<R: Read, W: Write> Session<R, W> {
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    /// Authenticates, asks the worker to prepare a plan and, once that succeeds, sends `Start`.
    ///
    /// # Errors
    ///
    /// Returns the worker's error, or an `Io` error when the connection fails.
    pub fn open(
        mut reader: R,
        mut writer: W,
        request: OpenRequest,
        progress_interval: Duration,
    ) -> Result<(Self, OperationPlan), OperationError> {
        match exchange(
            &mut reader,
            &mut writer,
            WorkerRequest::Hello {
                token: request.token,
            },
        )? {
            WorkerResponse::HelloAck => {}
            other => return Err(unexpected(other)),
        }
        let plan = match exchange(
            &mut reader,
            &mut writer,
            WorkerRequest::Prepare {
                device_fingerprint: request.device,
                image: request.image,
                volume_label: request.volume_label,
                verify: request.verify,
                automation: request.automation.map(Box::new),
            },
        )? {
            WorkerResponse::Plan { plan } => plan,
            other => return Err(unexpected(other)),
        };
        write_frame(
            &mut writer,
            &Envelope::new(
                Uuid::new_v4(),
                WorkerRequest::Start {
                    plan_id: plan.id,
                    confirmation: request.confirmation,
                },
            ),
        )
        .map_err(|e| io_error(&e))?;
        let session = Self {
            plan_id: plan.id,
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            progress_interval,
            last_progress: Mutex::new(None),
        };
        Ok((session, plan))
    }

    /// Blocks until the next event for this plan. Intermediate `Progress` events that arrive
    /// faster than the configured interval are dropped; every other event is delivered.
    ///
    /// # Errors
    ///
    /// `Closed` once the worker has closed the pipe, otherwise the worker's error.
    pub fn next_event(&self) -> Result<OperationEvent, SessionError> {
        let mut reader = lock(&self.reader);
        loop {
            let response: Envelope<WorkerResponse> =
                read_frame(&mut *reader).map_err(session_error)?;
            match response.message {
                WorkerResponse::Event { plan_id, event } if plan_id == self.plan_id => {
                    if self.should_skip(&event) {
                        continue;
                    }
                    return Ok(event);
                }
                WorkerResponse::Event { .. } => {
                    return Err(SessionError::Operation(OperationError::new(
                        ErrorCode::Internal,
                        "The worker returned an event for a different plan",
                    )));
                }
                WorkerResponse::Accepted => {}
                other => return Err(SessionError::Operation(unexpected(other))),
            }
        }
    }

    fn should_skip(&self, event: &OperationEvent) -> bool {
        let mut last = lock(&self.last_progress);
        match event {
            OperationEvent::Progress { phase, .. } => {
                let now = Instant::now();
                if let Some((last_phase, at)) = *last {
                    if last_phase == *phase && now.duration_since(at) < self.progress_interval {
                        return true;
                    }
                }
                *last = Some((*phase, now));
                false
            }
            OperationEvent::PhaseChanged { .. } => {
                *last = None;
                false
            }
            _ => false,
        }
    }

    /// Asks the worker to cancel at its next safe boundary.
    ///
    /// # Errors
    ///
    /// Returns an `Io` error when the pipe is already gone.
    pub fn cancel(&self) -> Result<(), OperationError> {
        let mut writer = lock(&self.writer);
        write_frame(
            &mut *writer,
            &Envelope::new(
                Uuid::new_v4(),
                WorkerRequest::Cancel {
                    plan_id: self.plan_id,
                },
            ),
        )
        .map_err(|e| io_error(&e))
    }

    /// Tells the worker to exit. Failure is ignored because the worker may already be gone.
    pub fn shutdown(&self) {
        let mut writer = lock(&self.writer);
        let _ = write_frame(
            &mut *writer,
            &Envelope::new(Uuid::new_v4(), WorkerRequest::Shutdown),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfus_core::{Architecture, ImageKind, WriteMode};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    struct ChanWriter(mpsc::Sender<Vec<u8>>);

    impl Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .send(buf.to_vec())
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ChanReader {
        rx: mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
        pos: usize,
    }

    impl Read for ChanReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.buf.len() {
                match self.rx.recv() {
                    Ok(next) => {
                        self.buf = next;
                        self.pos = 0;
                    }
                    Err(_) => return Ok(0),
                }
            }
            let count = out.len().min(self.buf.len() - self.pos);
            out[..count].copy_from_slice(&self.buf[self.pos..self.pos + count]);
            self.pos += count;
            Ok(count)
        }
    }

    type End = (ChanReader, ChanWriter);

    fn pair() -> (End, End) {
        let (to_worker, worker_rx) = mpsc::channel();
        let (to_client, client_rx) = mpsc::channel();
        let end = |rx, tx| {
            (
                ChanReader {
                    rx,
                    buf: Vec::new(),
                    pos: 0,
                },
                ChanWriter(tx),
            )
        };
        (end(client_rx, to_worker), end(worker_rx, to_client))
    }

    fn image() -> ImageAnalysis {
        ImageAnalysis {
            path: PathBuf::from("x.iso"),
            kind: ImageKind::HybridDisk,
            architecture: Architecture::X86_64,
            size_bytes: 1,
            fingerprint: Fingerprint([9; 32]),
            required_bytes: 1,
            temporary_bytes: 0,
            warnings: Vec::new(),
        }
    }

    fn plan(id: Uuid) -> OperationPlan {
        OperationPlan {
            id,
            device_fingerprint: Fingerprint([1; 32]),
            image_fingerprint: Fingerprint([9; 32]),
            disk_number: 3,
            mode: WriteMode::HybridRaw,
            source: PathBuf::from("x.iso"),
            volume_label: "RUSTFUS".into(),
            verify: true,
            bytes_to_write: 1,
            temporary_bytes: 0,
            actions: Vec::new(),
        }
    }

    fn open_request(confirmation: &str) -> OpenRequest {
        OpenRequest {
            token: "secret".into(),
            device: Fingerprint([1; 32]),
            image: image(),
            volume_label: "RUSTFUS".into(),
            verify: true,
            automation: None,
            confirmation: confirmation.into(),
        }
    }

    fn recv(reader: &mut ChanReader) -> Envelope<WorkerRequest> {
        read_frame(reader).expect("worker read")
    }

    fn reply(writer: &mut ChanWriter, request_id: Uuid, message: WorkerResponse) {
        write_frame(writer, &Envelope::new(request_id, message)).expect("worker write");
    }

    fn event(writer: &mut ChanWriter, plan_id: Uuid, event: OperationEvent) {
        reply(
            writer,
            Uuid::new_v4(),
            WorkerResponse::Event { plan_id, event },
        );
    }

    fn progress(completed_bytes: u64, phase: OperationPhase) -> OperationEvent {
        OperationEvent::Progress {
            phase,
            completed_bytes,
            total_bytes: 100,
            bytes_per_second: 1.0,
        }
    }

    /// Plays the worker through Hello and Prepare, returning the plan id it handed out.
    fn handshake(reader: &mut ChanReader, writer: &mut ChanWriter) -> Uuid {
        let hello = recv(reader);
        assert_eq!(
            hello.message,
            WorkerRequest::Hello {
                token: "secret".into()
            }
        );
        reply(writer, hello.request_id, WorkerResponse::HelloAck);
        let prepare = recv(reader);
        assert!(matches!(prepare.message, WorkerRequest::Prepare { .. }));
        let plan_id = Uuid::new_v4();
        reply(
            writer,
            prepare.request_id,
            WorkerResponse::Plan {
                plan: plan(plan_id),
            },
        );
        plan_id
    }

    fn spawn_worker(
        end: End,
        script: impl FnOnce(&mut ChanReader, &mut ChanWriter) + Send + 'static,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let (mut reader, mut writer) = end;
            script(&mut reader, &mut writer);
        })
    }

    #[test]
    fn full_conversation_delivers_events_then_reports_closed() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            let plan_id = handshake(reader, writer);
            let start = recv(reader);
            assert_eq!(
                start.message,
                WorkerRequest::Start {
                    plan_id,
                    confirmation: "ERASE".into()
                }
            );
            reply(writer, start.request_id, WorkerResponse::Accepted);
            event(writer, plan_id, progress(10, OperationPhase::Write));
            event(writer, plan_id, OperationEvent::Completed);
        });

        let (session, plan) =
            Session::open(client.0, client.1, open_request("ERASE"), Duration::ZERO).unwrap();
        assert_eq!(plan.disk_number, 3);
        assert_eq!(
            session.next_event(),
            Ok(progress(10, OperationPhase::Write))
        );
        assert_eq!(session.next_event(), Ok(OperationEvent::Completed));
        assert_eq!(session.next_event(), Err(SessionError::Closed));
        handle.join().unwrap();
    }

    #[test]
    fn a_wrong_confirmation_is_forwarded_untouched() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            handshake(reader, writer);
            let start = recv(reader);
            assert!(matches!(
                start.message,
                WorkerRequest::Start { ref confirmation, .. } if confirmation == "erase"
            ));
            reply(
                writer,
                start.request_id,
                WorkerResponse::Error {
                    error: OperationError::new(ErrorCode::ConfirmationRequired, "no"),
                },
            );
        });
        let (session, _) =
            Session::open(client.0, client.1, open_request("erase"), Duration::ZERO).unwrap();
        assert_eq!(
            session.next_event(),
            Err(SessionError::Operation(OperationError::new(
                ErrorCode::ConfirmationRequired,
                "no"
            )))
        );
        handle.join().unwrap();
    }

    #[test]
    fn rejected_token_surfaces_the_worker_error() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            let hello = recv(reader);
            reply(
                writer,
                hello.request_id,
                WorkerResponse::Error {
                    error: OperationError::new(ErrorCode::AccessDenied, "Worker token rejected"),
                },
            );
        });
        let error = Session::open(client.0, client.1, open_request("ERASE"), Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::AccessDenied);
        handle.join().unwrap();
    }

    #[test]
    fn prepare_failure_never_sends_start() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            let hello = recv(reader);
            reply(writer, hello.request_id, WorkerResponse::HelloAck);
            let prepare = recv(reader);
            reply(
                writer,
                prepare.request_id,
                WorkerResponse::Error {
                    error: OperationError::new(ErrorCode::DeviceChanged, "changed"),
                },
            );
            // The client must give up without sending anything else.
            assert!(read_frame::<_, WorkerRequest>(reader).is_err());
        });
        let error = Session::open(client.0, client.1, open_request("ERASE"), Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::DeviceChanged);
        handle.join().unwrap();
    }

    #[test]
    fn cancel_and_shutdown_reach_the_worker() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            let plan_id = handshake(reader, writer);
            let start = recv(reader);
            reply(writer, start.request_id, WorkerResponse::Accepted);
            assert_eq!(recv(reader).message, WorkerRequest::Cancel { plan_id });
            event(
                writer,
                plan_id,
                OperationEvent::Cancelled {
                    media_may_be_incomplete: true,
                },
            );
            assert_eq!(recv(reader).message, WorkerRequest::Shutdown);
        });
        let (session, _) =
            Session::open(client.0, client.1, open_request("ERASE"), Duration::ZERO).unwrap();
        session.cancel().unwrap();
        assert_eq!(
            session.next_event(),
            Ok(OperationEvent::Cancelled {
                media_may_be_incomplete: true
            })
        );
        session.shutdown();
        handle.join().unwrap();
    }

    #[test]
    fn event_for_another_plan_is_an_error() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            handshake(reader, writer);
            let start = recv(reader);
            reply(writer, start.request_id, WorkerResponse::Accepted);
            event(writer, Uuid::new_v4(), OperationEvent::Completed);
        });
        let (session, _) =
            Session::open(client.0, client.1, open_request("ERASE"), Duration::ZERO).unwrap();
        assert!(matches!(
            session.next_event(),
            Err(SessionError::Operation(OperationError {
                code: ErrorCode::Internal,
                ..
            }))
        ));
        handle.join().unwrap();
    }

    #[test]
    fn rapid_progress_is_coalesced_but_other_events_are_not() {
        let (client, worker) = pair();
        let handle = spawn_worker(worker, |reader, writer| {
            let plan_id = handshake(reader, writer);
            let start = recv(reader);
            reply(writer, start.request_id, WorkerResponse::Accepted);
            for n in 0..50 {
                event(writer, plan_id, progress(n, OperationPhase::Write));
            }
            event(
                writer,
                plan_id,
                OperationEvent::PhaseChanged {
                    phase: OperationPhase::Verify,
                },
            );
            event(writer, plan_id, progress(0, OperationPhase::Verify));
            event(writer, plan_id, progress(1, OperationPhase::Verify));
            event(writer, plan_id, OperationEvent::Completed);
        });
        let (session, _) = Session::open(
            client.0,
            client.1,
            open_request("ERASE"),
            Duration::from_secs(3600),
        )
        .unwrap();
        let mut received = Vec::new();
        while let Ok(next) = session.next_event() {
            received.push(next);
        }
        assert_eq!(
            received,
            vec![
                progress(0, OperationPhase::Write),
                OperationEvent::PhaseChanged {
                    phase: OperationPhase::Verify
                },
                progress(0, OperationPhase::Verify),
                OperationEvent::Completed,
            ]
        );
        handle.join().unwrap();
    }
}
