use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use lectern_core::{AssetId, BookFormat, BookId};
use sha2::{Digest, Sha256};
use walkdir::{DirEntry, WalkDir};

use crate::{
    DeviceBook, DeviceError, DeviceFormat, DeviceId, DeviceInfo, RemovalOutcome,
    TransferHistoryRecord,
    history::{TransferHistoryStore, valid_relative_device_path},
    kobo::{KoboDriver, hex_digest, sanitize_path_component},
};

const COPY_BUFFER_BYTES: usize = 256 * 1024;
const AUTHOR_COMPONENT_BYTES: usize = 80;
const TITLE_COMPONENT_BYTES: usize = 120;
const DEVICE_BOOK_LIMIT: usize = 10_000;
static NEXT_TRANSFER_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// One library asset that may be selected for a reader transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceTransferSource {
    /// Stable library asset identity.
    pub asset_id: AssetId,
    /// Library publication format.
    pub format: BookFormat,
    /// Current source path.
    pub path: PathBuf,
}

/// One logical library book offered to the transfer planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceTransferBook {
    /// Stable library book identity.
    pub book_id: BookId,
    /// Untrusted display title.
    pub title: String,
    /// Untrusted display-ready author string.
    pub authors: String,
    /// Available file representations.
    pub sources: Vec<DeviceTransferSource>,
}

/// Reusable deterministic preference order for a reader's compatible formats.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatPriority(Arc<[DeviceFormat]>);

impl FormatPriority {
    /// Creates a de-duplicated preference order.
    #[must_use]
    pub fn new(formats: impl IntoIterator<Item = DeviceFormat>) -> Self {
        let mut seen = HashSet::new();
        let formats = formats
            .into_iter()
            .filter(|format| seen.insert(*format))
            .collect::<Vec<_>>();
        Self(Arc::from(formats))
    }

    /// Kobo's initial source preference: EPUB, then PDF.
    #[must_use]
    pub fn kobo_default() -> Self {
        Self::new([DeviceFormat::Epub, DeviceFormat::Pdf])
    }

    /// Returns the ordered format slice.
    #[must_use]
    pub fn formats(&self) -> &[DeviceFormat] {
        &self.0
    }
}

impl Default for FormatPriority {
    fn default() -> Self {
        Self::kobo_default()
    }
}

/// Preflight disposition for one selected book.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlannedTransferAction {
    /// Destination is free and will receive a new file.
    Copy,
    /// Lectern owns an unchanged previous transfer that can be explicitly replaced.
    Replace,
    /// Destination already contains byte-identical content.
    AlreadyPresent,
    /// A different or externally modified file occupies the destination.
    Collision,
}

/// One validated item in a batch transfer plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedTransferItem {
    /// Stable library book identity.
    pub book_id: BookId,
    /// Stable source asset identity.
    pub asset_id: AssetId,
    /// Display title used only for progress and result messages.
    pub title: String,
    /// Selected source format.
    pub format: DeviceFormat,
    /// Validated source path.
    pub source_path: PathBuf,
    /// Source size observed during preflight.
    pub source_bytes: u64,
    /// Device-relative destination below `Books/`.
    pub relative_path: PathBuf,
    /// Planned duplicate behavior.
    pub action: PlannedTransferAction,
    /// Whether a matching Lectern history record owns the current destination.
    pub history_owned: bool,
}

/// Immutable, bounded transfer plan tied to one current mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferPlan {
    /// Stable target reader identity.
    pub device_id: DeviceId,
    /// Canonical target mount at preflight time.
    pub mount_path: PathBuf,
    /// Valid items in deterministic input order.
    pub items: Vec<PlannedTransferItem>,
    /// Per-book failures found without starting writes.
    pub failures: Vec<TransferFailure>,
    /// Conservative bytes required by new and replacement copies.
    pub required_bytes: u64,
}

impl TransferPlan {
    /// Number of existing different files in the plan.
    #[must_use]
    pub fn collision_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.action == PlannedTransferAction::Collision)
            .count()
    }

    /// Number of safe previous transfers eligible for replacement.
    #[must_use]
    pub fn replacement_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.action == PlannedTransferAction::Replace)
            .count()
    }
}

/// Caller decision for safe, history-owned duplicate destinations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicatePolicy {
    /// Leave prior device files unchanged.
    Skip,
    /// Replace only a previous Lectern transfer whose current hash still matches history.
    ReplaceTracked,
}

/// Control returned by a transfer progress observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferControl {
    /// Continue the current batch.
    Continue,
    /// Stop after cleaning the known incomplete output.
    Cancel,
}

/// Monotonic progress across one sequential batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferProgress {
    /// Zero-based current item index.
    pub item_index: usize,
    /// Total planned item count.
    pub total_items: usize,
    /// Current book title.
    pub current_title: String,
    /// Bytes copied for the current item.
    pub item_copied_bytes: u64,
    /// Current source size.
    pub item_total_bytes: u64,
    /// Bytes copied across completed items and the current item.
    pub batch_copied_bytes: u64,
    /// Conservative total bytes requiring copy work.
    pub batch_total_bytes: u64,
}

/// One selected book that could not be planned or copied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferFailure {
    /// Stable library book identity.
    pub book_id: BookId,
    /// Display title.
    pub title: String,
    /// Actionable failure message.
    pub message: String,
}

/// Completed disposition for one planned item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferItemDisposition {
    /// A new file was published.
    Transferred,
    /// A confirmed previous transfer was replaced.
    Replaced,
    /// Identical bytes were already present.
    AlreadyPresent,
    /// A duplicate was left unchanged by policy.
    Skipped,
}

