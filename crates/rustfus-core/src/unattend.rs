//! Small, deterministic `autounattend.xml` generator for the options exposed by Rustfus.
//!
//! The shape follows Microsoft's unattended-setup component schema.  The feature set is
//! intentionally narrow: Rustfus does not accept arbitrary commands or XML from the unelevated UI.

use crate::{ErrorCode, OperationError};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(default)]
pub struct UnattendConfig {
    /// Complete XML imported from Schneegans' full generator. When present, the guided fields
    /// below are deliberately ignored so new upstream options work without a Rustfus update.
    pub custom_xml: Option<String>,
    pub locale: String,
    pub time_zone: String,
    pub local_account: Option<String>,
    pub local_account_password: String,
    pub auto_logon: bool,
    pub computer_name: Option<String>,
    pub bypass_hardware_checks: bool,
    pub bypass_online_account: bool,
    pub skip_privacy_questions: bool,
}

impl UnattendConfig {
    pub fn validate(&self) -> Result<(), OperationError> {
        if let Some(xml) = &self.custom_xml {
            return validate_custom_xml(xml);
        }
        validate_locale(&self.locale)?;
        validate_time_zone(&self.time_zone)?;
        if let Some(name) = &self.local_account {
            validate_account_name(name)?;
        } else if self.auto_logon {
            return Err(invalid("Automatic logon requires a local account"));
        }
        if self.local_account_password.chars().count() > 127
            || self.local_account_password.chars().any(char::is_control)
        {
            return Err(invalid("The unattended local-account password is invalid"));
        }
        if let Some(name) = &self.computer_name {
            validate_computer_name(name)?;
        }
        Ok(())
    }

    pub fn to_xml_bytes(&self) -> Result<Vec<u8>, OperationError> {
        self.validate()?;
        if let Some(xml) = &self.custom_xml {
            return Ok(xml.as_bytes().to_vec());
        }
        let locale = escape_xml(&self.locale);
        let time_zone = escape_xml(&self.time_zone);
        let account = self.local_account.as_deref().map(escape_xml);
        let password = escape_xml(&self.local_account_password);
        let mut xml = String::from(
            r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
"#,
        );

        if self.bypass_hardware_checks {
            xml.push_str(
                r#"  <settings pass="windowsPE">
    <component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add"><Order>1</Order><Path>reg.exe add "HKLM\SYSTEM\Setup\LabConfig" /v BypassTPMCheck /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add"><Order>2</Order><Path>reg.exe add "HKLM\SYSTEM\Setup\LabConfig" /v BypassSecureBootCheck /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add"><Order>3</Order><Path>reg.exe add "HKLM\SYSTEM\Setup\LabConfig" /v BypassRAMCheck /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
      </RunSynchronous>
    </component>
  </settings>
"#,
            );
        }

        if self.bypass_online_account {
            xml.push_str(
                r#"  <settings pass="specialize">
    <component name="Microsoft-Windows-Deployment" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add"><Order>1</Order><Path>reg.exe add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\OOBE" /v BypassNRO /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
      </RunSynchronous>
    </component>
  </settings>
"#,
            );
        }

        write!(
            xml,
            r#"  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-International-Core" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <InputLocale>{locale}</InputLocale>
      <SystemLocale>{locale}</SystemLocale>
      <UserLocale>{locale}</UserLocale>
    </component>
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <TimeZone>{time_zone}</TimeZone>
"#
        )
        .expect("writing to a String cannot fail");
        if let Some(computer_name) = &self.computer_name {
            writeln!(
                xml,
                "      <ComputerName>{}</ComputerName>",
                escape_xml(computer_name)
            )
            .expect("writing to a String cannot fail");
        }
        if let Some(account) = &account {
            write!(
                xml,
                r#"      <UserAccounts>
        <LocalAccounts>
          <LocalAccount wcm:action="add">
            <Name>{account}</Name>
            <DisplayName>{account}</DisplayName>
            <Group>Administrators</Group>
            <Password><Value>{password}</Value><PlainText>true</PlainText></Password>
          </LocalAccount>
        </LocalAccounts>
      </UserAccounts>
"#
            )
            .expect("writing to a String cannot fail");
            if self.auto_logon {
                write!(
                    xml,
                    r"      <AutoLogon>
        <Username>{account}</Username>
        <Enabled>true</Enabled>
        <LogonCount>1</LogonCount>
        <Password><Value>{password}</Value><PlainText>true</PlainText></Password>
      </AutoLogon>
"
                )
                .expect("writing to a String cannot fail");
            }
        }
        write!(
            xml,
            r"      <OOBE>
        <HideEULAPage>true</HideEULAPage>
        <HideOnlineAccountScreens>{hide_online}</HideOnlineAccountScreens>
        <HideWirelessSetupInOOBE>{hide_online}</HideWirelessSetupInOOBE>
",
            hide_online = self.bypass_online_account,
        )
        .expect("writing to a String cannot fail");
        if self.skip_privacy_questions {
            xml.push_str("        <ProtectYourPC>3</ProtectYourPC>\n");
        }
        xml.push_str(
            r"      </OOBE>
    </component>
  </settings>
</unattend>
",
        );
        Ok(xml.into_bytes())
    }
}

