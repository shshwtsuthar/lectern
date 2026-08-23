use std::{
    io::Cursor,
    path::{Path, PathBuf},
    thread,
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use eframe::egui;
use image::{ImageReader, Limits};
use lectern_core::{BookId, BookSummary, LibraryQuery};
use lectern_storage::LibraryDatabase;

const COVER_QUEUE_CAPACITY: usize = 128;
const MIN_COVER_WORKERS: usize = 2;
const MAX_COVER_WORKERS: usize = 4;
const MAX_STORED_COVER_DIMENSION: u32 = 1_024;
const MAX_STORED_COVER_ALLOCATION: u64 = 16 * 1024 * 1024;

pub(crate) struct QueryRequest {
    pub(crate) generation: u64,
    pub(crate) query: LibraryQuery,
}

pub(crate) struct DecodedCover {
    pub(crate) size: [usize; 2],
    pub(crate) rgba: Vec<u8>,
}

pub(crate) enum WorkerEvent {
    QueryFinished {
        generation: u64,
        result: Result<Vec<BookSummary>, String>,
    },
    CoverFinished {
        id: BookId,
        result: Result<Option<DecodedCover>, String>,
    },
    Error(String),
}

pub(crate) struct WorkerSet {
    query_sender: Sender<QueryRequest>,
    cover_sender: Sender<BookId>,
    event_receiver: Receiver<WorkerEvent>,
}

impl WorkerSet {
    pub(crate) fn spawn(database_path: &Path, context: &egui::Context) -> Self {
        let (query_sender, query_receiver) = unbounded();
        let (cover_sender, cover_receiver) = bounded(COVER_QUEUE_CAPACITY);
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

        Self {
            query_sender,
            cover_sender,
            event_receiver,
        }
    }

    pub(crate) fn query(&self, request: QueryRequest) -> bool {
        self.query_sender.send(request).is_ok()
    }

    pub(crate) fn load_cover(&self, id: BookId) -> bool {
        match self.cover_sender.try_send(id) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn next_event(&self) -> Option<WorkerEvent> {
        self.event_receiver.try_recv().ok()
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
    let database = match LibraryDatabase::open(database_path) {
        Ok(database) => database,
        Err(error) => {
            publish(events, context, WorkerEvent::Error(error.to_string()));
            return;
        }
    };

    while let Ok(mut request) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            request = newer;
        }
        let result = database
            .query(&request.query)
            .map_err(|error| error.to_string());
        if !publish(
            events,
            context,
            WorkerEvent::QueryFinished {
                generation: request.generation,
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
