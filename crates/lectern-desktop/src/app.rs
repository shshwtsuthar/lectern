use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use directories::ProjectDirs;
use eframe::egui::{self, Align, Color32, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};
use lectern_core::{
    AssetHealth, AssetHealthReport, AssetId, AssetStorage, Book, BookFormat, BookId, BookSummary,
    LibraryQuery, SortOrder,
};
use lectern_desktop::export::{ExportError, ExportProgress, OverwritePolicy};
use lectern_import::{ImportProgress, ImportSummary};
use lectern_storage::LibraryDatabase;

use crate::{
    benchmark::{BenchmarkFrame, DesktopBenchmark},
    platform::{NoopAssetPlatform, PlatformAction, PlatformWorker, SystemAssetPlatform},
    workers::{
        DecodedCover, ExportRequest, ImportRequest, QueryQueueResult, QueryRequest, WorkerEvent,
        WorkerSet,
    },
};

const BACKGROUND: Color32 = Color32::from_rgb(18, 20, 24);
const PANEL: Color32 = Color32::from_rgb(24, 27, 32);
const CARD: Color32 = Color32::from_rgb(31, 35, 42);
const CARD_SELECTED: Color32 = Color32::from_rgb(39, 50, 62);
const BORDER: Color32 = Color32::from_rgb(52, 58, 67);
const ACCENT: Color32 = Color32::from_rgb(99, 179, 237);
const MUTED: Color32 = Color32::from_rgb(151, 160, 174);
const CARD_WIDTH: f32 = 174.0;
const CARD_HEIGHT: f32 = 286.0;
const CARD_GAP: f32 = 14.0;
const COVER_SIZE: Vec2 = Vec2::new(142.0, 206.0);
const MAX_CACHED_COVERS: usize = 256;
const QUERY_PAGE_SIZE: usize = 128;
const MAX_CACHED_QUERY_PAGES: usize = 6;

struct CachedCover {
    texture: egui::TextureHandle,
    last_used: u64,
}

struct CachedPage {
    books: Vec<BookSummary>,
    last_used: u64,
}

#[derive(Default)]
struct MetadataActions {
    save: bool,
    reset: bool,
    open: Option<(AssetId, PathBuf)>,
    reveal: Option<(AssetId, PathBuf)>,
    relink: Option<(AssetId, BookFormat)>,
    replace: Option<AssetReplaceSelection>,
    export: Option<(AssetId, BookFormat, PathBuf)>,
    detach: Option<AssetDetachConfirmation>,
    attach: Option<BookFormat>,
    remove: bool,
}

struct BookEditor {
    original: Book,
    title: String,
    authors: String,
    series: String,
    publisher: String,
    language: String,
    description: String,
    saving: bool,
    error: Option<String>,
}

#[derive(Default)]
struct AssetMaintenanceUi {
    scanning: bool,
    report: Option<AssetHealthReport>,
    show_report: bool,
    attaching_format: Option<BookFormat>,
    relinking_asset: Option<AssetId>,
    replacing_asset: Option<AssetId>,
    replace_confirmation: Option<AssetReplaceConfirmation>,
    detaching_asset: Option<AssetId>,
    detach_confirmation: Option<AssetDetachConfirmation>,
}

