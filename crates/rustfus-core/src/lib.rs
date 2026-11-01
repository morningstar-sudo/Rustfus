//! Shared domain model and fail-closed operation planning.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::PathBuf;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;
pub const CONFIRMATION_PHRASE: &str = "ERASE";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct DeviceSnapshot {
    pub instance_id: String,
    pub serial: Option<String>,
    pub model: String,
    pub bus: BusType,
    pub disk_number: u32,
    pub capacity_bytes: u64,
    pub logical_sector_size: u32,
    pub physical_sector_size: u32,
    pub volumes: Vec<String>,
    pub contains_system: bool,
    pub contains_boot: bool,
    pub contains_pagefile: bool,
    pub contains_crashdump: bool,
}

impl DeviceSnapshot {
    #[must_use]
    pub fn is_eligible_usb_target(&self) -> bool {
        self.bus == BusType::Usb
            && !self.contains_system
            && !self.contains_boot
            && !self.contains_pagefile
            && !self.contains_crashdump
            && self.capacity_bytes > 0
            && self.logical_sector_size.is_power_of_two()
            && self.physical_sector_size.is_power_of_two()
    }

    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut hasher = Sha256::new();
        update_field(&mut hasher, self.instance_id.as_bytes());
        update_field(&mut hasher, self.serial.as_deref().unwrap_or("").as_bytes());
        update_field(&mut hasher, self.model.as_bytes());
        hasher.update(self.disk_number.to_le_bytes());
        hasher.update(self.capacity_bytes.to_le_bytes());
        hasher.update(self.logical_sector_size.to_le_bytes());
        hasher.update(self.physical_sector_size.to_le_bytes());
        Fingerprint(hasher.finalize().into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusType {
    Usb,
    Virtual,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    WindowsInstaller,
    HybridDisk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAnalysis {
    pub path: PathBuf,
    pub kind: ImageKind,
    pub architecture: Architecture,
    pub size_bytes: u64,
    pub fingerprint: Fingerprint,
    pub required_bytes: u64,
    pub temporary_bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    WindowsMbrFat32,
    HybridRaw,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAction {
    pub destructive: bool,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPlan {
    pub id: Uuid,
    pub device_fingerprint: Fingerprint,
    pub image_fingerprint: Fingerprint,
    pub disk_number: u32,
    pub mode: WriteMode,
    pub source: PathBuf,
    pub volume_label: String,
    pub verify: bool,
    pub bytes_to_write: u64,
    pub temporary_bytes: u64,
    pub actions: Vec<PlannedAction>,
}

impl OperationPlan {
    /// Creates a destructive plan from immutable device and image snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsafe, too small, or the FAT label
    /// is invalid.
    pub fn create(
        device: &DeviceSnapshot,
        image: &ImageAnalysis,
        volume_label: &str,
        verify: bool,
    ) -> Result<Self, OperationError> {
        if !device.is_eligible_usb_target() {
            return Err(OperationError::new(
                ErrorCode::UnsafeTarget,
                "The selected device is not an eligible USB target",
            ));
        }
        if device.capacity_bytes < image.required_bytes {
            return Err(OperationError::new(
                ErrorCode::InsufficientCapacity,
                "The selected device is smaller than the required media size",
            ));
        }
        validate_volume_label(volume_label)?;

        let mode = match image.kind {
            ImageKind::WindowsInstaller => WriteMode::WindowsMbrFat32,
            ImageKind::HybridDisk => WriteMode::HybridRaw,
        };
        let actions = match mode {
            WriteMode::WindowsMbrFat32 => vec![
                destructive("Erase the selected USB disk"),
                destructive("Create an active MBR/FAT32 partition"),
                destructive("Install Microsoft boot code from the source ISO"),
                destructive("Copy and verify Windows Setup files"),
            ],
            WriteMode::HybridRaw => vec![
                destructive("Erase the selected USB disk"),
                destructive("Write the hybrid image byte-for-byte"),
                destructive("Read back and verify the image"),
            ],
        };

        Ok(Self {
            id: Uuid::new_v4(),
            device_fingerprint: device.fingerprint(),
            image_fingerprint: image.fingerprint,
            disk_number: device.disk_number,
            mode,
            source: image.path.clone(),
            volume_label: volume_label.to_owned(),
            verify,
            bytes_to_write: image.required_bytes,
            temporary_bytes: image.temporary_bytes,
            actions,
        })
    }

    /// Confirms that the selected media and source still match this plan.
    ///
    /// # Errors
    ///
    /// Returns an error for an incorrect confirmation phrase or changed device
    /// or image identity.
    pub fn revalidate(
        &self,
        current_device: &DeviceSnapshot,
        current_image: &ImageAnalysis,
        confirmation: &str,
    ) -> Result<(), OperationError> {
        if confirmation != CONFIRMATION_PHRASE {
            return Err(OperationError::new(
                ErrorCode::ConfirmationRequired,
                "The destructive confirmation phrase is incorrect",
            ));
        }
        if !current_device.is_eligible_usb_target()
            || current_device.disk_number != self.disk_number
            || current_device.fingerprint() != self.device_fingerprint
        {
            return Err(OperationError::new(
                ErrorCode::DeviceChanged,
                "The selected device changed after the plan was created",
            ));
        }
        if current_image.fingerprint != self.image_fingerprint {
            return Err(OperationError::new(
                ErrorCode::ImageChanged,
                "The source image changed after the plan was created",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint(pub [u8; 32]);

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Fingerprint")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    Analyze,
    Revalidate,
    Lock,
    Prepare,
    Write,
    Verify,
    Finalize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OperationEvent {
    PhaseChanged {
        phase: OperationPhase,
    },
    Progress {
        phase: OperationPhase,
        completed_bytes: u64,
        total_bytes: u64,
        bytes_per_second: f64,
    },
    Log {
        message: String,
    },
    Completed,
    Cancelled {
        media_may_be_incomplete: bool,
    },
    Failed {
        error: OperationError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AccessDenied,
    Cancelled,
    ConfirmationRequired,
    DeviceChanged,
    ImageChanged,
    InsufficientCapacity,
    InvalidImage,
    InvalidVolumeLabel,
    LockFailed,
    SignatureInvalid,
    UnsafeTarget,
    UnsupportedArchitecture,
    UnsupportedLargeFile,
    UnsupportedPlatform,
    VerificationFailed,
    Io,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct OperationError {
    pub code: ErrorCode,
    pub message: String,
    pub platform_code: Option<u32>,
}

impl OperationError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            platform_code: None,
        }
    }

    #[must_use]
    pub fn with_platform_code(mut self, platform_code: u32) -> Self {
        self.platform_code = Some(platform_code);
        self
    }
}

/// Validates the restricted FAT32 label accepted by the v0.1 UI.
///
/// # Errors
///
/// Returns `InvalidVolumeLabel` unless the label contains 1-11 uppercase ASCII
/// letters, digits, or spaces.
pub fn validate_volume_label(label: &str) -> Result<(), OperationError> {
    let valid = !label.is_empty()
        && label.len() <= 11
        && label
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ');
    if valid {
        Ok(())
    } else {
        Err(OperationError::new(
            ErrorCode::InvalidVolumeLabel,
            "The FAT32 volume label must contain 1-11 uppercase ASCII letters, digits, or spaces",
        ))
    }
}

fn update_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn destructive(description: &str) -> PlannedAction {
    PlannedAction {
        destructive: true,
        description: description.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb() -> DeviceSnapshot {
        DeviceSnapshot {
            instance_id: "USB\\VID_1234&PID_5678".into(),
            serial: Some("ABC123".into()),
            model: "Test USB".into(),
            bus: BusType::Usb,
            disk_number: 4,
            capacity_bytes: 32 * 1024 * 1024 * 1024,
            logical_sector_size: 512,
            physical_sector_size: 4096,
            volumes: vec!["E:".into()],
            contains_system: false,
            contains_boot: false,
            contains_pagefile: false,
            contains_crashdump: false,
        }
    }

    fn image() -> ImageAnalysis {
        ImageAnalysis {
            path: "test.iso".into(),
            kind: ImageKind::HybridDisk,
            architecture: Architecture::X86_64,
            size_bytes: 1024,
            fingerprint: Fingerprint([7; 32]),
            required_bytes: 1024,
            temporary_bytes: 0,
            warnings: vec![],
        }
    }

    #[test]
    fn device_fingerprint_changes_with_identity() {
        let first = usb();
        let mut second = first.clone();
        second.serial = Some("DIFFERENT".into());
        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn plan_requires_exact_confirmation_and_fresh_device() {
        let device = usb();
        let image = image();
        let plan = OperationPlan::create(&device, &image, "RUSTFUS", true).unwrap();
        assert_eq!(
            plan.revalidate(&device, &image, "erase").unwrap_err().code,
            ErrorCode::ConfirmationRequired
        );
        assert!(plan.revalidate(&device, &image, "ERASE").is_ok());

        let mut changed = device.clone();
        changed.capacity_bytes += 512;
        assert_eq!(
            plan.revalidate(&changed, &image, "ERASE").unwrap_err().code,
            ErrorCode::DeviceChanged
        );
    }

    #[test]
    fn system_disk_is_never_eligible() {
        let mut device = usb();
        device.contains_system = true;
        assert!(!device.is_eligible_usb_target());
    }

    #[test]
    fn volume_label_is_strict() {
        assert!(validate_volume_label("RUSTFUS").is_ok());
        assert!(validate_volume_label("rustfus").is_err());
        assert!(validate_volume_label("TOO-LONG-LABEL").is_err());
    }
}
