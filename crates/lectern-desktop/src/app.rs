use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use eframe::egui::{self, Align, Color32, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};
use lectern_core::{
    AssetHealth, AssetHealthReport, AssetId, AssetStorage, Book, BookFormat, BookId, BookSummary,
    ImportProgress, ImportSummary, LibraryQuery, SortOrder,
    organisation::{
        BookEdit, BookSelection, BulkTagEdit, BulkTagResult, ContributorFacet, ContributorId,
        ContributorRole, ContributorUsage, ExactFacets, LibraryGeneration, NameKind, SavedSearch,
        SavedSearchId, SearchExpression, SearchParseError, SelectionSnapshot, SelectionTagUsage,
        SeriesId, SeriesIndex, SeriesUsage, TagId, TagReference, TagUsage,
        VocabularyMutationResult, identity_key, normalize_name,
    },
};
use lectern_desktop::export::{ExportError, ExportProgress, OverwritePolicy};
use lectern_service::{SqliteLibraryService, default_database_path};

use crate::{
    benchmark::{BenchmarkFrame, DesktopBenchmark},
    curation::{BookCurationDraft, SeriesDraft},
    platform::{NoopAssetPlatform, PlatformAction, PlatformWorker, SystemAssetPlatform},
    workers::{
        DecodedCover, ExportRequest, ImportRequest, QueryQueueResult, QueryRequest,
        SavedSearchMutation, SelectionRequest, VocabularyEntityId, VocabularyKind,
        VocabularyMutation, VocabularyRequest, VocabularyRows, WorkerEvent, WorkerSet,
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
const VOCABULARY_PAGE_SIZE: u64 = 100;
const MAX_CACHED_QUERY_PAGES: usize = 6;

struct CachedCover {
    texture: egui::TextureHandle,
    last_used: u64,
}

struct CachedPage {
    books: Vec<BookSummary>,
    last_used: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum GridSelectionMode {
    Explicit(HashSet<BookId>),
    AllMatching {
        query: LibraryQuery,
        generation: LibraryGeneration,
        matching_books: u64,
        excluded: HashSet<BookId>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SelectionAnchor {
    index: usize,
    book_id: BookId,
}

#[derive(Default)]
struct GridSelection {
    mode: Option<GridSelectionMode>,
    anchor: Option<SelectionAnchor>,
}

impl GridSelection {
    fn is_active(&self) -> bool {
        self.mode.is_some()
    }

    fn contains(&self, id: BookId) -> bool {
        match &self.mode {
            Some(GridSelectionMode::Explicit(books)) => books.contains(&id),
            Some(GridSelectionMode::AllMatching { excluded, .. }) => !excluded.contains(&id),
            None => false,
        }
    }

    fn selected_count(&self) -> u64 {
        match &self.mode {
            Some(GridSelectionMode::Explicit(books)) => {
                u64::try_from(books.len()).unwrap_or(u64::MAX)
            }
            Some(GridSelectionMode::AllMatching {
                matching_books,
                excluded,
                ..
            }) => matching_books.saturating_sub(u64::try_from(excluded.len()).unwrap_or(u64::MAX)),
            None => 0,
        }
    }

    fn is_every_matching(&self) -> bool {
        matches!(
            &self.mode,
            Some(GridSelectionMode::AllMatching { excluded, .. }) if excluded.is_empty()
        )
    }

    fn matching_count(&self) -> Option<u64> {
        match &self.mode {
            Some(GridSelectionMode::AllMatching { matching_books, .. }) => Some(*matching_books),
            Some(GridSelectionMode::Explicit(_)) | None => None,
        }
    }

    fn toggle(&mut self, id: BookId, index: usize) {
        match self
            .mode
            .get_or_insert_with(|| GridSelectionMode::Explicit(HashSet::with_capacity(1)))
        {
            GridSelectionMode::Explicit(books) => {
                if !books.insert(id) {
                    books.remove(&id);
                }
            }
            GridSelectionMode::AllMatching { excluded, .. } => {
                if !excluded.insert(id) {
                    excluded.remove(&id);
                }
            }
        }
        self.anchor = Some(SelectionAnchor { index, book_id: id });
        if self.selected_count() == 0 {
            self.clear();
        }
    }

    fn install_range(&mut self, books: Vec<BookId>) {
        let books = books.into_iter().collect::<HashSet<_>>();
        if books.is_empty() {
            self.clear();
        } else {
            self.mode = Some(GridSelectionMode::Explicit(books));
        }
    }

    fn install_all_matching(&mut self, query: LibraryQuery, snapshot: SelectionSnapshot) {
        if snapshot.matching_books == 0 {
            self.clear();
            return;
        }
        self.mode = Some(GridSelectionMode::AllMatching {
            query,
            generation: snapshot.generation,
            matching_books: snapshot.matching_books,
            excluded: HashSet::new(),
        });
        self.anchor = None;
    }

    fn descriptor(&self) -> Option<BookSelection> {
        match &self.mode {
            Some(GridSelectionMode::Explicit(books)) => {
                Some(BookSelection::explicit(books.iter().copied().collect()))
            }
            Some(GridSelectionMode::AllMatching {
                query,
                generation,
                excluded,
                ..
            }) => Some(BookSelection::all_matching(
                query.clone(),
                *generation,
                excluded.iter().copied().collect(),
            )),
            None => None,
        }
    }

    fn clear(&mut self) {
        self.mode = None;
        self.anchor = None;
    }
}

enum PendingSelection {
    Range,
    AllMatching { query: LibraryQuery },
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
    contributor_lookup: Option<ContributorLookup>,
    series_lookup: Option<SeriesLookup>,
    tag_lookup: Option<TagLookup>,
}

struct BookEditor {
    original: Book,
    original_edit: BookEdit,
    title: String,
    publisher: String,
    language: String,
    description: String,
    curation: BookCurationDraft,
    contributor_suggestions: SuggestionState<ContributorUsage>,
    contributor_suggestion_row: Option<u64>,
    series_suggestions: SuggestionState<SeriesUsage>,
    tag_suggestions: SuggestionState<TagUsage>,
    tag_input: String,
    series_clear_restore: Option<SeriesDraft>,
    saving: bool,
    error: Option<String>,
}

struct ContributorLookup {
    generation: u64,
    row_id: u64,
    prefix: String,
}

struct SeriesLookup {
    generation: u64,
    prefix: String,
}

struct TagLookup {
    generation: u64,
    prefix: String,
}

struct FacetContributorLookup {
    generation: u64,
    prefix: String,
}

struct FacetSeriesLookup {
    generation: u64,
    prefix: String,
}

struct FacetTagLookup {
    generation: u64,
    prefix: String,
}

struct SuggestionState<T> {
    generation: u64,
    pending: bool,
    results: Vec<T>,
    error: Option<String>,
}

impl<T> Default for SuggestionState<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            pending: false,
            results: Vec::new(),
            error: None,
        }
    }
}

impl<T> SuggestionState<T> {
    fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = true;
        self.results.clear();
        self.error = None;
        self.generation
    }

    fn install(&mut self, generation: u64, result: Result<Vec<T>, String>) {
        if generation != self.generation {
            return;
        }
        self.pending = false;
        match result {
            Ok(results) => {
                self.results = results;
                self.error = None;
            }
            Err(error) => {
                self.results.clear();
                self.error = Some(error);
            }
        }
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum TagFacetMode {
    #[default]
    Include,
    Exclude,
}

#[derive(Default)]
struct FilterUi {
    contributor_input: String,
    contributor_author_only: bool,
    contributor_suggestions: SuggestionState<ContributorUsage>,
    series_input: String,
    series_suggestions: SuggestionState<SeriesUsage>,
    tag_input: String,
    tag_mode: TagFacetMode,
    tag_suggestions: SuggestionState<TagUsage>,
    contributor_labels: HashMap<ContributorId, String>,
    series_labels: HashMap<SeriesId, String>,
    tag_labels: HashMap<TagId, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BulkTagIntent {
    Add,
    Remove,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BulkTagActivity {
    #[default]
    Idle,
    LoadingPage,
    Applying,
}

#[derive(Default)]
struct BulkTagUi {
    discard_confirmation: bool,
    generation: u64,
    selection: Option<BookSelection>,
    selected_books: u64,
    page_offset: u64,
    page: Vec<SelectionTagUsage>,
    has_next_page: bool,
    intents: HashMap<TagId, BulkTagIntent>,
    queued_tags: HashMap<TagId, TagUsage>,
    new_tags: HashMap<String, String>,
    tag_input: String,
    suggestions: SuggestionState<TagUsage>,
    activity: BulkTagActivity,
    error: Option<String>,
}

impl BulkTagUi {
    fn is_open(&self) -> bool {
        self.selection.is_some()
    }

    fn page_pending(&self) -> bool {
        self.activity == BulkTagActivity::LoadingPage
    }

    fn applying(&self) -> bool {
        self.activity == BulkTagActivity::Applying
    }
}

#[derive(Default)]
struct SavedSearchUi {
    generation: u64,
    searches: Vec<SavedSearch>,
    initialized: bool,
    pending: bool,
    active: Option<SavedSearchId>,
    error: Option<String>,
    dialog: Option<SavedSearchDialog>,
}

enum SavedSearchDialog {
    Create {
        name: String,
        pending: bool,
        error: Option<String>,
    },
    Rename {
        search: SavedSearch,
        name: String,
        pending: bool,
        error: Option<String>,
    },
    Delete {
        search: SavedSearch,
        pending: bool,
        error: Option<String>,
    },
}

enum SavedSearchAction {
    Apply(SavedSearch),
    Create,
    Update(SavedSearchId),
    Rename(SavedSearch),
    Delete(SavedSearch),
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum OrganiserSection {
    #[default]
    Contributors,
    Series,
    Tags,
    SavedSearches,
}

impl OrganiserSection {
    const ALL: [Self; 4] = [
        Self::Contributors,
        Self::Series,
        Self::Tags,
        Self::SavedSearches,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Contributors => "Contributors",
            Self::Series => "Series",
            Self::Tags => "Tags",
            Self::SavedSearches => "Saved searches",
        }
    }

    const fn vocabulary_kind(self) -> Option<VocabularyKind> {
        match self {
            Self::Contributors => Some(VocabularyKind::Contributors),
            Self::Series => Some(VocabularyKind::Series),
            Self::Tags => Some(VocabularyKind::Tags),
            Self::SavedSearches => None,
        }
    }
}

#[derive(Clone)]
enum VocabularyEntity {
    Contributor(ContributorUsage),
    Series(SeriesUsage),
    Tag(TagUsage),
}

impl VocabularyEntity {
    const fn id(&self) -> VocabularyEntityId {
        match self {
            Self::Contributor(usage) => VocabularyEntityId::Contributor(usage.contributor.id),
            Self::Series(usage) => VocabularyEntityId::Series(usage.series.id),
            Self::Tag(usage) => VocabularyEntityId::Tag(usage.tag.id),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Contributor(usage) => &usage.contributor.display_name,
            Self::Series(usage) => &usage.series.name,
            Self::Tag(usage) => &usage.tag.name,
        }
    }

    const fn books(&self) -> u64 {
        match self {
            Self::Contributor(usage) => usage.books,
            Self::Series(usage) => usage.books,
            Self::Tag(usage) => usage.books,
        }
    }

    const fn saved_searches(&self) -> Option<u64> {
        match self {
            Self::Tag(usage) => Some(usage.saved_searches),
            Self::Contributor(_) | Self::Series(_) => None,
        }
    }
}

enum VocabularyPage {
    Contributors(Vec<ContributorUsage>),
    Series(Vec<SeriesUsage>),
    Tags(Vec<TagUsage>),
    SavedSearches(Vec<SavedSearch>),
}

impl VocabularyPage {
    fn len(&self) -> usize {
        match self {
            Self::Contributors(rows) => rows.len(),
            Self::Series(rows) => rows.len(),
            Self::Tags(rows) => rows.len(),
            Self::SavedSearches(rows) => rows.len(),
        }
    }
}

struct RenameVocabularyDialog {
    entity: VocabularyEntity,
    name: String,
    sort_name: String,
    pending: bool,
    error: Option<String>,
}

struct MergeVocabularyDialog {
    source: VocabularyEntity,
    input: String,
    suggestions: SuggestionState<VocabularyEntity>,
    target: Option<VocabularyEntity>,
    impact: Option<VocabularyMutationResult>,
    impact_pending: bool,
    pending: bool,
    error: Option<String>,
}

struct DeleteVocabularyDialog {
    entity: VocabularyEntity,
    impact: Option<VocabularyMutationResult>,
    impact_pending: bool,
    pending: bool,
    error: Option<String>,
}

enum VocabularyDialog {
    Rename(RenameVocabularyDialog),
    Merge(MergeVocabularyDialog),
    Delete(DeleteVocabularyDialog),
}

enum OrganiserRowAction {
    Rename(VocabularyEntity),
    Merge(VocabularyEntity),
    Delete(VocabularyEntity),
}

#[derive(Default)]
struct OrganiserUi {
    open: bool,
    section: OrganiserSection,
    search_input: String,
    applied_prefix: String,
    generation: u64,
    offset: u64,
    pending: bool,
    page: Option<VocabularyPage>,
    error: Option<String>,
    dialog: Option<VocabularyDialog>,
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
        let curation = BookCurationDraft::from_book(&book);
        let original_edit = curation
            .to_book_edit(
                &book,
                &book.title,
                book.publisher.as_deref().unwrap_or_default(),
                book.language.as_deref().unwrap_or_default(),
                book.description.as_deref().unwrap_or_default(),
            )
            .expect("stored normalized book metadata is editable");
        Self {
            title: book.title.clone(),
            publisher: book.publisher.clone().unwrap_or_default(),
            language: book.language.clone().unwrap_or_default(),
            description: book.description.clone().unwrap_or_default(),
            curation,
            original_edit,
            original: book,
            contributor_suggestions: SuggestionState::default(),
            contributor_suggestion_row: None,
            series_suggestions: SuggestionState::default(),
            tag_suggestions: SuggestionState::default(),
            tag_input: String::new(),
            series_clear_restore: None,
            saving: false,
            error: None,
        }
    }

    fn edit(&self) -> Result<BookEdit, String> {
        self.curation.to_book_edit(
            &self.original,
            &self.title,
            &self.publisher,
            &self.language,
            &self.description,
        )
    }

    fn changed(&self) -> bool {
        self.edit().map_or(true, |edit| edit != self.original_edit)
    }

    fn can_save(&self) -> bool {
        !self.saving && self.changed() && self.edit().is_ok()
    }
}

pub(crate) struct LecternApp {
    database_path: PathBuf,
    workers: WorkerSet,
    query: LibraryQuery,
    search_input: String,
    search_error: Option<SearchParseError>,
    filters: FilterUi,
    organiser: OrganiserUi,
    saved_searches: SavedSearchUi,
    bulk_tags: BulkTagUi,
    query_generation: u64,
    query_pending: bool,
    library_total: Option<usize>,
    pages: HashMap<usize, CachedPage>,
    pending_pages: HashSet<usize>,
    selected: Option<BookId>,
    grid_selection: GridSelection,
    selection_generation: u64,
    selection_pending: Option<PendingSelection>,
    grid_focus_id: Option<egui::Id>,
    bulk_generation: u64,
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
        let status = match SqliteLibraryService::open(&database_path) {
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
            search_input: String::new(),
            search_error: None,
            filters: FilterUi::default(),
            organiser: OrganiserUi::default(),
            saved_searches: SavedSearchUi::default(),
            bulk_tags: BulkTagUi::default(),
            query_generation: 0,
            query_pending: false,
            library_total: None,
            pages: HashMap::new(),
            pending_pages: HashSet::new(),
            selected: None,
            grid_selection: GridSelection::default(),
            selection_generation: 0,
            selection_pending: None,
            grid_focus_id: None,
            bulk_generation: 0,
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
        self.clear_grid_selection();
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
                WorkerEvent::SelectionSnapshotFinished { generation, result }
                    if generation == self.selection_generation =>
                {
                    self.selection_snapshot_finished(result);
                }
                WorkerEvent::SelectionRangeFinished { generation, result }
                    if generation == self.selection_generation =>
                {
                    self.selection_range_finished(result);
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
                        Ok(Some(book)) => {
                            let book_id = book.id;
                            self.editor = Some(BookEditor::new(book));
                            if let Some(benchmark) = &mut self.benchmark {
                                benchmark.editor_installed(book_id);
                            }
                        }
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
                WorkerEvent::BookSaved { id, result } => self.book_saved(id, result),
                WorkerEvent::ContributorSuggestions {
                    generation,
                    row_id,
                    result,
                } => {
                    if let Some(editor) = &mut self.editor
                        && editor.contributor_suggestion_row == Some(row_id)
                    {
                        editor.contributor_suggestions.install(generation, result);
                    }
                }
                WorkerEvent::SeriesSuggestions { generation, result } => {
                    if let Some(editor) = &mut self.editor {
                        editor.series_suggestions.install(generation, result);
                    }
                }
                WorkerEvent::TagSuggestions { generation, result } => {
                    if let Some(editor) = &mut self.editor {
                        editor.tag_suggestions.install(generation, result);
                    }
                }
                WorkerEvent::FacetContributorSuggestions { generation, result } => {
                    if let Ok(rows) = &result {
                        for usage in rows {
                            self.filters.contributor_labels.insert(
                                usage.contributor.id,
                                usage.contributor.display_name.clone(),
                            );
                        }
                    }
                    self.filters
                        .contributor_suggestions
                        .install(generation, result);
                }
                WorkerEvent::FacetSeriesSuggestions { generation, result } => {
                    if let Ok(rows) = &result {
                        for usage in rows {
                            self.filters
                                .series_labels
                                .insert(usage.series.id, usage.series.name.clone());
                        }
                    }
                    self.filters.series_suggestions.install(generation, result);
                }
                WorkerEvent::FacetTagSuggestions { generation, result } => {
                    if let Ok(rows) = &result {
                        for usage in rows {
                            self.filters
                                .tag_labels
                                .insert(usage.tag.id, usage.tag.name.clone());
                        }
                    }
                    self.filters.tag_suggestions.install(generation, result);
                }
                WorkerEvent::BulkTagSuggestions { generation, result } => {
                    self.bulk_tags.suggestions.install(generation, result);
                }
                WorkerEvent::MergeContributorSuggestions { generation, result } => {
                    self.install_merge_suggestions(
                        generation,
                        result.map(|rows| {
                            rows.into_iter()
                                .map(VocabularyEntity::Contributor)
                                .collect()
                        }),
                    );
                }
                WorkerEvent::MergeSeriesSuggestions { generation, result } => {
                    self.install_merge_suggestions(
                        generation,
                        result.map(|rows| rows.into_iter().map(VocabularyEntity::Series).collect()),
                    );
                }
                WorkerEvent::MergeTagSuggestions { generation, result } => {
                    self.install_merge_suggestions(
                        generation,
                        result.map(|rows| rows.into_iter().map(VocabularyEntity::Tag).collect()),
                    );
                }
                WorkerEvent::VocabularyLoaded {
                    generation,
                    kind,
                    offset,
                    result,
                } => self.vocabulary_loaded(generation, kind, offset, result),
                WorkerEvent::VocabularyImpact { entity, result } => {
                    self.vocabulary_impact_loaded(entity, result);
                }
                WorkerEvent::VocabularyMutated { mutation, result } => {
                    self.vocabulary_mutated(&mutation, result);
                }
                WorkerEvent::SelectionTagsLoaded {
                    generation,
                    offset,
                    result,
                } => self.selection_tags_loaded(generation, offset, result),
                WorkerEvent::BulkTagsApplied { generation, result } => {
                    self.bulk_tags_applied(generation, result);
                }
                WorkerEvent::SavedSearchesLoaded { generation, result } => {
                    self.saved_searches_loaded(generation, result);
                }
                WorkerEvent::SavedSearchPageLoaded {
                    generation,
                    offset,
                    result,
                } => self.saved_search_page_loaded(generation, offset, result),
                WorkerEvent::SavedSearchMutated {
                    generation,
                    mutation,
                    result,
                } => self.saved_search_mutated(generation, mutation, result),
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
                | WorkerEvent::SelectionSnapshotFinished { .. }
                | WorkerEvent::SelectionRangeFinished { .. }
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

    fn apply_benchmark_editor_request(&mut self) {
        let close_editor = self
            .benchmark
            .as_mut()
            .is_some_and(DesktopBenchmark::take_editor_close_request);
        if close_editor {
            self.clear_selection();
        }
        let book_id = self
            .benchmark
            .as_mut()
            .and_then(DesktopBenchmark::next_editor_request);
        if let Some(book_id) = book_id {
            self.select_book(book_id);
            if self.editor_loading != Some(book_id)
                && let Some(benchmark) = &mut self.benchmark
            {
                benchmark.editor_dispatch_failed();
            }
        }
    }

    fn apply_benchmark_selection_request(&mut self) {
        let clear = self
            .benchmark
            .as_mut()
            .is_some_and(DesktopBenchmark::take_selection_clear_request);
        if clear {
            self.clear_grid_selection();
        }
        let select_all = self
            .benchmark
            .as_mut()
            .is_some_and(DesktopBenchmark::next_selection_request);
        if select_all {
            self.select_all_matching();
            if self.selection_pending.is_none()
                && let Some(benchmark) = &mut self.benchmark
            {
                benchmark.selection_dispatch_failed();
            }
        }
    }

    fn apply_benchmark_bulk_tag_request(&mut self) {
        let query_requested = self
            .benchmark
            .as_mut()
            .is_some_and(DesktopBenchmark::next_bulk_query_request);
        if query_requested {
            self.query = LibraryQuery {
                search: "language:fr".to_owned(),
                sort: self.query.sort,
                ..LibraryQuery::default()
            };
            self.refresh_library();
            return;
        }

        let selection_requested = self
            .benchmark
            .as_mut()
            .is_some_and(DesktopBenchmark::next_bulk_selection_request);
        if selection_requested {
            self.select_all_matching();
            if self.selection_pending.is_none()
                && let Some(benchmark) = &mut self.benchmark
            {
                benchmark
                    .bulk_dispatch_failed("bulk benchmark selection request could not be queued");
            }
            return;
        }

        let edit = self
            .benchmark
            .as_mut()
            .and_then(DesktopBenchmark::next_bulk_edit);
        let Some(edit) = edit else {
            return;
        };
        let Some(selection) = self.grid_selection.descriptor() else {
            if let Some(benchmark) = &mut self.benchmark {
                benchmark.bulk_dispatch_failed(
                    "bulk benchmark edit had no installed selection descriptor",
                );
            }
            return;
        };
        let selected_books = self.grid_selection.selected_count();
        self.bulk_generation = self.bulk_generation.wrapping_add(1);
        self.bulk_tags = BulkTagUi {
            generation: self.bulk_generation,
            selection: Some(selection.clone()),
            selected_books,
            activity: BulkTagActivity::Applying,
            ..BulkTagUi::default()
        };
        if !self
            .workers
            .apply_bulk_tags(self.bulk_tags.generation, selection, edit)
        {
            self.bulk_tags.activity = BulkTagActivity::Idle;
            if let Some(benchmark) = &mut self.benchmark {
                benchmark.bulk_dispatch_failed(
                    "bulk benchmark edit could not be queued on the metadata worker",
                );
            }
        }
    }

    fn selection_shortcuts(&mut self, context: &egui::Context) {
        if (self.grid_selection.is_active() || self.selection_pending.is_some())
            && context
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.clear_grid_selection();
            "Selection cleared".clone_into(&mut self.status);
            return;
        }
        let grid_focused = self
            .grid_focus_id
            .is_some_and(|id| context.memory(|memory| memory.focused() == Some(id)));
        if grid_focused
            && context.input_mut(|input| {
                input.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::COMMAND,
                    egui::Key::A,
                ))
            })
        {
            self.select_all_matching();
        }
    }

    fn query_discarded(&mut self, offset: u64) {
        if let Ok(offset) = usize::try_from(offset) {
            self.pending_pages.remove(&offset);
        }
        self.query_pending = self.library_total.is_none() || self.pending_pages.contains(&0);
    }

    fn reset_bulk_tags(&mut self) {
        self.bulk_generation = self.bulk_generation.wrapping_add(1);
        self.bulk_tags = BulkTagUi::default();
    }

    fn clear_grid_selection(&mut self) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection_pending = None;
        self.grid_selection.clear();
        self.grid_focus_id = None;
        self.reset_bulk_tags();
    }

    fn toggle_grid_book(&mut self, id: BookId, index: usize) {
        self.reset_bulk_tags();
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection_pending = None;
        self.grid_selection.toggle(id, index);
        self.status = if self.grid_selection.is_active() {
            format!("{} selected", self.grid_selection.selected_count())
        } else {
            "Selection cleared".to_owned()
        };
    }

    fn select_range_to(&mut self, id: BookId, index: usize) {
        self.reset_bulk_tags();
        let Some(anchor) = self.grid_selection.anchor else {
            self.toggle_grid_book(id, index);
            return;
        };
        let offset = anchor.index.min(index);
        let length = anchor.index.max(index).saturating_sub(offset) + 1;
        let (Ok(offset), Ok(limit)) = (u64::try_from(offset), u32::try_from(length)) else {
            "Selection range exceeds this platform's supported size".clone_into(&mut self.status);
            return;
        };
        let generation = self.selection_generation.wrapping_add(1);
        let request = SelectionRequest::Range {
            generation,
            query: self.query.clone(),
            offset,
            limit,
        };
        if self.workers.resolve_selection(request) {
            self.selection_generation = generation;
            self.selection_pending = Some(PendingSelection::Range);
            self.status = format!("Selecting {length} books…");
        } else {
            "Selection worker is busy; try the range again".clone_into(&mut self.status);
        }
    }

    fn select_all_matching(&mut self) {
        self.reset_bulk_tags();
        if self.library_total == Some(0) || self.selection_pending.is_some() {
            return;
        }
        let generation = self.selection_generation.wrapping_add(1);
        let query = self.query.clone();
        let request = SelectionRequest::Snapshot {
            generation,
            query: query.clone(),
        };
        if self.workers.resolve_selection(request) {
            self.selection_generation = generation;
            self.selection_pending = Some(PendingSelection::AllMatching { query });
            "Selecting all matching books…".clone_into(&mut self.status);
        } else {
            "Selection worker is busy; try again".clone_into(&mut self.status);
        }
    }

    fn selection_snapshot_finished(&mut self, result: Result<SelectionSnapshot, String>) {
        let Some(PendingSelection::AllMatching { query }) = self.selection_pending.take() else {
            return;
        };
        match result {
            Ok(snapshot) => {
                self.grid_selection.install_all_matching(query, snapshot);
                self.status = format!("All {} matching selected", snapshot.matching_books);
                if let Some(benchmark) = &mut self.benchmark {
                    benchmark.selection_installed(snapshot.matching_books);
                    benchmark.bulk_selection_installed(snapshot.matching_books);
                }
            }
            Err(error) => self.status = format!("Could not select matching books: {error}"),
        }
    }

    fn selection_range_finished(&mut self, result: Result<Vec<BookId>, String>) {
        if !matches!(self.selection_pending.take(), Some(PendingSelection::Range)) {
            return;
        }
        match result {
            Ok(books) => {
                self.grid_selection.install_range(books);
                self.status = format!("{} selected", self.grid_selection.selected_count());
            }
            Err(error) => self.status = format!("Could not select range: {error}"),
        }
    }

    fn request_bulk_tag_panel(&mut self) {
        if self.grid_selection.selected_count() == 0 || self.selection_pending.is_some() {
            return;
        }
        if self.editor.as_ref().is_some_and(BookEditor::changed) {
            self.bulk_tags.discard_confirmation = true;
            return;
        }
        self.open_bulk_tag_panel();
    }

    fn open_bulk_tag_panel(&mut self) {
        let Some(selection) = self.grid_selection.descriptor() else {
            return;
        };
        let selected_books = self.grid_selection.selected_count();
        self.clear_selection();
        self.bulk_generation = self.bulk_generation.wrapping_add(1);
        self.bulk_tags = BulkTagUi {
            generation: self.bulk_generation,
            selection: Some(selection),
            selected_books,
            ..BulkTagUi::default()
        };
        self.load_bulk_tag_page(0);
    }

    fn load_bulk_tag_page(&mut self, offset: u64) {
        let Some(selection) = self.bulk_tags.selection.clone() else {
            return;
        };
        self.bulk_tags.activity = BulkTagActivity::LoadingPage;
        self.bulk_tags.error = None;
        if !self
            .workers
            .load_selection_tags(self.bulk_tags.generation, selection, offset)
        {
            self.bulk_tags.activity = BulkTagActivity::Idle;
            self.bulk_tags.error = Some("Metadata worker is unavailable".to_owned());
        }
    }

    fn selection_tags_loaded(
        &mut self,
        generation: u64,
        offset: u64,
        result: Result<Vec<SelectionTagUsage>, String>,
    ) {
        if !self.bulk_tags.is_open() || generation != self.bulk_tags.generation {
            return;
        }
        self.bulk_tags.activity = BulkTagActivity::Idle;
        match result {
            Ok(page) => {
                self.bulk_tags.has_next_page = page.len() == 100;
                self.bulk_tags.page_offset = offset;
                self.bulk_tags.page = page;
                self.bulk_tags.error = None;
            }
            Err(error) => {
                self.bulk_tags.page.clear();
                self.bulk_tags.error = Some(error);
            }
        }
    }

    fn apply_bulk_tag_changes(&mut self) {
        if self.bulk_tags.applying() {
            return;
        }
        let Some(selection) = self.bulk_tags.selection.clone() else {
            return;
        };
        let BulkTagEdit { add, remove } =
            build_bulk_tag_edit(&self.bulk_tags.intents, &self.bulk_tags.new_tags);
        if add.is_empty() && remove.is_empty() {
            return;
        }
        let edit = BulkTagEdit { add, remove };
        if self
            .workers
            .apply_bulk_tags(self.bulk_tags.generation, selection, edit)
        {
            self.bulk_tags.activity = BulkTagActivity::Applying;
            self.bulk_tags.error = None;
            "Applying tag changes…".clone_into(&mut self.status);
        } else {
            self.bulk_tags.error = Some("Metadata worker is unavailable".to_owned());
        }
    }

    fn bulk_tags_applied(&mut self, generation: u64, result: Result<BulkTagResult, String>) {
        if !self.bulk_tags.is_open() || generation != self.bulk_tags.generation {
            return;
        }
        self.bulk_tags.activity = BulkTagActivity::Idle;
        match result {
            Ok(result) => {
                self.status = format!(
                    "Updated {} books · {} tag links added · {} removed · {} tags created",
                    result.books_matched,
                    result.relationships_added,
                    result.relationships_removed,
                    result.tags_created,
                );
                if let Some(benchmark) = &mut self.benchmark {
                    benchmark.bulk_tags_completed(result);
                }
                self.refresh_library();
            }
            Err(error) => {
                if let Some(benchmark) = &mut self.benchmark {
                    benchmark.bulk_dispatch_failed(format!("bulk-tag apply failed: {error}"));
                }
                self.bulk_tags.error = Some(error.clone());
                self.status = format!("Could not apply tag changes: {error}");
            }
        }
    }

    fn reload_saved_searches(&mut self) {
        self.saved_searches.generation = self.saved_searches.generation.wrapping_add(1);
        self.saved_searches.initialized = true;
        self.saved_searches.pending = true;
        self.saved_searches.error = None;
        if !self
            .workers
            .load_saved_searches(self.saved_searches.generation)
        {
            self.saved_searches.pending = false;
            self.saved_searches.error = Some("Metadata worker is unavailable".to_owned());
        }
    }

    fn saved_searches_loaded(&mut self, generation: u64, result: Result<Vec<SavedSearch>, String>) {
        if generation != self.saved_searches.generation {
            return;
        }
        self.saved_searches.pending = false;
        match result {
            Ok(searches) => {
                if self
                    .saved_searches
                    .active
                    .is_some_and(|id| !searches.iter().any(|search| search.id == id))
                {
                    self.saved_searches.active = None;
                }
                self.saved_searches.searches = searches;
                self.saved_searches.error = None;
            }
            Err(error) => self.saved_searches.error = Some(error),
        }
    }

    fn active_saved_search(&self) -> Option<&SavedSearch> {
        let id = self.saved_searches.active?;
        self.saved_searches
            .searches
            .iter()
            .find(|search| search.id == id)
    }

    fn saved_search_modified(&self) -> bool {
        saved_search_is_modified(self.active_saved_search(), &self.query)
    }

    fn apply_saved_search(&mut self, search: SavedSearch) {
        if !self
            .saved_searches
            .searches
            .iter()
            .any(|candidate| candidate.id == search.id)
        {
            self.saved_searches.searches.push(search.clone());
        }
        self.query = search.query;
        self.search_input.clone_from(&self.query.search);
        self.search_error = None;
        self.saved_searches.active = Some(search.id);
        self.filters.contributor_input.clear();
        self.filters.series_input.clear();
        self.filters.tag_input.clear();
        self.filters.contributor_suggestions = SuggestionState::default();
        self.filters.series_suggestions = SuggestionState::default();
        self.filters.tag_suggestions = SuggestionState::default();
        self.hydrate_active_facet_labels();
        self.status = format!("Applied saved search {}", search.name);
        self.refresh_library();
    }

    fn hydrate_active_facet_labels(&mut self) {
        let contributors = self
            .query
            .facets
            .contributors
            .iter()
            .map(|facet| facet.contributor)
            .collect::<Vec<_>>();
        if !contributors.is_empty() {
            let generation = self.filters.contributor_suggestions.begin();
            if !self.workers.autocomplete_facet_contributors(
                generation,
                String::new(),
                contributors,
            ) {
                self.filters.contributor_suggestions.pending = false;
            }
        }
        if let Some(series) = self.query.facets.series {
            let generation = self.filters.series_suggestions.begin();
            if !self
                .workers
                .autocomplete_facet_series(generation, String::new(), vec![series])
            {
                self.filters.series_suggestions.pending = false;
            }
        }
        let mut tags = self.query.facets.included_tags.clone();
        tags.extend_from_slice(&self.query.facets.excluded_tags);
        tags.sort_unstable();
        tags.dedup();
        if !tags.is_empty() {
            let generation = self.filters.tag_suggestions.begin();
            if !self
                .workers
                .autocomplete_facet_tags(generation, String::new(), tags)
            {
                self.filters.tag_suggestions.pending = false;
            }
        }
    }

    fn begin_saved_search_action(&mut self, action: SavedSearchAction) {
        match action {
            SavedSearchAction::Apply(search) => self.apply_saved_search(search),
            SavedSearchAction::Create => {
                self.saved_searches.dialog = Some(SavedSearchDialog::Create {
                    name: String::new(),
                    pending: false,
                    error: None,
                });
            }
            SavedSearchAction::Update(id) => {
                self.start_saved_search_mutation(SavedSearchMutation::Update {
                    id,
                    query: self.query.clone(),
                });
            }
            SavedSearchAction::Rename(search) => {
                self.saved_searches.dialog = Some(SavedSearchDialog::Rename {
                    name: search.name.clone(),
                    search,
                    pending: false,
                    error: None,
                });
            }
            SavedSearchAction::Delete(search) => {
                self.saved_searches.dialog = Some(SavedSearchDialog::Delete {
                    search,
                    pending: false,
                    error: None,
                });
            }
        }
    }

    fn start_saved_search_mutation(&mut self, mutation: SavedSearchMutation) {
        if self.saved_searches.pending {
            return;
        }
        self.saved_searches.generation = self.saved_searches.generation.wrapping_add(1);
        self.saved_searches.pending = true;
        self.saved_searches.error = None;
        set_saved_search_dialog_pending(&mut self.saved_searches.dialog, true);
        if !self
            .workers
            .mutate_saved_search(self.saved_searches.generation, mutation)
        {
            self.saved_searches.pending = false;
            set_saved_search_dialog_pending(&mut self.saved_searches.dialog, false);
            set_saved_search_dialog_error(
                &mut self.saved_searches.dialog,
                "Metadata worker is unavailable".to_owned(),
            );
        }
    }

    fn saved_search_mutated(
        &mut self,
        generation: u64,
        mutation: SavedSearchMutation,
        result: Result<Vec<SavedSearch>, String>,
    ) {
        if generation != self.saved_searches.generation {
            return;
        }
        self.saved_searches.pending = false;
        match result {
            Ok(searches) => {
                self.saved_searches.initialized = true;
                match mutation {
                    SavedSearchMutation::Create { name, .. } => {
                        let key = identity_key(&name);
                        self.saved_searches.active = searches
                            .iter()
                            .find(|search| identity_key(&search.name) == key)
                            .map(|search| search.id);
                        self.status = format!("Saved current search as {name}");
                    }
                    SavedSearchMutation::Update { id, .. } => {
                        self.saved_searches.active = Some(id);
                        "Updated saved search".clone_into(&mut self.status);
                    }
                    SavedSearchMutation::Rename { id, name } => {
                        if self.saved_searches.active == Some(id) {
                            self.saved_searches.active = Some(id);
                        }
                        self.status = format!("Renamed saved search to {name}");
                    }
                    SavedSearchMutation::Delete { id } => {
                        if self.saved_searches.active == Some(id) {
                            self.saved_searches.active = None;
                        }
                        "Deleted saved search; books and vocabulary were unchanged"
                            .clone_into(&mut self.status);
                    }
                }
                self.saved_searches.searches = searches;
                self.saved_searches.dialog = None;
                self.saved_searches.error = None;
                self.clear_grid_selection();
                if self.organiser.open && self.organiser.section == OrganiserSection::SavedSearches
                {
                    self.organiser.page = None;
                    self.request_vocabulary_page(self.organiser.offset);
                }
            }
            Err(error) => {
                set_saved_search_dialog_pending(&mut self.saved_searches.dialog, false);
                if self.saved_searches.dialog.is_some() {
                    set_saved_search_dialog_error(&mut self.saved_searches.dialog, error);
                } else {
                    self.saved_searches.error = Some(error);
                }
            }
        }
    }

    fn book_saved(&mut self, id: BookId, result: Result<Book, String>) {
        match result {
            Ok(book) => {
                self.status = format!("Saved metadata for {}", book.title);
                if self.editor.as_ref().map(|editor| editor.original.id) == Some(id) {
                    self.editor = Some(BookEditor::new(book));
                }
                self.refresh_library();
            }
            Err(error) => {
                self.status = format!("Could not save metadata: {error}");
                if let Some(editor) = &mut self.editor
                    && editor.original.id == id
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
        let mut contributor_lookup = None;
        let mut series_lookup = None;
        let mut tag_lookup = None;
        let mut saved_search_action = None;
        ui.horizontal_centered(|ui| {
            let search = egui::TextEdit::singleline(&mut self.search_input)
                .hint_text("Search or use fields such as author:, tag:, format:…")
                .desired_width(380.0);
            if ui.add_sized([380.0, 34.0], search).changed() {
                match apply_search_input(&mut self.query, &self.search_input) {
                    Ok(changed) => {
                        query_changed |= changed;
                        self.search_error = None;
                    }
                    Err(error) => self.search_error = Some(error),
                }
            }

            ui.menu_button("Search help", |ui| {
                ui.label(RichText::new("Safe fielded search").strong());
                ui.label("title:foundation  author:le  contributor:\"ursula le guin\"");
                ui.label("series:earthsea  tag:\"science fiction\"  publisher:ace");
                ui.label("language:en  format:epub  file:missing");
                ui.separator();
                ui.label(RichText::new("Bare terms search all text fields. Clauses combine with AND; OR, regex, wildcards, grouping, and raw FTS are not accepted.").color(MUTED));
            });

            saved_search_action = self.saved_search_menu(ui);

            let facet_count = self.query.facets.contributors.len()
                + self.query.facets.included_tags.len()
                + self.query.facets.excluded_tags.len()
                + usize::from(self.query.facets.series.is_some());
            let filter_label = if facet_count == 0 {
                "Filters".to_owned()
            } else {
                format!("Filters ({facet_count})")
            };
            ui.menu_button(filter_label, |ui| {
                query_changed |= self.filters_popover(
                    ui,
                    &mut contributor_lookup,
                    &mut series_lookup,
                    &mut tag_lookup,
                );
            });

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

        if let Some(error) = &self.search_error {
            ui.label(
                RichText::new(error.to_string())
                    .color(Color32::LIGHT_RED)
                    .size(12.0),
            );
        }
        query_changed |= self.active_facet_chips(ui);

        if let Some(error) = &self.saved_searches.error {
            ui.label(
                RichText::new(format!("Saved searches: {error}"))
                    .color(Color32::LIGHT_RED)
                    .size(12.0),
            );
        }

        self.dispatch_facet_contributor_lookup(contributor_lookup);
        self.dispatch_facet_series_lookup(series_lookup);
        self.dispatch_facet_tag_lookup(tag_lookup);

        if query_changed {
            self.refresh_library();
        }
        if let Some(action) = saved_search_action {
            self.begin_saved_search_action(action);
        }
    }

    fn saved_search_menu(&mut self, ui: &mut egui::Ui) -> Option<SavedSearchAction> {
        let active = self.active_saved_search().cloned();
        let modified = self.saved_search_modified();
        let label = active.as_ref().map_or_else(
            || "Saved searches".to_owned(),
            |search| {
                format!(
                    "{}{}",
                    search.name,
                    if modified { " · Modified" } else { "" }
                )
            },
        );
        let mut action = None;
        let mut request_load = false;
        ui.menu_button(label, |ui| {
            ui.set_min_width(310.0);
            if !self.saved_searches.initialized {
                request_load = true;
            }
            if self.saved_searches.pending || !self.saved_searches.initialized {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Loading saved searches…").color(MUTED));
                });
            }
            if ui
                .add_enabled(
                    self.search_error.is_none()
                        && self.saved_searches.initialized
                        && !self.saved_searches.pending,
                    egui::Button::new("Save current search…"),
                )
                .clicked()
            {
                action = Some(SavedSearchAction::Create);
                ui.close();
            }
            if let Some(search) = active.as_ref() {
                if ui
                    .add_enabled(
                        modified && self.search_error.is_none() && !self.saved_searches.pending,
                        egui::Button::new("Update saved search"),
                    )
                    .clicked()
                {
                    action = Some(SavedSearchAction::Update(search.id));
                    ui.close();
                }
                if ui
                    .add_enabled(!self.saved_searches.pending, egui::Button::new("Rename…"))
                    .clicked()
                {
                    action = Some(SavedSearchAction::Rename(search.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(!self.saved_searches.pending, egui::Button::new("Delete…"))
                    .clicked()
                {
                    action = Some(SavedSearchAction::Delete(search.clone()));
                    ui.close();
                }
            }
            ui.separator();
            if self.saved_searches.initialized
                && self.saved_searches.searches.is_empty()
                && !self.saved_searches.pending
            {
                ui.label(RichText::new("No saved searches").color(MUTED));
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("saved-search-toolbar-list")
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for search in &self.saved_searches.searches {
                            let selected = self.saved_searches.active == Some(search.id);
                            if ui
                                .selectable_label(selected, &search.name)
                                .on_hover_text(saved_search_summary(search))
                                .clicked()
                            {
                                action = Some(SavedSearchAction::Apply(search.clone()));
                                ui.close();
                            }
                        }
                    });
            }
        });
        if request_load {
            self.reload_saved_searches();
        }
        action
    }

    fn filters_popover(
        &mut self,
        ui: &mut egui::Ui,
        contributor_lookup: &mut Option<FacetContributorLookup>,
        series_lookup: &mut Option<FacetSeriesLookup>,
        tag_lookup: &mut Option<FacetTagLookup>,
    ) -> bool {
        let mut changed = false;
        ui.set_min_width(390.0);
        ui.label(RichText::new("Required contributors").strong());
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.filters.contributor_input)
                        .hint_text("Find contributor")
                        .desired_width(235.0),
                )
                .changed()
            {
                let generation = self.filters.contributor_suggestions.begin();
                *contributor_lookup = Some(FacetContributorLookup {
                    generation,
                    prefix: self.filters.contributor_input.clone(),
                });
            }
            ui.checkbox(
                &mut self.filters.contributor_author_only,
                "Author role only",
            );
        });
        suggestion_status(ui, &self.filters.contributor_suggestions);
        let mut contributor_selected = None;
        for (index, usage) in self
            .filters
            .contributor_suggestions
            .results
            .iter()
            .take(8)
            .enumerate()
        {
            if ui
                .button(format!(
                    "{} · {} books",
                    usage.contributor.display_name, usage.books
                ))
                .clicked()
            {
                contributor_selected = Some(index);
            }
        }
        if let Some(index) = contributor_selected {
            let usage = &self.filters.contributor_suggestions.results[index];
            let facet = ContributorFacet {
                contributor: usage.contributor.id,
                author_only: self.filters.contributor_author_only,
            };
            self.query
                .facets
                .contributors
                .retain(|existing| existing.contributor != facet.contributor);
            self.query.facets.contributors.push(facet);
            self.query.facets.contributors.sort_unstable();
            self.filters
                .contributor_labels
                .insert(usage.contributor.id, usage.contributor.display_name.clone());
            self.filters.contributor_input.clear();
            self.filters.contributor_suggestions.results.clear();
            changed = true;
        }

        ui.separator();
        ui.label(RichText::new("Series").strong());
        if ui
            .add(
                egui::TextEdit::singleline(&mut self.filters.series_input)
                    .hint_text("Find series")
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            let generation = self.filters.series_suggestions.begin();
            *series_lookup = Some(FacetSeriesLookup {
                generation,
                prefix: self.filters.series_input.clone(),
            });
        }
        suggestion_status(ui, &self.filters.series_suggestions);
        let mut series_selected = None;
        for (index, usage) in self
            .filters
            .series_suggestions
            .results
            .iter()
            .take(8)
            .enumerate()
        {
            if ui
                .button(format!("{} · {} books", usage.series.name, usage.books))
                .clicked()
            {
                series_selected = Some(index);
            }
        }
        if let Some(index) = series_selected {
            let usage = &self.filters.series_suggestions.results[index];
            self.query.facets.series = Some(usage.series.id);
            self.filters
                .series_labels
                .insert(usage.series.id, usage.series.name.clone());
            self.filters.series_input.clear();
            self.filters.series_suggestions.results.clear();
            changed = true;
        }

        ui.separator();
        ui.label(RichText::new("Tags").strong());
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.filters.tag_mode,
                TagFacetMode::Include,
                "Require tag",
            );
            ui.selectable_value(
                &mut self.filters.tag_mode,
                TagFacetMode::Exclude,
                "Exclude tag",
            );
        });
        if ui
            .add(
                egui::TextEdit::singleline(&mut self.filters.tag_input)
                    .hint_text("Find tag")
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            let generation = self.filters.tag_suggestions.begin();
            *tag_lookup = Some(FacetTagLookup {
                generation,
                prefix: self.filters.tag_input.clone(),
            });
        }
        suggestion_status(ui, &self.filters.tag_suggestions);
        let mut tag_selected = None;
        for (index, usage) in self
            .filters
            .tag_suggestions
            .results
            .iter()
            .take(8)
            .enumerate()
        {
            if ui
                .button(format!("{} · {} books", usage.tag.name, usage.books))
                .clicked()
            {
                tag_selected = Some(index);
            }
        }
        if let Some(index) = tag_selected {
            let usage = &self.filters.tag_suggestions.results[index];
            match self.filters.tag_mode {
                TagFacetMode::Include => self.query.facets.include_tag(usage.tag.id),
                TagFacetMode::Exclude => self.query.facets.exclude_tag(usage.tag.id),
            }
            self.filters
                .tag_labels
                .insert(usage.tag.id, usage.tag.name.clone());
            self.filters.tag_input.clear();
            self.filters.tag_suggestions.results.clear();
            changed = true;
        }
        changed
    }

    fn active_facet_chips(&mut self, ui: &mut egui::Ui) -> bool {
        if self.query.facets == ExactFacets::default() {
            return false;
        }
        let mut remove_contributor = None;
        let mut remove_series = false;
        let mut remove_included = None;
        let mut remove_excluded = None;
        let mut clear = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Active filters").color(MUTED).size(12.0));
            for facet in &self.query.facets.contributors {
                let name = self
                    .filters
                    .contributor_labels
                    .get(&facet.contributor)
                    .cloned()
                    .unwrap_or_else(|| format!("Contributor {}", facet.contributor.value()));
                let role = if facet.author_only { " · Author" } else { "" };
                if ui.button(format!("{name}{role} ×")).clicked() {
                    remove_contributor = Some(facet.contributor);
                }
            }
            if let Some(series) = self.query.facets.series {
                let name = self
                    .filters
                    .series_labels
                    .get(&series)
                    .cloned()
                    .unwrap_or_else(|| format!("Series {}", series.value()));
                remove_series = ui.button(format!("{name} ×")).clicked();
            }
            for tag in &self.query.facets.included_tags {
                let name = self
                    .filters
                    .tag_labels
                    .get(tag)
                    .cloned()
                    .unwrap_or_else(|| format!("Tag {}", tag.value()));
                if ui.button(format!("+{name} ×")).clicked() {
                    remove_included = Some(*tag);
                }
            }
            for tag in &self.query.facets.excluded_tags {
                let name = self
                    .filters
                    .tag_labels
                    .get(tag)
                    .cloned()
                    .unwrap_or_else(|| format!("Tag {}", tag.value()));
                if ui.button(format!("−{name} ×")).clicked() {
                    remove_excluded = Some(*tag);
                }
            }
            clear = ui.small_button("Clear all").clicked();
        });
        if clear {
            self.query.facets = ExactFacets::default();
            return true;
        }
        let mut changed = false;
        if let Some(id) = remove_contributor {
            self.query
                .facets
                .contributors
                .retain(|facet| facet.contributor != id);
            changed = true;
        }
        if remove_series {
            self.query.facets.series = None;
            changed = true;
        }
        if let Some(id) = remove_included {
            self.query.facets.included_tags.retain(|tag| *tag != id);
            changed = true;
        }
        if let Some(id) = remove_excluded {
            self.query.facets.excluded_tags.retain(|tag| *tag != id);
            changed = true;
        }
        changed
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
                if ui.button("Organise library").clicked() {
                    self.open_organiser();
                }
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

    fn organiser_window(&mut self, context: &egui::Context) {
        if !self.organiser.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Organise library")
            .id(egui::Id::new("organise-library"))
            .open(&mut open)
            .default_size([760.0, 620.0])
            .min_size([620.0, 440.0])
            .show(context, |ui| self.organiser_contents(ui));
        self.organiser.open = open;
    }

    fn organiser_contents(&mut self, ui: &mut egui::Ui) {
        let mut requested_section = None;
        ui.horizontal(|ui| {
            for section in OrganiserSection::ALL {
                if ui
                    .selectable_label(self.organiser.section == section, section.label())
                    .clicked()
                {
                    requested_section = Some(section);
                }
            }
        });
        if let Some(section) = requested_section
            && section != self.organiser.section
        {
            self.organiser.section = section;
            self.organiser.search_input.clear();
            self.organiser.applied_prefix.clear();
            self.organiser.offset = 0;
            self.organiser.page = None;
            self.organiser.error = None;
            self.request_vocabulary_page(0);
        }

        ui.separator();
        if self.organiser.section == OrganiserSection::SavedSearches {
            self.saved_search_organiser_contents(ui);
            return;
        }

        let mut search = false;
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.organiser.search_input)
                    .hint_text(format!(
                        "Search {}",
                        self.organiser.section.label().to_lowercase()
                    ))
                    .desired_width(360.0),
            );
            search |=
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            search |= ui
                .add_enabled(!self.organiser.pending, egui::Button::new("Search"))
                .clicked();
            if !self.organiser.applied_prefix.is_empty() && ui.button("Clear").clicked() {
                self.organiser.search_input.clear();
                search = true;
            }
            if self.organiser.pending {
                ui.spinner();
            }
        });
        if search {
            self.organiser.applied_prefix = self.organiser.search_input.trim().to_owned();
            self.request_vocabulary_page(0);
        }

        if let Some(error) = &self.organiser.error {
            ui.label(RichText::new(error).color(Color32::LIGHT_RED));
        }

        let mut row_action = None;
        if let Some(page) = &self.organiser.page {
            let row_count = page.len();
            let first = self.organiser.offset.saturating_add(1);
            let last = self
                .organiser
                .offset
                .saturating_add(u64::try_from(row_count).unwrap_or(u64::MAX));
            ui.label(
                RichText::new(if row_count == 0 {
                    "No matching entities".to_owned()
                } else {
                    format!("Showing {first}–{last} · global usage counts")
                })
                .color(MUTED)
                .size(12.0),
            );
            egui::ScrollArea::vertical()
                .id_salt("vocabulary-rows")
                .max_height(450.0)
                .show_rows(ui, 38.0, row_count, |ui, range| {
                    for index in range {
                        let Some(entity) = vocabulary_entity_at(page, index) else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            ui.set_min_height(34.0);
                            ui.label(RichText::new(entity.name()).strong());
                            ui.label(
                                RichText::new(vocabulary_usage_label(&entity))
                                    .color(MUTED)
                                    .size(12.0),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Delete").clicked() {
                                    row_action = Some(OrganiserRowAction::Delete(entity.clone()));
                                }
                                if ui.small_button("Merge…").clicked() {
                                    row_action = Some(OrganiserRowAction::Merge(entity.clone()));
                                }
                                if ui.small_button("Rename…").clicked() {
                                    row_action = Some(OrganiserRowAction::Rename(entity.clone()));
                                }
                            });
                        });
                        ui.separator();
                    }
                });

            let has_previous = self.organiser.offset > 0;
            let has_next = row_count == usize::try_from(VOCABULARY_PAGE_SIZE).unwrap();
            let mut previous = false;
            let mut next = false;
            ui.horizontal(|ui| {
                previous = ui
                    .add_enabled(
                        has_previous && !self.organiser.pending,
                        egui::Button::new("Previous 100"),
                    )
                    .clicked();
                next = ui
                    .add_enabled(
                        has_next && !self.organiser.pending,
                        egui::Button::new("Next 100"),
                    )
                    .clicked();
            });
            if previous {
                self.request_vocabulary_page(
                    self.organiser.offset.saturating_sub(VOCABULARY_PAGE_SIZE),
                );
            } else if next {
                self.request_vocabulary_page(
                    self.organiser.offset.saturating_add(VOCABULARY_PAGE_SIZE),
                );
            }
        } else if self.organiser.pending {
            centered_message(
                ui,
                "Loading organisation",
                "Reading one bounded page…",
                true,
            );
        }

        if let Some(action) = row_action {
            self.begin_vocabulary_action(action);
        }
    }

    fn saved_search_organiser_contents(&mut self, ui: &mut egui::Ui) {
        ui.heading("Saved searches");
        ui.label(
            RichText::new(
                "Each saved search is a complete query, exact-filter, format, file-health, and sort projection.",
            )
            .color(MUTED),
        );
        let mut search_requested = false;
        let mut create = false;
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.organiser.search_input)
                    .hint_text("Search saved searches")
                    .desired_width(330.0),
            );
            search_requested |=
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            search_requested |= ui
                .add_enabled(!self.organiser.pending, egui::Button::new("Search"))
                .clicked();
            if !self.organiser.applied_prefix.is_empty() && ui.button("Clear").clicked() {
                self.organiser.search_input.clear();
                search_requested = true;
            }
            create = ui
                .add_enabled(
                    self.search_error.is_none() && !self.saved_searches.pending,
                    egui::Button::new("Save current search…"),
                )
                .clicked();
            if self.organiser.pending {
                ui.spinner();
            }
        });
        if search_requested {
            self.organiser.applied_prefix = self.organiser.search_input.trim().to_owned();
            self.request_vocabulary_page(0);
        }
        if let Some(error) = &self.organiser.error {
            ui.label(RichText::new(error).color(Color32::LIGHT_RED));
        }

        let mut action = create.then_some(SavedSearchAction::Create);
        if let Some(VocabularyPage::SavedSearches(searches)) = &self.organiser.page {
            let row_count = searches.len();
            let first = self.organiser.offset.saturating_add(1);
            let last = self
                .organiser
                .offset
                .saturating_add(u64::try_from(row_count).unwrap_or(u64::MAX));
            ui.label(
                RichText::new(if row_count == 0 {
                    "No matching saved searches".to_owned()
                } else {
                    format!("Showing {first}–{last} · at most 100 rows")
                })
                .color(MUTED)
                .size(12.0),
            );
            egui::ScrollArea::vertical()
                .id_salt("saved-search-manager-rows")
                .max_height(450.0)
                .show_rows(ui, 52.0, row_count, |ui, range| {
                    for index in range {
                        let Some(search) = searches.get(index) else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            ui.set_min_height(48.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&search.name).strong());
                                ui.label(
                                    RichText::new(saved_search_summary(search))
                                        .color(MUTED)
                                        .size(11.0),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add_enabled(
                                        !self.saved_searches.pending,
                                        egui::Button::new("Delete…"),
                                    )
                                    .clicked()
                                {
                                    action = Some(SavedSearchAction::Delete(search.clone()));
                                }
                                if ui
                                    .add_enabled(
                                        !self.saved_searches.pending,
                                        egui::Button::new("Rename…"),
                                    )
                                    .clicked()
                                {
                                    action = Some(SavedSearchAction::Rename(search.clone()));
                                }
                                if ui.button("Apply").clicked() {
                                    action = Some(SavedSearchAction::Apply(search.clone()));
                                }
                            });
                        });
                        ui.separator();
                    }
                });

            let has_previous = self.organiser.offset > 0;
            let has_next = row_count == usize::try_from(VOCABULARY_PAGE_SIZE).unwrap();
            let mut previous = false;
            let mut next = false;
            ui.horizontal(|ui| {
                previous = ui
                    .add_enabled(
                        has_previous && !self.organiser.pending,
                        egui::Button::new("Previous 100"),
                    )
                    .clicked();
                next = ui
                    .add_enabled(
                        has_next && !self.organiser.pending,
                        egui::Button::new("Next 100"),
                    )
                    .clicked();
            });
            if previous {
                self.request_vocabulary_page(
                    self.organiser.offset.saturating_sub(VOCABULARY_PAGE_SIZE),
                );
            } else if next {
                self.request_vocabulary_page(
                    self.organiser.offset.saturating_add(VOCABULARY_PAGE_SIZE),
                );
            }
        } else if self.organiser.pending {
            centered_message(
                ui,
                "Loading saved searches",
                "Reading one bounded page…",
                true,
            );
        }
        if let Some(action) = action {
            self.begin_saved_search_action(action);
        }
    }

    fn begin_vocabulary_action(&mut self, action: OrganiserRowAction) {
        match action {
            OrganiserRowAction::Rename(entity) => {
                let sort_name = match &entity {
                    VocabularyEntity::Contributor(usage) => usage.contributor.sort_name.clone(),
                    VocabularyEntity::Series(_) | VocabularyEntity::Tag(_) => String::new(),
                };
                let name = entity.name().to_owned();
                self.organiser.dialog = Some(VocabularyDialog::Rename(RenameVocabularyDialog {
                    entity,
                    name,
                    sort_name,
                    pending: false,
                    error: None,
                }));
            }
            OrganiserRowAction::Merge(source) => {
                self.organiser.dialog = Some(VocabularyDialog::Merge(MergeVocabularyDialog {
                    source,
                    input: String::new(),
                    suggestions: SuggestionState::default(),
                    target: None,
                    impact: None,
                    impact_pending: false,
                    pending: false,
                    error: None,
                }));
            }
            OrganiserRowAction::Delete(entity) => {
                let entity_id = entity.id();
                let mut dialog = DeleteVocabularyDialog {
                    entity,
                    impact: None,
                    impact_pending: true,
                    pending: false,
                    error: None,
                };
                if !self.workers.vocabulary_impact(entity_id) {
                    dialog.impact_pending = false;
                    dialog.error = Some("Metadata worker is unavailable".to_owned());
                }
                self.organiser.dialog = Some(VocabularyDialog::Delete(dialog));
            }
        }
    }

    fn vocabulary_dialog_window(&mut self, context: &egui::Context) {
        let Some(mut dialog) = self.organiser.dialog.take() else {
            return;
        };
        let mut close = false;
        let mut mutation = None;
        let mut impact_request = None;
        let mut merge_lookup = None;
        match &mut dialog {
            VocabularyDialog::Rename(rename) => {
                egui::Modal::new(egui::Id::new("rename-vocabulary")).show(context, |ui| {
                    ui.heading(format!("Rename {}", rename.entity.name()));
                    ui.label(
                        "This updates every affected book projection and saved search atomically.",
                    );
                    ui.label("Display name");
                    ui.add(egui::TextEdit::singleline(&mut rename.name).desired_width(420.0));
                    if matches!(rename.entity, VocabularyEntity::Contributor(_)) {
                        ui.label("Sort name");
                        ui.add(
                            egui::TextEdit::singleline(&mut rename.sort_name).desired_width(420.0),
                        );
                    }
                    vocabulary_dialog_error(ui, rename.error.as_deref());
                    ui.horizontal(|ui| {
                        if rename.pending {
                            ui.spinner();
                        }
                        if ui
                            .add_enabled(!rename.pending, egui::Button::new("Rename").fill(ACCENT))
                            .clicked()
                        {
                            mutation = rename_vocabulary_mutation(rename);
                            rename.pending = mutation.is_some();
                            if mutation.is_none() {
                                rename.error = Some("Name cannot be empty".to_owned());
                            }
                        }
                        if ui
                            .add_enabled(!rename.pending, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            }
            VocabularyDialog::Merge(merge) => {
                egui::Modal::new(egui::Id::new("merge-vocabulary")).show(context, |ui| {
                    ui.heading(format!("Merge {}", merge.source.name()));
                    ui.label("Choose the entity to keep. The named source will be removed.");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut merge.input)
                            .hint_text("Search for the target to keep")
                            .desired_width(440.0),
                    );
                    if response.changed() {
                        merge.target = None;
                        merge.impact = None;
                        let generation = merge.suggestions.begin();
                        merge_lookup = Some((generation, merge.input.clone(), merge.source.id()));
                    }
                    suggestion_status(ui, &merge.suggestions);
                    let mut selected = None;
                    for (index, entity) in merge.suggestions.results.iter().take(8).enumerate() {
                        if ui
                            .button(format!("{} · {} books", entity.name(), entity.books()))
                            .clicked()
                        {
                            selected = Some(index);
                        }
                    }
                    if let Some(index) = selected {
                        let target = merge.suggestions.results[index].clone();
                        target.name().clone_into(&mut merge.input);
                        merge.target = Some(target);
                        merge.suggestions.results.clear();
                        merge.impact = None;
                    }
                    if let Some(target) = &merge.target {
                        ui.label(
                            RichText::new(format!(
                                "Source: {}  →  Keep: {}",
                                merge.source.name(),
                                target.name()
                            ))
                            .strong(),
                        );
                    }
                    if merge.impact_pending {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking affected references…");
                        });
                    }
                    if let Some(impact) = merge.impact {
                        vocabulary_confirmation_copy(ui, "Merge", impact);
                    }
                    vocabulary_dialog_error(ui, merge.error.as_deref());
                    ui.horizontal(|ui| {
                        let can_review = merge.target.is_some()
                            && !merge.impact_pending
                            && !merge.pending
                            && merge.impact.is_none();
                        if ui
                            .add_enabled(can_review, egui::Button::new("Review merge"))
                            .clicked()
                        {
                            merge.impact_pending = true;
                            merge.error = None;
                            impact_request = Some(merge.source.id());
                        }
                        if ui
                            .add_enabled(
                                merge.impact.is_some() && !merge.pending,
                                egui::Button::new("Confirm merge").fill(ACCENT),
                            )
                            .clicked()
                        {
                            mutation = merge_vocabulary_mutation(merge);
                            merge.pending = mutation.is_some();
                        }
                        if merge.pending {
                            ui.spinner();
                        }
                        if ui
                            .add_enabled(!merge.pending, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            }
            VocabularyDialog::Delete(delete) => {
                egui::Modal::new(egui::Id::new("delete-vocabulary")).show(context, |ui| {
                    ui.heading(format!("Delete {}?", delete.entity.name()));
                    if delete.impact_pending {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking affected references…");
                        });
                    }
                    let can_delete = delete.impact.is_some_and(|impact| {
                        matches!(delete.entity, VocabularyEntity::Tag(_))
                            || (impact.books == 0 && impact.saved_searches == 0)
                    });
                    if let Some(impact) = delete.impact {
                        vocabulary_confirmation_copy(ui, "Delete", impact);
                        if !can_delete {
                            ui.label(
                                RichText::new(
                                    "Contributors and series can be deleted only when unused. Merge it instead.",
                                )
                                .color(Color32::LIGHT_RED),
                            );
                        }
                    }
                    vocabulary_dialog_error(ui, delete.error.as_deref());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                can_delete && !delete.pending,
                                egui::Button::new("Confirm delete").fill(Color32::DARK_RED),
                            )
                            .clicked()
                        {
                            mutation = delete_vocabulary_mutation(delete);
                            delete.pending = mutation.is_some();
                        }
                        if delete.pending {
                            ui.spinner();
                        }
                        if ui
                            .add_enabled(!delete.pending, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            }
        }

        if let Some((generation, prefix, source)) = merge_lookup {
            let queued = match source {
                VocabularyEntityId::Contributor(source) => self
                    .workers
                    .autocomplete_merge_contributors(generation, prefix, source),
                VocabularyEntityId::Series(source) => self
                    .workers
                    .autocomplete_merge_series(generation, prefix, source),
                VocabularyEntityId::Tag(source) => self
                    .workers
                    .autocomplete_merge_tags(generation, prefix, source),
            };
            if !queued && let VocabularyDialog::Merge(merge) = &mut dialog {
                merge.suggestions.pending = false;
                merge.suggestions.error = Some("Autocomplete worker is unavailable".to_owned());
            }
        }
        if let Some(entity) = impact_request
            && !self.workers.vocabulary_impact(entity)
        {
            match &mut dialog {
                VocabularyDialog::Merge(merge) => {
                    merge.impact_pending = false;
                    merge.error = Some("Metadata worker is unavailable".to_owned());
                }
                VocabularyDialog::Delete(delete) => {
                    delete.impact_pending = false;
                    delete.error = Some("Metadata worker is unavailable".to_owned());
                }
                VocabularyDialog::Rename(_) => {}
            }
        }
        if let Some(request) = mutation
            && !self.workers.mutate_vocabulary(request)
        {
            match &mut dialog {
                VocabularyDialog::Rename(rename) => {
                    rename.pending = false;
                    rename.error = Some("Metadata worker is unavailable".to_owned());
                }
                VocabularyDialog::Merge(merge) => {
                    merge.pending = false;
                    merge.error = Some("Metadata worker is unavailable".to_owned());
                }
                VocabularyDialog::Delete(delete) => {
                    delete.pending = false;
                    delete.error = Some("Metadata worker is unavailable".to_owned());
                }
            }
        }
        if !close {
            self.organiser.dialog = Some(dialog);
        }
    }

    fn saved_search_dialog_window(&mut self, context: &egui::Context) {
        let mut mutation = None;
        let mut cancel = false;
        match &mut self.saved_searches.dialog {
            Some(SavedSearchDialog::Create {
                name,
                pending,
                error,
            }) => {
                let normalized = normalize_name(NameKind::SavedSearch, name);
                let response =
                    egui::Modal::new(egui::Id::new("create-saved-search")).show(context, |ui| {
                        ui.set_max_width(450.0);
                        ui.heading("Save current search");
                        ui.label(
                            RichText::new(
                                "This saves the complete query, exact filters, format, file health, and sort order.",
                            )
                            .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.add_enabled(
                            !*pending,
                            egui::TextEdit::singleline(name)
                                .hint_text("Saved-search name")
                                .desired_width(f32::INFINITY),
                        );
                        if let Err(validation) = &normalized {
                            ui.label(
                                RichText::new(validation.to_string()).color(Color32::LIGHT_RED),
                            );
                        }
                        vocabulary_dialog_error(ui, error.as_deref());
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    normalized.is_ok() && !*pending,
                                    egui::Button::new("Save search"),
                                )
                                .clicked()
                            {
                                mutation = Some(SavedSearchMutation::Create {
                                    name: normalized.clone().expect("validated name"),
                                    query: self.query.clone(),
                                });
                            }
                            cancel = ui
                                .add_enabled(!*pending, egui::Button::new("Cancel"))
                                .clicked();
                            if *pending {
                                ui.spinner();
                            }
                        });
                    });
                cancel |= response.should_close() && !*pending;
            }
            Some(SavedSearchDialog::Rename {
                search,
                name,
                pending,
                error,
            }) => {
                let normalized = normalize_name(NameKind::SavedSearch, name);
                let response =
                    egui::Modal::new(egui::Id::new("rename-saved-search")).show(context, |ui| {
                        ui.set_max_width(430.0);
                        ui.heading("Rename saved search");
                        ui.label(format!(
                            "Rename “{}” without changing its projection.",
                            search.name
                        ));
                        ui.add_space(8.0);
                        ui.add_enabled(
                            !*pending,
                            egui::TextEdit::singleline(name).desired_width(f32::INFINITY),
                        );
                        if let Err(validation) = &normalized {
                            ui.label(
                                RichText::new(validation.to_string()).color(Color32::LIGHT_RED),
                            );
                        }
                        vocabulary_dialog_error(ui, error.as_deref());
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    normalized.is_ok()
                                        && normalized
                                            .as_ref()
                                            .is_ok_and(|value| value != &search.name)
                                        && !*pending,
                                    egui::Button::new("Rename"),
                                )
                                .clicked()
                            {
                                mutation = Some(SavedSearchMutation::Rename {
                                    id: search.id,
                                    name: normalized.clone().expect("validated name"),
                                });
                            }
                            cancel = ui
                                .add_enabled(!*pending, egui::Button::new("Cancel"))
                                .clicked();
                            if *pending {
                                ui.spinner();
                            }
                        });
                    });
                cancel |= response.should_close() && !*pending;
            }
            Some(SavedSearchDialog::Delete {
                search,
                pending,
                error,
            }) => {
                let response =
                    egui::Modal::new(egui::Id::new("delete-saved-search")).show(context, |ui| {
                        ui.set_max_width(450.0);
                        ui.heading("Delete saved search?");
                        ui.label(format!("Delete “{}”?", search.name));
                        ui.label(
                            RichText::new(
                                "Books, vocabulary, publication files, and the current grid query will not change.",
                            )
                            .color(MUTED),
                        );
                        vocabulary_dialog_error(ui, error.as_deref());
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !*pending,
                                    egui::Button::new(
                                        RichText::new("Delete saved search")
                                            .color(Color32::LIGHT_RED),
                                    ),
                                )
                                .clicked()
                            {
                                mutation = Some(SavedSearchMutation::Delete { id: search.id });
                            }
                            cancel = ui
                                .add_enabled(!*pending, egui::Button::new("Cancel"))
                                .clicked();
                            if *pending {
                                ui.spinner();
                            }
                        });
                    });
                cancel |= response.should_close() && !*pending;
            }
            None => {}
        }
        if cancel {
            self.saved_searches.dialog = None;
        } else if let Some(mutation) = mutation {
            self.start_saved_search_mutation(mutation);
        }
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
        self.clear_grid_selection();
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

    fn bulk_tag_panel(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut lookup = None;
        let mut selected_suggestion = None;
        let mut create_and_add = false;
        let mut intent_updates = Vec::new();
        let mut remove_new = None;
        let mut page_offset = None;
        let mut apply = false;
        egui::Panel::right("bulk-tag-editor")
            .default_size(390.0)
            .size_range(340.0..=560.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(18, 16)),
            )
            .show(ui, |ui| {
                let bulk = &mut self.bulk_tags;
                ui.horizontal(|ui| {
                    ui.heading("Bulk tags");
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        close = ui
                            .add_enabled(!bulk.applying(), egui::Button::new("×"))
                            .on_hover_text("Close bulk tag panel")
                            .clicked();
                    });
                });
                ui.label(
                    RichText::new(format!("{} target books", bulk.selected_books)).color(MUTED),
                );
                ui.label(
                    RichText::new(
                        "Only tag relationships change. Book files and other metadata are untouched.",
                    )
                    .color(MUTED)
                    .size(12.0),
                );
                ui.separator();

                ui.label(RichText::new("Find or create a tag").strong());
                let search = ui.add_enabled(
                    !bulk.applying(),
                    egui::TextEdit::singleline(&mut bulk.tag_input)
                        .hint_text("Tag name")
                        .desired_width(f32::INFINITY),
                );
                if search.changed() {
                    let generation = bulk.suggestions.begin();
                    lookup = Some((generation, bulk.tag_input.clone()));
                }
                suggestion_status(ui, &bulk.suggestions);
                for (index, usage) in bulk.suggestions.results.iter().take(8).enumerate() {
                    if ui
                        .add_enabled(
                            !bulk.applying(),
                            egui::Button::new(format!(
                                "{} · {} books",
                                usage.tag.name, usage.books
                            )),
                        )
                        .clicked()
                    {
                        selected_suggestion = Some(index);
                    }
                }
                if let Ok(name) = normalize_name(NameKind::Tag, &bulk.tag_input) {
                    let exact_exists = bulk
                        .suggestions
                        .results
                        .iter()
                        .any(|usage| identity_key(&usage.tag.name) == identity_key(&name));
                    if !exact_exists {
                        create_and_add = ui
                            .add_enabled(
                                !bulk.applying(),
                                egui::Button::new(format!("Create and add {name}")),
                            )
                            .clicked();
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Tags on the target books").strong());
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if bulk.page_pending() {
                            ui.spinner();
                        }
                    });
                });
                if let Some(error) = &bulk.error {
                    ui.label(RichText::new(error).color(Color32::LIGHT_RED));
                }

                egui::ScrollArea::vertical()
                    .id_salt("bulk-tag-rows")
                    .max_height(380.0)
                    .show(ui, |ui| {
                        for usage in &bulk.page {
                            let id = usage.usage.tag.id;
                            let mut intent = bulk.intents.get(&id).copied();
                            bulk_tag_row(
                                ui,
                                &usage.usage.tag.name,
                                bulk_tag_observed_state(
                                    usage.selected_books,
                                    bulk.selected_books,
                                ),
                                &mut intent,
                                !bulk.applying(),
                            );
                            if intent != bulk.intents.get(&id).copied() {
                                intent_updates.push((id, intent));
                            }
                        }
                        let page_ids = bulk
                            .page
                            .iter()
                            .map(|usage| usage.usage.tag.id)
                            .collect::<HashSet<_>>();
                        let mut queued = bulk
                            .queued_tags
                            .values()
                            .filter(|usage| !page_ids.contains(&usage.tag.id))
                            .collect::<Vec<_>>();
                        queued.sort_unstable_by(|left, right| {
                            identity_key(&left.tag.name).cmp(&identity_key(&right.tag.name))
                        });
                        for usage in queued {
                            let id = usage.tag.id;
                            let mut intent = bulk.intents.get(&id).copied();
                            bulk_tag_row(
                                ui,
                                &usage.tag.name,
                                "Queued from search",
                                &mut intent,
                                !bulk.applying(),
                            );
                            if intent != bulk.intents.get(&id).copied() {
                                intent_updates.push((id, intent));
                            }
                        }
                        let mut new_tags = bulk.new_tags.values().collect::<Vec<_>>();
                        new_tags.sort_unstable_by(|left, right| {
                            identity_key(left).cmp(&identity_key(right))
                        });
                        for name in new_tags {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(name).strong());
                                    ui.label(
                                        RichText::new("New · Add to all").color(MUTED).size(11.0),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !bulk.applying(),
                                                egui::Button::new("Remove"),
                                            )
                                            .clicked()
                                        {
                                            remove_new = Some(identity_key(name));
                                        }
                                    },
                                );
                            });
                            ui.separator();
                        }
                    });

                ui.horizontal(|ui| {
                    let previous = bulk.page_offset.saturating_sub(100);
                    if ui
                        .add_enabled(
                            !bulk.page_pending() && !bulk.applying() && bulk.page_offset > 0,
                            egui::Button::new("Previous"),
                        )
                        .clicked()
                    {
                        page_offset = Some(previous);
                    }
                    ui.label(format!("Page {}", bulk.page_offset / 100 + 1));
                    if ui
                        .add_enabled(
                            !bulk.page_pending() && !bulk.applying() && bulk.has_next_page,
                            egui::Button::new("Next"),
                        )
                        .clicked()
                    {
                        page_offset = Some(bulk.page_offset + 100);
                    }
                });
                ui.separator();
                let has_changes = !bulk.intents.is_empty() || !bulk.new_tags.is_empty();
                apply = ui
                    .add_enabled(
                        has_changes && !bulk.applying(),
                        egui::Button::new(if bulk.applying() {
                            "Applying tag changes…"
                        } else {
                            "Apply tag changes"
                        }),
                    )
                    .clicked();
                if bulk.applying() {
                    ui.spinner();
                }
            });

        if close {
            self.reset_bulk_tags();
        }
        for (id, intent) in intent_updates {
            match intent {
                Some(intent) => {
                    self.bulk_tags.intents.insert(id, intent);
                }
                None => {
                    self.bulk_tags.intents.remove(&id);
                }
            }
        }
        if let Some(index) = selected_suggestion {
            if let Some(usage) = self.bulk_tags.suggestions.results.get(index).cloned() {
                self.queue_existing_bulk_tag(usage);
            }
        } else if create_and_add {
            self.queue_new_bulk_tag();
        }
        if let Some(key) = remove_new {
            self.bulk_tags.new_tags.remove(&key);
        }
        if let Some(offset) = page_offset {
            self.load_bulk_tag_page(offset);
        }
        if apply {
            self.apply_bulk_tag_changes();
        }
        self.dispatch_bulk_tag_lookup(lookup);
    }

    fn queue_existing_bulk_tag(&mut self, usage: TagUsage) {
        let id = usage.tag.id;
        self.bulk_tags.queued_tags.insert(id, usage);
        self.bulk_tags.intents.insert(id, BulkTagIntent::Add);
        self.bulk_tags.tag_input.clear();
        self.bulk_tags.suggestions.results.clear();
    }

    fn queue_new_bulk_tag(&mut self) {
        let Ok(name) = normalize_name(NameKind::Tag, &self.bulk_tags.tag_input) else {
            return;
        };
        let key = identity_key(&name);
        if let Some(existing) = self
            .bulk_tags
            .suggestions
            .results
            .iter()
            .find(|usage| identity_key(&usage.tag.name) == key)
            .cloned()
        {
            self.queue_existing_bulk_tag(existing);
            return;
        }
        self.bulk_tags.new_tags.insert(key, name);
        self.bulk_tags.tag_input.clear();
        self.bulk_tags.suggestions.results.clear();
    }

    fn dispatch_bulk_tag_lookup(&mut self, lookup: Option<(u64, String)>) {
        let Some((generation, prefix)) = lookup else {
            return;
        };
        let mut selected = self
            .bulk_tags
            .page
            .iter()
            .map(|usage| usage.usage.tag.id)
            .collect::<Vec<_>>();
        selected.extend(self.bulk_tags.queued_tags.keys().copied());
        selected.sort_unstable();
        selected.dedup();
        if !self
            .workers
            .autocomplete_bulk_tags(generation, prefix, selected)
        {
            self.bulk_tags.suggestions.pending = false;
            self.bulk_tags.suggestions.error =
                Some("Autocomplete worker is unavailable".to_owned());
        }
    }

    fn bulk_tag_discard_confirmation_window(&mut self, context: &egui::Context) {
        if !self.bulk_tags.discard_confirmation {
            return;
        }
        let mut save = false;
        let mut discard = false;
        let mut cancel = false;
        egui::Window::new("Unsaved book changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(context, |ui| {
                ui.label("Opening bulk tags closes Book details.");
                ui.label("Save the edits first, or explicitly discard them to continue.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    save = ui
                        .add_enabled(
                            self.editor.as_ref().is_some_and(BookEditor::can_save),
                            egui::Button::new("Save edits"),
                        )
                        .clicked();
                    discard = ui.button("Discard edits and continue").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if save {
            self.bulk_tags.discard_confirmation = false;
            self.save_editor();
        } else if discard {
            self.bulk_tags.discard_confirmation = false;
            self.clear_selection();
            self.open_bulk_tag_panel();
        } else if cancel {
            self.bulk_tags.discard_confirmation = false;
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
        let mut contributor_lookup = None;
        let mut series_lookup = None;
        let mut tag_lookup = None;
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
                contributor_lookup = actions.contributor_lookup;
                series_lookup = actions.series_lookup;
                tag_lookup = actions.tag_lookup;
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
        self.dispatch_contributor_lookup(contributor_lookup);
        self.dispatch_series_lookup(series_lookup);
        self.dispatch_tag_lookup(tag_lookup);
    }

    fn dispatch_contributor_lookup(&mut self, lookup: Option<ContributorLookup>) {
        let Some(lookup) = lookup else {
            return;
        };
        let selected = self.editor.as_ref().map_or_else(Vec::new, |editor| {
            editor.curation.existing_contributor_ids()
        });
        if !self.workers.autocomplete_contributors(
            lookup.generation,
            lookup.row_id,
            lookup.prefix,
            selected,
        ) && let Some(editor) = &mut self.editor
        {
            editor.contributor_suggestions.pending = false;
            editor.contributor_suggestions.error =
                Some("Metadata worker is unavailable".to_owned());
        }
    }

    fn dispatch_series_lookup(&mut self, lookup: Option<SeriesLookup>) {
        let Some(lookup) = lookup else {
            return;
        };
        let selected = self
            .editor
            .as_ref()
            .map_or_else(Vec::new, |editor| editor.curation.existing_series_id());
        if !self
            .workers
            .autocomplete_series(lookup.generation, lookup.prefix, selected)
            && let Some(editor) = &mut self.editor
        {
            editor.series_suggestions.pending = false;
            editor.series_suggestions.error = Some("Metadata worker is unavailable".to_owned());
        }
    }

    fn dispatch_tag_lookup(&mut self, lookup: Option<TagLookup>) {
        let Some(lookup) = lookup else {
            return;
        };
        let selected = self
            .editor
            .as_ref()
            .map_or_else(Vec::new, |editor| editor.curation.existing_tag_ids());
        if !self
            .workers
            .autocomplete_tags(lookup.generation, lookup.prefix, selected)
            && let Some(editor) = &mut self.editor
        {
            editor.tag_suggestions.pending = false;
            editor.tag_suggestions.error = Some("Metadata worker is unavailable".to_owned());
        }
    }

    fn dispatch_facet_contributor_lookup(&mut self, lookup: Option<FacetContributorLookup>) {
        let Some(lookup) = lookup else {
            return;
        };
        let selected = self
            .query
            .facets
            .contributors
            .iter()
            .map(|facet| facet.contributor)
            .collect();
        if !self
            .workers
            .autocomplete_facet_contributors(lookup.generation, lookup.prefix, selected)
        {
            self.filters.contributor_suggestions.pending = false;
            self.filters.contributor_suggestions.error =
                Some("Autocomplete worker is unavailable".to_owned());
        }
    }

    fn dispatch_facet_series_lookup(&mut self, lookup: Option<FacetSeriesLookup>) {
        let Some(lookup) = lookup else {
            return;
        };
        let selected = self.query.facets.series.into_iter().collect();
        if !self
            .workers
            .autocomplete_facet_series(lookup.generation, lookup.prefix, selected)
        {
            self.filters.series_suggestions.pending = false;
            self.filters.series_suggestions.error =
                Some("Autocomplete worker is unavailable".to_owned());
        }
    }

    fn dispatch_facet_tag_lookup(&mut self, lookup: Option<FacetTagLookup>) {
        let Some(lookup) = lookup else {
            return;
        };
        let mut selected = self.query.facets.included_tags.clone();
        selected.extend_from_slice(&self.query.facets.excluded_tags);
        selected.sort_unstable();
        selected.dedup();
        if !self
            .workers
            .autocomplete_facet_tags(lookup.generation, lookup.prefix, selected)
        {
            self.filters.tag_suggestions.pending = false;
            self.filters.tag_suggestions.error =
                Some("Autocomplete worker is unavailable".to_owned());
        }
    }

    fn open_organiser(&mut self) {
        self.organiser.open = true;
        if self.organiser.page.is_none() && !self.organiser.pending {
            self.request_vocabulary_page(0);
        }
    }

    fn request_vocabulary_page(&mut self, offset: u64) {
        self.organiser.generation = self.organiser.generation.wrapping_add(1);
        self.organiser.offset = offset;
        self.organiser.pending = true;
        self.organiser.error = None;
        if self.organiser.section == OrganiserSection::SavedSearches {
            if !self.workers.search_saved_searches(
                self.organiser.generation,
                self.organiser.applied_prefix.clone(),
                offset,
            ) {
                self.organiser.pending = false;
                self.organiser.error = Some("Metadata worker is unavailable".to_owned());
            }
            return;
        }
        let Some(kind) = self.organiser.section.vocabulary_kind() else {
            self.organiser.pending = false;
            return;
        };
        let request = VocabularyRequest {
            generation: self.organiser.generation,
            kind,
            prefix: self.organiser.applied_prefix.clone(),
            offset,
        };
        if !self.workers.load_vocabulary(request) {
            self.organiser.pending = false;
            self.organiser.error = Some("Vocabulary worker is busy; try again".to_owned());
        }
    }

    fn saved_search_page_loaded(
        &mut self,
        generation: u64,
        offset: u64,
        result: Result<Vec<SavedSearch>, String>,
    ) {
        if generation != self.organiser.generation
            || self.organiser.section != OrganiserSection::SavedSearches
            || offset != self.organiser.offset
        {
            return;
        }
        self.organiser.pending = false;
        match result {
            Ok(searches) => {
                self.organiser.page = Some(VocabularyPage::SavedSearches(searches));
                self.organiser.error = None;
            }
            Err(error) => {
                self.organiser.page = None;
                self.organiser.error = Some(error);
            }
        }
    }

    fn vocabulary_loaded(
        &mut self,
        generation: u64,
        kind: VocabularyKind,
        offset: u64,
        result: Result<VocabularyRows, String>,
    ) {
        if generation != self.organiser.generation
            || self.organiser.section.vocabulary_kind() != Some(kind)
            || offset != self.organiser.offset
        {
            return;
        }
        self.organiser.pending = false;
        match result {
            Ok(VocabularyRows::Contributors(rows)) if kind == VocabularyKind::Contributors => {
                self.organiser.page = Some(VocabularyPage::Contributors(rows));
                self.organiser.error = None;
            }
            Ok(VocabularyRows::Series(rows)) if kind == VocabularyKind::Series => {
                self.organiser.page = Some(VocabularyPage::Series(rows));
                self.organiser.error = None;
            }
            Ok(VocabularyRows::Tags(rows)) if kind == VocabularyKind::Tags => {
                self.organiser.page = Some(VocabularyPage::Tags(rows));
                self.organiser.error = None;
            }
            Ok(_) => {
                self.organiser.page = None;
                self.organiser.error = Some("Vocabulary worker returned the wrong row type".into());
            }
            Err(error) => {
                self.organiser.page = None;
                self.organiser.error = Some(error);
            }
        }
    }

    fn install_merge_suggestions(
        &mut self,
        generation: u64,
        result: Result<Vec<VocabularyEntity>, String>,
    ) {
        if let Some(VocabularyDialog::Merge(dialog)) = &mut self.organiser.dialog {
            dialog.suggestions.install(generation, result);
        }
    }

    fn vocabulary_impact_loaded(
        &mut self,
        entity: VocabularyEntityId,
        result: Result<VocabularyMutationResult, String>,
    ) {
        match &mut self.organiser.dialog {
            Some(VocabularyDialog::Merge(dialog)) if dialog.source.id() == entity => {
                dialog.impact_pending = false;
                match result {
                    Ok(impact) => {
                        dialog.impact = Some(impact);
                        dialog.error = None;
                    }
                    Err(error) => dialog.error = Some(error),
                }
            }
            Some(VocabularyDialog::Delete(dialog)) if dialog.entity.id() == entity => {
                dialog.impact_pending = false;
                match result {
                    Ok(impact) => {
                        dialog.impact = Some(impact);
                        dialog.error = None;
                    }
                    Err(error) => dialog.error = Some(error),
                }
            }
            _ => {}
        }
    }

    fn vocabulary_mutated(
        &mut self,
        mutation: &VocabularyMutation,
        result: Result<VocabularyMutationResult, String>,
    ) {
        match result {
            Ok(impact) => {
                self.cache_merge_target_label();
                self.rewrite_active_facets_after_mutation(mutation);
                self.status = format!(
                    "Updated library organisation · {} books · {} saved searches · no book files changed",
                    impact.books, impact.saved_searches
                );
                self.organiser.dialog = None;
                self.organiser.page = None;
                self.clear_selection();
                self.refresh_library();
                self.reload_saved_searches();
                self.request_vocabulary_page(self.organiser.offset);
            }
            Err(error) => match &mut self.organiser.dialog {
                Some(VocabularyDialog::Rename(dialog)) => {
                    dialog.pending = false;
                    dialog.error = Some(error);
                }
                Some(VocabularyDialog::Merge(dialog)) => {
                    dialog.pending = false;
                    dialog.error = Some(error);
                }
                Some(VocabularyDialog::Delete(dialog)) => {
                    dialog.pending = false;
                    dialog.error = Some(error);
                }
                None => self.status = format!("Could not update library organisation: {error}"),
            },
        }
    }

    fn cache_merge_target_label(&mut self) {
        let Some(VocabularyDialog::Merge(dialog)) = &self.organiser.dialog else {
            return;
        };
        let Some(target) = &dialog.target else {
            return;
        };
        match target {
            VocabularyEntity::Contributor(usage) => {
                self.filters
                    .contributor_labels
                    .insert(usage.contributor.id, usage.contributor.display_name.clone());
            }
            VocabularyEntity::Series(usage) => {
                self.filters
                    .series_labels
                    .insert(usage.series.id, usage.series.name.clone());
            }
            VocabularyEntity::Tag(usage) => {
                self.filters
                    .tag_labels
                    .insert(usage.tag.id, usage.tag.name.clone());
            }
        }
    }

    fn rewrite_active_facets_after_mutation(&mut self, mutation: &VocabularyMutation) {
        match mutation {
            VocabularyMutation::RenameContributor {
                id, display_name, ..
            } => {
                self.filters
                    .contributor_labels
                    .insert(*id, display_name.clone());
            }
            VocabularyMutation::MergeContributors { source, target } => {
                let mut author_only = false;
                let mut matched = false;
                self.query.facets.contributors.retain(|facet| {
                    if facet.contributor == *source || facet.contributor == *target {
                        matched = true;
                        author_only |= facet.author_only;
                        false
                    } else {
                        true
                    }
                });
                if matched {
                    self.query.facets.contributors.push(ContributorFacet {
                        contributor: *target,
                        author_only,
                    });
                    self.query.facets.contributors.sort_unstable();
                }
                self.filters.contributor_labels.remove(source);
            }
            VocabularyMutation::DeleteContributor(id) => {
                self.query
                    .facets
                    .contributors
                    .retain(|facet| facet.contributor != *id);
                self.filters.contributor_labels.remove(id);
            }
            VocabularyMutation::RenameSeries { id, name } => {
                self.filters.series_labels.insert(*id, name.clone());
            }
            VocabularyMutation::MergeSeries { source, target } => {
                if self.query.facets.series == Some(*source) {
                    self.query.facets.series = Some(*target);
                }
                self.filters.series_labels.remove(source);
            }
            VocabularyMutation::DeleteSeries(id) => {
                if self.query.facets.series == Some(*id) {
                    self.query.facets.series = None;
                }
                self.filters.series_labels.remove(id);
            }
            VocabularyMutation::RenameTag { id, name } => {
                self.filters.tag_labels.insert(*id, name.clone());
            }
            VocabularyMutation::MergeTags { source, target } => {
                rewrite_tag_facet(&mut self.query.facets.included_tags, *source, *target);
                rewrite_tag_facet(&mut self.query.facets.excluded_tags, *source, *target);
                if self.query.facets.included_tags.contains(target) {
                    self.query.facets.excluded_tags.retain(|id| id != target);
                }
                self.filters.tag_labels.remove(source);
            }
            VocabularyMutation::DeleteTag { id, .. } => {
                self.query.facets.included_tags.retain(|tag| tag != id);
                self.query.facets.excluded_tags.retain(|tag| tag != id);
                self.filters.tag_labels.remove(id);
            }
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
        let Ok(edit) = editor.edit() else {
            return;
        };
        if self.workers.save_book(edit) {
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

        self.selection_bar(ui);
        ui.add_space(10.0);

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
                                    self.book_card(ui, &book, index);
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

    fn selection_bar(&mut self, ui: &mut egui::Ui) {
        let active = self.grid_selection.is_active();
        let pending = self.selection_pending.is_some();
        let label = if pending {
            "Resolving selection…".to_owned()
        } else if self.grid_selection.is_every_matching() {
            format!(
                "All {} matching selected",
                self.grid_selection.selected_count()
            )
        } else if let Some(matching) = self.grid_selection.matching_count() {
            format!(
                "{} selected from {matching} matching",
                self.grid_selection.selected_count()
            )
        } else if active {
            format!("{} selected", self.grid_selection.selected_count())
        } else {
            "Select books for bulk actions".to_owned()
        };
        let mut select_all = false;
        let mut clear = false;
        let mut bulk_tags = false;
        egui::Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if pending {
                        ui.spinner();
                    }
                    ui.label(RichText::new(label).strong());
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if active || pending {
                            clear = ui.button("Clear selection").clicked();
                        }
                        if active {
                            bulk_tags = ui
                                .add_enabled(
                                    !pending && !self.bulk_tags.is_open(),
                                    egui::Button::new("Bulk tags"),
                                )
                                .clicked();
                        }
                        if !self.grid_selection.is_every_matching() {
                            select_all = ui
                                .add_enabled(!pending, egui::Button::new("Select all matching"))
                                .clicked();
                        }
                    });
                });
            });
        if clear {
            self.clear_grid_selection();
            "Selection cleared".clone_into(&mut self.status);
        } else if select_all {
            self.select_all_matching();
        } else if bulk_tags {
            self.request_bulk_tag_panel();
        }
    }

    fn book_card(&mut self, ui: &mut egui::Ui, book: &BookSummary, index: usize) {
        let grid_selected = self.grid_selection.contains(book.id);
        let selected = grid_selected || self.selected == Some(book.id);
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
        let mut checkbox_clicked = false;
        let checkbox_response = self.grid_selection.is_active().then(|| {
            let mut checked = grid_selected;
            let checkbox_rect = egui::Rect::from_min_size(
                response.rect.left_top() + egui::vec2(8.0, 8.0),
                egui::vec2(24.0, 24.0),
            );
            ui.push_id(("grid-selection", book.id.value()), |ui| {
                ui.put(checkbox_rect, egui::Checkbox::without_text(&mut checked))
            })
            .inner
        });
        if let Some(checkbox) = &checkbox_response
            && checkbox.clicked()
        {
            checkbox.request_focus();
            self.grid_focus_id = Some(checkbox.id);
            checkbox_clicked = true;
            self.toggle_grid_book(book.id, index);
        }
        if response.clicked() && !checkbox_clicked {
            response.request_focus();
            self.grid_focus_id = Some(response.id);
            let modifiers = ui.input(|input| input.modifiers);
            if modifiers.shift {
                self.select_range_to(book.id, index);
            } else if modifiers.command || self.grid_selection.is_active() {
                self.toggle_grid_book(book.id, index);
            } else {
                self.select_book(book.id);
            }
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
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if self.benchmark.is_some() {
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            context.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
            context.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.wrapping_add(1);
        if let Some(benchmark) = &mut self.benchmark {
            let unstable_dt = ui.input(|input| input.unstable_dt);
            benchmark.frame_started(frame.info().cpu_usage, unstable_dt);
        }
        self.poll_workers(ui.ctx());
        self.selection_shortcuts(ui.ctx());
        self.apply_benchmark_sort_request();
        self.apply_benchmark_asset_action_request();
        self.apply_benchmark_editor_request();
        self.apply_benchmark_selection_request();
        self.apply_benchmark_bulk_tag_request();
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
        if self.bulk_tags.is_open() {
            self.bulk_tag_panel(ui);
        } else if self.selected.is_some() {
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
        self.bulk_tag_discard_confirmation_window(ui.ctx());
        self.organiser_window(ui.ctx());
        self.vocabulary_dialog_window(ui.ctx());
        self.saved_search_dialog_window(ui.ctx());

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
                    selection_pending: self.selection_pending.is_some(),
                    selected_books: self.grid_selection.selected_count(),
                    all_matching_selected: self.grid_selection.is_every_matching(),
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

fn bulk_tag_observed_state(selected_books: u64, target_books: u64) -> &'static str {
    if selected_books == 0 {
        "None"
    } else if selected_books == target_books {
        "All"
    } else {
        "Some"
    }
}

fn build_bulk_tag_edit(
    intents: &HashMap<TagId, BulkTagIntent>,
    new_tags: &HashMap<String, String>,
) -> BulkTagEdit {
    let mut intents = intents
        .iter()
        .map(|(id, intent)| (*id, *intent))
        .collect::<Vec<_>>();
    intents.sort_unstable_by_key(|(id, _)| *id);
    let mut add = intents
        .iter()
        .filter_map(|(id, intent)| {
            (*intent == BulkTagIntent::Add).then_some(TagReference::Existing(*id))
        })
        .collect::<Vec<_>>();
    let remove = intents
        .into_iter()
        .filter_map(|(id, intent)| (intent == BulkTagIntent::Remove).then_some(id))
        .collect::<Vec<_>>();
    let mut new_tags = new_tags.iter().collect::<Vec<_>>();
    new_tags.sort_unstable_by(|left, right| left.0.cmp(right.0));
    add.extend(
        new_tags
            .into_iter()
            .map(|(_, name)| TagReference::New(name.clone())),
    );
    BulkTagEdit { add, remove }
}

fn bulk_tag_row(
    ui: &mut egui::Ui,
    name: &str,
    observed: &str,
    intent: &mut Option<BulkTagIntent>,
    enabled: bool,
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).strong());
            ui.label(RichText::new(observed).color(MUTED).size(11.0));
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(enabled, |ui| {
                ui.selectable_value(intent, None, "Unchanged");
                ui.selectable_value(intent, Some(BulkTagIntent::Add), "Add to all");
                ui.selectable_value(intent, Some(BulkTagIntent::Remove), "Remove from all");
            });
        });
    });
    ui.separator();
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
            contributor_editor(ui, editor, editing_enabled, &mut actions);
            series_editor(ui, editor, editing_enabled, &mut actions);
            tag_editor(ui, editor, editing_enabled, &mut actions);
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
            if editor.changed()
                && let Err(error) = editor.edit()
            {
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

fn contributor_editor(
    ui: &mut egui::Ui,
    editor: &mut BookEditor,
    editing_enabled: bool,
    actions: &mut MetadataActions,
) {
    ui.add_space(4.0);
    ui.label(RichText::new("Contributors").strong());
    if editor.curation.contributors.is_empty() {
        ui.label(RichText::new("No contributors assigned.").color(MUTED));
    }

    let mut remove = None;
    let mut move_row = None;
    for index in 0..editor.curation.contributors.len() {
        let row_id = editor.curation.contributors[index].row_id;
        let mut selected_suggestion = None;
        let mut create_new = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                let previous_name = editor.curation.contributors[index].name.clone();
                let response = ui.add_enabled(
                    editing_enabled,
                    egui::TextEdit::singleline(&mut editor.curation.contributors[index].name)
                        .desired_width(180.0)
                        .hint_text("Contributor name"),
                );
                if response.changed() {
                    editor.curation.contributors[index].name_edited(&previous_name);
                    editor.contributor_suggestion_row = Some(row_id);
                    let generation = editor.contributor_suggestions.begin();
                    actions.contributor_lookup = Some(ContributorLookup {
                        generation,
                        row_id,
                        prefix: editor.curation.contributors[index].name.clone(),
                    });
                }
                egui::ComboBox::from_id_salt(("credit-role", row_id))
                    .selected_text(editor.curation.contributors[index].role.to_string())
                    .show_ui(ui, |ui| {
                        for role in ContributorRole::ALL {
                            ui.selectable_value(
                                &mut editor.curation.contributors[index].role,
                                role,
                                role.to_string(),
                            );
                        }
                    });
            });

            ui.horizontal(|ui| {
                let existing = editor.curation.contributors[index].existing_id.is_some();
                ui.label(RichText::new("Sort name").color(MUTED).size(11.0));
                ui.add_enabled(
                    editing_enabled && !existing,
                    egui::TextEdit::singleline(&mut editor.curation.contributors[index].sort_name)
                        .desired_width(180.0),
                )
                .on_disabled_hover_text(
                    "Existing sort names are library-wide; change them in Organise library",
                );
                if ui
                    .add_enabled(editing_enabled && index > 0, egui::Button::new("↑"))
                    .on_hover_text("Move credit earlier")
                    .clicked()
                {
                    move_row = Some((index, index - 1));
                }
                if ui
                    .add_enabled(
                        editing_enabled && index + 1 < editor.curation.contributors.len(),
                        egui::Button::new("↓"),
                    )
                    .on_hover_text("Move credit later")
                    .clicked()
                {
                    move_row = Some((index, index + 1));
                }
                if ui
                    .add_enabled(editing_enabled, egui::Button::new("Remove"))
                    .clicked()
                {
                    remove = Some(index);
                }
            });

            if editor.contributor_suggestion_row == Some(row_id) {
                suggestion_status(ui, &editor.contributor_suggestions);
                for (suggestion_index, usage) in editor
                    .contributor_suggestions
                    .results
                    .iter()
                    .take(8)
                    .enumerate()
                {
                    let label = format!(
                        "{} · {} · {} books",
                        usage.contributor.display_name, usage.contributor.sort_name, usage.books
                    );
                    if ui
                        .add_enabled(editing_enabled, egui::Button::new(label))
                        .clicked()
                    {
                        selected_suggestion = Some(suggestion_index);
                    }
                }
                let name = editor.curation.contributors[index].name.trim();
                let exact_exists = !name.is_empty()
                    && editor.contributor_suggestions.results.iter().any(|usage| {
                        identity_key(&usage.contributor.display_name) == identity_key(name)
                    });
                if !name.is_empty()
                    && !exact_exists
                    && !editor.contributor_suggestions.pending
                    && ui
                        .add_enabled(
                            editing_enabled,
                            egui::Button::new(format!("Create contributor ‘{name}’")),
                        )
                        .clicked()
                {
                    create_new = true;
                }
            }
        });

        if let Some(suggestion_index) = selected_suggestion {
            let usage = &editor.contributor_suggestions.results[suggestion_index];
            editor.curation.contributors[index].select_existing(
                usage.contributor.id,
                &usage.contributor.display_name,
                &usage.contributor.sort_name,
            );
            editor.contributor_suggestion_row = None;
            editor.contributor_suggestions.results.clear();
        } else if create_new {
            if let Err(error) = editor.curation.contributors[index].confirm_new() {
                editor.error = Some(error);
            } else {
                editor.contributor_suggestion_row = None;
                editor.contributor_suggestions.results.clear();
            }
        }
        ui.add_space(4.0);
    }
    if let Some(index) = remove {
        editor.curation.contributors.remove(index);
        editor.contributor_suggestion_row = None;
        editor.contributor_suggestions.results.clear();
    } else if let Some((from, to)) = move_row {
        editor.curation.contributors.swap(from, to);
    }
    if ui
        .add_enabled(editing_enabled, egui::Button::new("Add contributor"))
        .clicked()
    {
        let row_id = editor.curation.add_contributor();
        editor.contributor_suggestion_row = Some(row_id);
    }
}

