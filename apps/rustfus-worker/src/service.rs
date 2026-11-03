use rustfus_core::{
    ErrorCode, Fingerprint, ImageAnalysis, InstallAutomation, OperationError, OperationEvent,
    OperationPhase, OperationPlan,
};
use rustfus_protocol::{Envelope, WorkerRequest, WorkerResponse, read_frame, write_frame};
use rustfus_windows::{CancellationToken, StreamProgress};
use std::collections::HashMap;
#[cfg(windows)]
use std::io::BufReader;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

#[cfg(windows)]
pub fn run_pipe(
    pipe: std::fs::File,
    expected_token: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let read_pipe = pipe.try_clone()?;
    let mut reader = BufReader::new(read_pipe);
    let writer: SharedWriter = Arc::new(Mutex::new(Box::new(pipe)));
    run_transport(&mut reader, writer, Some(expected_token))
}

fn run_transport<R: Read>(
    reader: &mut R,
    writer: SharedWriter,
    expected_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut authenticated = false;
    let mut prepared = HashMap::<uuid::Uuid, PreparedOperation>::new();
    let mut cancellations = HashMap::<uuid::Uuid, CancellationToken>::new();
    let operation_active = Arc::new(AtomicBool::new(false));

    loop {
        let request: Envelope<WorkerRequest> = read_frame(reader)?;
        let response = match request.message {
            WorkerRequest::Hello { .. } if authenticated => WorkerResponse::Error {
                error: OperationError::new(
                    ErrorCode::AccessDenied,
                    "The worker handshake has already been completed",
                ),
            },
            WorkerRequest::Hello { token } => {
                authenticated = expected_token
                    .as_deref()
                    .is_some_and(|expected| expected == token);
                if authenticated {
                    WorkerResponse::HelloAck
                } else {
                    WorkerResponse::Error {
                        error: OperationError::new(
                            ErrorCode::AccessDenied,
                            "Worker token rejected",
                        ),
                    }
                }
            }
            _ if !authenticated => WorkerResponse::Error {
                error: OperationError::new(
                    ErrorCode::AccessDenied,
                    "The worker has not completed its authenticated handshake",
                ),
            },
            WorkerRequest::EnumerateDevices => match rustfus_windows::enumerate_usb_devices() {
                Ok(devices) => WorkerResponse::Devices { devices },
                Err(error) => WorkerResponse::Error { error },
            },
            WorkerRequest::AnalyzeImage { path } => match rustfus_windows::analyze_image(&path) {
                Ok(analysis) => WorkerResponse::Analysis { analysis },
                Err(error) => WorkerResponse::Error { error },
            },
            WorkerRequest::Prepare {
                device_fingerprint,
                image,
                volume_label,
                verify,
                automation,
            } => {
                match prepare_operation(
                    device_fingerprint,
                    &image,
                    &volume_label,
                    verify,
                    automation.map(|config| *config),
                ) {
                    Ok(operation) => {
                        let plan = operation.plan.clone();
                        prepared.insert(plan.id, operation);
                        WorkerResponse::Plan { plan }
                    }
                    Err(error) => WorkerResponse::Error { error },
                }
            }
            WorkerRequest::Start {
                plan_id,
                confirmation,
            } => {
                if operation_active.swap(true, Ordering::AcqRel) {
                    WorkerResponse::Error {
                        error: OperationError::new(
                            ErrorCode::Internal,
                            "This worker already has an active operation",
                        ),
                    }
                } else if let Some(operation) = prepared.remove(&plan_id) {
                    let cancellation = CancellationToken::default();
                    cancellations.insert(plan_id, cancellation.clone());
                    spawn_operation(
                        operation,
                        confirmation,
                        cancellation,
                        Arc::clone(&writer),
                        Arc::clone(&operation_active),
                        request.request_id,
                    );
                    WorkerResponse::Accepted
                } else {
                    operation_active.store(false, Ordering::Release);
                    WorkerResponse::Error {
                        error: OperationError::new(
                            ErrorCode::DeviceChanged,
                            "The plan does not exist or has already been consumed",
                        ),
                    }
                }
            }
            WorkerRequest::Cancel { plan_id } => {
                if let Some(cancellation) = cancellations.get(&plan_id) {
                    cancellation.cancel();
                    WorkerResponse::Accepted
                } else {
                    WorkerResponse::Error {
                        error: OperationError::new(
                            ErrorCode::Internal,
                            "No active operation matches the requested plan",
                        ),
                    }
                }
            }
            WorkerRequest::Shutdown => {
                for cancellation in cancellations.values() {
                    cancellation.cancel();
                }
                send_response(&writer, request.request_id, WorkerResponse::Accepted)?;
                break;
            }
        };
        send_response(&writer, request.request_id, response)?;
    }
    Ok(())
}

