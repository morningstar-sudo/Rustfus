//! Remembers what the UI was last shown so that it can refer to devices and images by opaque id
//! instead of supplying (and being able to forge) safety-relevant data.

use rustfus_core::{DeviceSnapshot, Fingerprint, ImageAnalysis};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use crate::json;

#[derive(Default)]
pub struct Registry {
    devices: HashMap<String, Fingerprint>,
    images: HashMap<String, ImageAnalysis>,
}

impl Registry {
    /// Replaces the known devices with the latest enumeration so stale ids stop resolving.
    pub fn replace_devices(&mut self, devices: &[DeviceSnapshot]) {
        self.devices = devices
            .iter()
            .map(|device| (json::device_id(device), device.fingerprint()))
            .collect();
    }

    /// Remembers an analyzed image and returns its id. Only the latest analysis is kept.
    pub fn remember_image(&mut self, image: ImageAnalysis) -> String {
        let id = json::image_id(&image);
        self.images.clear();
        self.images.insert(id.clone(), image);
        id
    }

    #[must_use]
    pub fn device(&self, id: &str) -> Option<Fingerprint> {
        self.devices.get(id).copied()
    }

    #[must_use]
    pub fn image(&self, id: &str) -> Option<ImageAnalysis> {
        self.images.get(id).cloned()
    }
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

pub fn global() -> MutexGuard<'static, Registry> {
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfus_core::{Architecture, BusType, ImageKind};
    use std::path::PathBuf;

    fn device(disk_number: u32) -> DeviceSnapshot {
        DeviceSnapshot {
            instance_id: format!("USB\\{disk_number}"),
            serial: None,
            model: "Stick".into(),
            bus: BusType::Usb,
            disk_number,
            capacity_bytes: 1 << 33,
            logical_sector_size: 512,
            physical_sector_size: 512,
            volumes: Vec::new(),
            contains_system: false,
            contains_boot: false,
            contains_pagefile: false,
            contains_crashdump: false,
        }
    }

    fn image(seed: u8) -> ImageAnalysis {
        ImageAnalysis {
            path: PathBuf::from("x.iso"),
            kind: ImageKind::HybridDisk,
            architecture: Architecture::X86_64,
            size_bytes: 1,
            fingerprint: Fingerprint([seed; 32]),
            required_bytes: 1,
            temporary_bytes: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn devices_resolve_until_the_next_enumeration_replaces_them() {
        let mut registry = Registry::default();
        let first = device(1);
        registry.replace_devices(std::slice::from_ref(&first));
        assert_eq!(
            registry.device(&json::device_id(&first)),
            Some(first.fingerprint())
        );
        registry.replace_devices(&[device(2)]);
        assert_eq!(registry.device(&json::device_id(&first)), None);
        assert_eq!(registry.device("not-an-id"), None);
    }

    #[test]
    fn only_the_latest_image_is_remembered() {
        let mut registry = Registry::default();
        let first = registry.remember_image(image(1));
        let second = registry.remember_image(image(2));
        assert!(registry.image(&first).is_none());
        assert_eq!(registry.image(&second), Some(image(2)));
    }
}
