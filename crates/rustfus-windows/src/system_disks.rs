//! Decides which physical disks hold the running Windows installation. The queries live in
//! `platform.rs`; this module only combines their results, and it fails closed: whenever a fact
//! cannot be established the answer is an `UnsafeTarget` error rather than a guess.

use rustfus_core::{ErrorCode, OperationError};
use std::collections::HashSet;

/// Disks that must never be offered as a write target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemDisks {
    /// Disks holding the Windows boot volume (`%SystemDrive%`).
    pub boot: HashSet<u32>,
    /// Disks holding the system partition (ESP or active partition).
    pub system: HashSet<u32>,
}

/// Raw results of the individual queries; `Err` carries a human-readable reason.
pub struct Inputs {
    pub boot: Result<HashSet<u32>, String>,
    pub registry_partition: Result<HashSet<u32>, String>,
    pub kernel_partition: Result<HashSet<u32>, String>,
    pub native_vhd_boot: Result<bool, String>,
}

fn unsafe_target(message: String) -> OperationError {
    OperationError::new(ErrorCode::UnsafeTarget, message)
}

/// Combines the query results into the set of protected disks.
///
/// # Errors
///
/// `UnsafeTarget` when Windows runs from a native-boot VHD, or when the boot disk or the system
/// partition disk cannot be determined from any source.
pub fn combine(inputs: Inputs) -> Result<SystemDisks, OperationError> {
    match inputs.native_vhd_boot {
        Ok(false) => {}
        Ok(true) => {
            return Err(unsafe_target(
                "Windows is running from a native-boot VHD; USB discovery was stopped".into(),
            ));
        }
        Err(reason) => {
            return Err(unsafe_target(format!(
                "Could not determine whether Windows booted from a VHD: {reason}; USB discovery was stopped"
            )));
        }
    }

    let boot = inputs.boot.map_err(|reason| {
        unsafe_target(format!(
            "Could not identify the Windows boot disk: {reason}; USB discovery was stopped"
        ))
    })?;
    if boot.is_empty() {
        return Err(unsafe_target(
            "The Windows boot volume reported no disks; USB discovery was stopped".into(),
        ));
    }

    // Two independent sources for the system partition. Disagreement keeps both answers (the
    // stricter reading); one failing source is tolerated, both failing is not.
    let system = match (inputs.registry_partition, inputs.kernel_partition) {
        (Ok(registry), Ok(kernel)) => registry.union(&kernel).copied().collect(),
        (Ok(only), Err(_)) | (Err(_), Ok(only)) => only,
        (Err(registry), Err(kernel)) => {
            return Err(unsafe_target(format!(
                "Could not identify the system partition disk (registry: {registry}; kernel: {kernel}); USB discovery was stopped"
            )));
        }
    };
    if system.is_empty() {
        return Err(unsafe_target(
            "The system partition reported no disks; USB discovery was stopped".into(),
        ));
    }

    Ok(SystemDisks { boot, system })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(disks: &[u32]) -> HashSet<u32> {
        disks.iter().copied().collect()
    }

    fn healthy() -> Inputs {
        Inputs {
            boot: Ok(set(&[0])),
            registry_partition: Ok(set(&[0])),
            kernel_partition: Ok(set(&[0])),
            native_vhd_boot: Ok(false),
        }
    }

    fn rejected(inputs: Inputs) -> OperationError {
        combine(inputs).unwrap_err()
    }

    #[test]
    fn agreeing_sources_protect_the_reported_disks() {
        let disks = combine(healthy()).unwrap();
        assert_eq!(disks.boot, set(&[0]));
        assert_eq!(disks.system, set(&[0]));
    }

    #[test]
    fn boot_and_system_may_live_on_different_disks() {
        let disks = combine(Inputs {
            boot: Ok(set(&[2])),
            registry_partition: Ok(set(&[0])),
            kernel_partition: Ok(set(&[0])),
            ..healthy()
        })
        .unwrap();
        assert_eq!((disks.boot, disks.system), (set(&[2]), set(&[0])));
    }

    #[test]
    fn disagreeing_partition_sources_are_united() {
        let disks = combine(Inputs {
            registry_partition: Ok(set(&[0])),
            kernel_partition: Ok(set(&[1])),
            ..healthy()
        })
        .unwrap();
        assert_eq!(disks.system, set(&[0, 1]));
    }

    #[test]
    fn one_failing_partition_source_is_tolerated() {
        for (registry, kernel) in [
            (Err("missing".to_owned()), Ok(set(&[3]))),
            (Ok(set(&[3])), Err("denied".to_owned())),
        ] {
            let disks = combine(Inputs {
                registry_partition: registry,
                kernel_partition: kernel,
                ..healthy()
            })
            .unwrap();
            assert_eq!(disks.system, set(&[3]));
        }
    }

    #[test]
    fn both_partition_sources_failing_stops_discovery() {
        let error = rejected(Inputs {
            registry_partition: Err("a".into()),
            kernel_partition: Err("b".into()),
            ..healthy()
        });
        assert_eq!(error.code, ErrorCode::UnsafeTarget);
        assert!(error.message.contains("registry: a") && error.message.contains("kernel: b"));
    }

    #[test]
    fn unknown_boot_disk_stops_discovery() {
        for boot in [Err("no volume".to_owned()), Ok(HashSet::new())] {
            let error = rejected(Inputs { boot, ..healthy() });
            assert_eq!(error.code, ErrorCode::UnsafeTarget);
        }
    }

    #[test]
    fn empty_system_partition_answer_stops_discovery() {
        let error = rejected(Inputs {
            registry_partition: Ok(HashSet::new()),
            kernel_partition: Ok(HashSet::new()),
            ..healthy()
        });
        assert_eq!(error.code, ErrorCode::UnsafeTarget);
    }

    #[test]
    fn native_vhd_boot_or_an_unanswerable_check_stops_discovery() {
        for vhd in [Ok(true), Err("api missing".to_owned())] {
            let error = rejected(Inputs {
                native_vhd_boot: vhd,
                ..healthy()
            });
            assert_eq!(error.code, ErrorCode::UnsafeTarget);
        }
    }
}