fn series_editor(
    ui: &mut egui::Ui,
    editor: &mut BookEditor,
    editing_enabled: bool,
    actions: &mut MetadataActions,
) {
    ui.add_space(4.0);
    ui.label(RichText::new("Series").strong());
    let mut selected_suggestion = None;
    let mut create_new = false;
    ui.horizontal(|ui| {
        let previous = editor.curation.series.clone();
        let response = ui.add_enabled(
            editing_enabled,
            egui::TextEdit::singleline(&mut editor.curation.series.name)
                .desired_width(205.0)
                .hint_text("Series name"),
        );
        if response.changed() {
            editor.curation.series.name_edited();
            if editor.curation.series.name.trim().is_empty()
                && !editor.curation.series.index.trim().is_empty()
            {
                editor.series_clear_restore = Some(previous);
            } else if editor.curation.series.name.trim().is_empty() {
                editor.curation.series.clear();
                editor.series_suggestions.results.clear();
            } else {
                let generation = editor.series_suggestions.begin();
                actions.series_lookup = Some(SeriesLookup {
                    generation,
                    prefix: editor.curation.series.name.clone(),
                });
            }
        }
        if ui
            .add_enabled(
                editing_enabled && !editor.curation.series.name.trim().is_empty(),
                egui::Button::new("Clear"),
            )
            .clicked()
        {
            if editor.curation.series.index.trim().is_empty() {
                editor.curation.series.clear();
            } else {
                editor.series_clear_restore = Some(editor.curation.series.clone());
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Book number").color(MUTED).size(11.0));
        ui.add_enabled(
            editing_enabled && !editor.curation.series.name.trim().is_empty(),
            egui::TextEdit::singleline(&mut editor.curation.series.index).desired_width(110.0),
        );
    });
    if !editor.curation.series.index.trim().is_empty()
        && let Err(error) = editor.curation.series.index.trim().parse::<SeriesIndex>()
    {
        ui.label(RichText::new(error.to_string()).color(Color32::LIGHT_RED));
    }

    if editor.series_clear_restore.is_some() {
        ui.label(
            RichText::new("Clear this series and its book number?").color(Color32::LIGHT_YELLOW),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(editing_enabled, egui::Button::new("Clear both"))
                .clicked()
            {
                editor.curation.series.clear();
                editor.series_clear_restore = None;
                editor.series_suggestions.results.clear();
            }
            if ui
                .add_enabled(editing_enabled, egui::Button::new("Keep series"))
                .clicked()
                && let Some(previous) = editor.series_clear_restore.take()
            {
                editor.curation.series = previous;
            }
        });
        return;
    }

    suggestion_status(ui, &editor.series_suggestions);
    for (suggestion_index, usage) in editor.series_suggestions.results.iter().take(8).enumerate() {
        if ui
            .add_enabled(
                editing_enabled,
                egui::Button::new(format!("{} · {} books", usage.series.name, usage.books)),
            )
            .clicked()
        {
            selected_suggestion = Some(suggestion_index);
        }
    }
    let name = editor.curation.series.name.trim();
    let exact_exists = !name.is_empty()
        && editor
            .series_suggestions
            .results
            .iter()
            .any(|usage| identity_key(&usage.series.name) == identity_key(name));
    if !name.is_empty()
        && editor.curation.series.existing_id.is_none()
        && !editor.curation.series.confirmed_new
        && !exact_exists
        && !editor.series_suggestions.pending
        && ui
            .add_enabled(
                editing_enabled,
                egui::Button::new(format!("Create series ‘{name}’")),
            )
            .clicked()
    {
        create_new = true;
    }
    if let Some(suggestion_index) = selected_suggestion {
        let usage = &editor.series_suggestions.results[suggestion_index];
        editor
            .curation
            .series
            .select_existing(usage.series.id, &usage.series.name);
        editor.series_suggestions.results.clear();
    } else if create_new {
        if let Err(error) = editor.curation.series.confirm_new() {
            editor.error = Some(error);
        } else {
            editor.series_suggestions.results.clear();
        }
    }
}

fn tag_editor(
    ui: &mut egui::Ui,
    editor: &mut BookEditor,
    editing_enabled: bool,
    actions: &mut MetadataActions,
) {
    ui.add_space(4.0);
    ui.label(RichText::new("Tags").strong());
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for (index, tag) in editor.curation.tags.iter().enumerate() {
            if ui
                .add_enabled(
                    editing_enabled,
                    egui::Button::new(format!("{} ×", tag.name)),
                )
                .on_hover_text("Remove this tag from the book")
                .clicked()
            {
                remove = Some(index);
            }
        }
    });
    if let Some(index) = remove {
        editor.curation.tags.remove(index);
    }

    let response = ui.add_enabled(
        editing_enabled,
        egui::TextEdit::singleline(&mut editor.tag_input)
            .desired_width(f32::INFINITY)
            .hint_text("Search or create a tag"),
    );
    if response.changed() {
        if editor.tag_input.trim().is_empty() {
            editor.tag_suggestions.results.clear();
            editor.tag_suggestions.error = None;
        } else {
            let generation = editor.tag_suggestions.begin();
            actions.tag_lookup = Some(TagLookup {
                generation,
                prefix: editor.tag_input.clone(),
            });
        }
    }

    let enter = response.has_focus()
        && !editor.tag_suggestions.pending
        && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let exact_suggestion =
        editor.tag_suggestions.results.iter().position(|usage| {
            identity_key(&usage.tag.name) == identity_key(editor.tag_input.trim())
        });
    let mut selected_suggestion = None;
    suggestion_status(ui, &editor.tag_suggestions);
    for (suggestion_index, usage) in editor.tag_suggestions.results.iter().take(8).enumerate() {
        if ui
            .add_enabled(
                editing_enabled,
                egui::Button::new(format!("{} · {} books", usage.tag.name, usage.books)),
            )
            .clicked()
        {
            selected_suggestion = Some(suggestion_index);
        }
    }

    let mut create_new = false;
    if !editor.tag_input.trim().is_empty()
        && exact_suggestion.is_none()
        && !editor.tag_suggestions.pending
    {
        create_new = ui
            .add_enabled(
                editing_enabled,
                egui::Button::new(format!("Create and add ‘{}’", editor.tag_input.trim())),
            )
            .clicked();
    }
    selected_suggestion =
        selected_suggestion.or_else(|| enter.then_some(exact_suggestion).flatten());
    create_new |= enter && exact_suggestion.is_none();

    if let Some(suggestion_index) = selected_suggestion {
        let usage = &editor.tag_suggestions.results[suggestion_index];
        editor
            .curation
            .add_existing_tag(usage.tag.id, &usage.tag.name);
        editor.tag_input.clear();
        editor.tag_suggestions.results.clear();
    } else if create_new {
        match editor.curation.add_new_tag(&editor.tag_input) {
            Ok(_) => {
                editor.tag_input.clear();
                editor.tag_suggestions.results.clear();
            }
            Err(error) => editor.error = Some(error),
        }
    }
}

