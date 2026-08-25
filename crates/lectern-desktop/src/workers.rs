use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use eframe::egui;
use image::{ImageReader, Limits};
use lectern_core::{
    AssetHealthReport, AssetId, Book, BookFormat, BookId, BookSummary, ImportProgress,
    ImportSummary, LibraryQuery, LibraryService,
    organisation::{
        BookEdit, ContributorId, ContributorUsage, SeriesId, SeriesUsage, TagId, TagUsage,
    },
};
use lectern_desktop::export::{
    ExportControl, ExportError, ExportOutcome, ExportProgress, OverwritePolicy, export_file,
};
use lectern_service::SqliteLibraryService;

const COVER_QUEUE_CAPACITY: usize = 128;
const QUERY_QUEUE_CAPACITY: usize = 1;
const MIN_COVER_WORKERS: usize = 2;
const MAX_COVER_WORKERS: usize = 4;
const MAX_STORED_COVER_DIMENSION: u32 = 1_024;
const MAX_STORED_COVER_ALLOCATION: u64 = 16 * 1024 * 1024;
const EXPORT_PROGRESS_BYTES: u64 = 16 * 1024 * 1024;
const EXPORT_PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct QueryRequest {
    pub(crate) generation: u64,
    pub(crate) query: LibraryQuery,
    pub(crate) offset: u64,
    pub(crate) limit: u32,
    pub(crate) include_total: bool,
}

pub(crate) struct QueryResult {
    pub(crate) total: Option<u64>,
    pub(crate) books: Vec<BookSummary>,
}

pub(crate) enum QueryQueueResult {
    Queued,
    Full,
    Disconnected,
}

pub(crate) struct ImportRequest {
    pub(crate) roots: Vec<PathBuf>,
}

pub(crate) struct ExportRequest {
    pub(crate) asset_id: AssetId,
    pub(crate) source: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) overwrite: OverwritePolicy,
    pub(crate) cancelled: Arc<AtomicBool>,
}

enum MetadataRequest {
    Load(BookId),
    Save(BookEdit),
    Remove { id: BookId, title: String },
}

enum AutocompleteRequest {
    Contributors {
        generation: u64,
        row_id: u64,
        prefix: String,
        selected: Vec<ContributorId>,
    },
    Series {
        generation: u64,
        prefix: String,
        selected: Vec<SeriesId>,
    },
    Tags {
        generation: u64,
        prefix: String,
        selected: Vec<TagId>,
    },
    FacetContributors {
        generation: u64,
        prefix: String,
        selected: Vec<ContributorId>,
    },
    FacetSeries {
        generation: u64,
        prefix: String,
        selected: Vec<SeriesId>,
    },
    FacetTags {
        generation: u64,
        prefix: String,
        selected: Vec<TagId>,
    },
}

enum AssetMaintenanceRequest {
    Scan,
    Attach {
        book_id: BookId,
        format: BookFormat,
        path: PathBuf,
    },
    Detach {
        asset_id: AssetId,
    },
    Relink {
        book_id: BookId,
        asset_id: AssetId,
        format: BookFormat,
        replacement_path: PathBuf,
    },
    Replace {
        book_id: BookId,
        asset_id: AssetId,
        format: BookFormat,
        replacement_path: PathBuf,
    },
}

pub(crate) struct DecodedCover {
    pub(crate) size: [usize; 2],
    pub(crate) rgba: Vec<u8>,
}