impl AssetMaintenanceUi {
    fn busy(&self) -> bool {
        self.scanning
            || self.attaching_format.is_some()
            || self.relinking_asset.is_some()
            || self.replacing_asset.is_some()
            || self.detaching_asset.is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssetDetachConfirmation {
    book_id: BookId,
    asset_id: AssetId,
    format: BookFormat,
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssetReplaceSelection {
    book_id: BookId,
    asset_id: AssetId,
    format: BookFormat,
    current_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssetReplaceConfirmation {
    selection: AssetReplaceSelection,
    replacement_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssetExportSelection {
    asset_id: AssetId,
    format: BookFormat,
    source: PathBuf,
    destination: PathBuf,
}

#[derive(Default)]
struct ExportUi {
    active: Option<ActiveExport>,
    overwrite_confirmation: Option<AssetExportSelection>,
}

struct ActiveExport {
    selection: AssetExportSelection,
    progress: ExportProgress,
    cancelled: Arc<AtomicBool>,
    cancelling: bool,
}

#[derive(Clone, Copy)]
struct MetadataOperationState {
    relinking_asset: Option<AssetId>,
    replacing_asset: Option<AssetId>,
    detaching_asset: Option<AssetId>,
    platform_busy: Option<(AssetId, PlatformAction)>,
    exporting_asset: Option<AssetId>,
    attaching_format: Option<BookFormat>,
    removal_busy: bool,
    library_operation_busy: bool,
}

#[derive(Clone)]
struct BookRemovalConfirmation {
    id: BookId,
    title: String,
    asset_count: usize,
    discards_unsaved_changes: bool,
}

#[derive(Default)]
struct BookRemovalUi {
    confirmation: Option<BookRemovalConfirmation>,
    removing: Option<BookId>,
}

impl BookEditor {
    fn new(book: Book) -> Self {
        Self {
            title: book.title.clone(),
            authors: book.authors.clone(),
            series: book.series.clone().unwrap_or_default(),
            publisher: book.publisher.clone().unwrap_or_default(),
            language: book.language.clone().unwrap_or_default(),
            description: book.description.clone().unwrap_or_default(),
            original: book,
            saving: false,
            error: None,
        }
    }

    fn book(&self) -> Book {
        Book {
            id: self.original.id,
            title: self.title.trim().to_owned(),
            authors: self.authors.trim().to_owned(),
            series: optional_metadata(&self.series),
            contributors: self.original.contributors.clone(),
            series_membership: self.original.series_membership.clone(),
            tags: self.original.tags.clone(),
            publisher: optional_metadata(&self.publisher),
            language: optional_metadata(&self.language),
            description: optional_metadata(&self.description),
            assets: self.original.assets.clone(),
        }
    }

    fn changed(&self) -> bool {
        self.book() != self.original
    }

    fn can_save(&self) -> bool {
        !self.saving && !self.title.trim().is_empty() && self.changed()
    }
}

pub(crate) struct LecternApp {
    database_path: PathBuf,
    workers: WorkerSet,
    query: LibraryQuery,
    query_generation: u64,
    query_pending: bool,
    library_total: Option<usize>,
    pages: HashMap<usize, CachedPage>,
    pending_pages: HashSet<usize>,
    selected: Option<BookId>,
    pending_covers: HashSet<BookId>,
    missing_covers: HashSet<BookId>,
    covers: HashMap<BookId, CachedCover>,
    frame_number: u64,
    status: String,
    importing: bool,
    import_progress: Option<ImportProgress>,
    import_summary: Option<ImportSummary>,
    show_import_summary: bool,
    asset_maintenance: AssetMaintenanceUi,
    platform_worker: PlatformWorker,
    platform_busy: Option<(AssetId, PlatformAction)>,
    export_ui: ExportUi,
    book_removal: BookRemovalUi,
    editor_loading: Option<BookId>,
    editor: Option<BookEditor>,
    benchmark: Option<DesktopBenchmark>,
}

impl LecternApp {
    pub(crate) fn new(
        creation_context: &eframe::CreationContext<'_>,
        benchmark: Option<DesktopBenchmark>,
    ) -> Self {
        configure_style(&creation_context.egui_ctx);
        let database_path = default_database_path();
        let status = match LibraryDatabase::open(&database_path) {
            Ok(_) => "Library ready".to_owned(),
            Err(error) => format!("Could not open library: {error}"),
        };
        let workers = WorkerSet::spawn(&database_path, &creation_context.egui_ctx);
        let platform_worker = if benchmark.is_some() {
            PlatformWorker::spawn(NoopAssetPlatform, &creation_context.egui_ctx)
        } else {
            PlatformWorker::spawn(SystemAssetPlatform::default(), &creation_context.egui_ctx)
        };
        let mut app = Self {
            database_path,
            workers,
            query: LibraryQuery::default(),
            query_generation: 0,
            query_pending: false,
            library_total: None,
            pages: HashMap::new(),
            pending_pages: HashSet::new(),
            selected: None,
            pending_covers: HashSet::new(),
            missing_covers: HashSet::new(),
            covers: HashMap::new(),
            frame_number: 0,
            status,
            importing: false,
            import_progress: None,
            import_summary: None,
            show_import_summary: false,
            asset_maintenance: AssetMaintenanceUi::default(),
            platform_worker,
            platform_busy: None,
            export_ui: ExportUi::default(),
            book_removal: BookRemovalUi::default(),
            editor_loading: None,
            editor: None,
            benchmark,
        };
        app.refresh_library();
        app
    }

    fn refresh_library(&mut self) {
        self.query_generation = self.query_generation.wrapping_add(1);
        self.query_pending = false;
        self.library_total = None;
        self.pages.clear();
        self.pending_pages.clear();
        self.request_page(0);
    }

    fn request_page(&mut self, offset: usize) {
        if self.pages.contains_key(&offset) || self.pending_pages.contains(&offset) {
            return;
        }
        let include_total = self.library_total.is_none();
        let page_offset = offset;
        let Ok(offset) = u64::try_from(offset) else {
            "Library result offset exceeds this platform's supported range"
                .clone_into(&mut self.status);
            return;
        };
        let request = QueryRequest {
            generation: self.query_generation,
            query: self.query.clone(),
            offset,
            limit: u32::try_from(QUERY_PAGE_SIZE).expect("page size fits u32"),
            include_total,
        };
        match self.workers.query(request) {
            QueryQueueResult::Queued => {
                self.pending_pages.insert(page_offset);
                self.query_pending |= include_total;
            }
            QueryQueueResult::Full => {
                self.query_pending |= include_total;
            }
            QueryQueueResult::Disconnected if include_total => {
                "Library query worker is unavailable".clone_into(&mut self.status);
            }
            QueryQueueResult::Disconnected => {}
        }
    }

    fn poll_workers(&mut self, context: &egui::Context) {
        while let Some(event) = self.workers.next_event() {
            match event {
                WorkerEvent::QueryFinished {
                    generation,
                    offset,
                    result,
                } if generation == self.query_generation => {
                    self.query_finished(offset, result);
                }
                WorkerEvent::QueryDiscarded { generation, offset }
                    if generation == self.query_generation =>
                {
                    self.query_discarded(offset);
                }
                WorkerEvent::CoverFinished { id, result } => {
                    self.pending_covers.remove(&id);
                    match result {
                        Ok(Some(cover)) => self.install_cover(context, id, &cover),
                        Ok(None) => {
                            self.missing_covers.insert(id);
                        }
                        Err(error) => {
                            self.missing_covers.insert(id);
                            self.status = format!("Could not load a cover: {error}");
                        }
                    }
                }
                WorkerEvent::ImportProgress(progress) => {
                    self.import_progress = Some(progress);
                    self.status = import_status(progress);
                }
                WorkerEvent::ImportFinished(result) => {
                    self.importing = false;
                    self.import_progress = None;
                    match result {
                        Ok(summary) => {
                            self.status = format!(
                                "Imported {} of {} books; {} failed",
                                summary.imported, summary.discovered, summary.failed
                            );
                            self.show_import_summary = summary.failed > 0;
                            self.import_summary = Some(summary);
                            self.covers.clear();
                            self.missing_covers.clear();
                            self.refresh_library();
                        }
                        Err(error) => self.status = format!("Import failed: {error}"),
                    }
                }
                WorkerEvent::BookLoaded { id, result } if self.selected == Some(id) => {
                    self.editor_loading = None;
                    match result {
                        Ok(Some(book)) => self.editor = Some(BookEditor::new(book)),
                        Ok(None) => {
                            "This book is no longer in the library".clone_into(&mut self.status);
                            self.clear_selection();
                        }
                        Err(error) => {
                            self.status = format!("Could not load metadata: {error}");
                            self.clear_selection();
                        }
                    }
                }
                WorkerEvent::BookSaved { book, result } => self.book_saved(book, result),
                WorkerEvent::BookRemoved { id, title, result } => {
                    self.book_removed(id, &title, result);
                }
                WorkerEvent::AssetHealthScanned(result) => self.asset_health_scanned(result),
                WorkerEvent::AssetAttached {
                    book_id,
                    format,
                    result,
                } => self.asset_attached(book_id, format, result),
                WorkerEvent::AssetDetached { asset_id, result } => {
                    self.asset_detached(asset_id, result);
                }
                WorkerEvent::AssetRelinked {
                    book_id,
                    asset_id,
                    result,
                } => self.asset_relinked(book_id, asset_id, result),
                WorkerEvent::AssetReplaced {
                    book_id,
                    asset_id,
                    replacement_path,
                    result,
                } => self.asset_replaced(book_id, asset_id, replacement_path, result),
                WorkerEvent::ExportProgress {
                    asset_id,
                    destination,
                    progress,
                } => self.export_progress(asset_id, &destination, progress),
                WorkerEvent::ExportFinished {
                    asset_id,
                    source,
                    destination,
                    result,
                } => self.export_finished(asset_id, source, destination, result),
                WorkerEvent::QueryFinished { .. }
                | WorkerEvent::QueryDiscarded { .. }
                | WorkerEvent::BookLoaded { .. } => {}
                WorkerEvent::Error(error) => self.status = format!("Background worker: {error}"),
            }
        }
        while let Some(event) = self.platform_worker.next_event() {
            if self.platform_busy == Some((event.asset_id, event.action)) {
                self.platform_busy = None;
            }
            match event.result {
                Ok(()) => {
                    self.status = match event.action {
                        PlatformAction::Open => "Opened book file".to_owned(),
                        PlatformAction::Reveal => "Revealed book file".to_owned(),
                    };
                }
                Err(error) => {
                    self.status = format!(
                        "Could not {} book file: {error}. Relink the asset if it moved.",
                        event.action
                    );
                    if let Some(editor) = &mut self.editor
                        && editor
                            .original
                            .assets
                            .iter()
                            .any(|asset| asset.id == event.asset_id)
                    {
                        editor.error = Some(error);
                    }
                }
            }
        }
        self.retry_initial_page_if_needed();
        self.evict_covers();
    }

    fn export_progress(
        &mut self,
        asset_id: AssetId,
        destination: &PathBuf,
        progress: ExportProgress,
    ) {
        let Some(active) = &mut self.export_ui.active else {
            return;
        };
        if active.selection.asset_id != asset_id
            || active.selection.destination.as_path() != destination
        {
            return;
        }
        active.progress = progress;
        self.status = format_export_progress(progress, active.cancelling);
    }

    fn export_finished(
        &mut self,
        asset_id: AssetId,
        source: PathBuf,
        destination: PathBuf,
        result: Result<lectern_desktop::export::ExportOutcome, ExportError>,
    ) {
        let Some(active) = self.export_ui.active.take() else {
            return;
        };
        if active.selection.asset_id != asset_id || active.selection.destination != destination {
            self.export_ui.active = Some(active);
            return;
        }
        match result {
            Ok(outcome) => {
                self.status = format!(
                    "Exported {} bytes to {}",
                    outcome.copied_bytes,
                    destination.display()
                );
            }
            Err(ExportError::DestinationExists(_)) => {
                self.status = "Export destination already exists; confirmation required".into();
                self.export_ui.overwrite_confirmation = Some(AssetExportSelection {
                    asset_id,
                    format: active.selection.format,
                    source,
                    destination,
                });
            }
            Err(ExportError::Cancelled) => {
                "Export cancelled; no partial copy was kept".clone_into(&mut self.status);
            }
            Err(error) => {
                self.status = format!("Could not export book file: {error}");
                if let Some(editor) = &mut self.editor
                    && editor
                        .original
                        .assets
                        .iter()
                        .any(|asset| asset.id == asset_id)
                {
                    editor.error = Some(error.to_string());
                }
            }
        }
    }

    fn retry_initial_page_if_needed(&mut self) {
        if self.query_pending && self.library_total.is_none() && self.pending_pages.is_empty() {
            self.request_page(0);
        }
    }

    fn query_finished(&mut self, offset: u64, result: Result<crate::workers::QueryResult, String>) {
        let Ok(offset) = usize::try_from(offset) else {
            "Library result offset exceeds this platform's supported range"
                .clone_into(&mut self.status);
            return;
        };
        self.pending_pages.remove(&offset);
        match result {
            Ok(result) => {
                let recovered = self.status.starts_with("Library query failed:");
                if let Some(total) = result.total {
                    let Ok(total_as_usize) = usize::try_from(total) else {
                        "Library is too large for this platform".clone_into(&mut self.status);
                        self.pages.clear();
                        self.library_total = None;
                        self.query_pending = false;
                        return;
                    };
                    self.library_total = Some(total_as_usize);
                    if let Some(benchmark) = &mut self.benchmark {
                        benchmark.library_installed(total, &result.books, self.query.sort);
                    }
                }
                self.pages.insert(
                    offset,
                    CachedPage {
                        books: result.books,
                        last_used: self.frame_number,
                    },
                );
                self.evict_pages();
                if recovered {
                    "Library ready".clone_into(&mut self.status);
                }
                self.query_pending =
                    self.library_total.is_none() || self.pending_pages.contains(&0);
            }
            Err(error) => {
                self.query_pending = false;
                self.status = format!("Library query failed: {error}");
            }
        }
    }

    fn apply_benchmark_sort_request(&mut self) {
        let sort = self
            .benchmark
            .as_mut()
            .and_then(DesktopBenchmark::next_sort_request);
        if let Some(sort) = sort {
            debug_assert_ne!(self.query.sort, sort);
            self.query.sort = sort;
            self.refresh_library();
        }
    }

    fn apply_benchmark_asset_action_request(&mut self) {
        let action = self
            .benchmark
            .as_mut()
            .and_then(DesktopBenchmark::next_asset_action_request);
        if let Some(action) = action {
            let queued = self.platform_worker.dispatch(
                AssetId::new(i64::MIN),
                action,
                PathBuf::from("/lectern-benchmark/no-op 'asset'.epub"),
            );
            if !queued && let Some(benchmark) = &mut self.benchmark {
                benchmark.asset_action_dispatch_failed();
            }
        }
    }

    fn query_discarded(&mut self, offset: u64) {
        if let Ok(offset) = usize::try_from(offset) {
            self.pending_pages.remove(&offset);
        }
        self.query_pending = self.library_total.is_none() || self.pending_pages.contains(&0);
    }

    fn book_saved(&mut self, book: Book, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.status = format!("Saved metadata for {}", book.title);
                if self.editor.as_ref().map(|editor| editor.original.id) == Some(book.id) {
                    self.editor = Some(BookEditor::new(book));
                }
                self.refresh_library();
            }
            Err(error) => {
                self.status = format!("Could not save metadata: {error}");
                if let Some(editor) = &mut self.editor
                    && editor.original.id == book.id
                {
                    editor.saving = false;
                    editor.error = Some(error);
                }
            }
        }
    }

    fn book_removed(&mut self, id: BookId, title: &str, result: Result<bool, String>) {
        if self.book_removal.removing == Some(id) {
            self.book_removal.removing = None;
        }
        match result {
            Ok(true) => {
                self.status = format!("Removed {title} from the library; book files were kept");
                self.covers.remove(&id);
                self.pending_covers.remove(&id);
                self.missing_covers.remove(&id);
                if self.selected == Some(id) {
                    self.clear_selection();
                }
                self.refresh_library();
            }
            Ok(false) => {
                "This book is no longer in the library".clone_into(&mut self.status);
                if self.selected == Some(id) {
                    self.clear_selection();
                }
                self.refresh_library();
            }
            Err(error) => {
                self.status = format!("Could not remove book: {error}");
                if let Some(editor) = &mut self.editor
                    && editor.original.id == id
                {
                    editor.error = Some(error);
                }
            }
        }
    }

    fn asset_health_scanned(&mut self, result: Result<AssetHealthReport, String>) {
        self.asset_maintenance.scanning = false;
        match result {
            Ok(report) => {
                self.status = asset_health_status(report);
                self.asset_maintenance.report = Some(report);
                self.asset_maintenance.show_report = true;
                self.refresh_library();
                self.reload_selected_book_after_asset_change();
            }
            Err(error) => self.status = format!("Could not scan library files: {error}"),
        }
    }

    fn asset_attached(&mut self, book_id: BookId, format: BookFormat, result: Result<(), String>) {
        if self.asset_maintenance.attaching_format == Some(format) {
            self.asset_maintenance.attaching_format = None;
        }
        match result {
            Ok(()) => {
                self.status = format!("Attached {format} file");
                self.refresh_library();
                if self.selected == Some(book_id) {
                    self.reload_selected_book_after_asset_change();
                }
            }
            Err(error) => {
                self.status = format!("Could not attach {format} file: {error}");
                if let Some(editor) = &mut self.editor
                    && editor.original.id == book_id
                {
                    editor.error = Some(error);
                }
            }
        }
    }

    fn asset_relinked(&mut self, book_id: BookId, asset_id: AssetId, result: Result<(), String>) {
        if self.asset_maintenance.relinking_asset == Some(asset_id) {
            self.asset_maintenance.relinking_asset = None;
        }
        match result {
            Ok(()) => {
                "Relinked book file".clone_into(&mut self.status);
                self.refresh_library();
                if self.selected == Some(book_id) {
                    self.reload_selected_book_after_asset_change();
                }
            }
            Err(error) => {
                self.status = format!("Could not relink book file: {error}");
                if let Some(editor) = &mut self.editor
                    && editor.original.id == book_id
                {
                    editor.error = Some(error);
                }
            }
        }
    }

    fn asset_detached(&mut self, asset_id: AssetId, result: Result<BookId, String>) {
        if self.asset_maintenance.detaching_asset == Some(asset_id) {
            self.asset_maintenance.detaching_asset = None;
        }
        match result {
            Ok(book_id) => {
                "Detached file from book; the file was kept on disk".clone_into(&mut self.status);
                if let Some(editor) = &mut self.editor
                    && editor.original.id == book_id
                {
                    editor.original.assets.retain(|asset| asset.id != asset_id);
                    editor.error = None;
                }
                self.refresh_library();
                if self.selected == Some(book_id) {
                    self.reload_selected_book_after_asset_change();
                }
            }
            Err(error) => {
                self.status = format!("Could not detach book file: {error}");
                if let Some(editor) = &mut self.editor
                    && editor
                        .original
                        .assets
                        .iter()
                        .any(|asset| asset.id == asset_id)
                {
                    editor.error = Some(error);
                }
            }
        }
    }

    fn asset_replaced(
        &mut self,
        book_id: BookId,
        asset_id: AssetId,
        replacement_path: PathBuf,
        result: Result<(), String>,
    ) {
        if self.asset_maintenance.replacing_asset == Some(asset_id) {
            self.asset_maintenance.replacing_asset = None;
        }
        match result {
            Ok(()) => {
                "Replaced book file; the old file was kept on disk".clone_into(&mut self.status);
                if let Some(editor) = &mut self.editor
                    && editor.original.id == book_id
                    && let Some(asset) = editor
                        .original
                        .assets
                        .iter_mut()
                        .find(|asset| asset.id == asset_id)
                {
                    asset.path = replacement_path;
                    asset.health = AssetHealth::Available;
                    editor.error = None;
                }
                self.refresh_library();
                if self.selected == Some(book_id) {
                    self.reload_selected_book_after_asset_change();
                }
            }
            Err(error) => {
                self.status = format!("Could not replace book file: {error}");
                if let Some(editor) = &mut self.editor
                    && editor.original.id == book_id
                {
                    editor.error = Some(error);
                }
            }
        }
    }

    fn install_cover(&mut self, context: &egui::Context, id: BookId, cover: &DecodedCover) {
        let image = egui::ColorImage::from_rgba_unmultiplied(cover.size, &cover.rgba);
        let texture = context.load_texture(
            format!("book-cover-{id}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.covers.insert(
            id,
            CachedCover {
                texture,
                last_used: self.frame_number,
            },
        );
    }

    fn evict_covers(&mut self) {
        if self.covers.len() <= MAX_CACHED_COVERS {
            return;
        }
        let excess = self.covers.len() - MAX_CACHED_COVERS;
        let mut oldest = self
            .covers
            .iter()
            .map(|(id, cover)| (*id, cover.last_used))
            .collect::<Vec<_>>();
        oldest.sort_unstable_by_key(|(_, last_used)| *last_used);
        for (id, _) in oldest.into_iter().take(excess) {
            self.covers.remove(&id);
        }
    }

    fn evict_pages(&mut self) {
        if self.pages.len() <= MAX_CACHED_QUERY_PAGES {
            return;
        }
        let excess = self.pages.len() - MAX_CACHED_QUERY_PAGES;
        let mut oldest = self
            .pages
            .iter()
            .map(|(offset, page)| (*offset, page.last_used))
            .collect::<Vec<_>>();
        oldest.sort_unstable_by_key(|(_, last_used)| *last_used);
        for (offset, _) in oldest.into_iter().take(excess) {
            self.pages.remove(&offset);
        }
    }

    fn book_at(&mut self, index: usize) -> Option<BookSummary> {
        let offset = query_page_offset(index);
        if let Some(page) = self.pages.get_mut(&offset) {
            page.last_used = self.frame_number;
            return page.books.get(index - offset).cloned();
        }
        self.request_page(offset);
        None
    }

    fn request_visible_range(&mut self, start: usize, end: usize) {
        for index in start..end.min(self.library_total.unwrap_or_default()) {
            self.request_page(query_page_offset(index));
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let (add_books, add_folder, rescan_files) = self.toolbar_actions(ui);
        if add_books
            && let Some(paths) = rfd::FileDialog::new()
                .set_title("Add books to Lectern")
                .add_filter("Ebooks", &["epub", "pdf"])
                .pick_files()
        {
            self.start_import(paths);
        }
        if add_folder
            && let Some(path) = rfd::FileDialog::new()
                .set_title("Add a folder to Lectern")
                .pick_folder()
        {
            self.start_import(vec![path]);
        }
        if rescan_files {
            self.start_asset_scan();
        }
        ui.add_space(10.0);

        let mut query_changed = false;
        ui.horizontal_centered(|ui| {
            let search = egui::TextEdit::singleline(&mut self.query.search)
                .hint_text("Search title, author, series, or publisher…")
                .desired_width(380.0);
            query_changed |= ui.add_sized([380.0, 34.0], search).changed();

            let previous_format = self.query.format;
            egui::ComboBox::from_id_salt("format-filter")
                .selected_text(
                    self.query
                        .format
                        .map_or_else(|| "All formats".to_owned(), |format| format.to_string()),
                )
                .width(120.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.query.format, None, "All formats");
                    for format in BookFormat::ALL {
                        ui.selectable_value(
                            &mut self.query.format,
                            Some(format),
                            format.to_string(),
                        );
                    }
                });
            query_changed |= previous_format != self.query.format;

            query_changed |= self.asset_health_filter(ui);

            let previous_sort = self.query.sort;
            egui::ComboBox::from_id_salt("library-sort")
                .selected_text(self.query.sort.to_string())
                .width(140.0)
                .show_ui(ui, |ui| {
                    for sort in SortOrder::ALL {
                        ui.selectable_value(&mut self.query.sort, sort, sort.to_string());
                    }
                });
            query_changed |= previous_sort != self.query.sort;
        });

        if query_changed {
            self.refresh_library();
        }
    }

    fn toolbar_actions(&mut self, ui: &mut egui::Ui) -> (bool, bool, bool) {
        let mut add_books = false;
        let mut add_folder = false;
        let mut rescan_files = false;
        let maintenance_busy =
            self.asset_maintenance.busy() || self.book_removal.removing.is_some();
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Lectern").size(26.0).strong());
            ui.label(
                RichText::new(format!("{} books", self.library_total.unwrap_or_default()))
                    .color(MUTED)
                    .size(13.0),
            );
            if self.query_pending {
                ui.spinner();
            }
            if self.importing {
                ui.label(RichText::new("Importing…").color(ACCENT).size(12.0));
            }
            if self.asset_maintenance.scanning {
                ui.label(RichText::new("Scanning files…").color(ACCENT).size(12.0));
            }
            if let Some(format) = self.asset_maintenance.attaching_format {
                ui.label(
                    RichText::new(format!("Attaching {format}…"))
                        .color(ACCENT)
                        .size(12.0),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                add_folder = ui
                    .add_enabled(
                        !self.importing && !maintenance_busy,
                        egui::Button::new("Add folder"),
                    )
                    .clicked();
                add_books = ui
                    .add_enabled(
                        !self.importing && !maintenance_busy,
                        egui::Button::new("Add books"),
                    )
                    .clicked();
                rescan_files = ui
                    .add_enabled(
                        !self.importing && !maintenance_busy,
                        egui::Button::new("Rescan files"),
                    )
                    .on_hover_text("Check the availability of referenced EPUB and PDF files")
                    .clicked();
                if self.asset_maintenance.report.is_some() && ui.button("File report").clicked() {
                    self.asset_maintenance.show_report = true;
                }
                if self
                    .import_summary
                    .as_ref()
                    .is_some_and(|summary| summary.failed > 0)
                    && ui.button("Import report").clicked()
                {
                    self.show_import_summary = true;
                }
            });
        });
        (add_books, add_folder, rescan_files)
    }

    fn asset_health_filter(&mut self, ui: &mut egui::Ui) -> bool {
        let previous = self.query.asset_health;
        egui::ComboBox::from_id_salt("asset-health-filter")
            .selected_text(
                self.query
                    .asset_health
                    .map_or_else(|| "All files".to_owned(), |health| health.to_string()),
            )
            .width(130.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.query.asset_health, None, "All files");
                ui.selectable_value(
                    &mut self.query.asset_health,
                    Some(AssetHealth::Missing),
                    "Missing files",
                );
                ui.selectable_value(
                    &mut self.query.asset_health,
                    Some(AssetHealth::Unreadable),
                    "Unreadable files",
                );
                ui.selectable_value(
                    &mut self.query.asset_health,
                    Some(AssetHealth::Unknown),
                    "Not checked",
                );
            });
        previous != self.query.asset_health
    }

    fn start_import(&mut self, roots: Vec<PathBuf>) {
        if roots.is_empty() {
            return;
        }
        if self.importing {
            "An import is already running".clone_into(&mut self.status);
            return;
        }
        if self.asset_maintenance.busy() || self.book_removal.removing.is_some() {
            "Library file maintenance is already running".clone_into(&mut self.status);
            return;
        }
        if self.workers.import(ImportRequest { roots }) {
            self.importing = true;
            self.import_progress = Some(ImportProgress::default());
            self.import_summary = None;
            "Discovering book files…".clone_into(&mut self.status);
        } else {
            "Import worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn start_asset_scan(&mut self) {
        if self.importing || self.asset_maintenance.busy() || self.book_removal.removing.is_some() {
            "Library file maintenance is already running".clone_into(&mut self.status);
            return;
        }
        if self.workers.rescan_reference_assets() {
            self.asset_maintenance.scanning = true;
            "Scanning referenced book files…".clone_into(&mut self.status);
        } else {
            "Library maintenance worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn accept_dropped_files(&mut self, ui: &egui::Ui) {
        let paths = ui.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<_>>()
        });
        if !paths.is_empty() {
            self.start_import(paths);
        }
    }

    fn import_summary_window(&mut self, context: &egui::Context) {
        let Some(summary) = &self.import_summary else {
            return;
        };
        let mut open = self.show_import_summary;
        egui::Window::new("Import report")
            .open(&mut open)
            .default_width(560.0)
            .collapsible(false)
            .show(context, |ui| {
                ui.heading(format!(
                    "{} imported · {} failed",
                    summary.imported, summary.failed
                ));
                ui.label(format!(
                    "{} book files were discovered.",
                    summary.discovered
                ));
                if !summary.failures.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for failure in summary.failures.iter().take(200) {
                                ui.label(
                                    RichText::new(failure.path.display().to_string()).strong(),
                                );
                                ui.label(RichText::new(&failure.message).color(MUTED));
                                ui.add_space(7.0);
                            }
                            let hidden = summary.failures.len().saturating_sub(200);
                            if hidden > 0 {
                                ui.label(format!("…and {hidden} more failures."));
                            }
                        });
                }
            });
        self.show_import_summary = open;
    }

    fn asset_health_report_window(&mut self, context: &egui::Context) {
        let Some(report) = self.asset_maintenance.report else {
            return;
        };
        let mut open = self.asset_maintenance.show_report;
        let mut show_missing = false;
        egui::Window::new("Library file report")
            .open(&mut open)
            .default_width(430.0)
            .collapsible(false)
            .show(context, |ui| {
                ui.heading(format!("{} referenced files checked", report.checked));
                ui.label(
                    RichText::new(format!("{} available", report.available))
                        .color(Color32::LIGHT_GREEN),
                );
                ui.label(
                    RichText::new(format!("{} missing", report.missing)).color(Color32::LIGHT_RED),
                );
                ui.label(
                    RichText::new(format!("{} unreadable", report.unreadable))
                        .color(Color32::LIGHT_RED),
                );
                if report.missing > 0 {
                    ui.add_space(8.0);
                    show_missing = ui.button("Show missing files").clicked();
                }
                if report.changed == 0 {
                    ui.add_space(8.0);
                    ui.label(RichText::new("No stored file health changed.").color(MUTED));
                }
            });
        self.asset_maintenance.show_report = open;
        if show_missing {
            self.query.asset_health = Some(AssetHealth::Missing);
            self.refresh_library();
        }
    }

    fn select_book(&mut self, id: BookId) {
        self.book_removal.confirmation = None;
        self.asset_maintenance.detach_confirmation = None;
        self.asset_maintenance.replace_confirmation = None;
        self.export_ui.overwrite_confirmation = None;
        self.selected = Some(id);
        self.editor = None;
        if self.workers.load_book(id) {
            self.editor_loading = Some(id);
        } else {
            self.editor_loading = None;
            "Metadata worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn clear_selection(&mut self) {
        self.book_removal.confirmation = None;
        self.asset_maintenance.detach_confirmation = None;
        self.asset_maintenance.replace_confirmation = None;
        self.export_ui.overwrite_confirmation = None;
        self.selected = None;
        self.editor_loading = None;
        self.editor = None;
    }

    fn reload_selected_book_after_asset_change(&mut self) {
        let has_unsaved_changes = self.editor.as_ref().is_some_and(BookEditor::changed);
        if has_unsaved_changes {
            return;
        }
        if let Some(id) = self.selected
            && self.workers.load_book(id)
        {
            self.editor_loading = Some(id);
            self.editor = None;
        }
    }

    fn metadata_panel(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut reset = false;
        let mut save = false;
        let mut open = None;
        let mut reveal = None;
        let mut relink = None;
        let mut replace = None;
        let mut export = None;
        let mut detach = None;
        let mut attach = None;
        let mut remove = false;
        let removal_busy = self.book_removal.removing.is_some();
        let library_operation_busy = self.importing || self.asset_maintenance.busy();
        egui::Panel::right("metadata-editor")
            .default_size(370.0)
            .size_range(310.0..=520.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(18, 16)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Book details");
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        close = ui.button("×").on_hover_text("Close editor").clicked();
                    });
                });
                ui.separator();

                if self.editor_loading.is_some() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(30.0);
                        ui.spinner();
                        ui.label(RichText::new("Loading metadata…").color(MUTED));
                    });
                    return;
                }

                let Some(editor) = &mut self.editor else {
                    ui.label(RichText::new("Metadata is unavailable.").color(MUTED));
                    return;
                };
                let actions = metadata_form(
                    ui,
                    editor,
                    MetadataOperationState {
                        relinking_asset: self.asset_maintenance.relinking_asset,
                        replacing_asset: self.asset_maintenance.replacing_asset,
                        detaching_asset: self.asset_maintenance.detaching_asset,
                        platform_busy: self.platform_busy,
                        exporting_asset: self
                            .export_ui
                            .active
                            .as_ref()
                            .map(|active| active.selection.asset_id),
                        attaching_format: self.asset_maintenance.attaching_format,
                        removal_busy,
                        library_operation_busy,
                    },
                );
                save = actions.save;
                reset = actions.reset;
                open = actions.open;
                reveal = actions.reveal;
                relink = actions.relink;
                replace = actions.replace;
                export = actions.export;
                detach = actions.detach;
                attach = actions.attach;
                remove = actions.remove;
            });

        save |= metadata_save_shortcut(ui, removal_busy, library_operation_busy);
        if close {
            self.clear_selection();
        } else if reset {
            if let Some(editor) = &mut self.editor {
                *editor = BookEditor::new(editor.original.clone());
            }
        } else if save {
            self.save_editor();
        } else if let Some((asset_id, path)) = open {
            self.start_platform_action(asset_id, PlatformAction::Open, path);
        } else if let Some((asset_id, path)) = reveal {
            self.start_platform_action(asset_id, PlatformAction::Reveal, path);
        } else if let Some((asset_id, format)) = relink {
            self.choose_asset_relink(asset_id, format);
        } else if let Some(selection) = replace {
            self.choose_asset_replacement(selection);
        } else if let Some((asset_id, format, source)) = export {
            self.choose_asset_export(asset_id, format, source);
        } else if let Some(confirmation) = detach {
            self.asset_maintenance.detach_confirmation = Some(confirmation);
        } else if let Some(format) = attach {
            self.choose_format_attachment(format);
        } else if remove {
            self.request_book_removal();
        }
    }

    fn start_platform_action(&mut self, asset_id: AssetId, action: PlatformAction, path: PathBuf) {
        if self.platform_busy.is_some() {
            return;
        }
        if self.platform_worker.dispatch(asset_id, action, path) {
            self.platform_busy = Some((asset_id, action));
            self.status = match action {
                PlatformAction::Open => "Opening book file…".to_owned(),
                PlatformAction::Reveal => "Revealing book file…".to_owned(),
            };
        } else {
            "Another file action is still starting".clone_into(&mut self.status);
        }
    }

    fn request_book_removal(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        self.book_removal.confirmation = Some(BookRemovalConfirmation {
            id: editor.original.id,
            title: editor.original.title.clone(),
            asset_count: editor.original.assets.len(),
            discards_unsaved_changes: editor.changed(),
        });
    }

    fn book_removal_confirmation_window(&mut self, context: &egui::Context) {
        let Some(confirmation) = self.book_removal.confirmation.clone() else {
            return;
        };
        let response =
            egui::Modal::new(egui::Id::new("book-removal-confirmation")).show(context, |ui| {
                ui.set_max_width(430.0);
                ui.heading("Remove from library?");
                ui.add_space(6.0);
                ui.label(format!("Remove “{}” from Lectern?", confirmation.title));
                ui.label(
                    RichText::new(removal_file_message(confirmation.asset_count)).color(MUTED),
                );
                if confirmation.discards_unsaved_changes {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Unsaved metadata changes will be discarded.")
                            .color(Color32::LIGHT_RED),
                    );
                }
                ui.add_space(14.0);
                let mut confirm = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    confirm = ui
                        .button(RichText::new("Remove from library").color(Color32::LIGHT_RED))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
                (confirm, cancel)
            });
        let should_close = response.should_close();
        let (confirm, cancel) = response.inner;
        if confirm {
            self.start_book_removal(confirmation);
        } else if cancel || should_close {
            self.book_removal.confirmation = None;
        }
    }

    fn asset_detach_confirmation_window(&mut self, context: &egui::Context) {
        let Some(confirmation) = self.asset_maintenance.detach_confirmation.clone() else {
            return;
        };
        let response =
            egui::Modal::new(egui::Id::new("asset-detach-confirmation")).show(context, |ui| {
                ui.set_max_width(470.0);
                ui.heading(format!("Detach {} file?", confirmation.format));
                ui.add_space(6.0);
                ui.label("Detach this file from the book?");
                ui.label(
                    RichText::new(confirmation.path.display().to_string())
                        .monospace()
                        .color(MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "The file will remain on disk. Lectern will only remove this relationship.",
                    )
                    .color(MUTED),
                );
                ui.add_space(14.0);
                let mut confirm = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    confirm = ui
                        .button(RichText::new("Detach from book").color(Color32::LIGHT_RED))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
                (confirm, cancel)
            });
        let should_close = response.should_close();
        let (confirm, cancel) = response.inner;
        if confirm {
            self.start_asset_detach(&confirmation);
        } else if cancel || should_close {
            self.asset_maintenance.detach_confirmation = None;
        }
    }

    fn start_asset_detach(&mut self, confirmation: &AssetDetachConfirmation) {
        self.asset_maintenance.detach_confirmation = None;
        if self.importing
            || self.asset_maintenance.busy()
            || self.book_removal.removing.is_some()
            || self.selected != Some(confirmation.book_id)
        {
            return;
        }
        if self.workers.detach_asset(confirmation.asset_id) {
            self.asset_maintenance.detaching_asset = Some(confirmation.asset_id);
            if let Some(editor) = &mut self.editor {
                editor.error = None;
            }
            self.status = format!("Detaching {} file…", confirmation.format);
        } else {
            "Library maintenance worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn asset_replace_confirmation_window(&mut self, context: &egui::Context) {
        let Some(confirmation) = self.asset_maintenance.replace_confirmation.clone() else {
            return;
        };
        let response =
            egui::Modal::new(egui::Id::new("asset-replace-confirmation")).show(context, |ui| {
                ui.set_max_width(500.0);
                ui.heading(format!("Replace {} file?", confirmation.selection.format));
                ui.add_space(6.0);
                ui.label("Current file:");
                ui.label(
                    RichText::new(confirmation.selection.current_path.display().to_string())
                        .monospace()
                        .color(MUTED),
                );
                ui.add_space(4.0);
                ui.label("Replacement file:");
                ui.label(
                    RichText::new(confirmation.replacement_path.display().to_string())
                        .monospace()
                        .color(MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "The old referenced file will remain on disk and will not be modified.",
                    )
                    .color(MUTED),
                );
                ui.add_space(14.0);
                let mut confirm = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    confirm = ui.button("Replace file").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
                (confirm, cancel)
            });
        let should_close = response.should_close();
        let (confirm, cancel) = response.inner;
        if confirm {
            self.start_asset_replacement(&confirmation);
        } else if cancel || should_close {
            self.asset_maintenance.replace_confirmation = None;
        }
    }

    fn start_asset_replacement(&mut self, confirmation: &AssetReplaceConfirmation) {
        self.asset_maintenance.replace_confirmation = None;
        if self.importing
            || self.asset_maintenance.busy()
            || self.book_removal.removing.is_some()
            || self.selected != Some(confirmation.selection.book_id)
        {
            return;
        }
        if self.workers.replace_reference_asset(
            confirmation.selection.book_id,
            confirmation.selection.asset_id,
            confirmation.selection.format,
            confirmation.replacement_path.clone(),
        ) {
            self.asset_maintenance.replacing_asset = Some(confirmation.selection.asset_id);
            if let Some(editor) = &mut self.editor {
                editor.error = None;
            }
            self.status = format!(
                "Validating replacement {} file…",
                confirmation.selection.format
            );
        } else {
            "Library maintenance worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn start_book_removal(&mut self, confirmation: BookRemovalConfirmation) {
        self.book_removal.confirmation = None;
        if self.book_removal.removing.is_some() {
            return;
        }
        if self
            .workers
            .remove_book(confirmation.id, confirmation.title)
        {
            self.book_removal.removing = Some(confirmation.id);
            if let Some(editor) = &mut self.editor {
                editor.error = None;
            }
            "Removing book from the library…".clone_into(&mut self.status);
        } else {
            "Metadata worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn save_editor(&mut self) {
        if self.book_removal.removing.is_some() || self.importing || self.asset_maintenance.busy() {
            return;
        }
        let Some(editor) = &mut self.editor else {
            return;
        };
        if !editor.can_save() {
            return;
        }
        let book = editor.book();
        if self.workers.save_book(book) {
            editor.saving = true;
            editor.error = None;
            "Saving metadata…".clone_into(&mut self.status);
        } else {
            "Metadata worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn choose_asset_relink(&mut self, asset_id: AssetId, format: BookFormat) {
        if self.asset_maintenance.busy() {
            return;
        }
        let extension = format_extension(format);
        let title = format!("Relink {format} file");
        let Some(path) = rfd::FileDialog::new()
            .set_title(&title)
            .add_filter(format.to_string(), &[extension])
            .pick_file()
        else {
            return;
        };
        let Some(book_id) = self.selected else {
            return;
        };
        if self
            .workers
            .relink_reference_asset(book_id, asset_id, format, path)
        {
            self.asset_maintenance.relinking_asset = Some(asset_id);
            if let Some(editor) = &mut self.editor {
                editor.error = None;
            }
            "Validating replacement file…".clone_into(&mut self.status);
        } else {
            "Library maintenance worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn choose_asset_replacement(&mut self, selection: AssetReplaceSelection) {
        if self.importing || self.asset_maintenance.busy() || self.book_removal.removing.is_some() {
            return;
        }
        let extension = format_extension(selection.format);
        let title = format!("Replace {} file", selection.format);
        let Some(replacement_path) = rfd::FileDialog::new()
            .set_title(&title)
            .add_filter(selection.format.to_string(), &[extension])
            .pick_file()
        else {
            return;
        };
        self.asset_maintenance.replace_confirmation = Some(AssetReplaceConfirmation {
            selection,
            replacement_path,
        });
    }

    fn choose_asset_export(&mut self, asset_id: AssetId, format: BookFormat, source: PathBuf) {
        if self.export_ui.active.is_some() {
            return;
        }
        let suggested_name = source.file_name().map_or_else(
            || format!("book.{}", format_extension(format)),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = format!("Export {format} copy");
        let Some(destination) = rfd::FileDialog::new()
            .set_title(&title)
            .set_file_name(suggested_name)
            .add_filter(format.to_string(), &[format_extension(format)])
            .save_file()
        else {
            return;
        };
        let selection = AssetExportSelection {
            asset_id,
            format,
            source,
            destination,
        };
        match selection.destination.try_exists() {
            Ok(true) => self.export_ui.overwrite_confirmation = Some(selection),
            Ok(false) => self.start_asset_export(selection, OverwritePolicy::Deny),
            Err(error) => self.status = format!("Could not inspect export destination: {error}"),
        }
    }

    fn start_asset_export(&mut self, selection: AssetExportSelection, overwrite: OverwritePolicy) {
        self.export_ui.overwrite_confirmation = None;
        if self.export_ui.active.is_some() {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        if self.workers.export(ExportRequest {
            asset_id: selection.asset_id,
            source: selection.source.clone(),
            destination: selection.destination.clone(),
            overwrite,
            cancelled: Arc::clone(&cancelled),
        }) {
            if let Some(editor) = &mut self.editor
                && editor
                    .original
                    .assets
                    .iter()
                    .any(|asset| asset.id == selection.asset_id)
            {
                editor.error = None;
            }
            self.status = format!("Starting export to {}…", selection.destination.display());
            self.export_ui.active = Some(ActiveExport {
                selection,
                progress: ExportProgress {
                    copied_bytes: 0,
                    total_bytes: 0,
                },
                cancelled,
                cancelling: false,
            });
        } else {
            "Export worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn cancel_export(&mut self) {
        let Some(active) = &mut self.export_ui.active else {
            return;
        };
        active.cancelled.store(true, Ordering::Relaxed);
        active.cancelling = true;
        "Cancelling export…".clone_into(&mut self.status);
    }

    fn export_overwrite_confirmation_window(&mut self, context: &egui::Context) {
        let Some(selection) = self.export_ui.overwrite_confirmation.clone() else {
            return;
        };
        let response =
            egui::Modal::new(egui::Id::new("export-overwrite-confirmation")).show(context, |ui| {
                ui.set_max_width(500.0);
                ui.heading(format!("Replace existing {} export?", selection.format));
                ui.add_space(6.0);
                ui.label("Source file:");
                ui.label(
                    RichText::new(selection.source.display().to_string())
                        .monospace()
                        .color(MUTED),
                );
                ui.add_space(4.0);
                ui.label("The destination already exists:");
                ui.label(
                    RichText::new(selection.destination.display().to_string())
                        .monospace()
                        .color(MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Only this destination copy will be replaced. The library reference and source file will not change.",
                    )
                    .color(MUTED),
                );
                ui.add_space(14.0);
                let mut replace = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    replace = ui.button("Replace destination").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
                (replace, cancel)
            });
        let should_close = response.should_close();
        let (replace, cancel) = response.inner;
        if replace {
            self.start_asset_export(selection, OverwritePolicy::Allow);
        } else if cancel || should_close {
            self.export_ui.overwrite_confirmation = None;
        }
    }

    fn choose_format_attachment(&mut self, format: BookFormat) {
        if self.importing || self.asset_maintenance.busy() || self.book_removal.removing.is_some() {
            return;
        }
        let Some(editor) = &self.editor else {
            return;
        };
        if editor.changed()
            || editor
                .original
                .assets
                .iter()
                .any(|asset| asset.format == format)
        {
            return;
        }
        let extension = format_extension(format);
        let title = format!("Attach {format} file");
        let Some(path) = rfd::FileDialog::new()
            .set_title(&title)
            .add_filter(format.to_string(), &[extension])
            .pick_file()
        else {
            return;
        };
        let book_id = editor.original.id;
        if self.workers.attach_reference_asset(book_id, format, path) {
            self.asset_maintenance.attaching_format = Some(format);
            if let Some(editor) = &mut self.editor {
                editor.error = None;
            }
            self.status = format!("Validating {format} file…");
        } else {
            "Library maintenance worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn library(&mut self, ui: &mut egui::Ui) {
        if self.query_pending && self.library_total.is_none() {
            centered_message(
                ui,
                "Opening your library…",
                "Reading the local index.",
                true,
            );
            return;
        }
        let book_count = self.library_total.unwrap_or_default();
        if book_count == 0 {
            if self.query.search.trim().is_empty() {
                centered_message(
                    ui,
                    "Your library is ready",
                    "Drop EPUB or PDF files here to start building it.",
                    false,
                );
            } else {
                centered_message(
                    ui,
                    "No books found",
                    "Try a different title, author, series, or publisher.",
                    false,
                );
            }
            return;
        }

        let columns = column_count(ui.available_width());
        let row_count = book_count.div_ceil(columns);
        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("library-grid")
            .auto_shrink([false, false]);
        if let Some(offset) = self
            .benchmark
            .as_ref()
            .and_then(DesktopBenchmark::scroll_offset)
        {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }
        scroll_area.show_rows(ui, CARD_HEIGHT, row_count, |ui, visible_rows| {
            self.request_visible_range(
                visible_rows.start.saturating_mul(columns),
                visible_rows.end.saturating_mul(columns),
            );
            for row in visible_rows {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = CARD_GAP;
                    for column in 0..columns {
                        let index = row * columns + column;
                        if index >= book_count {
                            break;
                        }
                        ui.allocate_ui_with_layout(
                            Vec2::new(CARD_WIDTH, CARD_HEIGHT),
                            egui::Layout::top_down(Align::Center),
                            |ui| {
                                if let Some(book) = self.book_at(index) {
                                    self.book_card(ui, &book);
                                } else {
                                    Self::loading_book_card(ui);
                                }
                            },
                        );
                    }
                });
            }
        });
    }

    fn book_card(&mut self, ui: &mut egui::Ui, book: &BookSummary) {
        let selected = self.selected == Some(book.id);
        let fill = if selected { CARD_SELECTED } else { CARD };
        let frame = egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0, if selected { ACCENT } else { BORDER }))
            .corner_radius(10)
            .inner_margin(10);
        let card = frame.show(ui, |ui| {
            ui.set_min_size(Vec2::new(CARD_WIDTH - 22.0, CARD_HEIGHT - 22.0));
            self.cover(ui, book);
            ui.add_space(7.0);
            ui.add(
                egui::Label::new(RichText::new(&book.title).strong().size(14.0))
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text(&book.title);
            let author = if book.authors.trim().is_empty() {
                "Unknown author"
            } else {
                &book.authors
            };
            ui.add(
                egui::Label::new(RichText::new(author).color(MUTED).size(12.0))
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text(author);
            if book.has_file_issue {
                ui.label(
                    RichText::new("File needs attention")
                        .color(Color32::LIGHT_RED)
                        .size(11.0),
                );
            }
        });
        let response = card
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            self.select_book(book.id);
        }
        if response.hovered() && !selected {
            ui.painter().rect_stroke(
                response.rect,
                10,
                Stroke::new(1.0, Color32::from_rgb(77, 87, 100)),
                StrokeKind::Inside,
            );
        }
    }

    fn loading_book_card(ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_min_size(Vec2::new(CARD_WIDTH - 22.0, CARD_HEIGHT - 22.0));
                ui.add_space((CARD_HEIGHT - COVER_SIZE.y) * 0.5);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(8.0);
                    ui.label(RichText::new("Loading book…").color(MUTED).size(12.0));
                });
            });
    }

    fn cover(&mut self, ui: &mut egui::Ui, book: &BookSummary) {
        if book.has_cover
            && !self.covers.contains_key(&book.id)
            && !self.pending_covers.contains(&book.id)
            && !self.missing_covers.contains(&book.id)
            && self.workers.load_cover(book.id)
        {
            self.pending_covers.insert(book.id);
        }

        if let Some(cover) = self.covers.get_mut(&book.id) {
            cover.last_used = self.frame_number;
            ui.add(cover_image(&cover.texture));
        } else {
            let (rect, _) = ui.allocate_exact_size(COVER_SIZE, Sense::hover());
            ui.painter()
                .rect_filled(rect, 7, Color32::from_rgb(41, 46, 55));
            let glyph = book
                .title
                .chars()
                .find(|character| character.is_alphanumeric())
                .unwrap_or('L');
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                glyph.to_uppercase().to_string(),
                FontId::proportional(38.0),
                Color32::from_rgb(116, 127, 143),
            );
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let export = self
            .export_ui
            .active
            .as_ref()
            .map(|active| (active.progress, active.cancelling));
        let mut cancel_export = false;
        ui.horizontal_centered(|ui| {
            if self.importing || export.is_some() {
                ui.spinner();
            }
            ui.label(RichText::new(&self.status).color(MUTED).size(12.0));
            if let Some((progress, cancelling)) = export {
                let fraction = if progress.total_bytes == 0 {
                    0.0
                } else {
                    export_fraction(progress)
                };
                ui.add(
                    egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                        .desired_width(120.0)
                        .show_percentage(),
                );
                cancel_export = ui
                    .add_enabled(!cancelling, egui::Button::new("Cancel export"))
                    .clicked();
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(self.database_path.display().to_string())
                        .color(Color32::from_rgb(105, 114, 127))
                        .size(11.0),
                );
            });
        });
        if cancel_export {
            self.cancel_export();
        }
    }
}