fn suggestion_status<T>(ui: &mut egui::Ui, state: &SuggestionState<T>) {
    if state.pending {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Searching library…").color(MUTED).size(11.0));
        });
    } else if let Some(error) = &state.error {
        ui.label(RichText::new(error).color(Color32::LIGHT_RED).size(11.0));
    }
}

fn vocabulary_entity_at(page: &VocabularyPage, index: usize) -> Option<VocabularyEntity> {
    match page {
        VocabularyPage::Contributors(rows) => {
            rows.get(index).cloned().map(VocabularyEntity::Contributor)
        }
        VocabularyPage::Series(rows) => rows.get(index).cloned().map(VocabularyEntity::Series),
        VocabularyPage::Tags(rows) => rows.get(index).cloned().map(VocabularyEntity::Tag),
        VocabularyPage::SavedSearches(_) => None,
    }
}

fn saved_search_summary(search: &SavedSearch) -> String {
    let exact_filters = search.query.facets.contributors.len()
        + search.query.facets.included_tags.len()
        + search.query.facets.excluded_tags.len()
        + usize::from(search.query.facets.series.is_some());
    let expression = if search.query.search.is_empty() {
        "All books"
    } else {
        &search.query.search
    };
    format!(
        "{expression} · {exact_filters} exact filters · {} · {} · {}",
        search
            .query
            .format
            .map_or_else(|| "all formats".to_owned(), |format| format.to_string()),
        search
            .query
            .asset_health
            .map_or_else(|| "all file states".to_owned(), |health| health.to_string()),
        search.query.sort,
    )
}