struct PreparedOperation {
    plan: OperationPlan,
    #[cfg_attr(not(windows), allow(dead_code))]
    image: ImageAnalysis,
    #[cfg_attr(not(windows), allow(dead_code))]
    automation: Option<InstallAutomation>,
}

fn prepare_operation(
    device_fingerprint: Fingerprint,
    untrusted_image: &ImageAnalysis,
    volume_label: &str,
    verify: bool,
    automation: Option<InstallAutomation>,
) -> Result<PreparedOperation, OperationError> {
    let image = rustfus_windows::analyze_image(&untrusted_image.path)?;
    if image.fingerprint != untrusted_image.fingerprint {
        return Err(OperationError::new(
            ErrorCode::ImageChanged,
            "The worker image fingerprint differs from the UI analysis",
        ));
    }
    let device = rustfus_windows::enumerate_usb_devices()?
        .into_iter()
        .find(|candidate| candidate.fingerprint() == device_fingerprint)
        .ok_or_else(|| {
            OperationError::new(
                ErrorCode::DeviceChanged,
                "The selected USB device is no longer present",
            )
        })?;
    if rustfus_windows::path_is_on_disk(&image.path, device.disk_number)? {
        return Err(OperationError::new(
            ErrorCode::UnsafeTarget,
            "The source image is stored on the selected target disk",
        ));
    }
    let plan = OperationPlan::create(&device, &image, volume_label, verify, automation.as_ref())?;
    Ok(PreparedOperation {
        plan,
        image,
        automation,
    })
}

fn spawn_operation(
    operation: PreparedOperation,
    confirmation: String,
    cancellation: CancellationToken,
    writer: SharedWriter,
    operation_active: Arc<AtomicBool>,
    request_id: uuid::Uuid,
) {
    std::thread::spawn(move || {
        let plan_id = operation.plan.id;
        let _ = send_response(
            &writer,
            request_id,
            WorkerResponse::Event {
                plan_id,
                event: OperationEvent::PhaseChanged {
                    phase: OperationPhase::Analyze,
                },
            },
        );
        let result = execute_operation(&operation, &confirmation, &cancellation, |progress| {
            let event = OperationEvent::Progress {
                phase: progress.phase,
                completed_bytes: progress.completed_bytes,
                total_bytes: progress.total_bytes,
                bytes_per_second: progress.bytes_per_second,
            };
            let _ = send_response(
                &writer,
                request_id,
                WorkerResponse::Event { plan_id, event },
            );
        });
        let event = match result {
            Ok(()) => OperationEvent::Completed,
            Err(error) if error.code == ErrorCode::Cancelled => OperationEvent::Cancelled {
                media_may_be_incomplete: true,
            },
            Err(error) => OperationEvent::Failed { error },
        };
        let _ = send_response(
            &writer,
            request_id,
            WorkerResponse::Event { plan_id, event },
        );
        operation_active.store(false, Ordering::Release);
    });
}

fn execute_operation<F>(
    operation: &PreparedOperation,
    confirmation: &str,
    cancellation: &CancellationToken,
    progress: F,
) -> Result<(), OperationError>
where
    F: FnMut(StreamProgress),
{
    #[cfg(not(windows))]
    {
        let _ = (operation, confirmation, cancellation, progress);
        Err(OperationError::new(
            ErrorCode::UnsupportedPlatform,
            "Physical USB writes are available only on Windows",
        ))
    }

    #[cfg(windows)]
    {
        let current_image = rustfus_windows::analyze_image(&operation.image.path)?;
        let current_device = fresh_device(operation.plan.disk_number)?;
        operation
            .plan
            .revalidate(&current_device, &current_image, confirmation)?;
        if rustfus_windows::path_is_on_disk(&current_image.path, current_device.disk_number)? {
            return Err(OperationError::new(
                ErrorCode::UnsafeTarget,
                "The source image is stored on the selected target disk",
            ));
        }

        match operation.plan.mode {
            rustfus_core::WriteMode::HybridRaw => {
                let _locks = rustfus_windows::lock_target_volumes(current_device.disk_number)?;
                let locked_device = fresh_device(operation.plan.disk_number)?;
                if locked_device.fingerprint() != operation.plan.device_fingerprint {
                    return Err(OperationError::new(
                        ErrorCode::DeviceChanged,
                        "Target identity changed while locking its volumes",
                    ));
                }

                let mut source = rustfus_windows::open_source_image(&current_image.path)?;
                let mut target =
                    rustfus_windows::open_physical_disk(current_device.disk_number, true)?;
                rustfus_windows::write_raw_image(
                    &mut source,
                    &mut target,
                    rustfus_windows::RawWriteOptions {
                        source_length: current_image.size_bytes,
                        sector_alignment: current_device.physical_sector_size,
                        verify: operation.plan.verify,
                        cancellation,
                    },
                    |disk| disk.sync_all().map_err(io_operation_error),
                    progress,
                )?;
                rustfus_windows::update_disk_properties(&target)?;
                Ok(())
            }
            rustfus_core::WriteMode::WindowsMbrFat32 => {
                let config = match operation.automation.as_ref() {
                    Some(InstallAutomation::Windows(config)) => Some(config),
                    None => None,
                    Some(InstallAutomation::Ubuntu(_)) => {
                        return Err(OperationError::new(
                            ErrorCode::Internal,
                            "Ubuntu automation reached the Windows media pipeline",
                        ));
                    }
                };
                rustfus_windows::create_windows_installer(
                    &operation.plan,
                    &current_device,
                    config,
                    cancellation,
                    progress,
                )
            }
            rustfus_core::WriteMode::UbuntuUefiFat32 => {
                let Some(InstallAutomation::Ubuntu(config)) = operation.automation.as_ref() else {
                    return Err(OperationError::new(
                        ErrorCode::Internal,
                        "Ubuntu media creation requires an autoinstall configuration",
                    ));
                };
                rustfus_windows::create_ubuntu_installer(
                    &operation.plan,
                    &current_device,
                    config,
                    cancellation,
                    progress,
                )
            }
        }
    }
}