pub(crate) enum WorkerEvent {
    QueryFinished {
        generation: u64,
        offset: u64,
        result: Result<QueryResult, String>,
    },
    QueryDiscarded {
        generation: u64,
        offset: u64,
    },
    CoverFinished {
        id: BookId,
        result: Result<Option<DecodedCover>, String>,
    },
    ImportProgress(ImportProgress),
    ImportFinished(Result<ImportSummary, String>),
    BookLoaded {
        id: BookId,
        result: Result<Option<Book>, String>,
    },
    BookSaved {
        id: BookId,
        result: Result<Book, String>,
    },
    ContributorSuggestions {
        generation: u64,
        row_id: u64,
        result: Result<Vec<ContributorUsage>, String>,
    },
    SeriesSuggestions {
        generation: u64,
        result: Result<Vec<SeriesUsage>, String>,
    },
    TagSuggestions {
        generation: u64,
        result: Result<Vec<TagUsage>, String>,
    },
    FacetContributorSuggestions {
        generation: u64,
        result: Result<Vec<ContributorUsage>, String>,
    },
    FacetSeriesSuggestions {
        generation: u64,
        result: Result<Vec<SeriesUsage>, String>,
    },
    FacetTagSuggestions {
        generation: u64,
        result: Result<Vec<TagUsage>, String>,
    },
    BookRemoved {
        id: BookId,
        title: String,
        result: Result<bool, String>,
    },
    AssetHealthScanned(Result<AssetHealthReport, String>),
    AssetAttached {
        book_id: BookId,
        format: BookFormat,
        result: Result<(), String>,
    },
    AssetDetached {
        asset_id: AssetId,
        result: Result<BookId, String>,
    },
    AssetRelinked {
        book_id: BookId,
        asset_id: AssetId,
        result: Result<(), String>,
    },
    AssetReplaced {
        book_id: BookId,
        asset_id: AssetId,
        replacement_path: PathBuf,
        result: Result<(), String>,
    },
    ExportProgress {
        asset_id: AssetId,
        destination: PathBuf,
        progress: ExportProgress,
    },
    ExportFinished {
        asset_id: AssetId,
        source: PathBuf,
        destination: PathBuf,
        result: Result<ExportOutcome, ExportError>,
    },
    Error(String),
}

pub(crate) struct WorkerSet {
    query_sender: Sender<QueryRequest>,
    cover_sender: Sender<BookId>,
    import_sender: Sender<ImportRequest>,
    metadata_sender: Sender<MetadataRequest>,
    autocomplete_sender: Sender<AutocompleteRequest>,
    asset_maintenance_sender: Sender<AssetMaintenanceRequest>,
    export_sender: Sender<ExportRequest>,
    event_receiver: Receiver<WorkerEvent>,
}

impl WorkerSet {
    pub(crate) fn spawn(database_path: &Path, context: &egui::Context) -> Self {
        let (query_sender, query_receiver) = bounded(QUERY_QUEUE_CAPACITY);
        let (cover_sender, cover_receiver) = bounded(COVER_QUEUE_CAPACITY);
        let (import_sender, import_receiver) = bounded(1);
        let (metadata_sender, metadata_receiver) = unbounded();
        let (autocomplete_sender, autocomplete_receiver) = unbounded();
        let (asset_maintenance_sender, asset_maintenance_receiver) = bounded(1);
        let (export_sender, export_receiver) = bounded(1);
        let (event_sender, event_receiver) = unbounded();

        spawn_query_worker(
            database_path.to_path_buf(),
            query_receiver,
            event_sender.clone(),
            context.clone(),
        );

        let worker_count = thread::available_parallelism()
            .map_or(MIN_COVER_WORKERS, std::num::NonZero::get)
            .clamp(MIN_COVER_WORKERS, MAX_COVER_WORKERS);
        for index in 0..worker_count {
            spawn_cover_worker(
                index,
                database_path.to_path_buf(),
                cover_receiver.clone(),
                event_sender.clone(),
                context.clone(),
            );
        }
        spawn_import_worker(
            database_path.to_path_buf(),
            import_receiver,
            event_sender.clone(),
            context.clone(),
        );
        spawn_metadata_worker(
            database_path.to_path_buf(),
            metadata_receiver,
            event_sender.clone(),
            context.clone(),
        );
        spawn_autocomplete_worker(
            database_path.to_path_buf(),
            autocomplete_receiver,
            event_sender.clone(),
            context.clone(),
        );
        spawn_asset_maintenance_worker(
            database_path.to_path_buf(),
            asset_maintenance_receiver,
            event_sender.clone(),
            context.clone(),
        );
        spawn_export_worker(export_receiver, event_sender, context.clone());

        Self {
            query_sender,
            cover_sender,
            import_sender,
            metadata_sender,
            autocomplete_sender,
            asset_maintenance_sender,
            export_sender,
            event_receiver,
        }
    }