fn saved_search_is_modified(search: Option<&SavedSearch>, query: &LibraryQuery) -> bool {
    search.is_some_and(|search| &search.query != query)
}

fn set_saved_search_dialog_pending(dialog: &mut Option<SavedSearchDialog>, pending: bool) {
    match dialog {
        Some(
            SavedSearchDialog::Create { pending: value, .. }
            | SavedSearchDialog::Rename { pending: value, .. }
            | SavedSearchDialog::Delete { pending: value, .. },
        ) => *value = pending,
        None => {}
    }
}

fn set_saved_search_dialog_error(dialog: &mut Option<SavedSearchDialog>, error: String) {
    match dialog {
        Some(
            SavedSearchDialog::Create { error: value, .. }
            | SavedSearchDialog::Rename { error: value, .. }
            | SavedSearchDialog::Delete { error: value, .. },
        ) => *value = Some(error),
        None => {}
    }
}

fn vocabulary_usage_label(entity: &VocabularyEntity) -> String {
    match entity.saved_searches() {
        Some(saved_searches) => {
            format!("{} books · {saved_searches} saved searches", entity.books())
        }
        None => format!("{} books", entity.books()),
    }
}

fn vocabulary_dialog_error(ui: &mut egui::Ui, error: Option<&str>) {
    if let Some(error) = error {
        ui.label(RichText::new(error).color(Color32::LIGHT_RED));
    }
}

