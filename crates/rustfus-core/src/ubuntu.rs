//! Validated `NoCloud` seed data for Ubuntu Subiquity autoinstall.

use crate::{ErrorCode, OperationError};
use serde::{Deserialize, Serialize};

const MAX_SEED_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UbuntuAutoinstallConfig {
    pub user_data: String,
    #[serde(default)]
    pub meta_data: String,
}

impl UbuntuAutoinstallConfig {
    pub fn validate(&self) -> Result<(), OperationError> {
        let combined = self.user_data.len().saturating_add(self.meta_data.len());
        if self.user_data.is_empty()
            || combined > MAX_SEED_BYTES
            || self.user_data.contains('\0')
            || self.meta_data.contains('\0')
        {
            return Err(invalid(
                "Ubuntu user-data and meta-data must be UTF-8 text totaling no more than 2 MiB",
            ));
        }
        let normalized = self.user_data.trim_start_matches('\u{feff}');
        let has_top_level_autoinstall = normalized
            .lines()
            .any(|line| line.len() == line.trim_start().len() && line.trim_end() == "autoinstall:");
        if normalized
            .lines()
            .next()
            .is_none_or(|line| line.trim() != "#cloud-config")
            || !has_top_level_autoinstall
        {
            return Err(invalid(
                "Ubuntu user-data must start with #cloud-config and contain a top-level autoinstall: key",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn bytes_to_write(&self) -> u64 {
        self.user_data.len().saturating_add(self.meta_data.len()) as u64
    }
}

fn invalid(message: impl Into<String>) -> OperationError {
    OperationError::new(ErrorCode::InvalidAutoinstallConfig, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_cloud_config_is_accepted_without_interpreting_advanced_options() {
        let config = UbuntuAutoinstallConfig {
            user_data:
                "#cloud-config\nautoinstall:\n  version: 1\n  late-commands:\n    - echo ok\n"
                    .into(),
            meta_data: "instance-id: rustfus\n".into(),
        };
        assert!(config.validate().is_ok());
        assert_eq!(
            config.bytes_to_write(),
            (config.user_data.len() + config.meta_data.len()) as u64
        );
    }

    #[test]
    fn ordinary_yaml_is_not_mistaken_for_nocloud_user_data() {
        for data in [
            "autoinstall:\n  version: 1\n",
            "#cloud-config\nusers: []\n",
            "#cloud-config\nwrapper:\n  autoinstall:\n    version: 1\n",
            "",
        ] {
            let config = UbuntuAutoinstallConfig {
                user_data: data.into(),
                meta_data: String::new(),
            };
            assert_eq!(
                config.validate().unwrap_err().code,
                ErrorCode::InvalidAutoinstallConfig
            );
        }
    }
}