    pub(crate) fn query(&self, request: QueryRequest) -> QueryQueueResult {
        match self.query_sender.try_send(request) {
            Ok(()) => QueryQueueResult::Queued,
            Err(TrySendError::Full(_)) => QueryQueueResult::Full,
            Err(TrySendError::Disconnected(_)) => QueryQueueResult::Disconnected,
        }
    }

    pub(crate) fn load_cover(&self, id: BookId) -> bool {
        match self.cover_sender.try_send(id) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn import(&self, request: ImportRequest) -> bool {
        self.import_sender.try_send(request).is_ok()
    }

    pub(crate) fn load_book(&self, id: BookId) -> bool {
        self.metadata_sender.send(MetadataRequest::Load(id)).is_ok()
    }

    pub(crate) fn save_book(&self, edit: BookEdit) -> bool {
        self.metadata_sender
            .send(MetadataRequest::Save(edit))
            .is_ok()
    }

    pub(crate) fn autocomplete_contributors(
        &self,
        generation: u64,
        row_id: u64,
        prefix: String,
        selected: Vec<ContributorId>,
    ) -> bool {
        self.autocomplete_sender
            .send(AutocompleteRequest::Contributors {
                generation,
                row_id,
                prefix,
                selected,
            })
            .is_ok()
    }

    pub(crate) fn autocomplete_series(
        &self,
        generation: u64,
        prefix: String,
        selected: Vec<SeriesId>,
    ) -> bool {
        self.autocomplete_sender
            .send(AutocompleteRequest::Series {
                generation,
                prefix,
                selected,
            })
            .is_ok()
    }

    pub(crate) fn autocomplete_tags(
        &self,
        generation: u64,
        prefix: String,
        selected: Vec<TagId>,
    ) -> bool {
        self.autocomplete_sender
            .send(AutocompleteRequest::Tags {
                generation,
                prefix,
                selected,
            })
            .is_ok()
    }

    pub(crate) fn autocomplete_facet_contributors(
        &self,
        generation: u64,
        prefix: String,
        selected: Vec<ContributorId>,
    ) -> bool {
        self.autocomplete_sender
            .send(AutocompleteRequest::FacetContributors {
                generation,
                prefix,
                selected,
            })
            .is_ok()
    }

    pub(crate) fn autocomplete_facet_series(
        &self,
        generation: u64,
        prefix: String,
        selected: Vec<SeriesId>,
    ) -> bool {
        self.autocomplete_sender
            .send(AutocompleteRequest::FacetSeries {
                generation,
                prefix,
                selected,
            })
            .is_ok()
    }

    pub(crate) fn autocomplete_facet_tags(
        &self,
        generation: u64,
        prefix: String,
        selected: Vec<TagId>,
    ) -> bool {
        self.autocomplete_sender
            .send(AutocompleteRequest::FacetTags {
                generation,
                prefix,
                selected,
            })
            .is_ok()
    }

    pub(crate) fn remove_book(&self, id: BookId, title: String) -> bool {
        self.metadata_sender
            .send(MetadataRequest::Remove { id, title })
            .is_ok()
    }

    pub(crate) fn rescan_reference_assets(&self) -> bool {
        self.asset_maintenance_sender
            .try_send(AssetMaintenanceRequest::Scan)
            .is_ok()
    }

    pub(crate) fn attach_reference_asset(
        &self,
        book_id: BookId,
        format: BookFormat,
        path: PathBuf,
    ) -> bool {
        self.asset_maintenance_sender
            .try_send(AssetMaintenanceRequest::Attach {
                book_id,
                format,
                path,
            })
            .is_ok()
    }

    pub(crate) fn detach_asset(&self, asset_id: AssetId) -> bool {
        self.asset_maintenance_sender
            .try_send(AssetMaintenanceRequest::Detach { asset_id })
            .is_ok()
    }

    pub(crate) fn relink_reference_asset(
        &self,
        book_id: BookId,
        asset_id: AssetId,
        format: BookFormat,
        replacement_path: PathBuf,
    ) -> bool {
        self.asset_maintenance_sender
            .try_send(AssetMaintenanceRequest::Relink {
                book_id,
                asset_id,
                format,
                replacement_path,
            })
            .is_ok()
    }

    pub(crate) fn replace_reference_asset(
        &self,
        book_id: BookId,
        asset_id: AssetId,
        format: BookFormat,
        replacement_path: PathBuf,
    ) -> bool {
        self.asset_maintenance_sender
            .try_send(AssetMaintenanceRequest::Replace {
                book_id,
                asset_id,
                format,
                replacement_path,
            })
            .is_ok()
    }

    pub(crate) fn export(&self, request: ExportRequest) -> bool {
        self.export_sender.try_send(request).is_ok()
    }

    pub(crate) fn next_event(&self) -> Option<WorkerEvent> {
        self.event_receiver.try_recv().ok()
    }
}

fn spawn_export_worker(
    receiver: Receiver<ExportRequest>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name("lectern-export".into())
        .spawn(move || export_worker(&receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start export worker: {error}")),
        );
    }
}

fn export_worker(
    receiver: &Receiver<ExportRequest>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    while let Ok(request) = receiver.recv() {
        let result = if request.cancelled.load(Ordering::Relaxed) {
            Err(ExportError::Cancelled)
        } else {
            let mut last_progress_bytes = 0;
            let mut last_progress_at = Instant::now();
            export_file(
                &request.source,
                &request.destination,
                request.overwrite,
                |progress| {
                    let now = Instant::now();
                    let should_publish = should_publish_export_progress(
                        progress,
                        last_progress_bytes,
                        now.duration_since(last_progress_at),
                    );
                    let progress_connected = !should_publish
                        || publish(
                            events,
                            context,
                            WorkerEvent::ExportProgress {
                                asset_id: request.asset_id,
                                destination: request.destination.clone(),
                                progress,
                            },
                        );
                    if should_publish {
                        last_progress_bytes = progress.copied_bytes;
                        last_progress_at = now;
                    }
                    if !progress_connected || request.cancelled.load(Ordering::Relaxed) {
                        ExportControl::Cancel
                    } else {
                        ExportControl::Continue
                    }
                },
            )
        };
        if !publish(
            events,
            context,
            WorkerEvent::ExportFinished {
                asset_id: request.asset_id,
                source: request.source,
                destination: request.destination,
                result,
            },
        ) {
            break;
        }
    }
}

fn should_publish_export_progress(
    progress: ExportProgress,
    last_progress_bytes: u64,
    elapsed: Duration,
) -> bool {
    last_progress_bytes == 0
        || progress.copied_bytes == progress.total_bytes
        || progress.copied_bytes.saturating_sub(last_progress_bytes) >= EXPORT_PROGRESS_BYTES
        || elapsed >= EXPORT_PROGRESS_INTERVAL
}

fn spawn_metadata_worker(
    database_path: PathBuf,
    receiver: Receiver<MetadataRequest>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name("lectern-metadata".into())
        .spawn(move || metadata_worker(&database_path, &receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start metadata worker: {error}")),
        );
    }
}

fn metadata_worker(
    database_path: &PathBuf,
    receiver: &Receiver<MetadataRequest>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    let mut service = match SqliteLibraryService::open(database_path) {
        Ok(service) => service,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(request) = receiver.recv() {
        let published = match request {
            MetadataRequest::Load(id) => {
                let result = service.get_book(id).map_err(|error| error.to_string());
                publish(events, context, WorkerEvent::BookLoaded { id, result })
            }
            MetadataRequest::Save(edit) => {
                let id = edit.id;
                let result = service
                    .update_metadata(&edit)
                    .map_err(|error| error.to_string())
                    .and_then(|()| service.get_book(id).map_err(|error| error.to_string()))
                    .and_then(|book| {
                        book.ok_or_else(|| "saved book disappeared before reload".to_owned())
                    });
                publish(events, context, WorkerEvent::BookSaved { id, result })
            }
            MetadataRequest::Remove { id, title } => {
                let result = service.remove_book(id).map_err(|error| error.to_string());
                publish(
                    events,
                    context,
                    WorkerEvent::BookRemoved { id, title, result },
                )
            }
        };
        if !published {
            break;
        }
    }
}

fn spawn_autocomplete_worker(
    database_path: PathBuf,
    receiver: Receiver<AutocompleteRequest>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name("lectern-autocomplete".into())
        .spawn(move || autocomplete_worker(&database_path, &receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start autocomplete worker: {error}")),
        );
    }
}

fn autocomplete_worker(
    database_path: &PathBuf,
    receiver: &Receiver<AutocompleteRequest>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    let mut service = match SqliteLibraryService::open(database_path) {
        Ok(service) => service,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(mut request) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            request = newer;
        }
        let published = match request {
            AutocompleteRequest::Contributors {
                generation,
                row_id,
                prefix,
                selected,
            } => publish(
                events,
                context,
                WorkerEvent::ContributorSuggestions {
                    generation,
                    row_id,
                    result: service
                        .autocomplete_contributors(&prefix, &selected, 50)
                        .map_err(|error| error.to_string()),
                },
            ),
            AutocompleteRequest::Series {
                generation,
                prefix,
                selected,
            } => publish(
                events,
                context,
                WorkerEvent::SeriesSuggestions {
                    generation,
                    result: service
                        .autocomplete_series(&prefix, &selected, 50)
                        .map_err(|error| error.to_string()),
                },
            ),
            AutocompleteRequest::Tags {
                generation,
                prefix,
                selected,
            } => publish(
                events,
                context,
                WorkerEvent::TagSuggestions {
                    generation,
                    result: service
                        .autocomplete_tags(&prefix, &selected, 50)
                        .map_err(|error| error.to_string()),
                },
            ),
            AutocompleteRequest::FacetContributors {
                generation,
                prefix,
                selected,
            } => publish(
                events,
                context,
                WorkerEvent::FacetContributorSuggestions {
                    generation,
                    result: service
                        .autocomplete_contributors(&prefix, &selected, 50)
                        .map_err(|error| error.to_string()),
                },
            ),
            AutocompleteRequest::FacetSeries {
                generation,
                prefix,
                selected,
            } => publish(
                events,
                context,
                WorkerEvent::FacetSeriesSuggestions {
                    generation,
                    result: service
                        .autocomplete_series(&prefix, &selected, 50)
                        .map_err(|error| error.to_string()),
                },
            ),
            AutocompleteRequest::FacetTags {
                generation,
                prefix,
                selected,
            } => publish(
                events,
                context,
                WorkerEvent::FacetTagSuggestions {
                    generation,
                    result: service
                        .autocomplete_tags(&prefix, &selected, 50)
                        .map_err(|error| error.to_string()),
                },
            ),
        };
        if !published {
            break;
        }
    }
}

fn spawn_asset_maintenance_worker(
    database_path: PathBuf,
    receiver: Receiver<AssetMaintenanceRequest>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name("lectern-asset-maintenance".into())
        .spawn(move || asset_maintenance_worker(&database_path, &receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start asset maintenance worker: {error}")),
        );
    }
}

fn asset_maintenance_worker(
    database_path: &PathBuf,
    receiver: &Receiver<AssetMaintenanceRequest>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    let mut service = match SqliteLibraryService::open(database_path) {
        Ok(service) => service,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(request) = receiver.recv() {
        let published = match request {
            AssetMaintenanceRequest::Scan => publish(
                events,
                context,
                WorkerEvent::AssetHealthScanned(
                    service.scan_assets().map_err(|error| error.to_string()),
                ),
            ),
            AssetMaintenanceRequest::Attach {
                book_id,
                format,
                path,
            } => {
                let result = service
                    .attach_asset(book_id, format, &path)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                publish(
                    events,
                    context,
                    WorkerEvent::AssetAttached {
                        book_id,
                        format,
                        result,
                    },
                )
            }
            AssetMaintenanceRequest::Detach { asset_id } => publish(
                events,
                context,
                WorkerEvent::AssetDetached {
                    asset_id,
                    result: service
                        .detach_asset(asset_id)
                        .map_err(|error| error.to_string()),
                },
            ),
            AssetMaintenanceRequest::Relink {
                book_id,
                asset_id,
                format,
                replacement_path,
            } => {
                let result = service
                    .relink_asset(asset_id, format, &replacement_path)
                    .map_err(|error| error.to_string());
                publish(
                    events,
                    context,
                    WorkerEvent::AssetRelinked {
                        book_id,
                        asset_id,
                        result,
                    },
                )
            }
            AssetMaintenanceRequest::Replace {
                book_id,
                asset_id,
                format,
                replacement_path,
            } => {
                let result = service
                    .replace_asset(asset_id, format, &replacement_path)
                    .map_err(|error| error.to_string());
                publish(
                    events,
                    context,
                    WorkerEvent::AssetReplaced {
                        book_id,
                        asset_id,
                        replacement_path,
                        result,
                    },
                )
            }
        };
        if !published {
            break;
        }
    }
}

fn spawn_import_worker(
    database_path: PathBuf,
    receiver: Receiver<ImportRequest>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name("lectern-import".into())
        .spawn(move || import_worker(&database_path, &receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start import worker: {error}")),
        );
    }
}

fn import_worker(
    database_path: &PathBuf,
    receiver: &Receiver<ImportRequest>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    while let Ok(request) = receiver.recv() {
        let result = SqliteLibraryService::open(database_path).and_then(|mut service| {
            service.import_publications(&request.roots, &mut |progress| {
                publish(events, context, WorkerEvent::ImportProgress(progress));
            })
        });
        let result = result.map_err(|error| error.to_string());
        if !publish(events, context, WorkerEvent::ImportFinished(result)) {
            break;
        }
    }
}

fn spawn_query_worker(
    database_path: PathBuf,
    receiver: Receiver<QueryRequest>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name("lectern-library-query".into())
        .spawn(move || query_worker(&database_path, &receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start library worker: {error}")),
        );
    }
}

fn query_worker(
    database_path: &PathBuf,
    receiver: &Receiver<QueryRequest>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    let mut service = match SqliteLibraryService::open(database_path) {
        Ok(service) => service,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(mut request) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            if newer.generation == request.generation
                && !publish(
                    events,
                    context,
                    WorkerEvent::QueryDiscarded {
                        generation: request.generation,
                        offset: request.offset,
                    },
                )
            {
                return;
            }
            request = newer;
        }
        let result = if request.include_total {
            service
                .query_library_page(&request.query, request.offset, request.limit)
                .map(|page| QueryResult {
                    total: Some(page.total),
                    books: page.books,
                })
        } else {
            service
                .query_library_window(&request.query, request.offset, request.limit)
                .map(|books| QueryResult { total: None, books })
        }
        .map_err(|error| error.to_string());
        if !publish(
            events,
            context,
            WorkerEvent::QueryFinished {
                generation: request.generation,
                offset: request.offset,
                result,
            },
        ) {
            break;
        }
    }
}

fn spawn_cover_worker(
    index: usize,
    database_path: PathBuf,
    receiver: Receiver<BookId>,
    events: Sender<WorkerEvent>,
    context: egui::Context,
) {
    let failure_events = events.clone();
    let failure_context = context.clone();
    let result = thread::Builder::new()
        .name(format!("lectern-cover-{index}"))
        .spawn(move || cover_worker(&database_path, &receiver, &events, &context));
    if let Err(error) = result {
        publish(
            &failure_events,
            &failure_context,
            WorkerEvent::Error(format!("Could not start cover worker: {error}")),
        );
    }
}

fn cover_worker(
    database_path: &PathBuf,
    receiver: &Receiver<BookId>,
    events: &Sender<WorkerEvent>,
    context: &egui::Context,
) {
    let mut service = match SqliteLibraryService::open(database_path) {
        Ok(service) => service,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(id) = receiver.recv() {
        let result = service
            .load_cover(id)
            .map_err(|error| error.to_string())
            .and_then(|cover| cover.map(|bytes| decode_cover(&bytes)).transpose());
        if !publish(events, context, WorkerEvent::CoverFinished { id, result }) {
            break;
        }
    }
}

fn decode_cover(encoded: &[u8]) -> Result<DecodedCover, String> {
    let mut reader = ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_STORED_COVER_DIMENSION);
    limits.max_image_height = Some(MAX_STORED_COVER_DIMENSION);
    limits.max_alloc = Some(MAX_STORED_COVER_ALLOCATION);
    reader.limits(limits);
    let rgba = reader
        .decode()
        .map_err(|error| error.to_string())?
        .into_rgba8();
    let width = usize::try_from(rgba.width()).map_err(|error| error.to_string())?;
    let height = usize::try_from(rgba.height()).map_err(|error| error.to_string())?;
    Ok(DecodedCover {
        size: [width, height],
        rgba: rgba.into_raw(),
    })
}

fn publish(events: &Sender<WorkerEvent>, context: &egui::Context, event: WorkerEvent) -> bool {
    if events.send(event).is_err() {
        return false;
    }
    context.request_repaint();
    true
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::Duration,
    };

