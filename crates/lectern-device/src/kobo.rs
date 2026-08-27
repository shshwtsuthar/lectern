use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{
    DeviceConnectionState, DeviceError, DeviceFormat, DeviceId, DeviceInfo, DeviceKind,
    MountedVolume,
};

const KOBO_MARKER: &str = ".kobo";
const VERSION_BYTES_LIMIT: u64 = 4 * 1024;
const SUPPORTED_FORMATS: [DeviceFormat; 6] = [
    DeviceFormat::Epub,
    DeviceFormat::Kepub,
    DeviceFormat::Pdf,
    DeviceFormat::Cbz,
    DeviceFormat::Cbr,
    DeviceFormat::Txt,
];

/// Capability-based Kobo mass-storage driver.
#[derive(Clone, Copy, Debug, Default)]
pub struct KoboDriver;

impl KoboDriver {
    /// Returns whether a mounted volume contains strong Kobo filesystem evidence.
    ///
    /// A real `.kobo` directory is sufficient. A volume label by itself is deliberately not.
    #[must_use]
    pub fn has_kobo_marker(mount_path: &Path) -> bool {
        let marker = mount_path.join(KOBO_MARKER);
        fs::symlink_metadata(marker).is_ok_and(|metadata| {
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink()
        })
    }

    /// Recognizes one mounted volume without reading or writing Kobo's private database.
    ///
    /// # Errors
    ///
    /// Returns an error when the marked mount cannot be canonicalized safely.
    pub fn detect(
        volume: &MountedVolume,
        stable_volume_id: Option<&str>,
    ) -> Result<Option<DeviceInfo>, DeviceError> {
        if !Self::has_kobo_marker(&volume.mount_path) {
            return Ok(None);
        }
        let mount_path = fs::canonicalize(&volume.mount_path).map_err(|error| {
            DeviceError::io("canonicalize Kobo mount", &volume.mount_path, error)
        })?;
        if !mount_path.is_dir() || !Self::has_kobo_marker(&mount_path) {
            return Ok(None);
        }

        let serial = read_kobo_serial(&mount_path).unwrap_or_else(|error| {
            tracing::warn!(
                mount = %mount_path.display(),
                error = %error,
                "could not read optional Kobo identity metadata"
            );
            None
        });
        let id = stable_device_id(volume, serial.as_deref(), stable_volume_id);
        let name = display_name(volume);
        tracing::info!(
            device_id = %id,
            mount = %mount_path.display(),
            model = "unknown",
            "detected Kobo reader"
        );
        Ok(Some(DeviceInfo {
            id,
            kind: DeviceKind::Kobo,
            name,
            manufacturer: "Kobo".to_owned(),
            model: None,
            mount_path,
            volume_name: volume.name.clone(),
            total_bytes: volume.total_bytes,
            free_bytes: volume.free_bytes.min(volume.total_bytes),
            state: DeviceConnectionState::Connected,
            supported_formats: Arc::from(SUPPORTED_FORMATS),
        }))
    }
}

pub(crate) fn is_candidate_volume(volume: &MountedVolume) -> bool {
    if volume.removable || volume_name_is_kobo(&volume.name.to_string_lossy()) {
        return true;
    }
    let mount = &volume.mount_path;
    if mount
        .file_name()
        .is_some_and(|name| volume_name_is_kobo(&name.to_string_lossy()))
    {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        return mount.starts_with("/media") || mount.starts_with("/run/media");
    }
    #[cfg(target_os = "macos")]
    {
        return mount.starts_with("/Volumes");
    }
    #[cfg(target_os = "windows")]
    {
        return mount.parent().is_none();
    }
    #[allow(unreachable_code)]
    false
}

fn read_kobo_serial(mount_path: &Path) -> Result<Option<String>, io::Error> {
    let path = mount_path.join(KOBO_MARKER).join("version");
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::with_capacity(256);
    file.by_ref()
        .take(VERSION_BYTES_LIMIT)
        .read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let candidate = text
        .split([',', '\r', '\n'])
        .next()
        .map(str::trim)
        .filter(|value| {
            (4..=128).contains(&value.len())
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
        });
    Ok(candidate.map(str::to_owned))
}

fn stable_device_id(
    volume: &MountedVolume,
    serial: Option<&str>,
    stable_volume_id: Option<&str>,
) -> DeviceId {
    let mut hash = Sha256::new();
    hash.update(b"lectern-kobo-device-v1\0");
    if let Some(serial) = serial {
        hash.update(b"serial\0");
        hash.update(serial.as_bytes());
    } else if let Some(volume_id) = stable_volume_id.filter(|value| !value.is_empty()) {
        hash.update(b"volume\0");
        hash.update(volume_id.as_bytes());
    } else {
        hash.update(b"fallback\0");
        hash.update(volume.name.to_string_lossy().as_bytes());
        hash.update(b"\0");
        hash.update(volume.file_system.to_string_lossy().as_bytes());
        hash.update(b"\0");
        hash.update(volume.total_bytes.to_le_bytes());
    }
    DeviceId::new(format!("kobo:{}", hex_digest(hash.finalize().as_slice())))
}