fn vocabulary_confirmation_copy(
    ui: &mut egui::Ui,
    operation: &str,
    impact: VocabularyMutationResult,
) {
    ui.label(format!(
        "{operation} affects {} books and {} saved searches.",
        impact.books, impact.saved_searches
    ));
    ui.label(RichText::new("No book files or publication bytes will change.").strong());
}

fn rename_vocabulary_mutation(dialog: &RenameVocabularyDialog) -> Option<VocabularyMutation> {
    if dialog.name.trim().is_empty() {
        return None;
    }
    Some(match &dialog.entity {
        VocabularyEntity::Contributor(usage) => VocabularyMutation::RenameContributor {
            id: usage.contributor.id,
            display_name: dialog.name.clone(),
            sort_name: dialog.sort_name.clone(),
        },
        VocabularyEntity::Series(usage) => VocabularyMutation::RenameSeries {
            id: usage.series.id,
            name: dialog.name.clone(),
        },
        VocabularyEntity::Tag(usage) => VocabularyMutation::RenameTag {
            id: usage.tag.id,
            name: dialog.name.clone(),
        },
    })
}

fn merge_vocabulary_mutation(dialog: &MergeVocabularyDialog) -> Option<VocabularyMutation> {
    dialog.impact?;
    let target = dialog.target.as_ref()?;
    match (&dialog.source, target) {
        (VocabularyEntity::Contributor(source), VocabularyEntity::Contributor(target)) => {
            Some(VocabularyMutation::MergeContributors {
                source: source.contributor.id,
                target: target.contributor.id,
            })
        }
        (VocabularyEntity::Series(source), VocabularyEntity::Series(target)) => {
            Some(VocabularyMutation::MergeSeries {
                source: source.series.id,
                target: target.series.id,
            })
        }
        (VocabularyEntity::Tag(source), VocabularyEntity::Tag(target)) => {
            Some(VocabularyMutation::MergeTags {
                source: source.tag.id,
                target: target.tag.id,
            })
        }
        _ => None,
    }
}