/// Successful or intentionally skipped item outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferItemOutcome {
    /// Stable library book identity.
    pub book_id: BookId,
    /// Device-relative path.
    pub relative_path: PathBuf,
    /// Final item disposition.
    pub disposition: TransferItemDisposition,
    /// Bytes newly written for this item.
    pub copied_bytes: u64,
}

/// Final reconciliation for one batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BatchTransferOutcome {
    /// Items transferred, replaced, already present, or skipped.
    pub items: Vec<TransferItemOutcome>,
    /// Per-book failures.
    pub failures: Vec<TransferFailure>,
    /// Exact bytes newly copied.
    pub copied_bytes: u64,
    /// Local-history persistence warning after device copies succeeded.
    pub history_error: Option<String>,
}

impl BatchTransferOutcome {
    /// Returns the number of files newly transferred or replaced.
    #[must_use]
    pub fn transferred_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| {
                matches!(
                    item.disposition,
                    TransferItemDisposition::Transferred | TransferItemDisposition::Replaced
                )
            })
            .count()
    }
}

pub(crate) fn build_plan(
    device: &DeviceInfo,
    history: &TransferHistoryStore,
    books: &[DeviceTransferBook],
    priority: &FormatPriority,
) -> Result<TransferPlan, DeviceError> {
    validate_connected_root(&device.mount_path)?;
    let mut items = Vec::with_capacity(books.len());
    let mut failures = Vec::new();
    let mut reserved_paths = HashSet::with_capacity(books.len());
    let mut required_bytes = 0_u64;

    for book in books {
        let Some(source) = select_source(book, priority) else {
            failures.push(TransferFailure {
                book_id: book.book_id,
                title: book.title.clone(),
                message: DeviceError::UnsupportedFormat(book.title.clone()).to_string(),
            });
            continue;
        };
        let metadata = match source.path.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                failures.push(failure(book, "source is not a regular file"));
                continue;
            }
            Err(error) => {
                failures.push(failure(book, &format!("source is unavailable: {error}")));
                continue;
            }
        };
        let canonical_source = match fs::canonicalize(&source.path) {
            Ok(path) if !path.starts_with(&device.mount_path) => path,
            Ok(_) => {
                failures.push(failure(book, "source resolves inside the target device"));
                continue;
            }
            Err(error) => {
                failures.push(failure(book, &format!("source is unavailable: {error}")));
                continue;
            }
        };
        let format = DeviceFormat::from_book_format(source.format);
        let relative_path =
            unique_relative_path(&book.authors, &book.title, format, &mut reserved_paths);
        validate_relative_destination(&relative_path)?;
        let destination = device.mount_path.join(&relative_path);
        validate_existing_parent_chain(&device.mount_path, &relative_path)?;
        let (action, history_owned) = inspect_destination(
            device,
            history,
            book,
            source,
            &canonical_source,
            metadata.len(),
            &relative_path,
            &destination,
        )?;
        if matches!(
            action,
            PlannedTransferAction::Copy | PlannedTransferAction::Replace
        ) {
            required_bytes = required_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| DeviceError::UnsafePath("transfer size overflowed".to_owned()))?;
        }
        items.push(PlannedTransferItem {
            book_id: book.book_id,
            asset_id: source.asset_id,
            title: book.title.clone(),
            format,
            source_path: canonical_source,
            source_bytes: metadata.len(),
            relative_path,
            action,
            history_owned,
        });
    }
    if device.total_bytes > 0 && required_bytes > device.free_bytes {
        return Err(DeviceError::InsufficientSpace {
            required_bytes,
            free_bytes: device.free_bytes,
        });
    }
    Ok(TransferPlan {
        device_id: device.id.clone(),
        mount_path: device.mount_path.clone(),
        items,
        failures,
        required_bytes,
    })
}

fn select_source<'a>(
    book: &'a DeviceTransferBook,
    priority: &FormatPriority,
) -> Option<&'a DeviceTransferSource> {
    priority.formats().iter().find_map(|format| {
        let book_format = format.book_format()?;
        book.sources
            .iter()
            .filter(|source| source.format == book_format)
            .min_by_key(|source| source.asset_id)
    })
}

fn failure(book: &DeviceTransferBook, message: &str) -> TransferFailure {
    TransferFailure {
        book_id: book.book_id,
        title: book.title.clone(),
        message: message.to_owned(),
    }
}