    use crossbeam_channel::unbounded;
    use eframe::egui;
    use lectern_core::AssetId;
    use lectern_desktop::export::{ExportProgress, OverwritePolicy};

    use super::{
        EXPORT_PROGRESS_BYTES, ExportRequest, WorkerEvent, export_worker,
        should_publish_export_progress,
    };

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "lectern-export-worker-test-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create worker test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove worker test directory");
        }
    }

    #[test]
    fn export_progress_is_prompt_then_throttled_by_bytes_or_time() {
        let first = ExportProgress {
            copied_bytes: 256 * 1024,
            total_bytes: 256 * 1024 * 1024,
        };
        assert!(should_publish_export_progress(first, 0, Duration::ZERO));
        assert!(!should_publish_export_progress(
            first,
            first.copied_bytes,
            Duration::from_millis(10),
        ));
        assert!(should_publish_export_progress(
            ExportProgress {
                copied_bytes: first.copied_bytes + EXPORT_PROGRESS_BYTES,
                total_bytes: first.total_bytes,
            },
            first.copied_bytes,
            Duration::from_millis(10),
        ));
        assert!(should_publish_export_progress(
            ExportProgress {
                copied_bytes: first.copied_bytes + 1,
                total_bytes: first.total_bytes,
            },
            first.copied_bytes,
            Duration::from_millis(50),
        ));
        assert!(should_publish_export_progress(
            ExportProgress {
                copied_bytes: first.total_bytes,
                total_bytes: first.total_bytes,
            },
            first.copied_bytes,
            Duration::ZERO,
        ));
    }

    #[test]
    fn export_worker_reports_progress_and_exact_completion() {
        let directory = TestDirectory::new();
        let source = directory.0.join("source.epub");
        let destination = directory.0.join("copy.epub");
        let bytes = vec![19_u8; 2 * 1024 * 1024];
        fs::write(&source, &bytes).expect("write source");
        let (requests, request_receiver) = unbounded();
        let (events, event_receiver) = unbounded();
        requests
            .send(ExportRequest {
                asset_id: AssetId::new(7),
                source,
                destination: destination.clone(),
                overwrite: OverwritePolicy::Deny,
                cancelled: Arc::new(AtomicBool::new(false)),
            })
            .expect("queue export");
        drop(requests);

        export_worker(&request_receiver, &events, &egui::Context::default());

        let published = event_receiver.try_iter().collect::<Vec<_>>();
        assert!(published.iter().any(|event| matches!(
            event,
            WorkerEvent::ExportProgress { progress, .. } if progress.copied_bytes > 0
        )));
        assert!(published.iter().any(|event| matches!(
            event,
            WorkerEvent::ExportFinished { result: Ok(outcome), .. }
                if outcome.copied_bytes
                    == u64::try_from(bytes.len()).expect("test byte length fits u64")
        )));
        assert_eq!(fs::read(destination).expect("read export"), bytes);
    }
}