fn display_name(volume: &MountedVolume) -> String {
    let volume_name = volume.name.to_string_lossy();
    let mount_name = volume
        .mount_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let label = [&volume_name, &mount_name]
        .into_iter()
        .find(|value| volume_name_is_kobo(value))
        .map(|value| value.trim());
    match label {
        Some(value) if !value.eq_ignore_ascii_case("koboeReader") => value.to_owned(),
        _ => "Kobo eReader".to_owned(),
    }
}

fn volume_name_is_kobo(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("koboeReader")
        || value.trim().to_ascii_lowercase().starts_with("kobo ")
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(*byte >> 4)]));
        result.push(char::from(HEX[usize::from(*byte & 0x0f)]));
    }
    result
}

/// Produces one portable filename component from untrusted book metadata.
///
/// Reserved characters and controls become single spaces, trailing dots/spaces are removed, DOS
/// device basenames are prefixed, and the result is truncated only at UTF-8 character boundaries.
#[must_use]
pub fn sanitize_path_component(value: &str, maximum_bytes: usize) -> String {
    let mut sanitized = String::with_capacity(value.len().min(maximum_bytes));
    let mut pending_space = false;
    for character in value.chars() {
        let forbidden = character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            );
        if forbidden || character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        if pending_space {
            sanitized.push(' ');
            pending_space = false;
        }
        if sanitized.len().saturating_add(character.len_utf8()) > maximum_bytes {
            break;
        }
        sanitized.push(character);
    }
    while sanitized.ends_with([' ', '.']) {
        sanitized.pop();
    }
    if sanitized.is_empty() || matches!(sanitized.as_str(), "." | "..") {
        "Untitled".clone_into(&mut sanitized);
    }
    if is_reserved_dos_basename(&sanitized) {
        sanitized.insert(0, '_');
        while sanitized.len() > maximum_bytes {
            sanitized.pop();
        }
    }
    sanitized
}

fn is_reserved_dos_basename(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes().first(), Some(b'1'..=b'9'))
            })
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs};

    use tempfile::tempdir;

    use super::{KoboDriver, sanitize_path_component};
    use crate::MountedVolume;

    fn volume(path: &Path, name: &str) -> MountedVolume {
        MountedVolume {
            name: OsString::from(name),
            mount_path: path.to_path_buf(),
            file_system: OsString::from("vfat"),
            total_bytes: 32 * 1024 * 1024,
            free_bytes: 20 * 1024 * 1024,
            removable: true,
        }
    }

    use std::path::Path;

    #[test]
    fn detects_real_marker_and_rejects_label_only_or_symlink_marker() {
        let directory = tempdir().unwrap();
        let named_only = volume(directory.path(), "KOBOeReader");
        assert!(KoboDriver::detect(&named_only, None).unwrap().is_none());

        let marker = directory.path().join(".kobo");
        fs::create_dir(&marker).unwrap();
        let detected = KoboDriver::detect(&named_only, Some("volume-1"))
            .unwrap()
            .expect("detect marker");
        assert_eq!(detected.name, "Kobo eReader");

        #[cfg(unix)]
        {
            fs::remove_dir(&marker).unwrap();
            std::os::unix::fs::symlink(directory.path(), &marker).unwrap();
            assert!(KoboDriver::detect(&named_only, None).unwrap().is_none());
        }
    }

    #[test]
    fn identity_prefers_serial_across_mount_paths() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        for root in [first.path(), second.path()] {
            fs::create_dir(root.join(".kobo")).unwrap();
            fs::write(root.join(".kobo/version"), "SN123456,4.41.0\n").unwrap();
        }
        let first = KoboDriver::detect(&volume(first.path(), "/dev/sdb1"), None)
            .unwrap()
            .unwrap();
        let second = KoboDriver::detect(&volume(second.path(), "/dev/sdc1"), None)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn sanitizes_unicode_reserved_names_and_traversal() {
        assert_eq!(sanitize_path_component("Normal title", 80), "Normal title");
        assert_eq!(
            sanitize_path_component("Les Misérables", 80),
            "Les Misérables"
        );
        assert_eq!(sanitize_path_component("../bad/path:*?", 80), ".. bad path");
        assert_eq!(sanitize_path_component("CON", 80), "_CON");
        assert_eq!(sanitize_path_component("\0\n", 80), "Untitled");
        let truncated = sanitize_path_component(&"é".repeat(100), 11);
        assert!(truncated.len() <= 11);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