fn invalid(message: impl Into<String>) -> OperationError {
    OperationError::new(ErrorCode::InvalidUnattendConfig, message)
}

const MAX_CUSTOM_XML_BYTES: usize = 2 * 1024 * 1024;

fn validate_custom_xml(xml: &str) -> Result<(), OperationError> {
    if xml.is_empty() || xml.len() > MAX_CUSTOM_XML_BYTES || xml.contains('\0') {
        return Err(invalid(
            "The imported autounattend.xml must be valid UTF-8 and no larger than 2 MiB",
        ));
    }
    let normalized = xml.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let lower = normalized.to_ascii_lowercase();
    let root = lower
        .find("<unattend")
        .ok_or_else(|| invalid("The imported file does not contain an unattend document root"))?;
    let root_end = lower[root..]
        .find('>')
        .map(|offset| root + offset)
        .ok_or_else(|| invalid("The imported unattend document root is incomplete"))?;
    if !lower[root..=root_end].contains("urn:schemas-microsoft-com:unattend")
        || !lower.contains("</unattend>")
    {
        return Err(invalid(
            "The imported file is not a Microsoft autounattend.xml document",
        ));
    }
    Ok(())
}

fn validate_locale(locale: &str) -> Result<(), OperationError> {
    let valid = (2..=35).contains(&locale.len())
        && locale
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !locale.starts_with('-')
        && !locale.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(invalid("The unattended locale is invalid"))
    }
}

fn validate_time_zone(time_zone: &str) -> Result<(), OperationError> {
    let valid = !time_zone.trim().is_empty()
        && time_zone.len() <= 128
        && !time_zone.chars().any(char::is_control);
    if valid {
        Ok(())
    } else {
        Err(invalid("The unattended Windows time-zone name is invalid"))
    }
}

fn validate_account_name(name: &str) -> Result<(), OperationError> {
    const RESERVED: [&str; 8] = [
        "administrator",
        "guest",
        "defaultaccount",
        "system",
        "network service",
        "local service",
        "none",
        "wdagutilityaccount",
    ];
    let forbidden = [
        '/', '\\', '[', ']', ':', ';', '|', '=', ',', '+', '*', '?', '<', '>', '"', '%',
    ];
    let valid = !name.trim().is_empty()
        && name == name.trim()
        && name.chars().count() <= 20
        && !name.ends_with('.')
        && !name
            .chars()
            .any(|character| character.is_control() || forbidden.contains(&character))
        && !RESERVED
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved));
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "The unattended local-account name is not accepted by Windows",
        ))
    }
}