impl eframe::App for LecternApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.wrapping_add(1);
        if let Some(benchmark) = &mut self.benchmark {
            let unstable_dt = ui.input(|input| input.unstable_dt);
            benchmark.frame_started(frame.info().cpu_usage, unstable_dt);
        }
        self.poll_workers(ui.ctx());
        self.apply_benchmark_sort_request();
        self.apply_benchmark_asset_action_request();
        self.accept_dropped_files(ui);
        let files_hovering = ui.input(|input| !input.raw.hovered_files.is_empty());

        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(24, 16)),
            )
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(18, 7)),
            )
            .show(ui, |ui| self.status_bar(ui));
        if self.selected.is_some() {
            self.metadata_panel(ui);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ui, |ui| self.library(ui));
        self.import_summary_window(ui.ctx());
        self.asset_health_report_window(ui.ctx());
        self.asset_detach_confirmation_window(ui.ctx());
        self.asset_replace_confirmation_window(ui.ctx());
        self.export_overwrite_confirmation_window(ui.ctx());
        self.book_removal_confirmation_window(ui.ctx());

        if files_hovering {
            let rect = ui.max_rect().shrink(18.0);
            ui.painter()
                .rect_filled(rect, 14, Color32::from_rgba_premultiplied(23, 43, 58, 238));
            ui.painter()
                .rect_stroke(rect, 14, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop EPUBs, PDFs, or folders to import",
                FontId::proportional(24.0),
                Color32::WHITE,
            );
        }

        if let Some(benchmark) = &mut self.benchmark {
            benchmark.frame_finished(
                ui.ctx(),
                &BenchmarkFrame {
                    viewport_width: ui.max_rect().width(),
                    viewport_height: ui.max_rect().height(),
                    pixels_per_point: ui.ctx().pixels_per_point(),
                    cached_covers: self.covers.len(),
                    pending_covers: self.pending_covers.len(),
                    missing_covers: self.missing_covers.len(),
                },
            );
        }
    }
}