fn unique_relative_path(
    authors: &str,
    title: &str,
    format: DeviceFormat,
    reserved: &mut HashSet<String>,
) -> PathBuf {
    let author = sanitize_path_component(authors, AUTHOR_COMPONENT_BYTES);
    let title = sanitize_path_component(title, TITLE_COMPONENT_BYTES);
    for suffix in 1_u32..=10_000 {
        let filename = if suffix == 1 {
            format!("{title}.{}", format.extension())
        } else {
            format!("{title} ({suffix}).{}", format.extension())
        };
        let relative = Path::new("Books").join(&author).join(filename);
        let key = relative.to_string_lossy().to_lowercase();
        if reserved.insert(key) {
            return relative;
        }
    }
    Path::new("Books").join(author).join(format!(
        "{title}-{}.{}",
        reserved.len(),
        format.extension()
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "duplicate inspection needs the validated source and its library identity"
)]
fn inspect_destination(
    device: &DeviceInfo,
    history: &TransferHistoryStore,
    book: &DeviceTransferBook,
    source: &DeviceTransferSource,
    canonical_source: &Path,
    source_bytes: u64,
    relative_path: &Path,
    destination: &Path,
) -> Result<(PlannedTransferAction, bool), DeviceError> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((PlannedTransferAction::Copy, false));
        }
        Err(error) => {
            return Err(DeviceError::io(
                "inspect device destination",
                destination,
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DeviceError::UnsafePath(format!(
            "destination is not a regular file: {}",
            destination.display()
        )));
    }
    let record = history.find_path(&device.id, relative_path);
    let history_owned = record.is_some_and(|record| {
        record.book_id == book.book_id.value() && record.source_asset_id == source.asset_id.value()
    });
    if metadata.len() != source_bytes {
        return Ok(if history_owned {
            let current_hash = hash_file(destination)?;
            let unchanged = record.is_some_and(|record| record.file_hash == current_hash);
            if unchanged {
                (PlannedTransferAction::Replace, true)
            } else {
                (PlannedTransferAction::Collision, false)
            }
        } else {
            (PlannedTransferAction::Collision, false)
        });
    }
    let source_hash = hash_file(canonical_source)?;
    let destination_hash = hash_file(destination)?;
    if source_hash == destination_hash {
        return Ok((PlannedTransferAction::AlreadyPresent, history_owned));
    }
    if history_owned && record.is_some_and(|record| record.file_hash == destination_hash) {
        Ok((PlannedTransferAction::Replace, true))
    } else {
        Ok((PlannedTransferAction::Collision, false))
    }
}

pub(crate) fn execute_plan(
    device: &DeviceInfo,
    history: &mut TransferHistoryStore,
    plan: &TransferPlan,
    duplicate_policy: DuplicatePolicy,
    mut progress: impl FnMut(TransferProgress) -> TransferControl,
) -> Result<BatchTransferOutcome, DeviceError> {
    if device.id != plan.device_id || device.mount_path != plan.mount_path {
        return Err(DeviceError::Disconnected);
    }
    validate_connected_root(&device.mount_path)?;
    let mut outcome = BatchTransferOutcome {
        failures: plan.failures.clone(),
        ..BatchTransferOutcome::default()
    };
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    let mut history_changed = false;

    let execution = (|| -> Result<(), DeviceError> {
        for (item_index, item) in plan.items.iter().enumerate() {
            validate_connected_root(&device.mount_path)?;
            let disposition = match item.action {
                PlannedTransferAction::AlreadyPresent => {
                    if item.history_owned {
                        let hash = hash_file(&device.mount_path.join(&item.relative_path))?;
                        history.upsert(history_record(device, item, hash));
                        history_changed = true;
                    }
                    outcome.items.push(TransferItemOutcome {
                        book_id: item.book_id,
                        relative_path: item.relative_path.clone(),
                        disposition: TransferItemDisposition::AlreadyPresent,
                        copied_bytes: 0,
                    });
                    continue;
                }
                PlannedTransferAction::Collision => {
                    outcome.items.push(TransferItemOutcome {
                        book_id: item.book_id,
                        relative_path: item.relative_path.clone(),
                        disposition: TransferItemDisposition::Skipped,
                        copied_bytes: 0,
                    });
                    continue;
                }
                PlannedTransferAction::Replace if duplicate_policy == DuplicatePolicy::Skip => {
                    outcome.items.push(TransferItemOutcome {
                        book_id: item.book_id,
                        relative_path: item.relative_path.clone(),
                        disposition: TransferItemDisposition::Skipped,
                        copied_bytes: 0,
                    });
                    continue;
                }
                PlannedTransferAction::Copy => TransferItemDisposition::Transferred,
                PlannedTransferAction::Replace => TransferItemDisposition::Replaced,
            };
            let copied_before = outcome.copied_bytes;
            match copy_item(
                device,
                item,
                disposition == TransferItemDisposition::Replaced,
                &mut buffer,
                |item_copied_bytes| {
                    progress(TransferProgress {
                        item_index,
                        total_items: plan.items.len(),
                        current_title: item.title.clone(),
                        item_copied_bytes,
                        item_total_bytes: item.source_bytes,
                        batch_copied_bytes: copied_before.saturating_add(item_copied_bytes),
                        batch_total_bytes: plan.required_bytes,
                    })
                },
            ) {
                Ok(hash) => {
                    outcome.copied_bytes = outcome.copied_bytes.saturating_add(item.source_bytes);
                    history.upsert(history_record(device, item, hash));
                    history_changed = true;
                    outcome.items.push(TransferItemOutcome {
                        book_id: item.book_id,
                        relative_path: item.relative_path.clone(),
                        disposition,
                        copied_bytes: item.source_bytes,
                    });
                }
                Err(error @ (DeviceError::Cancelled | DeviceError::Disconnected)) => {
                    return Err(error);
                }
                Err(error) => outcome.failures.push(TransferFailure {
                    book_id: item.book_id,
                    title: item.title.clone(),
                    message: error.to_string(),
                }),
            }
        }
        Ok(())
    })();
    if history_changed && let Err(error) = history.save() {
        if execution.is_ok() {
            outcome.history_error = Some(error.to_string());
        } else {
            tracing::warn!(error = %error, "could not persist completed device transfers after interruption");
        }
    }
    execution?;
    Ok(outcome)
}

fn history_record(
    device: &DeviceInfo,
    item: &PlannedTransferItem,
    hash: String,
) -> TransferHistoryRecord {
    TransferHistoryRecord {
        device_id: device.id.as_str().to_owned(),
        book_id: item.book_id.value(),
        source_asset_id: item.asset_id.value(),
        source_format: item.format.extension().to_owned(),
        device_relative_path: item.relative_path.clone(),
        source_bytes: item.source_bytes,
        file_hash: hash,
        transferred_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    }
}

