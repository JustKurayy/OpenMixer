use serde::Deserialize;
use std::{ffi::OsString, fmt, path::PathBuf};

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

#[derive(Debug, PartialEq)]
pub struct ReplacementCommand {
    pub program: &'static str,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
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

pub fn download_update(update: &UpdateInfo) -> Result<PathBuf, UpdateError> {
    const MAX_UPDATE_SIZE: u64 = 64 * 1024 * 1024;
    if update.size == 0 || update.size > MAX_UPDATE_SIZE {
        return Err(UpdateError::new(
            "GitHub reported an unexpected update size",
        ));
    }
    let mut response = ureq::get(&update.download_url)
        .header("Accept", "application/octet-stream")
        .header("User-Agent", "OpenMixer updater")
        .call()
        .map_err(|error| UpdateError::new(format!("Could not download update: {error}")))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_UPDATE_SIZE)
        .read_to_vec()
        .map_err(|error| UpdateError::new(format!("Could not read update: {error}")))?;
    if bytes.len() as u64 != update.size {
        return Err(UpdateError::new(
            "Downloaded update size does not match GitHub",
        ));
    }
    validate_windows_executable(&bytes)?;
    let path = std::env::temp_dir().join(format!(
        "openmixer-{}-{}.exe",
        update.version,
        std::process::id()
    ));
    std::fs::write(&path, bytes)
        .map_err(|error| UpdateError::new(format!("Could not save update: {error}")))?;
    Ok(path)
}

pub fn replacement_command(
    downloaded: &std::path::Path,
    current: &std::path::Path,
    process_id: u32,
) -> ReplacementCommand {
    const SCRIPT: &str = "$ErrorActionPreference='Stop';$source=$env:OPENMIXER_UPDATE_SOURCE;$target=$env:OPENMIXER_UPDATE_TARGET;$oldPid=[int]$env:OPENMIXER_UPDATE_PID;Wait-Process -Id $oldPid -ErrorAction SilentlyContinue;Move-Item -LiteralPath $source -Destination $target -Force;Start-Process -FilePath $target";
    ReplacementCommand {
        program: "powershell.exe",
        args: vec![
            "-NoProfile".into(),
            "-WindowStyle".into(),
            "Hidden".into(),
            "-Command".into(),
            SCRIPT.into(),
        ],
        env: vec![
            (
                "OPENMIXER_UPDATE_SOURCE".into(),
                downloaded.as_os_str().to_owned(),
            ),
            (
                "OPENMIXER_UPDATE_TARGET".into(),
                current.as_os_str().to_owned(),
            ),
            ("OPENMIXER_UPDATE_PID".into(), process_id.to_string().into()),
        ],
    }
}

#[cfg(target_os = "windows")]
pub fn install_downloaded_update(downloaded: &std::path::Path) -> Result<(), UpdateError> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let current = std::env::current_exe()
        .map_err(|error| UpdateError::new(format!("Could not locate OpenMixer: {error}")))?;
    let replacement = replacement_command(downloaded, &current, std::process::id());
    std::process::Command::new(replacement.program)
        .args(replacement.args)
        .envs(replacement.env)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| UpdateError::new(format!("Could not start updater: {error}")))?;
    Ok(())
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

    #[cfg(target_os = "windows")]
    #[test]
    fn replacement_command_moves_and_launches_paths_with_spaces_and_apostrophes() {
        use std::{process::Command, thread, time::Duration};

        let directory =
            std::env::temp_dir().join(format!("openmixer updater test {}", std::process::id()));
        let source = directory.join("new update's copy.cmd");
        let target = directory.join("installed app's copy.cmd");
        let restarted = directory.join("restarted.txt");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            &source,
            "@echo off\r\necho restarted>\"%~dp0restarted.txt\"\r\n",
        )
        .unwrap();
        std::fs::write(&target, "@echo off\r\necho old\r\n").unwrap();

        let command = replacement_command(&source, &target, i32::MAX as u32);
        let output = Command::new(command.program)
            .args(command.args)
            .envs(command.env)
            .output()
            .unwrap();
        for _ in 0..100 {
            if restarted.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let installed = std::fs::read_to_string(&target).unwrap();
        let source_exists = source.exists();
        let restarted_exists = restarted.exists();
        std::fs::remove_dir_all(&directory).unwrap();

        assert!(
            output.status.success(),
            "PowerShell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!source_exists, "downloaded update was not moved");
        assert!(installed.contains("echo restarted"));
        assert!(restarted_exists, "updated target was not launched");
    }
}