fn cover_image(texture: impl Into<egui::load::SizedTexture>) -> egui::Image<'static> {
    egui::Image::from_texture(texture)
        .fit_to_exact_size(COVER_SIZE)
        .corner_radius(7)
}

fn default_database_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("LECTERN_DATA_DIR") {
        return PathBuf::from(directory).join("library.sqlite3");
    }
    ProjectDirs::from("com", "Lectern", "Lectern").map_or_else(
        || PathBuf::from("lectern-library.sqlite3"),
        |directories| directories.data_dir().join("library.sqlite3"),
    )
}

fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(15, 17, 21);
    visuals.selection.bg_fill = Color32::from_rgb(42, 91, 124);
    context.set_visuals(visuals);

    context.global_style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(9.0, 7.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
    });
}

fn column_count(available_width: f32) -> usize {
    let mut columns = 1;
    let mut used = CARD_WIDTH;
    while used + CARD_GAP + CARD_WIDTH <= available_width {
        columns += 1;
        used += CARD_GAP + CARD_WIDTH;
    }
    columns
}

const fn query_page_offset(index: usize) -> usize {
    index / QUERY_PAGE_SIZE * QUERY_PAGE_SIZE
}

fn centered_message(ui: &mut egui::Ui, title: &str, detail: &str, spinner: bool) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            if spinner {
                ui.spinner();
                ui.add_space(8.0);
            }
            ui.heading(title);
            ui.label(RichText::new(detail).color(MUTED));
        });
    });
}