fn copy_item(
    device: &DeviceInfo,
    item: &PlannedTransferItem,
    replace: bool,
    buffer: &mut [u8],
    mut progress: impl FnMut(u64) -> TransferControl,
) -> Result<String, DeviceError> {
    let destination = secure_destination(&device.mount_path, &item.relative_path)?;
    if replace {
        verify_tracked_destination(&destination)?;
    } else if destination.exists() {
        return Err(DeviceError::DestinationCollision(
            item.relative_path.clone(),
        ));
    }
    let source_metadata = item
        .source_path
        .metadata()
        .map_err(|error| DeviceError::io("inspect transfer source", &item.source_path, error))?;
    if !source_metadata.is_file() || source_metadata.len() != item.source_bytes {
        return Err(DeviceError::SourceChanged(item.source_path.clone()));
    }
    let mut source = File::open(&item.source_path)
        .map_err(|error| DeviceError::io("open transfer source", &item.source_path, error))?;
    let (temporary_path, mut temporary) = reserve_transfer_temporary(&destination)?;
    let mut cleanup = TemporaryTransfer::new(temporary_path);
    let (copied, hash) = copy_stream(
        &mut source,
        &mut temporary,
        buffer,
        item.source_bytes,
        &mut progress,
    )?;
    if copied != item.source_bytes {
        return Err(DeviceError::SourceChanged(item.source_path.clone()));
    }
    temporary
        .flush()
        .map_err(|error| DeviceError::io("flush device transfer", &cleanup.path, error))?;
    temporary
        .sync_all()
        .map_err(|error| DeviceError::io("synchronize device transfer", &cleanup.path, error))?;
    drop(temporary);
    validate_connected_root(&device.mount_path)?;
    publish_transfer(&cleanup.path, &destination, replace)?;
    cleanup.published = true;
    tracing::info!(
        device_id = %device.id,
        book_id = item.book_id.value(),
        bytes = copied,
        "completed device transfer"
    );
    Ok(hash)
}

fn copy_stream(
    source: &mut dyn Read,
    destination: &mut dyn Write,
    buffer: &mut [u8],
    expected_bytes: u64,
    progress: &mut dyn FnMut(u64) -> TransferControl,
) -> Result<(u64, String), DeviceError> {
    let mut copied = 0_u64;
    let mut hash = Sha256::new();
    loop {
        let read = source
            .read(buffer)
            .map_err(|error| DeviceError::SourceUnavailable(error.to_string()))?;
        if read == 0 {
            break;
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|error| DeviceError::Platform(format!("device write failed: {error}")))?;
        hash.update(&buffer[..read]);
        copied = copied
            .checked_add(u64::try_from(read).expect("copy buffer length fits u64"))
            .ok_or_else(|| DeviceError::UnsafePath("transfer size overflowed".to_owned()))?;
        if progress(copied) == TransferControl::Cancel {
            return Err(DeviceError::Cancelled);
        }
        if copied > expected_bytes {
            return Err(DeviceError::SourceChanged(PathBuf::from("source")));
        }
    }
    Ok((copied, hex_digest(hash.finalize().as_slice())))
}

fn validate_connected_root(root: &Path) -> Result<(), DeviceError> {
    if root.is_dir() && KoboDriver::has_kobo_marker(root) {
        Ok(())
    } else {
        Err(DeviceError::Disconnected)
    }
}

fn validate_relative_destination(relative: &Path) -> Result<(), DeviceError> {
    if !valid_relative_device_path(relative) || relative.components().count() != 3 {
        return Err(DeviceError::UnsafePath(relative.display().to_string()));
    }
    Ok(())
}

fn validate_existing_parent_chain(root: &Path, relative: &Path) -> Result<(), DeviceError> {
    let parent = relative
        .parent()
        .ok_or_else(|| DeviceError::UnsafePath(relative.display().to_string()))?;
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(DeviceError::UnsafePath(relative.display().to_string()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(DeviceError::UnsafePath(current.display().to_string()));
                }
                let canonical = fs::canonicalize(&current).map_err(|error| {
                    DeviceError::io("canonicalize device directory", &current, error)
                })?;
                if !canonical.starts_with(root) {
                    return Err(DeviceError::UnsafePath(current.display().to_string()));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(DeviceError::io("inspect device directory", &current, error)),
        }
    }
    Ok(())
}

fn secure_destination(root: &Path, relative: &Path) -> Result<PathBuf, DeviceError> {
    validate_relative_destination(relative)?;
    let parent = relative
        .parent()
        .expect("relative destination has a parent");
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(DeviceError::UnsafePath(relative.display().to_string()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => return Err(DeviceError::UnsafePath(current.display().to_string())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| {
                    DeviceError::io("create device book directory", &current, error)
                })?;
            }
            Err(error) => {
                return Err(DeviceError::io(
                    "inspect device book directory",
                    &current,
                    error,
                ));
            }
        }
        let canonical = fs::canonicalize(&current).map_err(|error| {
            DeviceError::io("canonicalize device book directory", &current, error)
        })?;
        if !canonical.starts_with(root) {
            return Err(DeviceError::UnsafePath(current.display().to_string()));
        }
    }
    let destination = root.join(relative);
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(DeviceError::UnsafePath(destination.display().to_string()));
    }
    Ok(destination)
}

fn verify_tracked_destination(destination: &Path) -> Result<(), DeviceError> {
    let metadata = fs::symlink_metadata(destination)
        .map_err(|error| DeviceError::io("inspect replacement destination", destination, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DeviceError::UnsafePath(destination.display().to_string()));
    }
    Ok(())
}

