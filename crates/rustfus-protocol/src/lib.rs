//! Versioned, length-delimited messages for the UI/worker privilege boundary.

use rustfus_core::{
    DeviceSnapshot, Fingerprint, ImageAnalysis, OperationError, OperationEvent, OperationPlan,
    PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use uuid::Uuid;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub version: u16,
    pub request_id: Uuid,
    pub message: T,
}

impl<T> Envelope<T> {
    #[must_use]
    pub fn new(request_id: Uuid, message: T) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum WorkerRequest {
    Hello {
        token: String,
    },
    EnumerateDevices,
    AnalyzeImage {
        path: PathBuf,
    },
    Prepare {
        device_fingerprint: Fingerprint,
        image: ImageAnalysis,
        volume_label: String,
        verify: bool,
    },
    Start {
        plan_id: Uuid,
        confirmation: String,
    },
    Cancel {
        plan_id: Uuid,
    },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum WorkerResponse {
    HelloAck,
    Devices {
        devices: Vec<DeviceSnapshot>,
    },
    Analysis {
        analysis: ImageAnalysis,
    },
    Plan {
        plan: OperationPlan,
    },
    Event {
        plan_id: Uuid,
        event: OperationEvent,
    },
    Error {
        error: OperationError,
    },
    Accepted,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid JSON frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame length {0} exceeds the {MAX_FRAME_BYTES}-byte limit")]
    FrameTooLarge(usize),
    #[error("protocol version {received} is not supported; expected {expected}")]
    VersionMismatch { expected: u16, received: u16 },
}

pub fn write_frame<W: Write + ?Sized, T: Serialize>(
    writer: &mut W,
    envelope: &Envelope<T>,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_vec(envelope)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(payload.len()));
    }
    let length =
        u32::try_from(payload.len()).map_err(|_| ProtocolError::FrameTooLarge(payload.len()))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read + ?Sized, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<Envelope<T>, ProtocolError> {
    let mut length_bytes = [0_u8; 4];
    reader.read_exact(&mut length_bytes)?;
    let length = u32::from_le_bytes(length_bytes) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(length));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    let envelope: Envelope<T> = serde_json::from_slice(&payload)?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            received: envelope.version,
        });
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip_is_length_delimited() {
        let request_id = Uuid::new_v4();
        let envelope = Envelope::new(
            request_id,
            WorkerRequest::Hello {
                token: "secret".into(),
            },
        );
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &envelope).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );

        let decoded: Envelope<WorkerRequest> = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let oversized = u32::try_from(MAX_FRAME_BYTES).unwrap() + 1;
        let mut bytes = oversized.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        assert!(matches!(
            read_frame::<_, WorkerRequest>(&mut bytes.as_slice()),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn mismatched_version_is_rejected() {
        let mut envelope = Envelope::new(Uuid::new_v4(), WorkerRequest::Shutdown);
        envelope.version += 1;
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &envelope).unwrap();
        assert!(matches!(
            read_frame::<_, WorkerRequest>(&mut bytes.as_slice()),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }
}