#[cfg(windows)]
fn fresh_device(disk_number: u32) -> Result<rustfus_core::DeviceSnapshot, OperationError> {
    rustfus_windows::enumerate_usb_devices()?
        .into_iter()
        .find(|candidate| candidate.disk_number == disk_number)
        .ok_or_else(|| OperationError::new(ErrorCode::DeviceChanged, "Target USB was removed"))
}

#[cfg(windows)]
fn io_operation_error(error: io::Error) -> OperationError {
    let mut result = OperationError::new(ErrorCode::Io, error.to_string());
    if let Some(code) = error.raw_os_error() {
        result = result.with_platform_code(code as u32);
    }
    result
}

fn send_response(
    writer: &SharedWriter,
    request_id: uuid::Uuid,
    response: WorkerResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = writer
        .lock()
        .map_err(|_| io::Error::other("worker output lock is poisoned"))?;
    write_frame(&mut **writer, &Envelope::new(request_id, response))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Clone, Default)]
    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("capture lock is poisoned"))?
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn transport_input(messages: impl IntoIterator<Item = WorkerRequest>) -> Vec<u8> {
        let mut input = Vec::new();
        for message in messages {
            write_frame(&mut input, &Envelope::new(uuid::Uuid::new_v4(), message)).unwrap();
        }
        input
    }

    fn run_messages(messages: Vec<WorkerRequest>) -> Vec<WorkerResponse> {
        let input = transport_input(messages);
        let capture = CapturedWriter::default();
        let output = capture.clone();
        let writer: SharedWriter = Arc::new(Mutex::new(Box::new(capture)));
        run_transport(
            &mut Cursor::new(input),
            writer,
            Some("correct-token".into()),
        )
        .unwrap();
        let bytes = output.0.lock().unwrap().clone();
        let mut reader = Cursor::new(bytes);
        let mut responses = Vec::new();
        while reader.position() < reader.get_ref().len() as u64 {
            responses.push(
                read_frame::<_, WorkerResponse>(&mut reader)
                    .unwrap()
                    .message,
            );
        }
        responses
    }

    #[test]
    fn authenticated_transport_accepts_shutdown() {
        let responses = run_messages(vec![
            WorkerRequest::Hello {
                token: "correct-token".into(),
            },
            WorkerRequest::Shutdown,
        ]);
        assert!(matches!(responses[0], WorkerResponse::HelloAck));
        assert!(matches!(responses[1], WorkerResponse::Accepted));
    }

    #[test]
    fn wrong_and_repeated_handshakes_are_rejected() {
        let responses = run_messages(vec![
            WorkerRequest::Hello {
                token: "wrong-token".into(),
            },
            WorkerRequest::Hello {
                token: "correct-token".into(),
            },
            WorkerRequest::Hello {
                token: "correct-token".into(),
            },
            WorkerRequest::Shutdown,
        ]);
        assert!(matches!(responses[0], WorkerResponse::Error { .. }));
        assert!(matches!(responses[1], WorkerResponse::HelloAck));
        assert!(matches!(responses[2], WorkerResponse::Error { .. }));
        assert!(matches!(responses[3], WorkerResponse::Accepted));
    }
}