fn reserve_transfer_temporary(destination: &Path) -> Result<(PathBuf, File), DeviceError> {
    let parent = destination
        .parent()
        .expect("secure device destination has a parent");
    for _ in 0..100 {
        let id = NEXT_TRANSFER_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".lectern-transfer-{}-{id}.part",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(DeviceError::io(
                    "create partial device transfer",
                    path,
                    error,
                ));
            }
        }
    }
    Err(DeviceError::UnsafePath(
        "could not reserve a partial device-transfer path".to_owned(),
    ))
}

fn publish_transfer(
    temporary: &Path,
    destination: &Path,
    replace: bool,
) -> Result<(), DeviceError> {
    if replace {
        return replace_transfer(temporary, destination);
    }
    if destination.exists() {
        return Err(DeviceError::DestinationCollision(destination.to_path_buf()));
    }
    fs::rename(temporary, destination)
        .map_err(|error| DeviceError::io("publish device transfer", destination, error))
}

#[cfg(unix)]
fn replace_transfer(temporary: &Path, destination: &Path) -> Result<(), DeviceError> {
    fs::rename(temporary, destination)
        .map_err(|error| DeviceError::io("replace device transfer", destination, error))
}

#[cfg(windows)]
fn replace_transfer(temporary: &Path, destination: &Path) -> Result<(), DeviceError> {
    let parent = destination.parent().expect("destination has a parent");
    let id = NEXT_TRANSFER_TEMPORARY.fetch_add(1, Ordering::Relaxed);
    let backup = parent.join(format!(
        ".lectern-transfer-backup-{}-{id}.part",
        std::process::id()
    ));
    fs::rename(destination, &backup)
        .map_err(|error| DeviceError::io("reserve replaced device file", destination, error))?;
    if let Err(error) = fs::rename(temporary, destination) {
        let _restored = fs::rename(&backup, destination);
        return Err(DeviceError::io(
            "publish replacement device file",
            destination,
            error,
        ));
    }
    if let Err(error) = fs::remove_file(&backup) {
        tracing::warn!(
            error = %error,
            "could not remove replaced device-file backup"
        );
    }
    Ok(())
}

struct TemporaryTransfer {
    path: PathBuf,
    published: bool,
}

