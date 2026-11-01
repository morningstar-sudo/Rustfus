//! Flat JSON shapes handed to the UI. Safety-relevant data (fingerprints, snapshots) never leaves
//! Rust: the UI only sees opaque ids and display fields.

use rustfus_core::{DeviceSnapshot, ImageAnalysis, OperationError, OperationEvent, OperationPlan};
use serde_json::{Value, json};

#[must_use]
pub fn device_id(device: &DeviceSnapshot) -> String {
    device.fingerprint().to_string()
}

#[must_use]
pub fn image_id(image: &ImageAnalysis) -> String {
    image.fingerprint.to_string()
}

#[must_use]
pub fn devices(devices: &[DeviceSnapshot]) -> Value {
    let devices: Vec<Value> = devices
        .iter()
        .map(|device| {
            json!({
                "id": device_id(device),
                "model": device.model,
                "disk_number": device.disk_number,
                "capacity_bytes": device.capacity_bytes,
                "volumes": device.volumes,
                "eligible": device.is_eligible_usb_target(),
            })
        })
        .collect();
    json!({ "devices": devices })
}

#[must_use]
pub fn image(image: &ImageAnalysis) -> Value {
    json!({
        "id": image_id(image),
        "path": image.path.to_string_lossy(),
        "kind": serde_json::to_value(image.kind).unwrap_or(Value::Null),
        "architecture": serde_json::to_value(image.architecture).unwrap_or(Value::Null),
        "size_bytes": image.size_bytes,
        "required_bytes": image.required_bytes,
        "temporary_bytes": image.temporary_bytes,
        "warnings": image.warnings,
    })
}

#[must_use]
pub fn plan(plan: &OperationPlan) -> Value {
    json!({
        "id": plan.id.to_string(),
        "mode": serde_json::to_value(plan.mode).unwrap_or(Value::Null),
        "bytes_to_write": plan.bytes_to_write,
        "temporary_bytes": plan.temporary_bytes,
        "actions": serde_json::to_value(&plan.actions).unwrap_or(Value::Null),
    })
}

#[must_use]
pub fn event(event: &OperationEvent) -> Value {
    let terminal = matches!(
        event,
        OperationEvent::Completed
            | OperationEvent::Cancelled { .. }
            | OperationEvent::Failed { .. }
    );
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("terminal".into(), Value::Bool(terminal));
    }
    value
}

#[must_use]
pub fn error(error: &OperationError) -> Value {
    serde_json::to_value(error).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfus_core::{
        Architecture, BusType, ErrorCode, Fingerprint, ImageKind, OperationPhase, WriteMode,
    };
    use std::path::PathBuf;
    use uuid::Uuid;

    fn device() -> DeviceSnapshot {
        DeviceSnapshot {
            instance_id: "USB\\VID_1&PID_2\\3".into(),
            serial: Some("S".into()),
            model: "Test Stick".into(),
            bus: BusType::Usb,
            disk_number: 4,
            capacity_bytes: 32_000_000_000,
            logical_sector_size: 512,
            physical_sector_size: 4096,
            volumes: vec!["E:".into()],
            contains_system: false,
            contains_boot: false,
            contains_pagefile: false,
            contains_crashdump: false,
        }
    }

    #[test]
    fn device_json_exposes_only_display_fields_and_an_opaque_id() {
        let value = devices(&[device()]);
        let entry = &value["devices"][0];
        assert_eq!(entry["id"], device().fingerprint().to_string());
        assert_eq!(entry["model"], "Test Stick");
        assert_eq!(entry["disk_number"], 4);
        assert_eq!(entry["eligible"], true);
        assert!(entry.get("instance_id").is_none());
        assert!(entry.get("serial").is_none());
    }

    #[test]
    fn image_and_plan_json_use_snake_case_enums() {
        let analysis = ImageAnalysis {
            path: PathBuf::from("D:/x.iso"),
            kind: ImageKind::WindowsInstaller,
            architecture: Architecture::X86_64,
            size_bytes: 10,
            fingerprint: Fingerprint([7; 32]),
            required_bytes: 20,
            temporary_bytes: 5,
            warnings: vec!["w".into()],
        };
        let value = image(&analysis);
        assert_eq!(value["kind"], "windows_installer");
        assert_eq!(value["id"], Fingerprint([7; 32]).to_string());

        let plan_value = plan(&OperationPlan {
            id: Uuid::nil(),
            device_fingerprint: Fingerprint([1; 32]),
            image_fingerprint: Fingerprint([7; 32]),
            disk_number: 4,
            mode: WriteMode::HybridRaw,
            source: PathBuf::from("D:/x.iso"),
            volume_label: "RUSTFUS".into(),
            verify: true,
            bytes_to_write: 20,
            temporary_bytes: 0,
            actions: Vec::new(),
        });
        assert_eq!(plan_value["mode"], "hybrid_raw");
        assert!(plan_value.get("device_fingerprint").is_none());
    }

    #[test]
    fn event_json_marks_terminal_events() {
        let progress = event(&OperationEvent::Progress {
            phase: OperationPhase::Write,
            completed_bytes: 1,
            total_bytes: 2,
            bytes_per_second: 3.0,
        });
        assert_eq!(progress["event"], "progress");
        assert_eq!(progress["phase"], "write");
        assert_eq!(progress["terminal"], false);
        assert_eq!(event(&OperationEvent::Completed)["terminal"], true);
        let failed = event(&OperationEvent::Failed {
            error: OperationError::new(ErrorCode::Io, "boom"),
        });
        assert_eq!(failed["terminal"], true);
        assert_eq!(failed["error"]["code"], "io");
    }
}