fn import_status(progress: ImportProgress) -> String {
    if progress.discovered == 0 {
        return "Discovering book files…".to_owned();
    }
    format!(
        "Importing {}/{} · {} imported · {} failed",
        progress.processed, progress.discovered, progress.imported, progress.failed
    )
}

fn format_export_progress(progress: ExportProgress, cancelling: bool) -> String {
    if cancelling {
        return "Cancelling export…".to_owned();
    }
    format!(
        "Exporting {} of {} MiB…",
        progress.copied_bytes / (1024 * 1024),
        progress.total_bytes / (1024 * 1024)
    )
}

fn export_fraction(progress: ExportProgress) -> f32 {
    let thousandths = progress
        .copied_bytes
        .saturating_mul(1_000)
        .checked_div(progress.total_bytes)
        .unwrap_or_default()
        .min(1_000);
    f32::from(u16::try_from(thousandths).expect("export fraction is bounded")) / 1_000.0
}

fn asset_health_status(report: AssetHealthReport) -> String {
    format!(
        "Checked {} referenced files · {} missing · {} unreadable",
        report.checked, report.missing, report.unreadable
    )
}

fn metadata_text_field(ui: &mut egui::Ui, label: &str, value: &mut String, enabled: bool) {
    ui.add_space(4.0);
    ui.label(RichText::new(label).strong());
    ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(value).desired_width(f32::INFINITY),
    );
}

