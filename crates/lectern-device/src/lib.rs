//! Generic removable ebook-device support for Lectern.
//!
//! This crate owns capability-based device detection and failure-safe filesystem operations. It
//! deliberately contains no desktop-framework code and never writes a reader's private database.

mod history;
mod kobo;
mod manager;
mod platform;
mod transfer;

use std::{ffi::OsString, fmt, path::PathBuf, sync::Arc};

use lectern_core::{AssetId, BookFormat, BookId};

pub use history::{TransferHistoryRecord, transfer_history_path};
pub use kobo::{KoboDriver, sanitize_path_component};
pub use manager::{DeviceManager, ReconcileResult};
pub use platform::{MountedVolume, RemovableStorageProvider, SystemRemovableStorageProvider};
pub use transfer::{
    BatchTransferOutcome, DeviceTransferBook, DeviceTransferSource, DuplicatePolicy,
    FormatPriority, PlannedTransferAction, PlannedTransferItem, TransferControl, TransferFailure,
    TransferItemDisposition, TransferItemOutcome, TransferPlan, TransferProgress,
};

/// Stable identifier for one physical reader when the platform or reader exposes one.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(Arc<str>);

impl DeviceId {
    /// Creates a stable device identifier from a namespaced value.
    #[must_use]
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the stable identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Reader family handled by a device driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceKind {
    /// Kobo e-ink reader exposing mounted USB storage.
    Kobo,
}

/// Ebook format understood by a removable reader.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DeviceFormat {
    /// Standard EPUB publication.
    Epub,
    /// Kobo-enhanced EPUB publication.
    Kepub,
    /// Portable Document Format publication.
    Pdf,
    /// Comic Book ZIP archive.
    Cbz,
    /// Comic Book RAR archive.
    Cbr,
    /// Plain-text publication.
    Txt,
}

impl DeviceFormat {
    /// Returns the stable lowercase extension for the format.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Kepub => "kepub.epub",
            Self::Pdf => "pdf",
            Self::Cbz => "cbz",
            Self::Cbr => "cbr",
            Self::Txt => "txt",
        }
    }

    /// Converts a format currently represented by Lectern's library model.
    #[must_use]
    pub const fn from_book_format(format: BookFormat) -> Self {
        match format {
            BookFormat::Epub => Self::Epub,
            BookFormat::Pdf => Self::Pdf,
        }
    }

    /// Converts to a currently represented Lectern library format when possible.
    #[must_use]
    pub const fn book_format(self) -> Option<BookFormat> {
        match self {
            Self::Epub => Some(BookFormat::Epub),
            Self::Pdf => Some(BookFormat::Pdf),
            Self::Kepub | Self::Cbz | Self::Cbr | Self::Txt => None,
        }
    }
}

impl fmt::Display for DeviceFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Epub => formatter.write_str("EPUB"),
            Self::Kepub => formatter.write_str("KEPUB"),
            Self::Pdf => formatter.write_str("PDF"),
            Self::Cbz => formatter.write_str("CBZ"),
            Self::Cbr => formatter.write_str("CBR"),
            Self::Txt => formatter.write_str("TXT"),
        }
    }
}

/// Current connection/operation state for one reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceConnectionState {
    /// Mounted and available for operations.
    Connected,
    /// The operating system is processing an eject request.
    Ejecting,
}

/// Snapshot of a connected removable reader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    /// Stable physical-device identity when available.
    pub id: DeviceId,
    /// Driver family that recognized the device.
    pub kind: DeviceKind,
    /// User-facing reader name.
    pub name: String,
    /// Plain-text manufacturer name.
    pub manufacturer: String,
    /// Reliably detected model, or `None` when the reader did not expose one.
    pub model: Option<String>,
    /// Internal canonical mount root. Presentation code should not show this by default.
    pub mount_path: PathBuf,
    /// Mounted-volume name or device source retained for platform operations.
    pub volume_name: OsString,
    /// Total capacity in bytes when available.
    pub total_bytes: u64,
    /// Available capacity in bytes when available.
    pub free_bytes: u64,
    /// Current connection/operation state.
    pub state: DeviceConnectionState,
    /// Formats understood by this reader family.
    pub supported_formats: Arc<[DeviceFormat]>,
}

impl DeviceInfo {
    /// Returns occupied capacity without underflowing malformed platform values.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// One publication file visible in Lectern's controlled directory on a reader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceBook {
    /// Device-relative path, never an absolute path.
    pub relative_path: PathBuf,
    /// File format inferred from its final extension.
    pub format: DeviceFormat,
    /// Current file size.
    pub bytes: u64,
    /// Library book correlated through local transfer history, when practical.
    pub library_book_id: Option<BookId>,
    /// Source asset correlated through local transfer history, when practical.
    pub source_asset_id: Option<AssetId>,
    /// Whether Lectern has a matching transfer record and can verify before removal.
    pub managed_by_lectern: bool,
}

/// Outcome of removing a reader copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalOutcome {
    /// A verified sideloaded file was deleted.
    Removed,
    /// The history record was stale because the file was already absent.
    AlreadyMissing,
}

/// Failure from discovery or a removable-device operation.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The requested physical reader is no longer connected.
    #[error("the device is no longer connected")]
    Disconnected,
    /// Another operation owns this reader.
    #[error("another operation is already running on this device")]
    Busy,
    /// The selected source cannot be transferred.
    #[error("source is unavailable: {0}")]
    SourceUnavailable(String),
    /// No compatible source representation exists.
    #[error("no compatible EPUB or PDF source is available for {0}")]
    UnsupportedFormat(String),
    /// Reader capacity cannot satisfy the preflight requirement.
    #[error("device needs {required_bytes} bytes but only {free_bytes} bytes are free")]
    InsufficientSpace {
        /// Bytes required by the planned copies.
        required_bytes: u64,
        /// Bytes reported free immediately before the operation.
        free_bytes: u64,
    },
    /// A path failed the device-root confinement policy.
    #[error("unsafe device path: {0}")]
    UnsafePath(String),
    /// A caller cancelled a transfer.
    #[error("transfer was cancelled")]
    Cancelled,
    /// The source changed after preflight.
    #[error("source changed during transfer: {0}")]
    SourceChanged(PathBuf),
    /// A different file occupies a destination not owned by the matching transfer record.
    #[error("a different file already exists at {0}")]
    DestinationCollision(PathBuf),
    /// A previously transferred file changed outside Lectern and is protected from deletion.
    #[error("the device file changed after transfer and was not removed: {0}")]
    ModifiedDeviceFile(PathBuf),
    /// The reader's controlled directory exceeded the bounded listing limit.
    #[error("device book listing exceeded the {0}-file safety limit")]
    ListingLimit(usize),
    /// The platform boundary rejected or failed an operation.
    #[error("platform device operation failed: {0}")]
    Platform(String),
    /// Local transfer history was unavailable or malformed.
    #[error("transfer history is unavailable: {0}")]
    History(String),
    /// Internal synchronized state was poisoned by a panic.
    #[error("device state is unavailable")]
    State,
    /// A concrete filesystem operation failed.
    #[error("could not {operation} {path}: {source}")]
    Io {
        /// Description of the failed operation.
        operation: &'static str,
        /// Path associated with the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl DeviceError {
    pub(crate) fn io(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}