impl TemporaryTransfer {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for TemporaryTransfer {
    fn drop(&mut self) {
        if !self.published {
            let _removed = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn list_device_books(
    device: &DeviceInfo,
    history: &TransferHistoryStore,
) -> Result<Vec<DeviceBook>, DeviceError> {
    validate_connected_root(&device.mount_path)?;
    let books_root = device.mount_path.join("Books");
    match fs::symlink_metadata(&books_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(DeviceError::io(
                "inspect device Books directory",
                books_root,
                error,
            ));
        }
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(DeviceError::UnsafePath(books_root.display().to_string())),
    }
    let canonical_books = fs::canonicalize(&books_root).map_err(|error| {
        DeviceError::io("canonicalize device Books directory", &books_root, error)
    })?;
    if !canonical_books.starts_with(&device.mount_path) {
        return Err(DeviceError::UnsafePath(books_root.display().to_string()));
    }
    let mut books = Vec::new();
    for entry in WalkDir::new(&books_root)
        .max_depth(4)
        .follow_links(false)
        .into_iter()
        .filter_entry(safe_listing_entry)
    {
        let entry = entry.map_err(|error| {
            let path = error.path().unwrap_or(&books_root).to_path_buf();
            DeviceError::io(
                "list device books",
                path,
                error
                    .into_io_error()
                    .unwrap_or_else(|| io::Error::other("walk failed")),
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(format) = device_format_for_path(entry.path()) else {
            continue;
        };
        if books.len() >= DEVICE_BOOK_LIMIT {
            return Err(DeviceError::ListingLimit(DEVICE_BOOK_LIMIT));
        }
        let relative_path = entry
            .path()
            .strip_prefix(&device.mount_path)
            .map_err(|_| DeviceError::UnsafePath(entry.path().display().to_string()))?
            .to_path_buf();
        if !valid_relative_device_path(&relative_path) {
            return Err(DeviceError::UnsafePath(relative_path.display().to_string()));
        }
        let metadata = entry
            .metadata()
            .map_err(|error| DeviceError::io("inspect device book", entry.path(), error.into()))?;
        let record = history.find_path(&device.id, &relative_path);
        let correlated = record.filter(|record| record.source_bytes == metadata.len());
        books.push(DeviceBook {
            relative_path,
            format,
            bytes: metadata.len(),
            library_book_id: correlated.map(|record| BookId::new(record.book_id)),
            source_asset_id: correlated.map(|record| AssetId::new(record.source_asset_id)),
            managed_by_lectern: correlated.is_some(),
        });
    }
    books.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(books)
}

fn safe_listing_entry(entry: &DirEntry) -> bool {
    entry.depth() == 0 || !entry.file_type().is_symlink()
}

fn device_format_for_path(path: &Path) -> Option<DeviceFormat> {
    let name = path.file_name()?.to_string_lossy();
    if name.to_ascii_lowercase().ends_with(".kepub.epub") {
        return Some(DeviceFormat::Kepub);
    }
    let extension = path.extension()?.to_string_lossy();
    [
        ("epub", DeviceFormat::Epub),
        ("pdf", DeviceFormat::Pdf),
        ("cbz", DeviceFormat::Cbz),
        ("cbr", DeviceFormat::Cbr),
        ("txt", DeviceFormat::Txt),
    ]
    .into_iter()
    .find_map(|(candidate, format)| extension.eq_ignore_ascii_case(candidate).then_some(format))
}

pub(crate) fn remove_device_book(
    device: &DeviceInfo,
    history: &mut TransferHistoryStore,
    relative_path: &Path,
) -> Result<RemovalOutcome, DeviceError> {
    validate_connected_root(&device.mount_path)?;
    validate_relative_destination(relative_path)?;
    let record = history
        .find_path(&device.id, relative_path)
        .cloned()
        .ok_or_else(|| {
            DeviceError::UnsafePath("file is not owned by Lectern transfer history".to_owned())
        })?;
    validate_existing_parent_chain(&device.mount_path, relative_path)?;
    let target = device.mount_path.join(relative_path);
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            history.remove_path(&device.id, relative_path);
            if let Err(error) = history.save() {
                tracing::warn!(
                    device_id = %device.id,
                    error = %error,
                    "could not persist missing device-book reconciliation"
                );
            }
            return Ok(RemovalOutcome::AlreadyMissing);
        }
        Err(error) => {
            return Err(DeviceError::io(
                "inspect device book for removal",
                target,
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DeviceError::UnsafePath(target.display().to_string()));
    }
    if hash_file(&target)? != record.file_hash {
        return Err(DeviceError::ModifiedDeviceFile(relative_path.to_path_buf()));
    }
    fs::remove_file(&target)
        .map_err(|error| DeviceError::io("remove device book", &target, error))?;
    history.remove_path(&device.id, relative_path);
    if let Err(error) = history.save() {
        tracing::warn!(
            device_id = %device.id,
            error = %error,
            "could not persist removed device-book reconciliation"
        );
    }
    tracing::info!(
        device_id = %device.id,
        book_id = record.book_id,
        "removed device book"
    );
    Ok(RemovalOutcome::Removed)
}

fn hash_file(path: &Path) -> Result<String, DeviceError> {
    let mut file =
        File::open(path).map_err(|error| DeviceError::io("open file for hashing", path, error))?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    let mut hash = Sha256::new();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| DeviceError::io("hash file", path, error))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hex_digest(hash.finalize().as_slice()))
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        io::{self, Cursor, Write},
        sync::Arc,
    };

    use tempfile::tempdir;

    use super::{
        DuplicatePolicy, FormatPriority, PlannedTransferAction, TransferControl,
        TransferItemDisposition, build_plan, copy_stream, execute_plan, list_device_books,
        remove_device_book,
    };
    use crate::{
        DeviceConnectionState, DeviceFormat, DeviceId, DeviceInfo, DeviceKind, DeviceTransferBook,
        DeviceTransferSource, RemovalOutcome, history::TransferHistoryStore,
    };
    use lectern_core::{AssetId, BookFormat, BookId};

    struct FailingWriter(usize);

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                Err(io::Error::other("simulated device failure"))
            } else {
                let written = bytes.len().min(self.0);
                self.0 -= written;
                Ok(written)
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn device(root: &Path) -> DeviceInfo {
        fs::create_dir_all(root.join(".kobo")).unwrap();
        DeviceInfo {
            id: DeviceId::new("kobo:test"),
            kind: DeviceKind::Kobo,
            name: "Kobo eReader".to_owned(),
            manufacturer: "Kobo".to_owned(),
            model: None,
            mount_path: fs::canonicalize(root).unwrap(),
            volume_name: OsString::from("KOBOeReader"),
            total_bytes: 1024 * 1024,
            free_bytes: 1024 * 1024,
            state: DeviceConnectionState::Connected,
            supported_formats: Arc::from([DeviceFormat::Epub, DeviceFormat::Pdf]),
        }
    }

    fn book(source: &Path, format: BookFormat) -> DeviceTransferBook {
        DeviceTransferBook {
            book_id: BookId::new(1),
            title: "Title/../unsafe".to_owned(),
            authors: "Author:*?".to_owned(),
            sources: vec![DeviceTransferSource {
                asset_id: AssetId::new(2),
                format,
                path: source.to_path_buf(),
            }],
        }
    }

    use std::{fs, path::Path};

    #[test]
    fn plans_and_transfers_epub_then_recognizes_duplicate_and_removes_it() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, b"valid epub bytes").unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let history_path = fixture.path().join("history.json");
        let mut history = TransferHistoryStore::load(history_path.clone()).unwrap();
        let plan = build_plan(
            &device,
            &history,
            &[book(&source, BookFormat::Epub)],
            &FormatPriority::default(),
        )
        .unwrap();
        assert_eq!(plan.items[0].action, PlannedTransferAction::Copy);
        let outcome = execute_plan(&device, &mut history, &plan, DuplicatePolicy::Skip, |_| {
            TransferControl::Continue
        })
        .unwrap();
        assert_eq!(outcome.transferred_count(), 1);
        assert!(outcome.failures.is_empty());
        let relative = plan.items[0].relative_path.clone();
        assert!(device.mount_path.join(&relative).is_file());
        assert!(
            device
                .mount_path
                .join(".kobo/KoboReader.sqlite")
                .try_exists()
                .is_ok()
        );

        let history = TransferHistoryStore::load(history_path.clone()).unwrap();
        let duplicate = build_plan(
            &device,
            &history,
            &[book(&source, BookFormat::Epub)],
            &FormatPriority::default(),
        )
        .unwrap();
        assert_eq!(
            duplicate.items[0].action,
            PlannedTransferAction::AlreadyPresent
        );
        let listed = list_device_books(&device, &history).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].managed_by_lectern);