fn metadata_save_shortcut(
    ui: &mut egui::Ui,
    removal_busy: bool,
    library_operation_busy: bool,
) -> bool {
    !removal_busy
        && !library_operation_busy
        && ui.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::S,
            ))
        })
}

fn metadata_form(
    ui: &mut egui::Ui,
    editor: &mut BookEditor,
    state: MetadataOperationState,
) -> MetadataActions {
    let mut actions = MetadataActions::default();
    let editing_enabled = !editor.saving && !state.removal_busy && !state.library_operation_busy;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            metadata_text_field(ui, "Title", &mut editor.title, editing_enabled);
            if editor.title.trim().is_empty() {
                ui.label(RichText::new("A title is required.").color(Color32::LIGHT_RED));
            }
            metadata_text_field(ui, "Authors", &mut editor.authors, editing_enabled);
            metadata_text_field(ui, "Series", &mut editor.series, editing_enabled);
            metadata_text_field(ui, "Publisher", &mut editor.publisher, editing_enabled);
            metadata_text_field(ui, "Language", &mut editor.language, editing_enabled);

            ui.add_space(4.0);
            ui.label(RichText::new("Description").strong());
            ui.add_enabled(
                editing_enabled,
                egui::TextEdit::multiline(&mut editor.description)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Files").strong());
            asset_rows(
                ui,
                &editor.original,
                state,
                !editor.saving && !state.library_operation_busy && !state.removal_busy,
                &mut actions,
            );
            actions.attach = format_attachment_controls(
                ui,
                editor,
                state.attaching_format,
                state.library_operation_busy || state.removal_busy,
            );

            if let Some(error) = &editor.error {
                ui.add_space(8.0);
                ui.label(RichText::new(error).color(Color32::LIGHT_RED));
            }

            ui.add_space(12.0);
            (actions.save, actions.reset) = metadata_save_controls(ui, editor, editing_enabled);

            ui.add_space(12.0);
            ui.separator();
            actions.remove = book_removal_controls(
                ui,
                editor,
                state.relinking_asset,
                state.removal_busy,
                state.library_operation_busy,
            );
        });
    actions
}

