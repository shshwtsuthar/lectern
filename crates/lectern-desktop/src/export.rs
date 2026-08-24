//! Bounded, failure-safe publication export.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Fixed copy buffer used by every export worker.
pub const EXPORT_BUFFER_BYTES: usize = 256 * 1024;

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Whether a previously confirmed destination may be replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverwritePolicy {
    /// Reject any existing destination, including one created during the copy.
    Deny,
    /// Replace an existing regular file after the caller's separate confirmation.
    Allow,
}

/// Control returned by an export progress observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportControl {
    /// Continue copying.
    Continue,
    /// Stop and remove the temporary output.
    Cancel,
}

/// Monotonic byte progress for one copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportProgress {
    /// Bytes written to the temporary output.
    pub copied_bytes: u64,
    /// Source size observed immediately before copying.
    pub total_bytes: u64,
}

/// Successful export reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportOutcome {
    /// Exact bytes copied.
    pub copied_bytes: u64,
    /// Number of fixed-size buffer writes, including a final partial write.
    pub chunks: u64,
}

/// Failure from a bounded export operation.
#[derive(Debug)]
pub enum ExportError {
    /// The source was absent, unreadable, or not a regular file.
    SourceUnavailable(String),
    /// The destination already exists and overwrite was not confirmed.
    DestinationExists(PathBuf),
    /// Source and destination resolve to the same file.
    SameFile(PathBuf),
    /// The source size changed while it was being copied.
    SourceChanged {
        /// Size observed before copying.
        expected_bytes: u64,
        /// Bytes actually read.
        copied_bytes: u64,
    },
    /// The destination cannot safely receive a regular-file export.
    InvalidDestination(String),
    /// The caller cancelled after a progress update.
    Cancelled,
    /// A filesystem operation failed.
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Path associated with the failure.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceUnavailable(message) | Self::InvalidDestination(message) => {
                formatter.write_str(message)
            }
            Self::DestinationExists(path) => {
                write!(formatter, "destination already exists: {}", path.display())
            }
            Self::SameFile(path) => write!(
                formatter,
                "source and destination are the same file: {}",
                path.display()
            ),
            Self::SourceChanged {
                expected_bytes,
                copied_bytes,
            } => write!(
                formatter,
                "source changed while exporting (expected {expected_bytes} bytes, copied {copied_bytes})"
            ),
            Self::Cancelled => formatter.write_str("export was cancelled"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "could not {operation} {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ExportError {}

/// Copies `source` byte-for-byte to `destination` with bounded memory and atomic publication.
///
/// The copy is written to a new temporary file in the destination directory. A deny-overwrite
/// publish uses an atomic hard-link creation so a racing destination is never replaced. Unix
/// overwrite publication uses atomic rename; other supported platforms preserve the old file if
/// their standard rename primitive cannot replace it.
///
/// # Errors
///
/// Returns [`ExportError`] when either path is unsafe or unavailable, copying or publication
/// fails, the source changes size, or the progress observer cancels the operation.
pub fn export_file(
    source: &Path,
    destination: &Path,
    overwrite: OverwritePolicy,
    progress: impl FnMut(ExportProgress) -> ExportControl,
) -> Result<ExportOutcome, ExportError> {
    validate_source_and_destination(source, destination, overwrite)?;
    let metadata = source
        .metadata()
        .map_err(|error| ExportError::SourceUnavailable(error.to_string()))?;
    let mut source_file =
        File::open(source).map_err(|error| ExportError::SourceUnavailable(error.to_string()))?;
    export_reader(
        &mut source_file,
        source,
        metadata.len(),
        destination,
        overwrite,
        progress,
    )
}

fn validate_source_and_destination(
    source: &Path,
    destination: &Path,
    overwrite: OverwritePolicy,
) -> Result<(), ExportError> {
    let metadata = source
        .metadata()
        .map_err(|error| ExportError::SourceUnavailable(error.to_string()))?;
    if !metadata.is_file() {
        return Err(ExportError::SourceUnavailable(format!(
            "source is not a readable regular file: {}",
            source.display()
        )));
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    let Some(parent) = parent else {
        return Err(ExportError::InvalidDestination(format!(
            "destination has no parent directory: {}",
            destination.display()
        )));
    };
    if !parent.is_dir() {
        return Err(ExportError::InvalidDestination(format!(
            "destination directory is unavailable: {}",
            parent.display()
        )));
    }
    if destination.exists() {
        if fs::canonicalize(source).ok() == fs::canonicalize(destination).ok() {
            return Err(ExportError::SameFile(destination.to_path_buf()));
        }
        let destination_metadata = destination.metadata().map_err(|source| ExportError::Io {
            operation: "inspect destination",
            path: destination.to_path_buf(),
            source,
        })?;
        if !destination_metadata.is_file() {
            return Err(ExportError::InvalidDestination(format!(
                "destination is not a regular file: {}",
                destination.display()
            )));
        }
        if overwrite == OverwritePolicy::Deny {
            return Err(ExportError::DestinationExists(destination.to_path_buf()));
        }
    }
    Ok(())
}

fn export_reader(
    source: &mut dyn Read,
    source_path: &Path,
    total_bytes: u64,
    destination: &Path,
    overwrite: OverwritePolicy,
    mut progress: impl FnMut(ExportProgress) -> ExportControl,
) -> Result<ExportOutcome, ExportError> {
    let (temporary_path, mut temporary_file) = create_temporary(destination)?;
    let mut cleanup = TemporaryCleanup::new(temporary_path);
    let mut buffer = vec![0_u8; EXPORT_BUFFER_BYTES].into_boxed_slice();
    let mut copied_bytes = 0_u64;
    let mut chunks = 0_u64;
    loop {
        let read = source.read(&mut buffer).map_err(|source| ExportError::Io {
            operation: "read source",
            path: source_path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        temporary_file
            .write_all(&buffer[..read])
            .map_err(|source| ExportError::Io {
                operation: "write temporary export",
                path: cleanup.path.clone(),
                source,
            })?;
        copied_bytes = copied_bytes
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| ExportError::InvalidDestination("export size overflowed".into()))?;
        chunks += 1;
        if progress(ExportProgress {
            copied_bytes,
            total_bytes,
        }) == ExportControl::Cancel
        {
            return Err(ExportError::Cancelled);
        }
    }
    if copied_bytes != total_bytes {
        return Err(ExportError::SourceChanged {
            expected_bytes: total_bytes,
            copied_bytes,
        });
    }
    temporary_file.flush().map_err(|source| ExportError::Io {
        operation: "flush temporary export",
        path: cleanup.path.clone(),
        source,
    })?;
    drop(temporary_file);
    publish(&cleanup.path, destination, overwrite)?;
    cleanup.published = true;
    Ok(ExportOutcome {
        copied_bytes,
        chunks,
    })
}

fn create_temporary(destination: &Path) -> Result<(PathBuf, File), ExportError> {
    let parent = destination
        .parent()
        .expect("destination parent was validated");
    for _ in 0..100 {
        let id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".lectern-export-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(ExportError::Io {
                    operation: "create temporary export",
                    path,
                    source,
                });
            }
        }
    }
    Err(ExportError::InvalidDestination(format!(
        "could not reserve a temporary export beside {}",
        destination.display()
    )))
}