        let mut history = TransferHistoryStore::load(history_path).unwrap();
        assert_eq!(
            remove_device_book(&device, &mut history, &relative).unwrap(),
            RemovalOutcome::Removed
        );
        assert!(!device.mount_path.join(relative).exists());
    }

    #[test]
    fn supports_pdf_and_prefers_epub_deterministically() {
        let fixture = tempdir().unwrap();
        let epub = fixture.path().join("source.epub");
        let pdf = fixture.path().join("source.pdf");
        fs::write(&epub, b"epub").unwrap();
        fs::write(&pdf, b"pdf").unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let history = TransferHistoryStore::load(fixture.path().join("history.json")).unwrap();
        let mut candidate = book(&pdf, BookFormat::Pdf);
        candidate.sources.push(DeviceTransferSource {
            asset_id: AssetId::new(3),
            format: BookFormat::Epub,
            path: epub,
        });
        let plan = build_plan(&device, &history, &[candidate], &FormatPriority::default()).unwrap();
        assert_eq!(plan.items[0].format, DeviceFormat::Epub);

        let pdf_only = book(&pdf, BookFormat::Pdf);
        let pdf_plan =
            build_plan(&device, &history, &[pdf_only], &FormatPriority::default()).unwrap();
        assert_eq!(pdf_plan.items[0].format, DeviceFormat::Pdf);
        let mut history = history;
        let outcome = execute_plan(
            &device,
            &mut history,
            &pdf_plan,
            DuplicatePolicy::Skip,
            |_| TransferControl::Continue,
        )
        .unwrap();
        assert_eq!(outcome.transferred_count(), 1);
        assert!(
            device
                .mount_path
                .join(&pdf_plan.items[0].relative_path)
                .is_file()
        );
    }

    #[test]
    fn duplicate_metadata_gets_unique_paths_and_untracked_collision_is_skipped() {
        let fixture = tempdir().unwrap();
        let first_source = fixture.path().join("first.epub");
        let second_source = fixture.path().join("second.epub");
        fs::write(&first_source, b"first").unwrap();
        fs::write(&second_source, b"other").unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let history_path = fixture.path().join("history.json");
        let history = TransferHistoryStore::load(history_path).unwrap();
        let first = book(&first_source, BookFormat::Epub);
        let mut second = book(&second_source, BookFormat::Epub);
        second.book_id = BookId::new(3);
        second.sources[0].asset_id = AssetId::new(4);
        let plan = build_plan(
            &device,
            &history,
            &[first.clone(), second],
            &FormatPriority::default(),
        )
        .unwrap();
        assert_ne!(plan.items[0].relative_path, plan.items[1].relative_path);

        let destination = device.mount_path.join(&plan.items[0].relative_path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"wrong").unwrap();
        let collision =
            build_plan(&device, &history, &[first], &FormatPriority::default()).unwrap();
        assert_eq!(collision.items[0].action, PlannedTransferAction::Collision);
        let mut history = history;
        let outcome = execute_plan(
            &device,
            &mut history,
            &collision,
            DuplicatePolicy::Skip,
            |_| TransferControl::Continue,
        )
        .unwrap();
        assert_eq!(
            outcome.items[0].disposition,
            TransferItemDisposition::Skipped
        );
        assert_eq!(fs::read(destination).unwrap(), b"wrong");
    }

    #[test]
    fn reports_unsupported_missing_and_insufficient_space() {
        let fixture = tempdir().unwrap();
        let mount = tempdir().unwrap();
        let mut target = device(mount.path());
        let history = TransferHistoryStore::load(fixture.path().join("history.json")).unwrap();
        let unsupported = DeviceTransferBook {
            book_id: BookId::new(1),
            title: "Unsupported".to_owned(),
            authors: "Author".to_owned(),
            sources: Vec::new(),
        };
        let missing = book(&fixture.path().join("missing.epub"), BookFormat::Epub);
        let plan = build_plan(
            &target,
            &history,
            &[unsupported, missing],
            &FormatPriority::default(),
        )
        .unwrap();
        assert_eq!(plan.failures.len(), 2);

        let source = fixture.path().join("big.epub");
        fs::write(&source, vec![0_u8; 32]).unwrap();
        target.free_bytes = 4;
        assert!(matches!(
            build_plan(
                &target,
                &history,
                &[book(&source, BookFormat::Epub)],
                &FormatPriority::default()
            ),
            Err(crate::DeviceError::InsufficientSpace { .. })
        ));
    }

    #[test]
    fn cancellation_and_partial_write_failure_do_not_publish_outputs() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, vec![7_u8; 600_000]).unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let history_path = fixture.path().join("history.json");
        let mut history = TransferHistoryStore::load(history_path).unwrap();
        let plan = build_plan(
            &device,
            &history,
            &[book(&source, BookFormat::Epub)],
            &FormatPriority::default(),
        )
        .unwrap();
        assert!(matches!(
            execute_plan(&device, &mut history, &plan, DuplicatePolicy::Skip, |_| {
                TransferControl::Cancel
            }),
            Err(crate::DeviceError::Cancelled)
        ));
        assert!(
            !device
                .mount_path
                .join(&plan.items[0].relative_path)
                .exists()
        );
        let author = device.mount_path.join("Books/Author");
        if author.exists() {
            assert!(fs::read_dir(author).unwrap().next().is_none());
        }

        let mut reader = Cursor::new(vec![1_u8; 32]);
        let mut writer = FailingWriter(8);
        let mut buffer = [0_u8; 16];
        assert!(
            copy_stream(&mut reader, &mut writer, &mut buffer, 32, &mut |_| {
                TransferControl::Continue
            })
            .is_err()
        );
    }

    #[test]
    fn cancellation_preserves_history_for_completed_batch_items() {
        let fixture = tempdir().unwrap();
        let first_source = fixture.path().join("first.epub");
        let second_source = fixture.path().join("second.epub");
        fs::write(&first_source, vec![1_u8; 32]).unwrap();
        fs::write(&second_source, vec![2_u8; 600_000]).unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let history_path = fixture.path().join("history.json");
        let mut history = TransferHistoryStore::load(history_path.clone()).unwrap();
        let first = DeviceTransferBook {
            book_id: BookId::new(10),
            title: "First".to_owned(),
            authors: "Author".to_owned(),
            sources: vec![DeviceTransferSource {
                asset_id: AssetId::new(20),
                format: BookFormat::Epub,
                path: first_source,
            }],
        };
        let second = DeviceTransferBook {
            book_id: BookId::new(11),
            title: "Second".to_owned(),
            authors: "Author".to_owned(),
            sources: vec![DeviceTransferSource {
                asset_id: AssetId::new(21),
                format: BookFormat::Epub,
                path: second_source,
            }],
        };
        let plan = build_plan(
            &device,
            &history,
            &[first, second],
            &FormatPriority::default(),
        )
        .unwrap();
        assert!(matches!(
            execute_plan(
                &device,
                &mut history,
                &plan,
                DuplicatePolicy::Skip,
                |progress| if progress.item_index == 1 {
                    TransferControl::Cancel
                } else {
                    TransferControl::Continue
                }
            ),
            Err(crate::DeviceError::Cancelled)
        ));

        let history = TransferHistoryStore::load(history_path).unwrap();
        assert!(
            history
                .find_path(&device.id, &plan.items[0].relative_path)
                .is_some()
        );
        assert!(
            device
                .mount_path
                .join(&plan.items[0].relative_path)
                .is_file()
        );
        assert!(
            !device
                .mount_path
                .join(&plan.items[1].relative_path)
                .exists()
        );
    }

    #[test]
    fn disconnect_during_transfer_cleans_partial_file() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, vec![7_u8; 600_000]).unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let mut history = TransferHistoryStore::load(fixture.path().join("history.json")).unwrap();
        let plan = build_plan(
            &device,
            &history,
            &[book(&source, BookFormat::Epub)],
            &FormatPriority::default(),
        )
        .unwrap();
        let mut disconnected = false;
        let result = execute_plan(&device, &mut history, &plan, DuplicatePolicy::Skip, |_| {
            if !disconnected {
                fs::remove_dir_all(device.mount_path.join(".kobo")).unwrap();
                disconnected = true;
            }
            TransferControl::Continue
        });
        assert!(matches!(result, Err(crate::DeviceError::Disconnected)));
        assert!(
            !device
                .mount_path
                .join(&plan.items[0].relative_path)
                .exists()
        );
    }

    #[test]
    fn removal_reconciles_missing_files_and_rejects_internal_or_outside_paths() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, b"epub").unwrap();
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let history_path = fixture.path().join("history.json");
        let mut history = TransferHistoryStore::load(history_path).unwrap();
        let plan = build_plan(
            &device,
            &history,
            &[book(&source, BookFormat::Epub)],
            &FormatPriority::default(),
        )
        .unwrap();
        execute_plan(&device, &mut history, &plan, DuplicatePolicy::Skip, |_| {
            TransferControl::Continue
        })
        .unwrap();
        let relative = &plan.items[0].relative_path;
        fs::remove_file(device.mount_path.join(relative)).unwrap();
        assert_eq!(
            remove_device_book(&device, &mut history, relative).unwrap(),
            RemovalOutcome::AlreadyMissing
        );
        assert!(
            remove_device_book(&device, &mut history, Path::new("Books/../outside.epub")).is_err()
        );
        assert!(
            remove_device_book(&device, &mut history, Path::new(".kobo/KoboReader.sqlite"))
                .is_err()
        );
    }

    #[test]
    fn unavailable_local_history_does_not_block_transfer_or_removal() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, b"epub").unwrap();
        let history_path = fixture.path().join("history.json");
        fs::write(&history_path, b"future or corrupt history").unwrap();
        let mut history = TransferHistoryStore::load_best_effort(history_path.clone());
        let mount = tempdir().unwrap();
        let device = device(mount.path());
        let plan = build_plan(
            &device,
            &history,
            &[book(&source, BookFormat::Epub)],
            &FormatPriority::default(),
        )
        .unwrap();
        let outcome = execute_plan(&device, &mut history, &plan, DuplicatePolicy::Skip, |_| {
            TransferControl::Continue
        })
        .unwrap();
        assert_eq!(outcome.transferred_count(), 1);
        assert!(outcome.history_error.is_some());
        assert_eq!(
            remove_device_book(&device, &mut history, &plan.items[0].relative_path).unwrap(),
            RemovalOutcome::Removed
        );
        assert_eq!(
            fs::read(history_path).unwrap(),
            b"future or corrupt history"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_for_transfer_and_removal() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, b"epub").unwrap();
        let mount = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let device = device(mount.path());
        std::os::unix::fs::symlink(outside.path(), device.mount_path.join("Books")).unwrap();
        let history = TransferHistoryStore::load(fixture.path().join("history.json")).unwrap();
        assert!(
            build_plan(
                &device,
                &history,
                &[book(&source, BookFormat::Epub)],
                &FormatPriority::default()
            )
            .is_err()
        );
    }
}