fn asset_rows(
    ui: &mut egui::Ui,
    book: &Book,
    state: MetadataOperationState,
    action_enabled: bool,
    actions: &mut MetadataActions,
) {
    for asset in &book.assets {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} · {}", asset.format, asset.storage))
                    .color(MUTED)
                    .size(12.0),
            );
            let health_color = match asset.health {
                AssetHealth::Available => Color32::LIGHT_GREEN,
                AssetHealth::Missing | AssetHealth::Unreadable => Color32::LIGHT_RED,
                AssetHealth::Unknown => MUTED,
            };
            ui.label(
                RichText::new(asset.health.to_string())
                    .color(health_color)
                    .size(12.0),
            );
            let platform_enabled =
                state.platform_busy.is_none() && asset.storage == AssetStorage::Reference;
            let open_text = if state.platform_busy == Some((asset.id, PlatformAction::Open)) {
                "Opening…"
            } else {
                "Open"
            };
            if ui
                .add_enabled(platform_enabled, egui::Button::new(open_text))
                .on_disabled_hover_text("Managed file locations are not available yet")
                .clicked()
            {
                actions.open = Some((asset.id, asset.path.clone()));
            }
            let menu_text = if state.detaching_asset == Some(asset.id) {
                "Detaching…"
            } else if state.replacing_asset == Some(asset.id) {
                "Replacing…"
            } else if state.exporting_asset == Some(asset.id) {
                "Exporting…"
            } else {
                "File actions"
            };
            ui.add_enabled_ui(
                platform_enabled
                    || (action_enabled
                        && state.relinking_asset.is_none()
                        && state.detaching_asset.is_none()
                        && state.replacing_asset.is_none()),
                |ui| {
                    ui.menu_button(menu_text, |ui| {
                        let reveal_text =
                            if state.platform_busy == Some((asset.id, PlatformAction::Reveal)) {
                                "Revealing…"
                            } else {
                                "Reveal in file manager"
                            };
                        if ui
                            .add_enabled(platform_enabled, egui::Button::new(reveal_text))
                            .clicked()
                        {
                            actions.reveal = Some((asset.id, asset.path.clone()));
                            ui.close();
                        }
                        let export = ui
                            .add_enabled(
                                asset.storage == AssetStorage::Reference
                                    && state.exporting_asset.is_none(),
                                egui::Button::new("Export a copy…"),
                            )
                            .on_disabled_hover_text(if asset.storage == AssetStorage::Managed {
                                "Managed-file export is not available yet"
                            } else {
                                "Another export is already running"
                            });
                        if export.clicked() {
                            actions.export = Some((asset.id, asset.format, asset.path.clone()));
                            ui.close();
                        }
                        if asset.storage == AssetStorage::Reference
                            && asset.health.has_issue()
                            && ui
                                .add_enabled(
                                    action_enabled
                                        && state.relinking_asset.is_none()
                                        && state.detaching_asset.is_none()
                                        && state.replacing_asset.is_none(),
                                    egui::Button::new("Relink"),
                                )
                                .clicked()
                        {
                            actions.relink = Some((asset.id, asset.format));
                            ui.close();
                        }
                        if asset.storage == AssetStorage::Reference
                            && ui
                                .add_enabled(
                                    action_enabled
                                        && state.relinking_asset.is_none()
                                        && state.detaching_asset.is_none()
                                        && state.replacing_asset.is_none(),
                                    egui::Button::new("Replace file"),
                                )
                                .clicked()
                        {
                            actions.replace = Some(AssetReplaceSelection {
                                book_id: book.id,
                                asset_id: asset.id,
                                format: asset.format,
                                current_path: asset.path.clone(),
                            });
                            ui.close();
                        }
                        let detach = ui
                            .add_enabled(
                                book.assets.len() > 1
                                    && action_enabled
                                    && state.relinking_asset.is_none()
                                    && state.detaching_asset.is_none()
                                    && state.replacing_asset.is_none(),
                                egui::Button::new("Detach from book"),
                            )
                            .on_disabled_hover_text("A logical book must retain at least one file");
                        if detach.clicked() {
                            actions.detach = Some(AssetDetachConfirmation {
                                book_id: book.id,
                                asset_id: asset.id,
                                format: asset.format,
                                path: asset.path.clone(),
                            });
                            ui.close();
                        }
                    });
                },
            );
        });
        ui.add(
            egui::Label::new(
                RichText::new(asset.path.display().to_string())
                    .monospace()
                    .color(MUTED)
                    .size(11.0),
            )
            .wrap()
            .selectable(true),
        );
        ui.add_space(4.0);
    }
}