fn publish(
    temporary: &Path,
    destination: &Path,
    overwrite: OverwritePolicy,
) -> Result<(), ExportError> {
    match overwrite {
        OverwritePolicy::Deny => match fs::hard_link(temporary, destination) {
            Ok(()) => fs::remove_file(temporary).map_err(|source| ExportError::Io {
                operation: "remove published temporary export",
                path: temporary.to_path_buf(),
                source,
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(ExportError::DestinationExists(destination.to_path_buf()))
            }
            Err(source) => Err(ExportError::Io {
                operation: "publish export",
                path: destination.to_path_buf(),
                source,
            }),
        },
        OverwritePolicy::Allow => replace_published_file(temporary, destination),
    }
}

#[cfg(unix)]
fn replace_published_file(temporary: &Path, destination: &Path) -> Result<(), ExportError> {
    fs::rename(temporary, destination).map_err(|source| ExportError::Io {
        operation: "replace export destination",
        path: destination.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn replace_published_file(temporary: &Path, destination: &Path) -> Result<(), ExportError> {
    if !destination.exists() {
        return fs::rename(temporary, destination).map_err(|source| ExportError::Io {
            operation: "publish export",
            path: destination.to_path_buf(),
            source,
        });
    }
    let backup = reserve_backup_path(destination)?;
    fs::rename(destination, &backup).map_err(|source| ExportError::Io {
        operation: "reserve existing destination",
        path: destination.to_path_buf(),
        source,
    })?;
    if let Err(source) = fs::rename(temporary, destination) {
        let _restored = fs::rename(&backup, destination);
        return Err(ExportError::Io {
            operation: "publish replacement export",
            path: destination.to_path_buf(),
            source,
        });
    }
    fs::remove_file(&backup).map_err(|source| ExportError::Io {
        operation: "remove replaced destination backup",
        path: backup,
        source,
    })
}

#[cfg(windows)]
fn reserve_backup_path(destination: &Path) -> Result<PathBuf, ExportError> {
    let parent = destination
        .parent()
        .expect("destination parent was validated");
    for _ in 0..100 {
        let id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".lectern-export-backup-{}-{id}.tmp",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(ExportError::InvalidDestination(
        "could not reserve a replacement backup path".into(),
    ))
}

struct TemporaryCleanup {
    path: PathBuf,
    published: bool,
}

impl TemporaryCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        if !self.published {
            let _removed = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Cursor, Read},
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{ExportControl, ExportError, OverwritePolicy, export_file, export_reader};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("lectern-export-test-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn temporary_exports(&self) -> usize {
            std::fs::read_dir(&self.0)
                .expect("read test directory")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".lectern-export-")
                })
                .count()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove test directory");
        }
    }

    #[test]
    fn exports_exact_bytes_without_overwriting() {
        let directory = TestDirectory::new();
        let source = directory.path().join("source.epub");
        let destination = directory.path().join("copy.epub");
        let bytes = vec![42_u8; 700_000];
        std::fs::write(&source, &bytes).expect("write source");
        let mut progress = Vec::new();

        let outcome = export_file(&source, &destination, OverwritePolicy::Deny, |sample| {
            progress.push(sample);
            ExportControl::Continue
        })
        .expect("export file");

        assert_eq!(
            outcome.copied_bytes,
            u64::try_from(bytes.len()).expect("test byte length fits u64")
        );
        assert_eq!(std::fs::read(destination).expect("read copy"), bytes);
        assert_eq!(
            progress.last().expect("final progress").copied_bytes,
            outcome.copied_bytes
        );
        assert_eq!(directory.temporary_exports(), 0);
    }

    #[test]
    fn collision_and_cancellation_preserve_destination_and_cleanup() {
        let directory = TestDirectory::new();
        let source = directory.path().join("source.pdf");
        let destination = directory.path().join("existing.pdf");
        std::fs::write(&source, vec![7_u8; 700_000]).expect("write source");
        std::fs::write(&destination, b"existing").expect("write destination");

        assert!(matches!(
            export_file(&source, &destination, OverwritePolicy::Deny, |_| ExportControl::Continue),
            Err(ExportError::DestinationExists(path)) if path == destination
        ));
        assert_eq!(
            std::fs::read(&destination).expect("read existing"),
            b"existing"
        );
        let cancelled = directory.path().join("cancelled.pdf");
        assert!(matches!(
            export_file(&source, &cancelled, OverwritePolicy::Deny, |_| {
                ExportControl::Cancel
            }),
            Err(ExportError::Cancelled)
        ));
        assert!(!cancelled.exists());
        assert_eq!(directory.temporary_exports(), 0);
    }

    #[test]
    fn confirmed_overwrite_replaces_existing_regular_file() {
        let directory = TestDirectory::new();
        let source = directory.path().join("source.epub");
        let destination = directory.path().join("existing.epub");
        std::fs::write(&source, b"replacement bytes").expect("write source");
        std::fs::write(&destination, b"old bytes").expect("write destination");

        export_file(&source, &destination, OverwritePolicy::Allow, |_| {
            ExportControl::Continue
        })
        .expect("replace destination");

        assert_eq!(
            std::fs::read(destination).expect("read replacement"),
            b"replacement bytes"
        );
        assert_eq!(directory.temporary_exports(), 0);
    }

    struct FailingReader {
        inner: Cursor<Vec<u8>>,
        reads: usize,
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads > 1 {
                return Err(io::Error::other("injected read failure"));
            }
            self.inner.read(buffer)
        }
    }

    #[test]
    fn mid_copy_failure_removes_partial_output() {
        let directory = TestDirectory::new();
        let destination = directory.path().join("failed.epub");
        let mut reader = FailingReader {
            inner: Cursor::new(vec![9_u8; 700_000]),
            reads: 0,
        };

        assert!(matches!(
            export_reader(
                &mut reader,
                Path::new("injected-source.epub"),
                700_000,
                &destination,
                OverwritePolicy::Deny,
                |_| ExportControl::Continue,
            ),
            Err(ExportError::Io {
                operation: "read source",
                ..
            })
        ));
        assert!(!destination.exists());
        assert_eq!(directory.temporary_exports(), 0);
    }

    #[test]
    fn source_size_change_prevents_publication() {
        let directory = TestDirectory::new();
        let destination = directory.path().join("changed.epub");
        let mut reader = Cursor::new(vec![3_u8; 32]);

        assert!(matches!(
            export_reader(
                &mut reader,
                Path::new("changed-source.epub"),
                64,
                &destination,
                OverwritePolicy::Deny,
                |_| ExportControl::Continue,
            ),
            Err(ExportError::SourceChanged {
                expected_bytes: 64,
                copied_bytes: 32,
            })
        ));
        assert!(!destination.exists());
        assert_eq!(directory.temporary_exports(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn exports_non_unicode_paths() {
        use std::os::unix::ffi::OsStringExt;

        let directory = TestDirectory::new();
        let source = directory
            .path()
            .join(std::ffi::OsString::from_vec(b"source-\xFF.epub".to_vec()));
        let destination = directory
            .path()
            .join(std::ffi::OsString::from_vec(b"copy-\xFE.epub".to_vec()));
        std::fs::write(&source, b"non-Unicode path bytes").expect("write source");

        export_file(&source, &destination, OverwritePolicy::Deny, |_| {
            ExportControl::Continue
        })
        .expect("export non-Unicode path");

        assert_eq!(
            std::fs::read(destination).expect("read copy"),
            b"non-Unicode path bytes"
        );
    }
}
