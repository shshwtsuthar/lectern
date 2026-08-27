use std::{
    ffi::OsString,
    fs,
    path::{Component, Path},
    process::{Command, Output},
};

use sysinfo::Disks;

use crate::{DeviceError, DeviceInfo};

/// One mounted local volume reported by the operating-system adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountedVolume {
    /// Platform volume label or backing-device source.
    pub name: OsString,
    /// Mounted filesystem root.
    pub mount_path: std::path::PathBuf,
    /// Platform filesystem type.
    pub file_system: OsString,
    /// Total filesystem capacity.
    pub total_bytes: u64,
    /// Available filesystem capacity.
    pub free_bytes: u64,
    /// Whether the platform identifies the volume as removable.
    pub removable: bool,
}

/// Operating-system boundary for mounted removable storage.
pub trait RemovableStorageProvider: Send + Sync + 'static {
    /// Returns the currently mounted local volumes.
    ///
    /// # Errors
    ///
    /// Returns a platform error when mounted-volume enumeration fails.
    fn list_mounted_volumes(&self) -> Result<Vec<MountedVolume>, DeviceError>;

    /// Returns a stable filesystem/volume identity when the platform exposes one.
    fn stable_volume_id(&self, volume: &MountedVolume) -> Option<String>;

    /// Requests a real unmount/eject operation from the operating system.
    ///
    /// # Errors
    ///
    /// Returns a platform error when the validated eject request fails.
    fn eject(&self, device: &DeviceInfo) -> Result<(), DeviceError>;
}

/// Native mounted-volume discovery plus validated platform eject commands.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRemovableStorageProvider;

impl RemovableStorageProvider for SystemRemovableStorageProvider {
    fn list_mounted_volumes(&self) -> Result<Vec<MountedVolume>, DeviceError> {
        let disks = Disks::new_with_refreshed_list();
        Ok(disks
            .list()
            .iter()
            .map(|disk| MountedVolume {
                name: disk.name().to_os_string(),
                mount_path: disk.mount_point().to_path_buf(),
                file_system: disk.file_system().to_os_string(),
                total_bytes: disk.total_space(),
                free_bytes: disk.available_space(),
                removable: disk.is_removable(),
            })
            .collect())
    }

    fn stable_volume_id(&self, volume: &MountedVolume) -> Option<String> {
        stable_volume_id(volume)
    }

    fn eject(&self, device: &DeviceInfo) -> Result<(), DeviceError> {
        eject_device(device)
    }
}

#[cfg(target_os = "linux")]
fn stable_volume_id(volume: &MountedVolume) -> Option<String> {
    let source = fs::canonicalize(Path::new(&volume.name)).ok()?;
    let entries = fs::read_dir("/dev/disk/by-uuid").ok()?;
    for entry in entries.flatten() {
        if fs::canonicalize(entry.path()).ok().as_deref() == Some(source.as_path()) {
            let id = entry.file_name().to_string_lossy().trim().to_owned();
            if valid_identifier(&id) {
                return Some(id);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn stable_volume_id(volume: &MountedVolume) -> Option<String> {
    let output = Command::new("diskutil")
        .arg("info")
        .arg(&volume.mount_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "Volume UUID")
                .then(|| value.trim().to_owned())
                .filter(|value| valid_identifier(value))
        })
}

#[cfg(target_os = "windows")]
fn stable_volume_id(volume: &MountedVolume) -> Option<String> {
    if !valid_windows_drive_root(&volume.mount_path) {
        return None;
    }
    let output = Command::new("mountvol.exe")
        .arg(&volume.mount_path)
        .arg("/L")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (id.starts_with(r"\\?\Volume{") && id.ends_with(r"}\") && valid_identifier(&id)).then_some(id)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn stable_volume_id(_volume: &MountedVolume) -> Option<String> {
    None
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|character| !character.is_control() && character != '\0')
}

#[cfg(target_os = "linux")]
fn eject_device(device: &DeviceInfo) -> Result<(), DeviceError> {
    let source = Path::new(&device.volume_name);
    if !source.is_absolute()
        || !source.starts_with("/dev")
        || source
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(DeviceError::Platform(format!(
            "refusing unsafe Linux block-device path {}",
            source.display()
        )));
    }
    checked_command(
        Command::new("udisksctl")
            .arg("unmount")
            .arg("--block-device")
            .arg(source),
        "unmount Kobo",
    )?;
    checked_command(
        Command::new("udisksctl")
            .arg("power-off")
            .arg("--block-device")
            .arg(source),
        "power off Kobo",
    )
}

#[cfg(target_os = "macos")]
fn eject_device(device: &DeviceInfo) -> Result<(), DeviceError> {
    if !device.mount_path.is_absolute() || !device.mount_path.starts_with("/Volumes") {
        return Err(DeviceError::Platform(format!(
            "refusing unsafe macOS volume path {}",
            device.mount_path.display()
        )));
    }
    checked_command(
        Command::new("diskutil")
            .arg("eject")
            .arg(&device.mount_path),
        "eject Kobo",
    )
}

#[cfg(target_os = "windows")]
fn eject_device(device: &DeviceInfo) -> Result<(), DeviceError> {
    if !valid_windows_drive_root(&device.mount_path) {
        return Err(DeviceError::Platform(format!(
            "refusing unsafe Windows volume path {}",
            device.mount_path.display()
        )));
    }
    checked_command(
        Command::new("mountvol.exe")
            .arg(&device.mount_path)
            .arg("/P"),
        "eject Kobo",
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn eject_device(_device: &DeviceInfo) -> Result<(), DeviceError> {
    Err(DeviceError::Platform(
        "safe eject is unsupported on this operating system".to_owned(),
    ))
}

#[cfg(target_os = "windows")]
fn valid_windows_drive_root(path: &Path) -> bool {
    let value = path.as_os_str().to_string_lossy();
    let bytes = value.as_bytes();
    bytes.len() == 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn checked_command(command: &mut Command, operation: &'static str) -> Result<(), DeviceError> {
    let output = command
        .output()
        .map_err(|error| DeviceError::Platform(format!("could not {operation}: {error}")))?;
    command_result(&output, operation)
}

fn command_result(output: &Output, operation: &'static str) -> Result<(), DeviceError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim().chars().take(512).collect::<String>();
    let detail = if detail.is_empty() {
        format!("process exited with {}", output.status)
    } else {
        detail
    };
    Err(DeviceError::Platform(format!(
        "could not {operation}: {detail}"
    )))
}

#[cfg(test)]
mod tests {
    use super::valid_identifier;

    #[test]
    fn stable_identifiers_are_bounded_and_single_line() {
        assert!(valid_identifier("1234-ABCD"));
        assert!(!valid_identifier(""));
        assert!(!valid_identifier("bad\nvalue"));
        assert!(!valid_identifier(&"x".repeat(257)));
    }
}
