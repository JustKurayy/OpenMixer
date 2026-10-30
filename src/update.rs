use serde::Deserialize;
use std::fmt;

pub const WINDOWS_ASSET_NAME: &str = "openmixer-windows-x64.exe";
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/JustKurayy/OpenMixer/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/JustKurayy/OpenMixer/releases/download/";

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug)]
pub struct UpdateError(String);

impl UpdateError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for UpdateError {}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

pub fn check_for_update(current_version: &str) -> Result<Option<UpdateInfo>, UpdateError> {
    let mut response = ureq::get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "OpenMixer updater")
        .call()
        .map_err(|error| UpdateError::new(format!("Could not check GitHub: {error}")))?;
    let body = response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()
        .map_err(|error| UpdateError::new(format!("Could not read GitHub response: {error}")))?;
    parse_release(&body, current_version)
}

pub fn parse_release(json: &str, current_version: &str) -> Result<Option<UpdateInfo>, UpdateError> {
    let release: GitHubRelease = serde_json::from_str(json)
        .map_err(|error| UpdateError::new(format!("Invalid GitHub response: {error}")))?;
    if release.prerelease || !is_newer_version(&release.tag_name, current_version)? {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == WINDOWS_ASSET_NAME)
        .ok_or_else(|| UpdateError::new("The release has no Windows x64 executable"))?;
    if !asset.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
        return Err(UpdateError::new(
            "GitHub returned an unexpected download URL",
        ));
    }
    Ok(Some(UpdateInfo {
        version: release.tag_name.trim_start_matches('v').to_owned(),
        download_url: asset.browser_download_url,
        size: asset.size,
    }))
}

pub fn is_newer_version(latest: &str, current: &str) -> Result<bool, UpdateError> {
    Ok(parse_version(latest)? > parse_version(current)?)
}

fn parse_version(version: &str) -> Result<(u64, u64, u64), UpdateError> {
    let parts: Vec<_> = version.trim_start_matches('v').split('.').collect();
    if parts.len() != 3 {
        return Err(UpdateError::new(format!("Invalid version: {version}")));
    }
    let number = |part: &str| {
        part.parse::<u64>()
            .map_err(|_| UpdateError::new(format!("Invalid version: {version}")))
    };
    Ok((number(parts[0])?, number(parts[1])?, number(parts[2])?))
}

pub fn validate_windows_executable(bytes: &[u8]) -> Result<(), UpdateError> {
    if bytes.len() < 64 || &bytes[0..2] != b"MZ" {
        return Err(UpdateError::new(
            "Downloaded file is not a Windows executable",
        ));
    }
    let pe_offset = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    if bytes.get(pe_offset..pe_offset.saturating_add(4)) != Some(b"PE\0\0") {
        return Err(UpdateError::new(
            "Downloaded executable has an invalid PE header",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{
        "tag_name": "v0.2.0",
        "prerelease": false,
        "assets": [
            {
                "name": "openmixer-windows-x64.exe",
                "browser_download_url": "https://github.com/JustKurayy/OpenMixer/releases/download/v0.2.0/openmixer-windows-x64.exe",
                "size": 8400000
            }
        ]
    }"#;

    #[test]
    fn semantic_versions_compare_numerically() {
        assert!(is_newer_version("v0.10.0", "0.9.9").unwrap());
        assert!(!is_newer_version("v0.2.0", "0.2.0").unwrap());
        assert!(!is_newer_version("v0.1.9", "0.2.0").unwrap());
    }

    #[test]
    fn malformed_versions_are_rejected() {
        assert!(is_newer_version("nightly", "0.2.0").is_err());
        assert!(is_newer_version("v1.2", "0.2.0").is_err());
    }

    #[test]
    fn stable_newer_release_selects_the_fixed_executable_asset() {
        let update = parse_release(RELEASE, "0.1.1").unwrap().unwrap();
        assert_eq!(update.version, "0.2.0");
        assert_eq!(update.size, 8_400_000);
        assert_eq!(
            update.download_url,
            "https://github.com/JustKurayy/OpenMixer/releases/download/v0.2.0/openmixer-windows-x64.exe"
        );
    }

    #[test]
    fn prerelease_and_current_release_are_ignored() {
        assert!(
            parse_release(&RELEASE.replace("false", "true"), "0.1.1")
                .unwrap()
                .is_none()
        );
        assert!(parse_release(RELEASE, "0.2.0").unwrap().is_none());
    }

    #[test]
    fn missing_asset_and_bad_json_are_errors() {
        assert!(
            parse_release(
                &RELEASE.replace("openmixer-windows-x64.exe", "other.exe"),
                "0.1.1"
            )
            .is_err()
        );
        assert!(parse_release("not json", "0.1.1").is_err());
    }

    #[test]
    fn executable_validation_rejects_html_and_truncated_pe_files() {
        assert!(validate_windows_executable(b"<html>not an exe</html>").is_err());
        assert!(validate_windows_executable(b"MZ").is_err());

        let mut valid = vec![0_u8; 132];
        valid[0..2].copy_from_slice(b"MZ");
        valid[0x3c..0x40].copy_from_slice(&128_u32.to_le_bytes());
        valid[128..132].copy_from_slice(b"PE\0\0");
        assert!(validate_windows_executable(&valid).is_ok());
    }
}
