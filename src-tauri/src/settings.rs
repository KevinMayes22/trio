//! Connection settings. Organisation, project and the team last chosen live in a JSON file in the app's config
//! directory; the PAT lives in the Windows Credential Manager and never touches the webview.

use std::fs;
use std::path::Path;

use keyring::Entry;
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "uk.co.kmayes.trio";
const KEYRING_USER: &str = "azure-devops-pat";
/// Overrides the stored PAT; handy for development.
const PAT_ENV_VAR: &str = "TRIO_PAT";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub org: String,
    pub project: String,
    pub team: String,
    pub demo: bool,
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            // Windows editors (and PowerShell) like to prepend a byte-order mark.
            .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("Could not create settings folder: {e}"))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| format!("Could not save settings: {e}"))
    }

    pub fn has_connection_details(&self) -> bool {
        !self.org.is_empty() && !self.project.is_empty()
    }
}

/// Accepts `MyOrg`, `https://dev.azure.com/MyOrg` or a longer URL copied from the browser.
pub fn normalize_org(input: &str) -> String {
    let input = input.trim().trim_matches('/');
    match input.split_once("dev.azure.com/") {
        Some((_, rest)) => rest.split('/').next().unwrap_or_default().to_string(),
        None => input.to_string(),
    }
}

fn pat_entry() -> Result<Entry, String> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| format!("Credential store unavailable: {e}"))
}

pub fn load_pat() -> Result<Option<String>, String> {
    if let Ok(pat) = std::env::var(PAT_ENV_VAR) {
        if !pat.trim().is_empty() {
            return Ok(Some(pat.trim().to_string()));
        }
    }
    match pat_entry()?.get_password() {
        Ok(pat) => Ok(Some(pat)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Could not read the saved PAT: {e}")),
    }
}

pub fn store_pat(pat: &str) -> Result<(), String> {
    pat_entry()?.set_password(pat.trim()).map_err(|e| format!("Could not save the PAT: {e}"))
}

pub fn forget_pat() -> Result<(), String> {
    match pat_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Could not remove the PAT: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_organisation_input() {
        assert_eq!(normalize_org("MyOrg"), "MyOrg");
        assert_eq!(normalize_org("  MyOrg/ "), "MyOrg");
        assert_eq!(normalize_org("https://dev.azure.com/MyOrg"), "MyOrg");
        assert_eq!(normalize_org("https://dev.azure.com/MyOrg/My%20Project/_boards"), "MyOrg");
    }

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let dir = std::env::temp_dir().join(format!("trio-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(Settings::load(&path), Settings::default());

        let settings = Settings { org: "o".into(), project: "p".into(), team: "t".into(), demo: true };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn settings_files_with_a_byte_order_mark_still_load() {
        let dir = std::env::temp_dir().join(format!("trio-bom-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "\u{feff}{\"org\":\"o\",\"demo\":true}").unwrap();
        let settings = Settings::load(&path);
        assert_eq!((settings.org.as_str(), settings.demo), ("o", true));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_settings_fall_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("trio-corrupt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::remove_dir_all(dir).ok();
    }
}
