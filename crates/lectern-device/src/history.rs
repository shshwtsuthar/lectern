use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{DeviceError, DeviceId};

const HISTORY_SCHEMA_VERSION: u32 = 1;
static NEXT_HISTORY_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// One non-authoritative correlation between a Lectern asset and a reader file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferHistoryRecord {
    /// Stable reader identity.
    pub device_id: String,
    /// Stable library book identity.
    pub book_id: i64,
    /// Stable source asset identity.
    pub source_asset_id: i64,
    /// Lowercase source format.
    pub source_format: String,
    /// Path relative to the reader root.
    pub device_relative_path: PathBuf,
    /// Source size observed during transfer.
    pub source_bytes: u64,
    /// SHA-256 of the exact bytes copied.
    pub file_hash: String,
    /// Unix timestamp recorded after successful publication.
    pub transferred_at_unix_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct HistoryFile {
    schema_version: u32,
    records: Vec<TransferHistoryRecord>,
}

pub(crate) struct TransferHistoryStore {
    path: PathBuf,
    records: Vec<TransferHistoryRecord>,
}

impl TransferHistoryStore {
    pub(crate) fn load(path: PathBuf) -> Result<Self, DeviceError> {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    records: Vec::new(),
                });
            }
            Err(error) => return Err(DeviceError::io("read transfer history", &path, error)),
        };
        let file = serde_json::from_slice::<HistoryFile>(&bytes)
            .map_err(|error| DeviceError::History(error.to_string()))?;
        if file.schema_version != HISTORY_SCHEMA_VERSION {
            return Err(DeviceError::History(format!(
                "unsupported schema version {}",
                file.schema_version
            )));
        }
        if file
            .records
            .iter()
            .any(|record| !valid_relative_device_path(&record.device_relative_path))
        {
            return Err(DeviceError::History(
                "history contains an unsafe device path".to_owned(),
            ));
        }
        Ok(Self {
            path,
            records: file.records,
        })
    }

    pub(crate) fn records_for(
        &self,
        device_id: &DeviceId,
    ) -> impl Iterator<Item = &TransferHistoryRecord> {
        self.records
            .iter()
            .filter(move |record| record.device_id == device_id.as_str())
    }

    pub(crate) fn find_path(
        &self,
        device_id: &DeviceId,
        relative_path: &Path,
    ) -> Option<&TransferHistoryRecord> {
        self.records_for(device_id)
            .find(|record| record.device_relative_path == relative_path)
    }

    pub(crate) fn upsert(&mut self, record: TransferHistoryRecord) {
        self.records.retain(|existing| {
            !(existing.device_id == record.device_id
                && (existing.device_relative_path == record.device_relative_path
                    || (existing.book_id == record.book_id
                        && existing.source_asset_id == record.source_asset_id)))
        });
        self.records.push(record);
    }

    pub(crate) fn remove_path(&mut self, device_id: &DeviceId, relative_path: &Path) -> bool {
        let original = self.records.len();
        self.records.retain(|record| {
            record.device_id != device_id.as_str() || record.device_relative_path != relative_path
        });
        self.records.len() != original
    }

    pub(crate) fn save(&self) -> Result<(), DeviceError> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                DeviceError::io("create transfer-history directory", parent, error)
            })?;
        }
        let bytes = serde_json::to_vec_pretty(&HistoryFile {
            schema_version: HISTORY_SCHEMA_VERSION,
            records: self.records.clone(),
        })
        .map_err(|error| DeviceError::History(error.to_string()))?;
        let (temporary_path, mut temporary) = reserve_temporary(&self.path)?;
        let mut cleanup = TemporaryHistory::new(temporary_path);
        temporary
            .write_all(&bytes)
            .map_err(|error| DeviceError::io("write transfer history", &cleanup.path, error))?;
        temporary
            .flush()
            .map_err(|error| DeviceError::io("flush transfer history", &cleanup.path, error))?;
        temporary.sync_all().map_err(|error| {
            DeviceError::io("synchronize transfer history", &cleanup.path, error)
        })?;
        drop(temporary);
        replace_history(&cleanup.path, &self.path)?;
        cleanup.published = true;
        Ok(())
    }
}

/// Returns the local transfer-history path beside the selected library database.
#[must_use]
pub fn transfer_history_path(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("device-transfers.json")
}

pub(crate) fn valid_relative_device_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && path.starts_with("Books")
}

fn reserve_temporary(destination: &Path) -> Result<(PathBuf, File), DeviceError> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    for _ in 0..100 {
        let id = NEXT_HISTORY_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".lectern-device-history-{}-{id}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(DeviceError::io(
                    "reserve transfer-history temporary file",
                    path,
                    error,
                ));
            }
        }
    }
    Err(DeviceError::History(
        "could not reserve a transfer-history temporary file".to_owned(),
    ))
}

#[cfg(unix)]
fn replace_history(temporary: &Path, destination: &Path) -> Result<(), DeviceError> {
    fs::rename(temporary, destination)
        .map_err(|error| DeviceError::io("publish transfer history", destination, error))
}

#[cfg(windows)]
fn replace_history(temporary: &Path, destination: &Path) -> Result<(), DeviceError> {
    if !destination.exists() {
        return fs::rename(temporary, destination)
            .map_err(|error| DeviceError::io("publish transfer history", destination, error));
    }
    let backup = destination.with_extension(format!(
        "lectern-backup-{}",
        NEXT_HISTORY_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::rename(destination, &backup).map_err(|error| {
        DeviceError::io("reserve previous transfer history", destination, error)
    })?;
    if let Err(error) = fs::rename(temporary, destination) {
        let _restored = fs::rename(&backup, destination);
        return Err(DeviceError::io(
            "publish transfer history",
            destination,
            error,
        ));
    }
    fs::remove_file(&backup)
        .map_err(|error| DeviceError::io("remove previous transfer history", backup, error))
}

struct TemporaryHistory {
    path: PathBuf,
    published: bool,
}

impl TemporaryHistory {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for TemporaryHistory {
    fn drop(&mut self) {
        if !self.published {
            let _removed = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TransferHistoryRecord, TransferHistoryStore, transfer_history_path};
    use crate::DeviceId;
    use tempfile::tempdir;

    #[test]
    fn history_round_trips_and_rejects_traversal() {
        let directory = tempdir().unwrap();
        let path = transfer_history_path(&directory.path().join("library.sqlite3"));
        let mut history = TransferHistoryStore::load(path.clone()).unwrap();
        history.upsert(TransferHistoryRecord {
            device_id: "kobo:1".to_owned(),
            book_id: 1,
            source_asset_id: 2,
            source_format: "epub".to_owned(),
            device_relative_path: "Books/Author/Title.epub".into(),
            source_bytes: 12,
            file_hash: "abcd".to_owned(),
            transferred_at_unix_seconds: 5,
        });
        history.save().unwrap();
        let loaded = TransferHistoryStore::load(path.clone()).unwrap();
        assert_eq!(loaded.records_for(&DeviceId::new("kobo:1")).count(), 1);

        let bytes = fs::read(&path).unwrap();
        let text = String::from_utf8(bytes)
            .unwrap()
            .replace("Books/Author/Title.epub", "../outside.epub");
        fs::write(&path, text).unwrap();
        assert!(TransferHistoryStore::load(path).is_err());
    }

    use std::fs;
}
