use std::{
    io::Cursor,
    path::{Path, PathBuf},
    thread,
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use eframe::egui;
use image::{ImageReader, Limits};
use lectern_core::{
    AssetHealthReport, AssetId, Book, BookFormat, BookId, BookSummary, LibraryQuery,
};
use lectern_import::{ImportProgress, ImportSummary, import_paths, validate_publication};
use lectern_storage::LibraryDatabase;

const COVER_QUEUE_CAPACITY: usize = 128;
const QUERY_QUEUE_CAPACITY: usize = 1;
const MIN_COVER_WORKERS: usize = 2;
const MAX_COVER_WORKERS: usize = 4;
const MAX_STORED_COVER_DIMENSION: u32 = 1_024;
const MAX_STORED_COVER_ALLOCATION: u64 = 16 * 1024 * 1024;

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

enum MetadataRequest {
    Load(BookId),
    Save(Book),
    Remove { id: BookId, title: String },
}

enum AssetMaintenanceRequest {
    Scan,
    Relink {
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
        book: Book,
        result: Result<(), String>,
    },
    BookRemoved {
        id: BookId,
        title: String,
        result: Result<bool, String>,
    },
    AssetHealthScanned(Result<AssetHealthReport, String>),
    AssetRelinked {
        book_id: BookId,
        asset_id: AssetId,
        result: Result<(), String>,
    },
    Error(String),
}

pub(crate) struct WorkerSet {
    query_sender: Sender<QueryRequest>,
    cover_sender: Sender<BookId>,
    import_sender: Sender<ImportRequest>,
    metadata_sender: Sender<MetadataRequest>,
    asset_maintenance_sender: Sender<AssetMaintenanceRequest>,
    event_receiver: Receiver<WorkerEvent>,
}

impl WorkerSet {
    pub(crate) fn spawn(database_path: &Path, context: &egui::Context) -> Self {
        let (query_sender, query_receiver) = bounded(QUERY_QUEUE_CAPACITY);
        let (cover_sender, cover_receiver) = bounded(COVER_QUEUE_CAPACITY);
        let (import_sender, import_receiver) = bounded(1);
        let (metadata_sender, metadata_receiver) = unbounded();
        let (asset_maintenance_sender, asset_maintenance_receiver) = bounded(1);
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
        spawn_asset_maintenance_worker(
            database_path.to_path_buf(),
            asset_maintenance_receiver,
            event_sender,
            context.clone(),
        );

        Self {
            query_sender,
            cover_sender,
            import_sender,
            metadata_sender,
            asset_maintenance_sender,
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

    pub(crate) fn save_book(&self, book: Book) -> bool {
        self.metadata_sender
            .send(MetadataRequest::Save(book))
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

    pub(crate) fn next_event(&self) -> Option<WorkerEvent> {
        self.event_receiver.try_recv().ok()
    }
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
    let database = match LibraryDatabase::open(database_path) {
        Ok(database) => database,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(request) = receiver.recv() {
        let published = match request {
            MetadataRequest::Load(id) => {
                let result = database.get_book(id).map_err(|error| error.to_string());
                publish(events, context, WorkerEvent::BookLoaded { id, result })
            }
            MetadataRequest::Save(book) => {
                let result = database.save_book(&book).map_err(|error| error.to_string());
                publish(events, context, WorkerEvent::BookSaved { book, result })
            }
            MetadataRequest::Remove { id, title } => {
                let result = database.remove_book(id).map_err(|error| error.to_string());
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
    let mut database = match LibraryDatabase::open(database_path) {
        Ok(database) => database,
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
                    database
                        .rescan_reference_assets()
                        .map_err(|error| error.to_string()),
                ),
            ),
            AssetMaintenanceRequest::Relink {
                book_id,
                asset_id,
                format,
                replacement_path,
            } => {
                let result = validate_publication(&replacement_path, format)
                    .and_then(|()| {
                        database
                            .relink_reference_asset(asset_id, &replacement_path, format)
                            .map_err(lectern_import::ImportError::from)
                    })
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
        let result = import_paths(database_path, &request.roots, |progress| {
            publish(events, context, WorkerEvent::ImportProgress(progress));
        })
        .map_err(|error| error.to_string());
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
    let mut database = match LibraryDatabase::open(database_path) {
        Ok(database) => database,
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
            database
                .query_page(&request.query, request.offset, request.limit)
                .map(|page| QueryResult {
                    total: Some(page.total),
                    books: page.books,
                })
        } else {
            database
                .query_window(&request.query, request.offset, request.limit)
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
    let database = match LibraryDatabase::open(database_path) {
        Ok(database) => database,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(id) = receiver.recv() {
        let result = database
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