fn format_attachment_controls(
    ui: &mut egui::Ui,
    editor: &BookEditor,
    attaching_format: Option<BookFormat>,
    operation_busy: bool,
) -> Option<BookFormat> {
    let missing = missing_book_formats(&editor.original);
    if missing.is_empty() {
        ui.label(
            RichText::new("All supported formats are attached.")
                .color(MUTED)
                .size(11.0),
        );
        return None;
    }

    let enabled = !editor.saving && !editor.changed() && !operation_busy;
    let hover = if editor.changed() {
        "Save or reset metadata changes before attaching another format"
    } else {
        "Validate and attach another file without changing metadata or the cover"
    };
    let mut selected = None;
    ui.horizontal_wrapped(|ui| {
        for format in missing {
            let text = if attaching_format == Some(format) {
                format!("Attaching {format}…")
            } else {
                format!("Add {format}…")
            };
            if ui
                .add_enabled(enabled, egui::Button::new(text))
                .on_hover_text(hover)
                .clicked()
            {
                selected = Some(format);
            }
        }
    });
    selected
}

fn metadata_save_controls(
    ui: &mut egui::Ui,
    editor: &BookEditor,
    editing_enabled: bool,
) -> (bool, bool) {
    let mut save = false;
    let mut reset = false;
    ui.horizontal(|ui| {
        let button_text = if editor.saving { "Saving…" } else { "Save" };
        save = ui
            .add_enabled(
                editor.can_save() && editing_enabled,
                egui::Button::new(button_text),
            )
            .on_hover_text("Save metadata (Ctrl/Cmd-S)")
            .clicked();
        reset = ui
            .add_enabled(
                editor.changed() && !editor.saving && editing_enabled,
                egui::Button::new("Reset"),
            )
            .clicked();
        if editor.saving {
            ui.spinner();
        }
    });
    (save, reset)
}

fn book_removal_controls(
    ui: &mut egui::Ui,
    editor: &BookEditor,
    relinking_asset: Option<AssetId>,
    removal_busy: bool,
    library_operation_busy: bool,
) -> bool {
    ui.add_space(8.0);
    ui.label(RichText::new("Library").strong());
    ui.label(
        RichText::new("Remove this entry and its cached cover from Lectern. Book files are kept.")
            .color(MUTED)
            .size(12.0),
    );
    let enabled =
        !editor.saving && relinking_asset.is_none() && !removal_busy && !library_operation_busy;
    let button = if removal_busy {
        egui::Button::new("Removing…")
    } else {
        egui::Button::new(RichText::new("Remove from library").color(Color32::LIGHT_RED))
    };
    ui.add_enabled(enabled, button)
        .on_hover_text("Your EPUB and PDF files will not be deleted")
        .clicked()
}

fn removal_file_message(asset_count: usize) -> String {
    if asset_count == 1 {
        "The original book file will not be deleted.".into()
    } else {
        format!("The {asset_count} original book files will not be deleted.")
    }
}

fn missing_book_formats(book: &Book) -> Vec<BookFormat> {
    BookFormat::ALL
        .into_iter()
        .filter(|format| !book.assets.iter().any(|asset| asset.format == *format))
        .collect()
}

fn format_extension(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Epub => "epub",
        BookFormat::Pdf => "pdf",
    }
}

fn optional_metadata(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui;
    use lectern_core::{
        AssetHealth, AssetHealthReport, AssetId, AssetStorage, Book, BookAsset, BookFormat, BookId,
    };
    use lectern_desktop::export::ExportProgress;
    use lectern_import::ImportProgress;

    use super::{
        BookEditor, CARD_GAP, CARD_WIDTH, COVER_SIZE, QUERY_PAGE_SIZE, asset_health_status,
        column_count, cover_image, export_fraction, format_export_progress, import_status,
        query_page_offset, removal_file_message,
    };

    #[test]
    fn grid_always_has_a_column() {
        assert_eq!(column_count(0.0), 1);
        assert_eq!(column_count(CARD_WIDTH), 1);
    }

    #[test]
    fn grid_adds_only_complete_columns() {
        assert_eq!(column_count(CARD_WIDTH * 2.0 + CARD_GAP - 1.0), 1);
        assert_eq!(column_count(CARD_WIDTH * 2.0 + CARD_GAP), 2);
    }

    #[test]
    fn query_pages_align_result_indices_to_fixed_windows() {
        assert_eq!(query_page_offset(0), 0);
        assert_eq!(query_page_offset(QUERY_PAGE_SIZE - 1), 0);
        assert_eq!(query_page_offset(QUERY_PAGE_SIZE), QUERY_PAGE_SIZE);
        assert_eq!(query_page_offset(QUERY_PAGE_SIZE + 17), QUERY_PAGE_SIZE);
    }

    #[test]
    fn decoded_covers_stay_within_the_fixed_grid_slot() {
        egui::__run_test_ui(|ui| {
            for source_size in [egui::vec2(320.0, 480.0), egui::vec2(900.0, 300.0)] {
                let texture =
                    egui::load::SizedTexture::new(egui::TextureId::Managed(1), source_size);
                let response = ui.add(cover_image(texture));

                assert!(response.rect.width() <= COVER_SIZE.x);
                assert!(response.rect.height() <= COVER_SIZE.y);
            }
        });
    }

    #[test]
    fn import_progress_is_human_readable() {
        assert_eq!(
            import_status(ImportProgress::default()),
            "Discovering book files…"
        );
        assert_eq!(
            import_status(ImportProgress {
                discovered: 10,
                processed: 4,
                imported: 3,
                failed: 1,
            }),
            "Importing 4/10 · 3 imported · 1 failed"
        );
    }

    #[test]
    fn export_progress_is_bounded_and_human_readable() {
        let halfway = ExportProgress {
            copied_bytes: 128 * 1024 * 1024,
            total_bytes: 256 * 1024 * 1024,
        };

        assert_eq!(export_fraction(halfway), 0.5);
        assert_eq!(
            format_export_progress(halfway, false),
            "Exporting 128 of 256 MiB…"
        );
        assert_eq!(format_export_progress(halfway, true), "Cancelling export…");
    }

    #[test]
    fn asset_health_status_is_human_readable() {
        assert_eq!(
            asset_health_status(AssetHealthReport {
                checked: 10,
                available: 7,
                missing: 2,
                unreadable: 1,
                changed: 3,
            }),
            "Checked 10 referenced files · 2 missing · 1 unreadable"
        );
    }

    #[test]
    fn removal_confirmation_describes_preserved_source_files() {
        assert_eq!(
            removal_file_message(1),
            "The original book file will not be deleted."
        );
        assert_eq!(
            removal_file_message(2),
            "The 2 original book files will not be deleted."
        );
    }

    #[test]
    fn attachment_options_include_only_missing_formats() {
        let mut book = Book {
            id: BookId::new(7),
            title: "Dune".into(),
            authors: "Frank Herbert".into(),
            series: None,
            contributors: Vec::new(),
            series_membership: None,
            tags: Vec::new(),
            publisher: None,
            language: None,
            description: None,
            assets: vec![BookAsset {
                id: AssetId::new(11),
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
                health: AssetHealth::Available,
                path: PathBuf::from("/books/dune.epub"),
            }],
        };

        assert_eq!(super::missing_book_formats(&book), vec![BookFormat::Pdf]);
        assert_eq!(super::format_extension(BookFormat::Pdf), "pdf");
        book.assets.push(BookAsset {
            id: AssetId::new(12),
            format: BookFormat::Pdf,
            storage: AssetStorage::Reference,
            health: AssetHealth::Available,
            path: PathBuf::from("/books/dune.pdf"),
        });
        assert!(super::missing_book_formats(&book).is_empty());
    }

    #[test]
    fn metadata_editor_normalizes_saved_values() {
        let book = Book {
            id: BookId::new(7),
            title: "Dune".into(),
            authors: "Frank Herbert".into(),
            series: Some("Dune".into()),
            contributors: Vec::new(),
            series_membership: None,
            tags: Vec::new(),
            publisher: None,
            language: Some("en".into()),
            description: None,
            assets: vec![BookAsset {
                id: AssetId::new(11),
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
                health: AssetHealth::Unknown,
                path: PathBuf::from("/books/dune.epub"),
            }],
        };
        let mut editor = BookEditor::new(book);
        editor.title = "  Dune Messiah  ".into();
        editor.series = "   ".into();

        let saved = editor.book();

        assert_eq!(saved.title, "Dune Messiah");
        assert_eq!(saved.series, None);
        assert!(editor.changed());
    }
}