fn delete_vocabulary_mutation(dialog: &DeleteVocabularyDialog) -> Option<VocabularyMutation> {
    let impact = dialog.impact?;
    Some(match &dialog.entity {
        VocabularyEntity::Contributor(usage) => {
            VocabularyMutation::DeleteContributor(usage.contributor.id)
        }
        VocabularyEntity::Series(usage) => VocabularyMutation::DeleteSeries(usage.series.id),
        VocabularyEntity::Tag(usage) => VocabularyMutation::DeleteTag {
            id: usage.tag.id,
            confirmed: impact,
        },
    })
}

fn rewrite_tag_facet(tags: &mut Vec<TagId>, source: TagId, target: TagId) {
    for tag in tags.iter_mut() {
        if *tag == source {
            *tag = target;
        }
    }
    tags.sort_unstable();
    tags.dedup();
}

fn apply_search_input(query: &mut LibraryQuery, input: &str) -> Result<bool, SearchParseError> {
    let canonical = SearchExpression::parse(input)?.canonical();
    let changed = query.search != canonical;
    query.search = canonical;
    Ok(changed)
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use eframe::egui;
    use lectern_core::ImportProgress;
    use lectern_core::organisation::{
        BulkTagEdit, LibraryGeneration, SavedSearch, SavedSearchId, SelectionSnapshot, TagId,
        TagReference,
    };
    use lectern_core::{
        AssetHealth, AssetHealthReport, AssetId, AssetStorage, Book, BookAsset, BookFormat, BookId,
        LibraryQuery,
    };
    use lectern_desktop::export::ExportProgress;

    use super::{
        BookEditor, BulkTagIntent, CARD_GAP, CARD_WIDTH, COVER_SIZE, GridSelection,
        QUERY_PAGE_SIZE, apply_search_input, asset_health_status, build_bulk_tag_edit,
        bulk_tag_observed_state, column_count, cover_image, export_fraction,
        format_export_progress, import_status, query_page_offset, removal_file_message,
        saved_search_is_modified, saved_search_summary,
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
    fn saved_searches_track_complete_projection_modifications() {
        let saved = SavedSearch {
            id: SavedSearchId::new(7),
            name: "Recently added".into(),
            query: LibraryQuery::default(),
        };
        assert!(!saved_search_is_modified(
            Some(&saved),
            &LibraryQuery::default()
        ));
        let changed = LibraryQuery {
            search: "language:fr".into(),
            ..LibraryQuery::default()
        };
        assert!(saved_search_is_modified(Some(&saved), &changed));
        assert!(!saved_search_is_modified(None, &changed));
        assert!(saved_search_summary(&saved).contains("0 exact filters"));
    }

    #[test]
    fn explicit_grid_selection_toggles_stable_ids_and_tracks_the_anchor() {
        let mut selection = GridSelection::default();
        selection.toggle(BookId::new(7), 130);
        selection.toggle(BookId::new(9), 131);

        assert_eq!(selection.selected_count(), 2);
        assert!(selection.contains(BookId::new(7)));
        assert_eq!(selection.anchor.expect("selection anchor").index, 131);

        selection.toggle(BookId::new(7), 130);
        assert_eq!(selection.selected_count(), 1);
        assert!(!selection.contains(BookId::new(7)));
    }

    #[test]
    fn range_selection_installs_only_resolved_ids() {
        let mut selection = GridSelection::default();
        selection.toggle(BookId::new(1), 127);
        selection.install_range(vec![BookId::new(1), BookId::new(2), BookId::new(3)]);

        assert_eq!(selection.selected_count(), 3);
        assert!(selection.contains(BookId::new(2)));
        assert_eq!(selection.anchor.expect("selection anchor").index, 127);
    }

    #[test]
    fn all_matching_selection_uses_exclusions_without_materializing_matches() {
        let mut selection = GridSelection::default();
        selection.install_all_matching(
            LibraryQuery::default(),
            SelectionSnapshot {
                matching_books: 10_000,
                generation: LibraryGeneration {
                    connection_changes: 4,
                    data_version: 9,
                },
            },
        );

        assert!(selection.is_every_matching());
        assert_eq!(selection.selected_count(), 10_000);
        selection.toggle(BookId::new(17), 17);
        assert_eq!(selection.selected_count(), 9_999);
        assert!(!selection.contains(BookId::new(17)));
        assert!(!selection.is_every_matching());
    }

    #[test]
    fn bulk_tag_states_and_edit_are_exact_and_deterministic() {
        assert_eq!(bulk_tag_observed_state(0, 10), "None");
        assert_eq!(bulk_tag_observed_state(4, 10), "Some");
        assert_eq!(bulk_tag_observed_state(10, 10), "All");

        let intents = HashMap::from([
            (TagId::new(9), BulkTagIntent::Remove),
            (TagId::new(3), BulkTagIntent::Add),
            (TagId::new(7), BulkTagIntent::Add),
        ]);
        let new_tags = HashMap::from([
            ("zulu".to_owned(), "Zulu".to_owned()),
            ("alpha".to_owned(), "Alpha".to_owned()),
        ]);

        assert_eq!(
            build_bulk_tag_edit(&intents, &new_tags),
            BulkTagEdit {
                add: vec![
                    TagReference::Existing(TagId::new(3)),
                    TagReference::Existing(TagId::new(7)),
                    TagReference::New("Alpha".to_owned()),
                    TagReference::New("Zulu".to_owned()),
                ],
                remove: vec![TagId::new(9)],
            }
        );
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
        editor.curation.series.name = " Dune Chronicles ".into();
        editor.curation.series.confirm_new().unwrap();
        editor.curation.series.index = "2.000000".into();

        let saved = editor.edit().unwrap();

        assert_eq!(saved.title, "Dune Messiah");
        assert_eq!(saved.series.unwrap().index.unwrap().to_string(), "2");
        assert!(editor.changed());
    }

    #[test]
    fn invalid_structured_search_preserves_the_last_valid_projection() {
        let mut query = LibraryQuery::default();
        assert!(apply_search_input(&mut query, "author:le tag:\"science fiction\"").unwrap());
        let valid = query.search.clone();

        let error = apply_search_input(&mut query, "author:le OR tag:fantasy").unwrap_err();

        assert!(error.to_string().contains("unsupported"));
        assert_eq!(query.search, valid);
    }
}