fn validate_computer_name(name: &str) -> Result<(), OperationError> {
    let valid = !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.bytes().all(|byte| byte.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "The unattended computer name must be 1-15 letters, digits or hyphens",
        ))
    }
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> UnattendConfig {
        UnattendConfig {
            custom_xml: None,
            locale: "vi-VN".into(),
            time_zone: "SE Asia Standard Time".into(),
            local_account: Some("Rustfus User".into()),
            local_account_password: String::new(),
            auto_logon: false,
            computer_name: Some("RUSTFUS-PC".into()),
            bypass_hardware_checks: true,
            bypass_online_account: true,
            skip_privacy_questions: true,
        }
    }

    #[test]
    fn generated_xml_contains_only_requested_automation() {
        let xml = String::from_utf8(config().to_xml_bytes().unwrap()).unwrap();
        assert!(xml.contains("BypassTPMCheck"));
        assert!(xml.contains("BypassNRO"));
        assert!(xml.contains("<InputLocale>vi-VN</InputLocale>"));
        assert!(xml.contains("<TimeZone>SE Asia Standard Time</TimeZone>"));
        assert!(xml.contains("<Name>Rustfus User</Name>"));
        assert!(xml.contains("<ProtectYourPC>3</ProtectYourPC>"));
    }

    #[test]
    fn optional_bypasses_are_really_omitted() {
        let mut value = config();
        value.bypass_hardware_checks = false;
        value.bypass_online_account = false;
        value.skip_privacy_questions = false;
        let xml = String::from_utf8(value.to_xml_bytes().unwrap()).unwrap();
        assert!(!xml.contains("LabConfig"));
        assert!(!xml.contains("BypassNRO"));
        assert!(!xml.contains("ProtectYourPC"));
        assert!(xml.contains("<HideOnlineAccountScreens>false</HideOnlineAccountScreens>"));
    }

    #[test]
    fn account_names_are_escaped_and_dangerous_names_are_rejected() {
        let mut value = config();
        value.local_account = Some("A & B".into());
        value.local_account_password = "p<&>\"'".into();
        value.auto_logon = true;
        let xml = String::from_utf8(value.to_xml_bytes().unwrap()).unwrap();
        assert!(xml.contains("<Name>A &amp; B</Name>"));
        assert!(xml.contains("<Value>p&lt;&amp;&gt;&quot;&apos;</Value>"));
        assert!(xml.contains("<AutoLogon>"));

        for invalid_name in ["Administrator", "bad/name", "trailing.", ""] {
            value.local_account = Some(invalid_name.into());
            assert_eq!(
                value.to_xml_bytes().unwrap_err().code,
                ErrorCode::InvalidUnattendConfig
            );
        }
    }

    #[test]
    fn account_and_computer_name_are_optional_user_choices() {
        let mut value = config();
        value.local_account = None;
        value.computer_name = None;
        let xml = String::from_utf8(value.to_xml_bytes().unwrap()).unwrap();
        assert!(!xml.contains("<UserAccounts>"));
        assert!(!xml.contains("<ComputerName>"));

        value.auto_logon = true;
        assert_eq!(
            value.to_xml_bytes().unwrap_err().code,
            ErrorCode::InvalidUnattendConfig
        );
    }

    #[test]
    fn complete_schneegans_xml_is_preserved_without_losing_extensions() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <Extensions xmlns="https://schneegans.de/windows/unattend-generator/">
    <File path="C:\Windows\Setup\Scripts\specialize.ps1">custom</File>
  </Extensions>
</unattend>
"#;
        let config = UnattendConfig {
            custom_xml: Some(xml.into()),
            ..UnattendConfig::default()
        };
        assert_eq!(config.to_xml_bytes().unwrap(), xml.as_bytes());
    }

    #[test]
    fn imported_xml_must_have_the_microsoft_unattend_root() {
        for xml in ["", "<unattend></unattend>", "<html></html>"] {
            let config = UnattendConfig {
                custom_xml: Some(xml.into()),
                ..UnattendConfig::default()
            };
            assert_eq!(
                config.to_xml_bytes().unwrap_err().code,
                ErrorCode::InvalidUnattendConfig
            );
        }
    }
}
