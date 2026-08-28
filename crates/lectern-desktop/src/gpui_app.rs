//! Lectern's native GPUI desktop application.

use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Entity, Image, ImageFormat, KeyBinding, ListAlignment, ListState,
    ObjectFit, Render, SharedString, StatefulInteractiveElement, StyledImage, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, img, list, prelude::*, px, relative, rems, size,
};
use gpui_base::{
    AlertDialog, AlertDialogBackdrop, AlertDialogDescription, AlertDialogPopup, AlertDialogTitle,
    Checkbox, CheckboxState,
    input::{InputEvent, InputState, TextareaState},
};
use gpui_platform::application;
use lectern_core::{
    AssetHealth, AssetId, AssetStorage, Book, BookAsset, BookFormat, BookId, BookRating,
    BookSummary, ImportSummary, LibraryQuery, LibraryService,
    organisation::{
        BookSelection, BulkRemovalResult, Contributor, ContributorCredit, ContributorId,
        ContributorRole, LibraryGeneration, NameKind, SelectionSnapshot, Series, SeriesId,
        SeriesIndex, SeriesMembership, SeriesUsage, Tag, TagColor, TagId, TagUsage, identity_key,
        normalize_name,
    },
};
use lectern_service::{LibraryServiceError, SqliteLibraryService, default_database_path};
use lectern_ui::{
    AccentColor, ActionListItem, ActionMenu, Button, ButtonSize, ButtonVariant, ColorMode,
    ColorSwatch, EntityChip, IconButton, LecternAssets, PrimerTheme, StarRating, TablerIcon,
    TagChip, TextArea, TextInput, install_fonts, install_theme,
};
use serde::{Deserialize, Serialize};

use crate::curation::BookCurationDraft;

const BENCHMARK_OUTPUT_ENV: &str = "LECTERN_GPUI_BENCHMARK_OUTPUT";
const BENCHMARK_WORKLOAD_ENV: &str = "LECTERN_GPUI_BENCHMARK_WORKLOAD";
const ROOT_REM_PX: f32 = 16.0;
const WINDOW_WIDTH_PX: f32 = 900.0;
const WINDOW_HEIGHT_PX: f32 = 620.0;
const EMPTY_LIBRARY_CONTENT_WIDTH_PX: f32 = 480.0;
const BOOK_CARD_WIDTH_PX: f32 = 160.0;
const BOOK_COVER_HEIGHT_PX: f32 = 216.0;
const TOP_BAR_HEIGHT_PX: f32 = 48.0;
const SELECTION_BAR_HEIGHT_PX: f32 = 48.0;
const BOTTOM_BAR_HEIGHT_PX: f32 = 24.0;
const BULK_REMOVAL_DIALOG_WIDTH_PX: f32 = 460.0;
const BOOK_DETAIL_PANEL_WIDTH_PX: f32 = 420.0;
const THEME_DIALOG_WIDTH_PX: f32 = 400.0;
const LIBRARY_PAGE_SIZE: u32 = 128;

gpui::actions!(
    lectern_library,
    [
        /// Select every book in the current library projection.
        SelectAllBooks,
        /// Leave multi-book selection mode.
        ClearBookSelection
    ]
);

/// Runs Lectern's native GPUI desktop application.
///
/// # Panics
///
/// Panics if GPUI cannot open the application window or benchmark output cannot be serialized and
/// written.
pub fn run(main_entry: Instant) {
    let benchmark = env::var_os(BENCHMARK_OUTPUT_ENV).map(|path| BenchmarkRun {
        output: PathBuf::from(path),
        workload: match env::var(BENCHMARK_WORKLOAD_ENV).as_deref() {
            Ok("library-selection") => BenchmarkWorkload::LibrarySelection,
            Ok("book-detail") => BenchmarkWorkload::BookDetail,
            Ok(workload) => panic!("unsupported GPUI benchmark workload {workload:?}"),
            Err(env::VarError::NotPresent) => BenchmarkWorkload::EmptyLibraryAddBooks,
            Err(error) => panic!("read GPUI benchmark workload: {error}"),
        },
        main_entry,
        initial_render: None,
        action_started: None,
        selection_painted: None,
        detail_painted: None,
        confirmation_started: None,
    });

    application()
        .with_assets(LecternAssets)
        .run(move |cx: &mut App| {
            gpui_base::init(cx);
            install_fonts(cx).expect("install bundled Lectern fonts");
            cx.bind_keys([
                KeyBinding::new("cmd-a", SelectAllBooks, Some("LecternLibrary")),
                KeyBinding::new("ctrl-a", SelectAllBooks, Some("LecternLibrary")),
                KeyBinding::new("escape", ClearBookSelection, Some("LecternLibrary")),
            ]);
            let database_path = default_database_path();
            let appearance = load_appearance(&database_path);
            install_theme(
                cx,
                PrimerTheme::with_accent(appearance.mode, appearance.accent),
            );
            let bounds =
                Bounds::centered(None, size(px(WINDOW_WIDTH_PX), px(WINDOW_HEIGHT_PX)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Lectern".into()),
                        ..Default::default()
                    }),
                    window_decorations: Some(WindowDecorations::Server),
                    ..Default::default()
                },
                move |window, cx| {
                    window.set_rem_size(px(ROOT_REM_PX));
                    cx.new(|_| LecternView::new(benchmark, database_path, appearance))
                },
            )
            .expect("open Lectern GPUI window");
            cx.activate(true);
        });
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent asynchronous UI operations require independent flags"
)]
struct LecternView {
    database_path: PathBuf,
    library_state: LibraryState,
    library_total: u64,
    books: Vec<LibraryBook>,
    query: LibraryQuery,
    selection: GridSelection,
    selection_generation: u64,
    selection_pending: Option<PendingSelection>,
    detail_editor: Option<BookDetailEditor>,
    detail_loading: Option<BookId>,
    removal_confirmation: Option<BulkRemovalConfirmation>,
    appearance: AppearanceSettings,
    appearance_dirty: bool,
    theme_dialog_open: bool,
    removing: bool,
    busy: bool,
    status: Option<SharedString>,
    initial_frame_scheduled: bool,
    benchmark: Option<BenchmarkRun>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AppearanceSettings {
    mode: ColorMode,
    accent: AccentColor,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            mode: ColorMode::Light,
            accent: AccentColor::Mauve,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct PersistedAppearance {
    mode: String,
    accent: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent editor menus and operations require independent flags"
)]
struct BookDetailEditor {
    original: Book,
    curation: BookCurationDraft,
    list_state: ListState,
    title: Entity<InputState>,
    contributors: Vec<ContributorField>,
    series_input: Option<Entity<InputState>>,
    series_menu_open: bool,
    series_suggestions: Vec<SeriesUsage>,
    series_suggestion_generation: u64,
    series_suggestions_loading: bool,
    series_index: Option<Entity<InputState>>,
    series_index_generation: u64,
    series_index_availability: SeriesIndexAvailability,
    publisher: Option<Entity<InputState>>,
    publication_date: Option<Entity<InputState>>,
    rating: BookRating,
    language: String,
    language_menu_open: bool,
    description: Option<Entity<TextareaState>>,
    tag_input: Option<Entity<InputState>>,
    tag_menu_open: bool,
    tag_creation_name: Option<String>,
    tag_suggestions: Vec<TagUsage>,
    tag_suggestion_generation: u64,
    tag_suggestions_loading: bool,
    role_picker: Option<u64>,
    dirty: bool,
    operation: DetailOperation,
    remove_confirmation: bool,
    error: Option<SharedString>,
    error_section: DetailErrorSection,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DetailOperation {
    #[default]
    Idle,
    Saving,
    Assets,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DetailErrorSection {
    #[default]
    Information,
    Publication,
    Files,
    Series,
    Contributors,
    Tags,
    Library,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SeriesIndexAvailability {
    #[default]
    Idle,
    Checking,
    Available,
    Conflict,
}

struct ContributorField {
    row_id: u64,
    persisted_id: Option<ContributorId>,
    persisted_name: String,
    persisted_sort_name: String,
    name: Entity<InputState>,
    role: ContributorRole,
}

impl BookDetailEditor {
    fn new(book: Book, window: &mut Window, cx: &mut Context<LecternView>) -> Self {
        let curation = BookCurationDraft::from_book(&book);
        let contributors = curation
            .contributors
            .iter()
            .map(|draft| ContributorField {
                row_id: draft.row_id,
                persisted_id: draft.existing_id,
                persisted_name: draft.name.clone(),
                persisted_sort_name: draft.sort_name.clone(),
                name: metadata_input(window, cx, "Contributor name", draft.name.clone()),
                role: draft.role,
            })
            .collect::<Vec<_>>();
        let contributor_count = contributors.len();
        Self {
            title: metadata_input(window, cx, "Book title", book.title.clone()),
            contributors,
            series_input: None,
            series_menu_open: false,
            series_suggestions: Vec::new(),
            series_suggestion_generation: 0,
            series_suggestions_loading: false,
            series_index: None,
            series_index_generation: 0,
            series_index_availability: SeriesIndexAvailability::Idle,
            publisher: None,
            publication_date: None,
            rating: book.rating,
            language: book.language.clone().unwrap_or_default(),
            language_menu_open: false,
            description: None,
            tag_input: None,
            tag_menu_open: false,
            tag_creation_name: None,
            tag_suggestions: Vec::new(),
            tag_suggestion_generation: 0,
            tag_suggestions_loading: false,
            role_picker: None,
            list_state: ListState::new(
                detail_item_count(contributor_count),
                ListAlignment::Top,
                px(64.),
            ),
            original: book,
            curation,
            dirty: false,
            operation: DetailOperation::Idle,
            remove_confirmation: false,
            error: None,
            error_section: DetailErrorSection::Information,
        }
    }

    fn build_edit(&self, cx: &App) -> Result<lectern_core::organisation::BookEdit, String> {
        let mut curation = self.curation.clone();
        for (draft, field) in curation.contributors.iter_mut().zip(&self.contributors) {
            let name = field.name.read(cx).value().to_string();
            draft.name.clone_from(&name);
            draft.role = field.role;
            let unchanged_existing = field.persisted_id.is_some() && name == field.persisted_name;
            if unchanged_existing {
                draft.existing_id = field.persisted_id;
                draft.sort_name.clone_from(&field.persisted_sort_name);
                draft.confirmed_new = false;
            } else {
                draft.existing_id = None;
                draft.sort_name.clone_from(&name);
                draft.confirm_new()?;
            }
        }

        curation.series.index = self.series_index.as_ref().map_or_else(
            || curation.series.index.clone(),
            |state| state.read(cx).value().to_string(),
        );
        if curation.series.name.trim().is_empty() {
            curation.series.clear();
        }

        let publisher = self.publisher.as_ref().map_or_else(
            || self.original.publisher.clone().unwrap_or_default(),
            |state| state.read(cx).value().to_string(),
        );
        let language = self.language.clone();
        let publication_date = self.publication_date.as_ref().map_or_else(
            || {
                self.original
                    .publication_date
                    .map(|date| date.to_string())
                    .unwrap_or_default()
            },
            |state| state.read(cx).value().to_string(),
        );
        let description = self.description.as_ref().map_or_else(
            || self.original.description.clone().unwrap_or_default(),
            |state| state.read(cx).value().to_string(),
        );
        curation.to_book_edit(
            &self.original,
            self.title.read(cx).value().as_str(),
            &publisher,
            &publication_date,
            &language,
            &description,
            self.rating,
        )
    }

    fn set_inputs_disabled(&self, disabled: bool, cx: &mut App) {
        self.title
            .update(cx, |state, cx| state.set_disabled(disabled, cx));
        for contributor in &self.contributors {
            contributor
                .name
                .update(cx, |state, cx| state.set_disabled(disabled, cx));
        }
        for state in [
            self.series_input.as_ref(),
            self.series_index.as_ref(),
            self.publisher.as_ref(),
            self.publication_date.as_ref(),
            self.tag_input.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            state.update(cx, |state, cx| state.set_disabled(disabled, cx));
        }
        if let Some(state) = &self.description {
            state.update(cx, |state, cx| state.set_disabled(disabled, cx));
        }
    }
}

fn metadata_input(
    window: &mut Window,
    cx: &mut Context<LecternView>,
    placeholder: &'static str,
    value: String,
) -> Entity<InputState> {
    let state = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
    });
    cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change)
            && let Some(editor) = &mut this.detail_editor
        {
            editor.dirty = true;
            editor.error = None;
        }
        cx.notify();
    })
    .detach();
    state
}

fn metadata_textarea(
    window: &mut Window,
    cx: &mut Context<LecternView>,
    placeholder: &'static str,
    value: String,
) -> Entity<TextareaState> {
    let state = cx.new(|cx| {
        TextareaState::new(window, cx)
            .rows(8)
            .placeholder(placeholder)
            .default_value(value)
    });
    cx.subscribe(&state, |this, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change)
            && let Some(editor) = &mut this.detail_editor
        {
            editor.dirty = true;
            editor.error = None;
        }
        cx.notify();
    })
    .detach();
    state
}

fn tag_search_input(window: &mut Window, cx: &mut Context<LecternView>) -> Entity<InputState> {
    let state = cx.new(|cx| InputState::new(window, cx).placeholder("Add or find a tag…"));
    cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            let query = state.read(cx).value().to_string();
            this.request_detail_tag_suggestions(query, cx);
        }
        cx.notify();
    })
    .detach();
    state
}

fn series_search_input(window: &mut Window, cx: &mut Context<LecternView>) -> Entity<InputState> {
    let state = cx.new(|cx| InputState::new(window, cx).placeholder("Add or find a series…"));
    cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            let query = state.read(cx).value().to_string();
            this.request_detail_series_suggestions(query, cx);
        }
        cx.notify();
    })
    .detach();
    state
}

fn series_index_input(
    window: &mut Window,
    cx: &mut Context<LecternView>,
    value: String,
) -> Entity<InputState> {
    let state = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Book number")
            .default_value(value)
    });
    cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            if let Some(editor) = &mut this.detail_editor {
                editor.dirty = true;
                editor.error = None;
            }
            let value = state.read(cx).value().to_string();
            this.request_series_index_availability(&value, cx);
        }
        cx.notify();
    })
    .detach();
    state
}

fn appearance_path(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("appearance.json")
}

fn load_appearance(database_path: &Path) -> AppearanceSettings {
    let Ok(bytes) = fs::read(appearance_path(database_path)) else {
        return AppearanceSettings::default();
    };
    let Ok(persisted) = serde_json::from_slice::<PersistedAppearance>(&bytes) else {
        return AppearanceSettings::default();
    };
    let Some(mode) = ColorMode::parse(&persisted.mode) else {
        return AppearanceSettings::default();
    };
    let Some(accent) = AccentColor::parse(&persisted.accent) else {
        return AppearanceSettings::default();
    };
    AppearanceSettings { mode, accent }
}

fn persist_appearance(database_path: &Path, appearance: AppearanceSettings) -> Result<(), String> {
    let path = appearance_path(database_path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&PersistedAppearance {
        mode: appearance.mode.as_str().to_owned(),
        accent: appearance.accent.as_str().to_owned(),
    })
    .map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

impl LecternView {
    fn new(
        benchmark: Option<BenchmarkRun>,
        database_path: PathBuf,
        appearance: AppearanceSettings,
    ) -> Self {
        let library_state = if benchmark.is_some() {
            LibraryState::Ready
        } else {
            LibraryState::Loading
        };
        let populated_benchmark = benchmark.as_ref().is_some_and(|benchmark| {
            matches!(
                benchmark.workload,
                BenchmarkWorkload::LibrarySelection | BenchmarkWorkload::BookDetail
            )
        });
        Self {
            database_path,
            library_state,
            library_total: if populated_benchmark { 50_000 } else { 0 },
            books: if populated_benchmark {
                benchmark_library_books()
            } else {
                Vec::new()
            },
            query: LibraryQuery::default(),
            selection: GridSelection::default(),
            selection_generation: 0,
            selection_pending: None,
            detail_editor: None,
            detail_loading: None,
            removal_confirmation: None,
            appearance,
            appearance_dirty: false,
            theme_dialog_open: false,
            removing: false,
            busy: false,
            status: None,
            initial_frame_scheduled: false,
            benchmark,
        }
    }

    fn initial_frame_presented(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(benchmark) = &mut self.benchmark {
            benchmark.initial_render = Some(benchmark.main_entry.elapsed());
            match benchmark.workload {
                BenchmarkWorkload::EmptyLibraryAddBooks => self.start_add_books(window, cx),
                BenchmarkWorkload::LibrarySelection => {
                    self.start_benchmark_selection(window, cx);
                }
                BenchmarkWorkload::BookDetail => self.start_benchmark_book_detail(window, cx),
            }
            return;
        }
        self.load_library(window, cx);
    }

    fn start_benchmark_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self
            .books
            .first()
            .expect("selection benchmark has a first book")
            .summary
            .id;
        let benchmark = self
            .benchmark
            .as_mut()
            .expect("selection benchmark state is present");
        benchmark.action_started = Some(Instant::now());
        self.selection.begin_explicit();
        self.selection.toggle(first, 0);
        cx.notify();
        cx.on_next_frame(window, Self::benchmark_selection_presented);
    }

    fn benchmark_selection_presented(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let benchmark = self
            .benchmark
            .as_mut()
            .expect("selection benchmark state is present");
        benchmark.selection_painted = Some(
            benchmark
                .action_started
                .expect("selection benchmark action started")
                .elapsed(),
        );
        self.removal_confirmation = Some(BulkRemovalConfirmation {
            selection: self
                .selection
                .descriptor()
                .expect("selection benchmark descriptor is present"),
            selected_books: self.selection.selected_count(),
        });
        benchmark.confirmation_started = Some(Instant::now());
        cx.notify();
        cx.on_next_frame(window, Self::benchmark_confirmation_presented);
    }

    fn start_benchmark_book_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let benchmark = self
            .benchmark
            .as_mut()
            .expect("book-detail benchmark state is present");
        benchmark.action_started = Some(Instant::now());
        self.detail_editor = Some(BookDetailEditor::new(benchmark_book_detail(), window, cx));
        cx.notify();
        cx.on_next_frame(window, Self::benchmark_book_detail_presented);
    }

    fn benchmark_book_detail_presented(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let book = self
            .detail_editor
            .as_ref()
            .expect("book-detail benchmark has a presented book")
            .original
            .clone();
        let title = book.title.clone();
        let publication_date = book
            .publication_date
            .map(|date| date.to_string())
            .unwrap_or_default();
        let rating_half_stars = book.rating.half_stars();
        let contributor_count = book.contributors.len();
        let tag_count = book.tags.len();
        let asset_count = book.assets.len();
        let mut benchmark = self
            .benchmark
            .take()
            .expect("book-detail benchmark state is present");
        benchmark.detail_painted = Some(
            benchmark
                .action_started
                .expect("book-detail action was started")
                .elapsed(),
        );
        benchmark.finish_book_detail(
            self.library_total,
            self.books.len(),
            title,
            publication_date,
            rating_half_stars,
            contributor_count,
            tag_count,
            asset_count,
        );
        cx.quit();
    }

    fn benchmark_confirmation_presented(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let benchmark = self
            .benchmark
            .take()
            .expect("selection benchmark state is present");
        benchmark.finish_selection(
            self.library_total,
            self.books.len(),
            self.selection.selected_count(),
            self.removal_confirmation.is_some(),
        );
        cx.quit();
    }

    fn load_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_selection();
        self.library_state = LibraryState::Loading;
        let database_path = self.database_path.clone();
        let load = cx.background_executor().spawn(async move {
            load_library_snapshot(&database_path).map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                this.library_state = LibraryState::Ready;
                match result {
                    Ok(snapshot) => this.apply_snapshot(snapshot),
                    Err(error) => {
                        this.library_total = 0;
                        this.books.clear();
                        this.status = Some(format!("Could not open the library: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_add_books(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.removing || self.selection_pending.is_some() {
            return;
        }
        self.busy = true;
        self.status = None;
        if let Some(benchmark) = &mut self.benchmark {
            benchmark.action_started = Some(Instant::now());
        }
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            if let Some(benchmark) = this.benchmark.take() {
                benchmark.finish();
                cx.quit();
                return;
            }
            Self::prompt_for_books(window, cx);
        });
    }

    fn prompt_for_books(window: &mut Window, cx: &mut Context<Self>) {
        let response = rfd::AsyncFileDialog::new()
            .set_title("Add books to Lectern")
            .add_filter("EPUB or PDF", &["epub", "pdf"])
            .pick_files();
        cx.spawn_in(window, async move |this, cx| {
            let selection = response.await.map(|files| {
                files
                    .into_iter()
                    .map(|file| file.path().to_path_buf())
                    .collect::<Vec<_>>()
            });
            match selection {
                Some(paths) if !paths.is_empty() => {
                    this.update_in(cx, |this, window, cx| {
                        this.import_books(paths, window, cx);
                    })
                    .ok();
                }
                _ => {
                    this.update(cx, |this, cx| {
                        this.busy = false;
                        this.status = None;
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn import_books(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let database_path = self.database_path.clone();
        let import = cx.background_executor().spawn(async move {
            import_books_and_load_library(&database_path, &paths).map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = import.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.library_state = LibraryState::Ready;
                match result {
                    Ok((summary, snapshot)) => {
                        this.clear_selection();
                        this.apply_snapshot(snapshot);
                        this.status = import_status(&summary);
                    }
                    Err(error) => {
                        this.status = Some(format!("Could not add books: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_snapshot(&mut self, snapshot: LibrarySnapshot) {
        self.library_total = snapshot.total;
        self.books = snapshot
            .books
            .into_iter()
            .map(|book| LibraryBook {
                summary: book.summary,
                cover: book
                    .cover
                    .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes))),
            })
            .collect();
    }

    fn clear_selection(&mut self) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection_pending = None;
        self.selection.clear();
        self.removal_confirmation = None;
    }

    fn begin_selection(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.removing {
            return;
        }
        self.selection.begin_explicit();
        self.status = None;
        cx.notify();
    }

    fn open_theme_dialog(&mut self, cx: &mut Context<Self>) {
        self.theme_dialog_open = true;
        cx.notify();
    }

    fn close_theme_dialog(&mut self, cx: &mut Context<Self>) {
        self.theme_dialog_open = false;
        if self.appearance_dirty {
            self.appearance_dirty = false;
            let database_path = self.database_path.clone();
            let appearance = self.appearance;
            let save = cx
                .background_executor()
                .spawn(async move { persist_appearance(&database_path, appearance) });
            cx.spawn(async move |this, cx| {
                let result = save.await;
                this.update(cx, |this, cx| {
                    if let Err(error) = result {
                        this.status = Some(format!("Could not save appearance: {error}").into());
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn set_color_mode(&mut self, mode: ColorMode, cx: &mut Context<Self>) {
        self.apply_appearance(
            AppearanceSettings {
                mode,
                ..self.appearance
            },
            cx,
        );
    }

    fn set_accent_color(&mut self, accent: AccentColor, cx: &mut Context<Self>) {
        self.apply_appearance(
            AppearanceSettings {
                accent,
                ..self.appearance
            },
            cx,
        );
    }

    fn apply_appearance(&mut self, appearance: AppearanceSettings, cx: &mut Context<Self>) {
        if self.appearance == appearance {
            return;
        }
        self.appearance = appearance;
        self.appearance_dirty = true;
        install_theme(
            cx,
            PrimerTheme::with_accent(appearance.mode, appearance.accent),
        );
        cx.notify();
    }

    fn clear_selection_action(&mut self, cx: &mut Context<Self>) {
        if self.removing || (!self.selection.is_active() && self.selection_pending.is_none()) {
            return;
        }
        self.clear_selection();
        self.status = Some("Selection cleared.".into());
        cx.notify();
    }

    fn toggle_book(&mut self, id: BookId, index: usize, cx: &mut Context<Self>) {
        if self.busy || self.removing || self.selection_pending.is_some() {
            return;
        }
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection.toggle(id, index);
        self.status = None;
        cx.notify();
    }

    fn handle_book_click(
        &mut self,
        id: BookId,
        index: usize,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if modifiers.shift && self.selection.anchor.is_some() {
            self.select_range_to(index, window, cx);
        } else if self.selection.is_active() || modifiers.secondary() {
            self.toggle_book(id, index, cx);
        } else {
            self.open_book_detail(id, window, cx);
        }
    }

    fn open_book_detail(&mut self, id: BookId, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.removing || self.detail_loading.is_some() {
            return;
        }
        if let Some(editor) = &self.detail_editor {
            if editor.original.id == id {
                return;
            }
            if editor.dirty {
                self.status = Some("Save or reset the open book before switching books.".into());
                cx.notify();
                return;
            }
        }
        self.detail_loading = Some(id);
        self.status = Some("Opening book details…".into());
        cx.notify();

        let database_path = self.database_path.clone();
        let load = cx
            .background_executor()
            .spawn(async move { load_book(&database_path, id).map_err(|error| error.to_string()) });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update_in(cx, |this, window, cx| {
                if this.detail_loading != Some(id) {
                    return;
                }
                this.detail_loading = None;
                match result {
                    Ok(Some(book)) => {
                        this.detail_editor = Some(BookDetailEditor::new(book, window, cx));
                        this.status = None;
                    }
                    Ok(None) => {
                        this.status = Some("That book is no longer in the library.".into());
                    }
                    Err(error) => {
                        this.status = Some(format!("Could not open book details: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn close_book_detail(&mut self, cx: &mut Context<Self>) {
        self.detail_editor = None;
        self.detail_loading = None;
        self.status = None;
        cx.notify();
    }

    fn reset_book_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(book) = self
            .detail_editor
            .as_ref()
            .map(|editor| editor.original.clone())
        else {
            return;
        };
        self.detail_editor = Some(BookDetailEditor::new(book, window, cx));
        self.status = Some("Book details reset.".into());
        cx.notify();
    }

    fn add_detail_contributor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if editor.operation != DetailOperation::Idle || self.removing {
            return;
        }
        let row_id = editor.curation.add_contributor();
        let field = ContributorField {
            row_id,
            persisted_id: None,
            persisted_name: String::new(),
            persisted_sort_name: String::new(),
            name: metadata_input(window, cx, "Contributor name", String::new()),
            role: ContributorRole::Author,
        };
        let editor = self
            .detail_editor
            .as_mut()
            .expect("detail editor remains available");
        editor.contributors.push(field);
        editor
            .list_state
            .reset(detail_item_count(editor.contributors.len()));
        editor.list_state.scroll_to_reveal_item(
            DETAIL_CONTRIBUTOR_START_ITEM + editor.contributors.len().saturating_sub(1),
        );
        editor.dirty = true;
        editor.error = None;
        cx.notify();
    }

    fn set_contributor_role(&mut self, row_id: u64, role: ContributorRole, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        let Some(field) = editor
            .contributors
            .iter_mut()
            .find(|field| field.row_id == row_id)
        else {
            return;
        };
        field.role = role;
        if let Some(draft) = editor
            .curation
            .contributors
            .iter_mut()
            .find(|draft| draft.row_id == row_id)
        {
            draft.role = role;
        }
        editor.dirty = true;
        editor.role_picker = None;
        editor.error = None;
        cx.notify();
    }

    fn set_contributor_role_picker_open(
        &mut self,
        row_id: u64,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        editor.role_picker = open.then_some(row_id);
        cx.notify();
    }

    fn set_detail_language(&mut self, code: &'static str, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if editor.operation != DetailOperation::Idle || editor.language == code {
            return;
        }
        code.clone_into(&mut editor.language);
        editor.language_menu_open = false;
        editor.dirty = true;
        editor.error = None;
        cx.notify();
    }

    fn set_language_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.detail_editor {
            editor.language_menu_open = open;
            cx.notify();
        }
    }

    fn set_detail_rating(&mut self, half_stars: u8, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if editor.operation != DetailOperation::Idle {
            return;
        }
        let half_stars = if half_stars > 0 && editor.rating.half_stars() == half_stars {
            0
        } else {
            half_stars
        };
        let Some(rating) = BookRating::from_half_stars(half_stars) else {
            return;
        };
        if rating == editor.rating {
            return;
        }
        editor.rating = rating;
        editor.dirty = true;
        editor.error = None;
        cx.notify();
    }

    fn move_detail_contributor(&mut self, row_id: u64, offset: isize, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        let Some(index) = editor
            .contributors
            .iter()
            .position(|field| field.row_id == row_id)
        else {
            return;
        };
        let Some(target) = index.checked_add_signed(offset) else {
            return;
        };
        if target >= editor.contributors.len() {
            return;
        }
        editor.contributors.swap(index, target);
        editor.curation.contributors.swap(index, target);
        editor.list_state.remeasure_items(
            DETAIL_CONTRIBUTOR_START_ITEM
                ..DETAIL_CONTRIBUTOR_START_ITEM + editor.contributors.len(),
        );
        editor.dirty = true;
        editor.error = None;
        cx.notify();
    }

    fn set_series_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let query = {
            let Some(editor) = &mut self.detail_editor else {
                return;
            };
            editor.series_menu_open = open;
            if !open {
                cx.notify();
                return;
            }
            editor
                .series_input
                .as_ref()
                .map_or_else(String::new, |input| input.read(cx).value().to_string())
        };
        self.request_detail_series_suggestions(query, cx);
    }

    fn request_detail_series_suggestions(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if !editor.series_menu_open {
            return;
        }
        editor.series_suggestion_generation = editor.series_suggestion_generation.wrapping_add(1);
        let generation = editor.series_suggestion_generation;
        editor.series_suggestions_loading = true;
        let selected = editor.curation.existing_series_id();
        let database_path = self.database_path.clone();
        let load = cx.background_executor().spawn(async move {
            let mut service =
                SqliteLibraryService::open(&database_path).map_err(|error| error.to_string())?;
            service
                .autocomplete_series(query.trim(), &selected, 50)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                let Some(editor) = &mut this.detail_editor else {
                    return;
                };
                if editor.series_suggestion_generation != generation {
                    return;
                }
                editor.series_suggestions_loading = false;
                match result {
                    Ok(suggestions) => {
                        editor.series_suggestions = suggestions;
                        editor.error = None;
                    }
                    Err(error) => {
                        editor.error = Some(error.into());
                        editor.error_section = DetailErrorSection::Series;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn select_existing_detail_series(
        &mut self,
        series: &Series,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = {
            let Some(editor) = &mut self.detail_editor else {
                return;
            };
            editor
                .curation
                .series
                .select_existing(series.id, &series.name);
            editor.series_menu_open = false;
            editor.dirty = true;
            editor.error = None;
            if let Some(input) = &editor.series_input {
                input.update(cx, |state, cx| state.set_value("", window, cx));
            }
            if let Some(input) = &editor.series_index {
                input.update(cx, |state, cx| state.set_disabled(false, cx));
            }
            editor
                .series_index
                .as_ref()
                .map_or_else(String::new, |input| input.read(cx).value().to_string())
        };
        self.request_series_index_availability(&index, cx);
        cx.notify();
    }

    fn create_detail_series(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        editor.curation.series.name = name;
        editor.curation.series.name_edited();
        match editor.curation.series.confirm_new() {
            Ok(()) => {
                editor.series_menu_open = false;
                editor.series_index_availability = SeriesIndexAvailability::Idle;
                editor.dirty = true;
                editor.error = None;
                if let Some(input) = &editor.series_input {
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                }
                if let Some(input) = &editor.series_index {
                    input.update(cx, |state, cx| state.set_disabled(false, cx));
                }
            }
            Err(error) => {
                editor.error = Some(error.into());
                editor.error_section = DetailErrorSection::Series;
            }
        }
        cx.notify();
    }

    fn remove_detail_series(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        editor.curation.series.clear();
        editor.series_menu_open = false;
        editor.series_index_generation = editor.series_index_generation.wrapping_add(1);
        editor.series_index_availability = SeriesIndexAvailability::Idle;
        editor.dirty = true;
        editor.error = None;
        if let Some(input) = &editor.series_input {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        if let Some(input) = &editor.series_index {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.set_disabled(true, cx);
            });
        }
        cx.notify();
    }

    fn request_series_index_availability(&mut self, value: &str, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        editor.series_index_generation = editor.series_index_generation.wrapping_add(1);
        let generation = editor.series_index_generation;
        let Some(series_id) = editor.curation.series.existing_id else {
            editor.series_index_availability = SeriesIndexAvailability::Idle;
            cx.notify();
            return;
        };
        let value = value.trim();
        if value.is_empty() {
            editor.series_index_availability = SeriesIndexAvailability::Idle;
            cx.notify();
            return;
        }
        let Ok(index) = value.parse::<SeriesIndex>() else {
            editor.series_index_availability = SeriesIndexAvailability::Idle;
            cx.notify();
            return;
        };
        let book_id = editor.original.id;
        editor.series_index_availability = SeriesIndexAvailability::Checking;
        let database_path = self.database_path.clone();
        let check = cx.background_executor().spawn(async move {
            let mut service =
                SqliteLibraryService::open(&database_path).map_err(|error| error.to_string())?;
            service
                .series_index_is_available(series_id, index, book_id)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = check.await;
            this.update(cx, |this, cx| {
                let Some(editor) = &mut this.detail_editor else {
                    return;
                };
                if editor.series_index_generation != generation {
                    return;
                }
                match result {
                    Ok(true) => {
                        editor.series_index_availability = SeriesIndexAvailability::Available;
                        editor.error = None;
                    }
                    Ok(false) => {
                        editor.series_index_availability = SeriesIndexAvailability::Conflict;
                    }
                    Err(error) => {
                        editor.series_index_availability = SeriesIndexAvailability::Idle;
                        editor.error = Some(error.into());
                        editor.error_section = DetailErrorSection::Series;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn remove_detail_contributor(&mut self, row_id: u64, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        let Some(index) = editor
            .contributors
            .iter()
            .position(|field| field.row_id == row_id)
        else {
            return;
        };
        editor.contributors.remove(index);
        editor.curation.contributors.remove(index);
        editor
            .list_state
            .reset(detail_item_count(editor.contributors.len()));
        editor.dirty = true;
        editor.error = None;
        cx.notify();
    }

    fn set_tag_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let query = {
            let Some(editor) = &mut self.detail_editor else {
                return;
            };
            editor.tag_menu_open = open;
            if !open {
                editor.tag_creation_name = None;
                cx.notify();
                return;
            }
            editor
                .tag_input
                .as_ref()
                .map_or_else(String::new, |input| input.read(cx).value().to_string())
        };
        self.request_detail_tag_suggestions(query, cx);
    }

    fn request_detail_tag_suggestions(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if !editor.tag_menu_open {
            return;
        }
        editor.tag_suggestion_generation = editor.tag_suggestion_generation.wrapping_add(1);
        let generation = editor.tag_suggestion_generation;
        editor.tag_suggestions_loading = true;
        editor.tag_creation_name = None;
        let selected = editor.curation.existing_tag_ids();
        let database_path = self.database_path.clone();
        let load = cx.background_executor().spawn(async move {
            let mut service =
                SqliteLibraryService::open(&database_path).map_err(|error| error.to_string())?;
            service
                .autocomplete_tags(query.trim(), &selected, 50)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                let Some(editor) = &mut this.detail_editor else {
                    return;
                };
                if editor.tag_suggestion_generation != generation {
                    return;
                }
                editor.tag_suggestions_loading = false;
                match result {
                    Ok(suggestions) => {
                        editor.tag_suggestions = suggestions;
                        editor.error = None;
                    }
                    Err(error) => {
                        editor.error = Some(error.into());
                        editor.error_section = DetailErrorSection::Tags;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn toggle_existing_detail_tag(&mut self, tag: &Tag, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        let changed = if editor
            .curation
            .tags
            .iter()
            .any(|selected| selected.existing_id == Some(tag.id))
        {
            editor.curation.remove_tag_id(tag.id)
        } else {
            editor
                .curation
                .add_existing_tag(tag.id, &tag.name, tag.color)
        };
        if changed {
            let tags_item = detail_tags_item_index(editor.contributors.len());
            editor.list_state.remeasure_items(tags_item..tags_item + 1);
            editor.dirty = true;
            editor.error = None;
        }
        cx.notify();
    }

    fn begin_detail_tag_creation(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        match normalize_name(NameKind::Tag, name) {
            Ok(name) => {
                editor.tag_creation_name = Some(name);
                editor.error = None;
            }
            Err(error) => {
                editor.error = Some(error.to_string().into());
                editor.error_section = DetailErrorSection::Tags;
            }
        }
        cx.notify();
    }

    fn create_detail_tag(&mut self, color: TagColor, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        let Some(name) = editor.tag_creation_name.clone() else {
            return;
        };
        match editor.curation.add_new_tag(&name, color) {
            Ok(added) => {
                if let Some(input) = &editor.tag_input {
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                }
                editor.tag_creation_name = None;
                editor.tag_menu_open = false;
                editor.dirty |= added;
                editor.error = None;
                let tags_item = detail_tags_item_index(editor.contributors.len());
                editor.list_state.remeasure_items(tags_item..tags_item + 1);
            }
            Err(error) => {
                editor.error = Some(error.into());
                editor.error_section = DetailErrorSection::Tags;
            }
        }
        cx.notify();
    }

    fn remove_detail_tag(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if index >= editor.curation.tags.len() {
            return;
        }
        editor.curation.tags.remove(index);
        let tags_item = detail_tags_item_index(editor.contributors.len());
        editor.list_state.remeasure_items(tags_item..tags_item + 1);
        editor.dirty = true;
        editor.error = None;
        cx.notify();
    }

    fn save_book_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.detail_editor else {
            return;
        };
        if editor.operation != DetailOperation::Idle
            || self.removing
            || !editor.dirty
            || matches!(
                editor.series_index_availability,
                SeriesIndexAvailability::Checking | SeriesIndexAvailability::Conflict
            )
        {
            return;
        }
        let edit = match editor.build_edit(cx) {
            Ok(edit) => edit,
            Err(error) => {
                let error_section = metadata_error_section(&error);
                let editor = self
                    .detail_editor
                    .as_mut()
                    .expect("detail editor remains available");
                editor.error = Some(error.into());
                editor.error_section = error_section;
                let item = match error_section {
                    DetailErrorSection::Information => DETAIL_INFORMATION_ITEM,
                    DetailErrorSection::Publication => DETAIL_PUBLICATION_ITEM,
                    DetailErrorSection::Files => DETAIL_FILES_ITEM,
                    DetailErrorSection::Series => DETAIL_SERIES_ITEM,
                    DetailErrorSection::Contributors => DETAIL_CONTRIBUTOR_START_ITEM,
                    DetailErrorSection::Tags => detail_tags_item_index(editor.contributors.len()),
                    DetailErrorSection::Library => {
                        detail_library_item_index(editor.contributors.len())
                    }
                };
                editor.list_state.scroll_to_reveal_item(item);
                cx.notify();
                return;
            }
        };
        self.detail_editor
            .as_mut()
            .expect("detail editor remains available")
            .operation = DetailOperation::Saving;
        self.detail_editor
            .as_ref()
            .expect("detail editor remains available")
            .set_inputs_disabled(true, cx);
        self.status = Some("Saving book details…".into());
        cx.notify();

        let id = edit.id;
        let database_path = self.database_path.clone();
        let save = cx
            .background_executor()
            .spawn(async move { save_book_and_load_library(&database_path, &edit) });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            this.update_in(cx, |this, window, cx| {
                if this
                    .detail_editor
                    .as_ref()
                    .is_none_or(|editor| editor.original.id != id)
                {
                    return;
                }
                match result {
                    Ok((book, snapshot)) => {
                        this.apply_snapshot(snapshot);
                        this.detail_editor = Some(BookDetailEditor::new(book, window, cx));
                        this.status = Some("Book details saved.".into());
                    }
                    Err(error) => {
                        let editor = this
                            .detail_editor
                            .as_mut()
                            .expect("matching detail editor remains available");
                        editor.operation = DetailOperation::Idle;
                        editor.error_section = metadata_error_section(&error);
                        editor.error = Some(error.into());
                        editor.set_inputs_disabled(false, cx);
                        this.status = Some("Could not save book details.".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn prompt_for_detail_assets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if editor.operation != DetailOperation::Idle || editor.dirty || self.removing {
            return;
        }
        editor.operation = DetailOperation::Assets;
        editor.set_inputs_disabled(true, cx);
        cx.notify();
        let response = rfd::AsyncFileDialog::new()
            .set_title("Add EPUB or PDF assets")
            .add_filter("EPUB or PDF", &["epub", "pdf"])
            .pick_files();
        cx.spawn_in(window, async move |this, cx| {
            let paths = response.await.map(|files| {
                files
                    .into_iter()
                    .map(|file| file.path().to_path_buf())
                    .collect::<Vec<_>>()
            });
            this.update_in(cx, |this, window, cx| match paths {
                Some(paths) if !paths.is_empty() => this.attach_detail_assets(paths, window, cx),
                _ => {
                    if let Some(editor) = &mut this.detail_editor {
                        editor.operation = DetailOperation::Idle;
                        editor.set_inputs_disabled(false, cx);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn attach_detail_assets(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.detail_editor.as_ref().map(|editor| editor.original.id) else {
            return;
        };
        self.status = Some("Adding book assets…".into());
        let database_path = self.database_path.clone();
        let attach = cx
            .background_executor()
            .spawn(async move { attach_assets_and_load_library(&database_path, id, &paths) });
        cx.spawn_in(window, async move |this, cx| {
            let result = attach.await;
            this.update_in(cx, |this, window, cx| {
                if this
                    .detail_editor
                    .as_ref()
                    .is_none_or(|editor| editor.original.id != id)
                {
                    return;
                }
                match result {
                    Ok(completion) => {
                        this.apply_snapshot(completion.snapshot);
                        this.detail_editor =
                            Some(BookDetailEditor::new(completion.book, window, cx));
                        this.status = Some(completion.message.into());
                    }
                    Err(error) => {
                        let editor = this
                            .detail_editor
                            .as_mut()
                            .expect("matching detail editor remains available");
                        editor.operation = DetailOperation::Idle;
                        editor.error = Some(error.into());
                        editor.error_section = DetailErrorSection::Files;
                        editor.set_inputs_disabled(false, cx);
                        this.status = Some("Could not add book assets.".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn detach_detail_asset(&mut self, asset: AssetId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if editor.original.assets.len() <= 1
            || editor.dirty
            || editor.operation != DetailOperation::Idle
            || self.removing
        {
            return;
        }
        let id = editor.original.id;
        editor.operation = DetailOperation::Assets;
        editor.set_inputs_disabled(true, cx);
        self.status = Some("Removing book asset…".into());
        cx.notify();
        let database_path = self.database_path.clone();
        let detach = cx
            .background_executor()
            .spawn(async move { detach_asset_and_load_library(&database_path, id, asset) });
        cx.spawn_in(window, async move |this, cx| {
            let result = detach.await;
            this.update_in(cx, |this, window, cx| {
                if this
                    .detail_editor
                    .as_ref()
                    .is_none_or(|editor| editor.original.id != id)
                {
                    return;
                }
                match result {
                    Ok((book, snapshot)) => {
                        this.apply_snapshot(snapshot);
                        this.detail_editor = Some(BookDetailEditor::new(book, window, cx));
                        this.status = Some("Book asset removed.".into());
                    }
                    Err(error) => {
                        let editor = this
                            .detail_editor
                            .as_mut()
                            .expect("matching detail editor remains available");
                        editor.operation = DetailOperation::Idle;
                        editor.error = Some(error.into());
                        editor.error_section = DetailErrorSection::Files;
                        editor.set_inputs_disabled(false, cx);
                        this.status = Some("Could not remove book asset.".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn request_detail_removal(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.detail_editor else {
            return;
        };
        if editor.operation != DetailOperation::Idle || self.removing {
            return;
        }
        editor.remove_confirmation = true;
        let library_item = detail_item_count(editor.contributors.len()) - 1;
        editor
            .list_state
            .remeasure_items(library_item..library_item + 1);
        cx.notify();
    }

    fn cancel_detail_removal(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.detail_editor {
            editor.remove_confirmation = false;
            let library_item = detail_item_count(editor.contributors.len()) - 1;
            editor
                .list_state
                .remeasure_items(library_item..library_item + 1);
            cx.notify();
        }
    }

    fn start_detail_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.detail_editor else {
            return;
        };
        if !editor.remove_confirmation || editor.operation != DetailOperation::Idle || self.removing
        {
            return;
        }
        let id = editor.original.id;
        self.removing = true;
        self.detail_editor
            .as_ref()
            .expect("detail editor remains available")
            .set_inputs_disabled(true, cx);
        self.status = Some("Removing book from the library…".into());
        cx.notify();
        let database_path = self.database_path.clone();
        let remove = cx
            .background_executor()
            .spawn(async move { remove_book_and_load_library(&database_path, id) });
        cx.spawn_in(window, async move |this, cx| {
            let result = remove.await;
            this.update(cx, |this, cx| {
                this.removing = false;
                match result {
                    Ok((removed, snapshot)) => {
                        this.apply_snapshot(snapshot);
                        this.detail_editor = None;
                        this.status = Some(if removed {
                            "Removed the book from the library; book files were kept.".into()
                        } else {
                            "That book was already absent from the library.".into()
                        });
                    }
                    Err(error) => {
                        let editor = this
                            .detail_editor
                            .as_mut()
                            .expect("detail editor remains available after failed removal");
                        editor.remove_confirmation = false;
                        editor.error = Some(error.into());
                        editor.error_section = DetailErrorSection::Library;
                        editor.set_inputs_disabled(false, cx);
                        this.status = Some("Could not remove the book.".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_range_to(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.removing || self.selection_pending.is_some() {
            return;
        }
        let Some(anchor) = self.selection.anchor else {
            return;
        };
        let offset = anchor.index.min(index);
        let length = anchor.index.max(index).saturating_sub(offset) + 1;
        let (Ok(offset), Ok(limit)) = (u64::try_from(offset), u32::try_from(length)) else {
            self.status = Some("Selection range exceeds this platform's supported size.".into());
            cx.notify();
            return;
        };
        let generation = self.selection_generation.wrapping_add(1);
        self.selection_generation = generation;
        self.selection_pending = Some(PendingSelection::Range);
        self.status = None;
        cx.notify();

        let database_path = self.database_path.clone();
        let query = self.query.clone();
        let resolve = cx.background_executor().spawn(async move {
            resolve_selection_range(&database_path, &query, offset, limit)
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = resolve.await;
            this.update(cx, |this, cx| {
                if generation != this.selection_generation
                    || !matches!(this.selection_pending, Some(PendingSelection::Range))
                {
                    return;
                }
                this.selection_pending = None;
                match result {
                    Ok(books) => {
                        this.selection.install_range(books);
                        this.status = None;
                    }
                    Err(error) => {
                        this.status = Some(format!("Could not select that range: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_all_matching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.removing
            || self.library_total == 0
            || self.selection_pending.is_some()
            || self.selection.is_every_matching()
        {
            return;
        }
        let generation = self.selection_generation.wrapping_add(1);
        self.selection_generation = generation;
        self.selection_pending = Some(PendingSelection::AllMatching);
        self.status = None;
        cx.notify();

        let database_path = self.database_path.clone();
        let query = self.query.clone();
        let resolved_query = query.clone();
        let resolve = cx.background_executor().spawn(async move {
            resolve_selection_snapshot(&database_path, &query).map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = resolve.await;
            this.update(cx, |this, cx| {
                if generation != this.selection_generation
                    || !matches!(this.selection_pending, Some(PendingSelection::AllMatching))
                {
                    return;
                }
                this.selection_pending = None;
                match result {
                    Ok(snapshot) => {
                        this.selection
                            .install_all_matching(resolved_query, snapshot);
                        this.status = None;
                    }
                    Err(error) => {
                        this.status =
                            Some(format!("Could not select matching books: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn request_bulk_removal(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.removing || self.selection_pending.is_some() {
            return;
        }
        let Some(selection) = self.selection.descriptor() else {
            return;
        };
        let selected_books = self.selection.selected_count();
        if selected_books == 0 {
            return;
        }
        self.removal_confirmation = Some(BulkRemovalConfirmation {
            selection,
            selected_books,
        });
        cx.notify();
    }

    fn cancel_bulk_removal(&mut self, cx: &mut Context<Self>) {
        self.removal_confirmation = None;
        cx.notify();
    }

    fn start_bulk_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirmation) = self.removal_confirmation.take() else {
            return;
        };
        if self.busy
            || self.removing
            || self.selection.selected_count() != confirmation.selected_books
            || self.selection.descriptor().as_ref() != Some(&confirmation.selection)
        {
            self.status = Some(
                "The selection changed; review it before removing books."
                    .to_owned()
                    .into(),
            );
            cx.notify();
            return;
        }

        self.removing = true;
        self.status = Some(
            format!(
                "Removing {} selected {} from the library…",
                confirmation.selected_books,
                pluralize_book(confirmation.selected_books),
            )
            .into(),
        );
        cx.notify();

        let database_path = self.database_path.clone();
        let remove = cx.background_executor().spawn(async move {
            remove_books_and_load_library(&database_path, &confirmation.selection)
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = remove.await;
            this.update(cx, |this, cx| {
                this.removing = false;
                match result {
                    Ok(completion) => {
                        this.clear_selection();
                        let status = format!(
                            "Removed {} {} from the library; book files were kept.",
                            completion.result.books_removed,
                            pluralize_book(completion.result.books_removed),
                        );
                        match completion.snapshot {
                            Ok(snapshot) => {
                                this.apply_snapshot(snapshot);
                                this.status = Some(status.into());
                            }
                            Err(error) => {
                                this.library_total = 0;
                                this.books.clear();
                                this.status = Some(
                                    format!("{status} Could not refresh the library: {error}")
                                        .into(),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        this.status =
                            Some(format!("Could not remove selected books: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add_books_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        let button_label = if self.busy {
            "Adding books…"
        } else {
            "Add books"
        };
        Button::new(id, button_label)
            .variant(ButtonVariant::Primary)
            .leading_icon(TablerIcon::Upload)
            .disabled(self.busy || self.removing || self.selection_pending.is_some())
            .on_click(cx.listener(|this, _, window, cx| {
                this.start_add_books(window, cx);
            }))
    }

    fn selection_bar(&self, theme: &PrimerTheme, cx: &mut Context<Self>) -> gpui::Div {
        let selected_books = self.selection.selected_count();
        let label = if self.removing {
            format!(
                "Removing {selected_books} selected {}…",
                pluralize_book(selected_books)
            )
        } else if self.selection_pending.is_some() {
            "Resolving selection…".to_owned()
        } else {
            selection_status(&self.selection)
        };

        div()
            .flex_none()
            .h(px(SELECTION_BAR_HEIGHT_PX))
            .p(theme.spacing.small)
            .border_b(theme.border.thin)
            .border_color(theme.border.muted)
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(theme.typography.body_size)
                    .font_weight(theme.typography.button_weight)
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.small)
                    .when(!self.selection.is_every_matching(), |actions| {
                        actions.child(
                            Button::new("select-all-matching", "Select all matching")
                                .disabled(
                                    self.busy || self.removing || self.selection_pending.is_some(),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.select_all_matching(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("clear-selection", "Clear selection")
                            .disabled(self.removing)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clear_selection_action(cx);
                            })),
                    )
                    .child(
                        Button::new("remove-selected", "Remove from library")
                            .variant(ButtonVariant::Danger)
                            .disabled(
                                selected_books == 0
                                    || self.busy
                                    || self.removing
                                    || self.selection_pending.is_some(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_bulk_removal(cx);
                            })),
                    ),
            )
    }

    fn bulk_removal_dialog(&self, theme: &PrimerTheme, cx: &mut Context<Self>) -> AlertDialog {
        let confirmation = self
            .removal_confirmation
            .as_ref()
            .expect("bulk removal dialog requires a confirmation");
        let selected_books = confirmation.selected_books;
        let entity = cx.entity().downgrade();
        let cancel_entity = entity.clone();
        let confirm_entity = entity.clone();
        let close_entity = entity.clone();

        AlertDialog::new(cx)
            .open(true)
            .on_open_change(move |open, _, _, cx| {
                if !open {
                    _ = close_entity.update(cx, LecternView::cancel_bulk_removal);
                }
            })
            .on_cancel(move |_, _, cx| {
                _ = cancel_entity.update(cx, LecternView::cancel_bulk_removal);
                true
            })
            .on_ok(move |_, window, cx| {
                _ = confirm_entity.update(cx, |this, cx| {
                    this.start_bulk_removal(window, cx);
                });
                true
            })
            .backdrop(
                AlertDialogBackdrop::new()
                    .absolute()
                    .inset_0()
                    .bg(theme.dialog.backdrop),
            )
            .popup(
                AlertDialogPopup::new()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(BULK_REMOVAL_DIALOG_WIDTH_PX))
                            .p(theme.spacing.extra_large)
                            .rounded(theme.dialog.radius)
                            .border(theme.border.thin)
                            .border_color(theme.border.muted)
                            .bg(theme.surface.background)
                            .flex()
                            .flex_col()
                            .gap(theme.spacing.medium)
                            .child(
                                AlertDialogTitle::new()
                                    .text_size(theme.typography.title_size)
                                    .font_weight(theme.typography.title_weight)
                                    .child("Remove selected books from library?"),
                            )
                            .child(
                                AlertDialogDescription::new()
                                    .flex()
                                    .flex_col()
                                    .gap(theme.spacing.small)
                                    .text_size(theme.typography.body_size)
                                    .child(format!(
                                        "Remove {selected_books} selected {} from Lectern?",
                                        pluralize_book(selected_books),
                                    ))
                                    .child(
                                        div()
                                            .text_color(theme.surface.muted_foreground)
                                            .child("Their EPUB and PDF files will remain on disk. This only removes Lectern’s metadata, cached covers, and file relationships."),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(theme.spacing.medium)
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(theme.spacing.small)
                                    .child(
                                        Button::new("cancel-bulk-removal", "Cancel").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.cancel_bulk_removal(cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(
                                            "confirm-bulk-removal",
                                            "Remove from library",
                                        )
                                        .variant(ButtonVariant::Danger)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.start_bulk_removal(window, cx);
                                        })),
                                    ),
                            ),
                    ),
            )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the function declares one cohesive appearance-dialog hierarchy"
    )]
    fn theme_dialog(&self, theme: &PrimerTheme, cx: &mut Context<Self>) -> AlertDialog {
        let close_entity = cx.entity().downgrade();
        let cancel_entity = close_entity.clone();
        let mode_buttons = ColorMode::ALL
            .into_iter()
            .map(|mode| {
                let label = if self.appearance.mode == mode {
                    format!("✓ {mode}")
                } else {
                    mode.to_string()
                };
                let button = Button::new(format!("appearance-mode-{}", mode.as_str()), label)
                    .size(ButtonSize::Small);
                let button = if self.appearance.mode == mode {
                    button.variant(ButtonVariant::Primary)
                } else {
                    button
                };
                button.on_click(cx.listener(move |this, _, _, cx| {
                    this.set_color_mode(mode, cx);
                }))
            })
            .collect::<Vec<_>>();
        let accent_choices = AccentColor::ALL
            .into_iter()
            .map(|accent| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(theme.spacing.small)
                    .child(
                        ColorSwatch::new(
                            format!("appearance-accent-{}", accent.as_str()),
                            accent.to_string(),
                            theme.accent_swatch(accent),
                        )
                        .selected(self.appearance.accent == accent)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_accent_color(accent, cx);
                        })),
                    )
                    .child(
                        div()
                            .text_size(theme.typography.body_size)
                            .text_color(theme.surface.muted_foreground)
                            .child(accent.to_string()),
                    )
            })
            .collect::<Vec<_>>();

        AlertDialog::new(cx)
            .open(true)
            .on_open_change(move |open, _, _, cx| {
                if !open {
                    _ = close_entity.update(cx, LecternView::close_theme_dialog);
                }
            })
            .on_cancel(move |_, _, cx| {
                _ = cancel_entity.update(cx, LecternView::close_theme_dialog);
                true
            })
            .backdrop(
                AlertDialogBackdrop::new()
                    .absolute()
                    .inset_0()
                    .bg(theme.dialog.backdrop),
            )
            .popup(
                AlertDialogPopup::new()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(THEME_DIALOG_WIDTH_PX))
                            .p(theme.spacing.extra_large)
                            .rounded(theme.dialog.radius)
                            .border(theme.border.thin)
                            .border_color(theme.border.muted)
                            .bg(theme.surface.background)
                            .flex()
                            .flex_col()
                            .gap(theme.spacing.large)
                            .child(
                                AlertDialogTitle::new()
                                    .text_size(theme.typography.title_size)
                                    .font_weight(theme.typography.title_weight)
                                    .child("Appearance"),
                            )
                            .child(
                                AlertDialogDescription::new()
                                    .text_size(theme.typography.body_size)
                                    .text_color(theme.surface.muted_foreground)
                                    .child("Choose Lectern’s main theme and accent color."),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(theme.spacing.small)
                                    .child(
                                        div()
                                            .font_weight(theme.typography.button_weight)
                                            .child("Theme"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(theme.spacing.small)
                                            .children(mode_buttons),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(theme.spacing.medium)
                                    .child(
                                        div()
                                            .font_weight(theme.typography.button_weight)
                                            .child("Accent color"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_start()
                                            .justify_between()
                                            .gap(theme.spacing.medium)
                                            .children(accent_choices),
                                    ),
                            )
                            .child(
                                div().mt(theme.spacing.small).flex().justify_end().child(
                                    Button::new("close-theme-dialog", "Done")
                                        .size(ButtonSize::Small)
                                        .variant(ButtonVariant::Primary)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.close_theme_dialog(cx);
                                        })),
                                ),
                            ),
                    ),
            )
    }

    fn loading_view(theme: &PrimerTheme) -> gpui::Div {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.surface.muted_foreground)
            .text_size(theme.typography.body_size)
            .child("Opening your library…")
    }

    fn empty_library_view(&self, theme: &PrimerTheme, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.top_bar(theme, false, true, cx))
            .child(
                div().flex_1().flex().items_center().justify_center().child(
                    div()
                        .w(px(EMPTY_LIBRARY_CONTENT_WIDTH_PX))
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_center()
                        .gap(theme.spacing.medium)
                        .child(
                            div()
                                .text_size(theme.typography.title_size)
                                .font_weight(theme.typography.title_weight)
                                .line_height(relative(theme.typography.title_line_height))
                                .child("Your library is empty"),
                        )
                        .child(
                            div()
                                .text_color(theme.surface.muted_foreground)
                                .text_size(theme.typography.body_size)
                                .font_weight(theme.typography.body_weight)
                                .line_height(relative(theme.typography.body_line_height))
                                .child("Add EPUB or PDF files to start building your library."),
                        )
                        .child(div().h(theme.spacing.small))
                        .child(self.add_books_button("empty-add-books", cx))
                        .when_some(self.status.clone(), |content, status| {
                            content.child(
                                div()
                                    .text_color(theme.surface.muted_foreground)
                                    .text_size(theme.typography.body_size)
                                    .child(status),
                            )
                        }),
                ),
            )
    }

    fn top_bar(
        &self,
        theme: &PrimerTheme,
        selection_active: bool,
        selection_locked: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex_none()
            .h(px(TOP_BAR_HEIGHT_PX))
            .flex()
            .items_center()
            .justify_between()
            .px(theme.spacing.large)
            .py(theme.spacing.small)
            .border_b(theme.border.thin)
            .border_color(theme.border.muted)
            .child(
                div()
                    .font_family(theme.typography.wordmark_family)
                    .text_size(theme.typography.title_size)
                    .font_weight(theme.typography.wordmark_weight)
                    .child("Lectern"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.small)
                    .child(
                        IconButton::new(
                            "open-theme-dialog",
                            "Choose theme and accent color",
                            TablerIcon::Palette,
                        )
                        .disabled(self.removing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_theme_dialog(cx);
                        })),
                    )
                    .child(
                        Button::new("begin-selection", "Select books")
                            .disabled(selection_active || selection_locked)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.begin_selection(cx);
                            })),
                    )
                    .child(self.add_books_button("library-add-books", cx)),
            )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one declarative render tree keeps the three fixed library regions auditable"
    )]
    fn library_view(&self, theme: &PrimerTheme, cx: &mut Context<Self>) -> gpui::Div {
        let book_count = format!(
            "{} {}",
            self.library_total,
            if self.library_total == 1 {
                "book"
            } else {
                "books"
            }
        );
        let selection_active = self.selection.is_active();
        let selection_locked = self.busy || self.removing || self.selection_pending.is_some();
        let detail_book = self
            .detail_editor
            .as_ref()
            .map(|editor| editor.original.id)
            .or(self.detail_loading);
        let cards = self
            .books
            .iter()
            .enumerate()
            .map(|(index, book)| {
                book_card(
                    book,
                    index,
                    self.selection.contains(book.summary.id)
                        || detail_book == Some(book.summary.id),
                    selection_active,
                    selection_locked,
                    theme,
                    cx,
                )
            })
            .collect::<Vec<_>>();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.top_bar(theme, selection_active, selection_locked, cx))
            .when(
                selection_active || self.selection_pending.is_some(),
                |content| content.child(self.selection_bar(theme, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("library-scroll")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .bg(theme.surface.muted_background)
                            .p(theme.spacing.extra_large)
                            .flex()
                            .flex_col()
                            .gap(theme.spacing.large)
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_start()
                                    .gap(theme.spacing.extra_large)
                                    .children(cards),
                            )
                            .when(
                                self.library_total
                                    > u64::try_from(self.books.len()).unwrap_or(u64::MAX),
                                |content| {
                                    content.child(
                                        div()
                                            .text_color(theme.surface.muted_foreground)
                                            .text_size(theme.typography.body_size)
                                            .child(format!(
                                                "Showing the first {} books.",
                                                self.books.len()
                                            )),
                                    )
                                },
                            ),
                    )
                    .when_some(self.detail_editor.as_ref(), |body, editor| {
                        body.child(book_detail_panel(editor, theme, cx))
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(BOTTOM_BAR_HEIGHT_PX))
                    .px(theme.spacing.extra_large)
                    .border_t(theme.border.thin)
                    .border_color(theme.border.muted)
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_color(theme.surface.muted_foreground)
                    .text_size(theme.typography.body_size)
                    .child(div().flex_none().child(book_count))
                    .when_some(self.status.clone(), |bar, status| {
                        bar.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ml(theme.spacing.medium)
                                .truncate()
                                .text_right()
                                .child(status),
                        )
                    }),
            )
    }
}

impl Render for LecternView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.initial_frame_scheduled {
            self.initial_frame_scheduled = true;
            cx.on_next_frame(window, Self::initial_frame_presented);
        }
        let theme = PrimerTheme::current(cx);
        let content = match self.library_state {
            LibraryState::Loading => Self::loading_view(&theme),
            LibraryState::Ready if self.books.is_empty() => self.empty_library_view(&theme, cx),
            LibraryState::Ready => self.library_view(&theme, cx),
        };
        let modal_open = self.removal_confirmation.is_some() || self.theme_dialog_open;

        div()
            .size_full()
            .bg(theme.surface.background)
            .font_family(theme.typography.body_family)
            .text_color(theme.surface.foreground)
            .flex()
            .flex_col()
            .key_context("LecternLibrary")
            .on_action(cx.listener(|this, _: &SelectAllBooks, window, cx| {
                this.select_all_matching(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearBookSelection, _, cx| {
                this.clear_selection_action(cx);
            }))
            .child(
                div()
                    .size_full()
                    .when(modal_open, |background| {
                        background.opacity(theme.dialog.background_content_opacity)
                    })
                    .child(content),
            )
            .when(self.removal_confirmation.is_some(), |root| {
                root.child(self.bulk_removal_dialog(&theme, cx))
            })
            .when(self.theme_dialog_open, |root| {
                root.child(self.theme_dialog(&theme, cx))
            })
    }
}

#[derive(Clone, Copy)]
enum LibraryState {
    Loading,
    Ready,
}

struct LoadedBook {
    summary: BookSummary,
    cover: Option<Vec<u8>>,
}

struct LibrarySnapshot {
    total: u64,
    books: Vec<LoadedBook>,
}

struct LibraryBook {
    summary: BookSummary,
    cover: Option<Arc<Image>>,
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
        self.anchor = Some(SelectionAnchor { index });
        if self.selected_count() == 0 {
            self.clear();
        }
    }

    fn begin_explicit(&mut self) {
        if self.mode.is_none() {
            self.mode = Some(GridSelectionMode::Explicit(HashSet::with_capacity(1)));
            self.anchor = None;
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
        } else {
            self.mode = Some(GridSelectionMode::AllMatching {
                query,
                generation: snapshot.generation,
                matching_books: snapshot.matching_books,
                excluded: HashSet::new(),
            });
            self.anchor = None;
        }
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

#[derive(Clone, Copy)]
enum PendingSelection {
    Range,
    AllMatching,
}

#[derive(Clone)]
struct BulkRemovalConfirmation {
    selection: BookSelection,
    selected_books: u64,
}

struct RemovalCompletion {
    result: BulkRemovalResult,
    snapshot: Result<LibrarySnapshot, LibraryServiceError>,
}

fn load_library_snapshot(path: &Path) -> Result<LibrarySnapshot, LibraryServiceError> {
    let mut service = SqliteLibraryService::open(path)?;
    load_library_snapshot_from(&mut service)
}

fn load_book(path: &Path, id: BookId) -> Result<Option<Book>, LibraryServiceError> {
    let mut service = SqliteLibraryService::open(path)?;
    service.get_book(id)
}

fn load_library_snapshot_from(
    service: &mut SqliteLibraryService,
) -> Result<LibrarySnapshot, LibraryServiceError> {
    let page = service.query_library_page(&LibraryQuery::default(), 0, LIBRARY_PAGE_SIZE)?;
    let mut books = Vec::with_capacity(page.books.len());
    for summary in page.books {
        let cover = if summary.has_cover {
            service.load_cover(summary.id)?
        } else {
            None
        };
        books.push(LoadedBook { summary, cover });
    }
    Ok(LibrarySnapshot {
        total: page.total,
        books,
    })
}

fn import_books_and_load_library(
    path: &Path,
    roots: &[PathBuf],
) -> Result<(ImportSummary, LibrarySnapshot), LibraryServiceError> {
    let mut service = SqliteLibraryService::open(path)?;
    let summary = service.import_publications(roots, &mut |_| {})?;
    let snapshot = load_library_snapshot_from(&mut service)?;
    Ok((summary, snapshot))
}

fn resolve_selection_snapshot(
    path: &Path,
    query: &LibraryQuery,
) -> Result<SelectionSnapshot, LibraryServiceError> {
    let mut service = SqliteLibraryService::open(path)?;
    service.selection_snapshot(query)
}

fn resolve_selection_range(
    path: &Path,
    query: &LibraryQuery,
    offset: u64,
    limit: u32,
) -> Result<Vec<BookId>, LibraryServiceError> {
    let mut service = SqliteLibraryService::open(path)?;
    service.query_library_ids_window(query, offset, limit)
}

fn remove_books_and_load_library(
    path: &Path,
    selection: &BookSelection,
) -> Result<RemovalCompletion, LibraryServiceError> {
    let mut service = SqliteLibraryService::open(path)?;
    let result = service.remove_books(selection)?;
    let snapshot = load_library_snapshot_from(&mut service);
    Ok(RemovalCompletion { result, snapshot })
}

fn save_book_and_load_library(
    path: &Path,
    edit: &lectern_core::organisation::BookEdit,
) -> Result<(Book, LibrarySnapshot), String> {
    let mut service = SqliteLibraryService::open(path).map_err(|error| error.to_string())?;
    service
        .update_metadata(edit)
        .map_err(|error| error.to_string())?;
    let book = service
        .get_book(edit.id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "The saved book is no longer in the library.".to_owned())?;
    let snapshot = load_library_snapshot_from(&mut service).map_err(|error| error.to_string())?;
    Ok((book, snapshot))
}

struct AssetAttachCompletion {
    book: Book,
    snapshot: LibrarySnapshot,
    message: String,
}

fn attach_assets_and_load_library(
    path: &Path,
    id: BookId,
    paths: &[PathBuf],
) -> Result<AssetAttachCompletion, String> {
    let mut service = SqliteLibraryService::open(path).map_err(|error| error.to_string())?;
    let mut attached = 0_usize;
    let mut failures = Vec::new();
    for asset_path in paths {
        let Some(format) = book_format_for_path(asset_path) else {
            failures.push(format!(
                "{} is not an EPUB or PDF",
                asset_path.to_string_lossy()
            ));
            continue;
        };
        match service.attach_asset(id, format, asset_path) {
            Ok(_) => attached += 1,
            Err(error) => failures.push(format!("{}: {error}", asset_path.to_string_lossy())),
        }
    }
    let book = service
        .get_book(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "The book is no longer in the library.".to_owned())?;
    let snapshot = load_library_snapshot_from(&mut service).map_err(|error| error.to_string())?;
    let message = if failures.is_empty() {
        format!(
            "Added {attached} book {}.",
            if attached == 1 { "asset" } else { "assets" }
        )
    } else {
        format!(
            "Added {attached} book {}; {} failed. {}",
            if attached == 1 { "asset" } else { "assets" },
            failures.len(),
            failures[0]
        )
    };
    Ok(AssetAttachCompletion {
        book,
        snapshot,
        message,
    })
}

fn detach_asset_and_load_library(
    path: &Path,
    id: BookId,
    asset: AssetId,
) -> Result<(Book, LibrarySnapshot), String> {
    let mut service = SqliteLibraryService::open(path).map_err(|error| error.to_string())?;
    let detached_book = service
        .detach_asset(asset)
        .map_err(|error| error.to_string())?;
    if detached_book != id {
        return Err("The selected asset belongs to a different book.".to_owned());
    }
    let book = service
        .get_book(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "The book is no longer in the library.".to_owned())?;
    let snapshot = load_library_snapshot_from(&mut service).map_err(|error| error.to_string())?;
    Ok((book, snapshot))
}

fn remove_book_and_load_library(
    path: &Path,
    id: BookId,
) -> Result<(bool, LibrarySnapshot), String> {
    let mut service = SqliteLibraryService::open(path).map_err(|error| error.to_string())?;
    let removed = service.remove_book(id).map_err(|error| error.to_string())?;
    let snapshot = load_library_snapshot_from(&mut service).map_err(|error| error.to_string())?;
    Ok((removed, snapshot))
}

fn book_format_for_path(path: &Path) -> Option<BookFormat> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("epub") {
        Some(BookFormat::Epub)
    } else if extension.eq_ignore_ascii_case("pdf") {
        Some(BookFormat::Pdf)
    } else {
        None
    }
}

fn import_status(summary: &ImportSummary) -> Option<SharedString> {
    if summary.discovered == 0 {
        return Some("No EPUB or PDF files were found.".into());
    }
    if summary.failed == 0 {
        return None;
    }
    let details = summary
        .failures
        .first()
        .map_or_else(String::new, |failure| format!(" {}", failure.message));
    Some(
        format!(
            "Added {} of {} book files.{details}",
            summary.imported, summary.discovered
        )
        .into(),
    )
}

const DETAIL_INFORMATION_ITEM: usize = 0;
const DETAIL_PUBLICATION_ITEM: usize = 1;
const DETAIL_FILES_ITEM: usize = 2;
const DETAIL_SERIES_ITEM: usize = 3;
const DETAIL_CONTRIBUTOR_START_ITEM: usize = 4;

const fn detail_contributor_footer_item(contributor_count: usize) -> usize {
    DETAIL_CONTRIBUTOR_START_ITEM + contributor_count
}

const fn detail_tags_item_index(contributor_count: usize) -> usize {
    detail_contributor_footer_item(contributor_count) + 1
}

const fn detail_library_item_index(contributor_count: usize) -> usize {
    detail_tags_item_index(contributor_count) + 1
}

const fn detail_item_count(contributor_count: usize) -> usize {
    detail_library_item_index(contributor_count) + 1
}

fn book_detail_panel(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    div()
        .id("book-detail-panel")
        .w(px(BOOK_DETAIL_PANEL_WIDTH_PX))
        .flex_none()
        .min_h_0()
        .border_l(theme.border.thin)
        .border_color(theme.border.muted)
        .bg(theme.surface.background)
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .h(px(TOP_BAR_HEIGHT_PX))
                .px(theme.spacing.large)
                .py(theme.spacing.small)
                .border_b(theme.border.thin)
                .border_color(theme.border.muted)
                .flex()
                .items_center()
                .justify_between()
                .gap(theme.spacing.small)
                .child(
                    div()
                        .flex_none()
                        .text_size(theme.typography.title_size)
                        .font_weight(theme.typography.title_weight)
                        .child("Book details"),
                )
                .child(
                    Button::new("close-book-detail", "Close")
                        .disabled(editor.operation != DetailOperation::Idle)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.close_book_detail(cx);
                        })),
                ),
        )
        .child(
            list(
                editor.list_state.clone(),
                cx.processor(LecternView::render_book_detail_item),
            )
            .flex_1()
            .min_h_0(),
        )
        .child(book_detail_action_bar(editor, theme, cx))
        .into_any_element()
}

fn book_detail_action_bar(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::Div {
    let editing_busy = editor.operation != DetailOperation::Idle;
    div()
        .flex_none()
        .h(px(TOP_BAR_HEIGHT_PX))
        .px(theme.spacing.large)
        .py(theme.spacing.small)
        .border_t(theme.border.thin)
        .border_color(theme.border.muted)
        .flex()
        .items_center()
        .justify_end()
        .gap(theme.spacing.small)
        .child(
            Button::new(
                "save-book-detail",
                if editor.operation == DetailOperation::Saving {
                    "Saving…"
                } else {
                    "Save"
                },
            )
            .size(ButtonSize::Small)
            .variant(ButtonVariant::Primary)
            .disabled(
                !editor.dirty
                    || editing_busy
                    || matches!(
                        editor.series_index_availability,
                        SeriesIndexAvailability::Checking | SeriesIndexAvailability::Conflict
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.save_book_detail(window, cx);
            })),
        )
        .child(
            Button::new("reset-book-detail", "Reset")
                .size(ButtonSize::Small)
                .disabled(!editor.dirty || editing_busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.reset_book_detail(window, cx);
                })),
        )
}

impl LecternView {
    fn ensure_book_detail_item_inputs(
        &mut self,
        item: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &self.detail_editor else {
            return;
        };
        let contributor_count = editor.contributors.len();
        let disabled = editor.operation != DetailOperation::Idle;

        if item == DETAIL_INFORMATION_ITEM && editor.description.is_none() {
            let description = editor.original.description.clone().unwrap_or_default();
            let description = metadata_textarea(window, cx, "Add a description", description);
            if disabled {
                description.update(cx, |state, cx| state.set_disabled(true, cx));
            }
            if let Some(editor) = &mut self.detail_editor {
                editor.description = Some(description);
            }
        } else if item == DETAIL_PUBLICATION_ITEM
            && (editor.publisher.is_none() || editor.publication_date.is_none())
        {
            let publisher = editor.original.publisher.clone().unwrap_or_default();
            let publication_date = editor
                .original
                .publication_date
                .map(|date| date.to_string())
                .unwrap_or_default();
            let publisher = metadata_input(window, cx, "Publisher", publisher);
            let publication_date =
                metadata_input(window, cx, "YYYY, YYYY-MM, or YYYY-MM-DD", publication_date);
            if disabled {
                publisher.update(cx, |state, cx| state.set_disabled(true, cx));
                publication_date.update(cx, |state, cx| state.set_disabled(true, cx));
            }
            if let Some(editor) = &mut self.detail_editor {
                editor.publisher = Some(publisher);
                editor.publication_date = Some(publication_date);
            }
        } else if item == DETAIL_SERIES_ITEM && editor.series_input.is_none() {
            let index = editor.curation.series.index.clone();
            let series_input = series_search_input(window, cx);
            let index = series_index_input(window, cx, index);
            if disabled {
                series_input.update(cx, |state, cx| state.set_disabled(true, cx));
            }
            let index_disabled = disabled || editor.curation.series.name.trim().is_empty();
            index.update(cx, |state, cx| state.set_disabled(index_disabled, cx));
            if let Some(editor) = &mut self.detail_editor {
                editor.series_input = Some(series_input);
                editor.series_index = Some(index);
            }
        } else if item == detail_tags_item_index(contributor_count) && editor.tag_input.is_none() {
            let input = tag_search_input(window, cx);
            if disabled {
                input.update(cx, |state, cx| state.set_disabled(true, cx));
            }
            if let Some(editor) = &mut self.detail_editor {
                editor.tag_input = Some(input);
            }
        }
    }

    fn render_book_detail_item(
        &mut self,
        item: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.ensure_book_detail_item_inputs(item, window, cx);
        let theme = PrimerTheme::current(cx);
        let Some(editor) = &self.detail_editor else {
            return div().into_any_element();
        };
        let contributor_count = editor.contributors.len();
        let contributor_footer = detail_contributor_footer_item(contributor_count);
        let tags = detail_tags_item_index(contributor_count);
        let library = detail_library_item_index(contributor_count);
        let editing_busy = editor.operation != DetailOperation::Idle;

        let content = if item == DETAIL_INFORMATION_ITEM {
            detail_information_item(editor, &theme, cx)
        } else if item == DETAIL_PUBLICATION_ITEM {
            detail_publication_item(editor, &theme)
        } else if item == DETAIL_FILES_ITEM {
            detail_files_item(editor, &theme, cx)
        } else if item == DETAIL_SERIES_ITEM {
            detail_series_item(editor, &theme, cx)
        } else if (DETAIL_CONTRIBUTOR_START_ITEM..contributor_footer).contains(&item) {
            let index = item - DETAIL_CONTRIBUTOR_START_ITEM;
            let row = contributor_field_row(
                &editor.contributors[index],
                ContributorRowPresentation {
                    index,
                    contributor_count,
                    editing_busy,
                    role_picker_open: editor.role_picker == Some(editor.contributors[index].row_id),
                },
                &theme,
                cx,
            );
            div()
                .when(index == 0, |item| {
                    item.child(detail_section_label("Contributors", &theme))
                })
                .child(row)
                .into_any_element()
        } else if item == contributor_footer {
            detail_contributor_footer(editor, &theme, cx)
        } else if item == tags {
            detail_tags_item(editor, &theme, cx)
        } else if item == library {
            detail_library_item(editor, &theme, cx)
        } else {
            div().into_any_element()
        };

        let starts_section = matches!(
            item,
            DETAIL_INFORMATION_ITEM
                | DETAIL_PUBLICATION_ITEM
                | DETAIL_FILES_ITEM
                | DETAIL_SERIES_ITEM
        ) || item == DETAIL_CONTRIBUTOR_START_ITEM
            || item == tags
            || item == library;
        let ends_section = matches!(
            item,
            DETAIL_INFORMATION_ITEM
                | DETAIL_PUBLICATION_ITEM
                | DETAIL_FILES_ITEM
                | DETAIL_SERIES_ITEM
        ) || item == contributor_footer
            || item == tags;

        div()
            .px(theme.spacing.large)
            .pt(if starts_section {
                theme.spacing.large
            } else {
                rems(0.)
            })
            .pb(if ends_section || item == library {
                theme.spacing.large
            } else {
                rems(0.)
            })
            .when(ends_section, |section| {
                section
                    .border_b(theme.border.thin)
                    .border_color(theme.border.muted)
            })
            .child(content)
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
struct LanguageOption {
    code: &'static str,
    name: &'static str,
}

fn language_options() -> &'static [LanguageOption] {
    static LANGUAGES: OnceLock<Vec<LanguageOption>> = OnceLock::new();
    LANGUAGES.get_or_init(|| {
        let mut languages = isolang::languages()
            .filter_map(|language| {
                Some(LanguageOption {
                    code: language.to_639_1()?,
                    name: language.to_name(),
                })
            })
            .collect::<Vec<_>>();
        languages.sort_unstable_by_key(|language| language.name);
        languages
    })
}

fn detail_language_menu(
    editor: &BookDetailEditor,
    _theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let language_label = if editor.language.is_empty() {
        "Select language".to_owned()
    } else if let Some(language) = language_options()
        .iter()
        .find(|language| language.code == editor.language)
    {
        format!("{} — {}", language.name, language.code)
    } else {
        editor.language.clone()
    };
    let current = editor.language.clone();
    let options = std::iter::once(
        ActionListItem::new("language-unspecified", "Not specified")
            .selected(current.is_empty())
            .on_click(cx.listener(|this, _, _, cx| {
                this.set_detail_language("", cx);
            })),
    )
    .chain(language_options().iter().map(|language| {
        let code = language.code;
        ActionListItem::new(
            format!("language-{code}"),
            format!("{} — {code}", language.name),
        )
        .selected(current == code)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_detail_language(code, cx);
        }))
    }));
    ActionMenu::new(
        "detail-language-menu",
        Button::new("detail-language-trigger", format!("{language_label} ▾"))
            .full_width()
            .disabled(editor.operation != DetailOperation::Idle),
        div().flex().flex_col().children(options),
    )
    .width(rems(17.))
    .open(editor.language_menu_open)
    .on_open_change(cx.listener(|this, open, _, cx| {
        this.set_language_menu_open(*open, cx);
    }))
    .into_any_element()
}

fn tag_color(theme: &PrimerTheme, color: TagColor) -> gpui::Hsla {
    match color {
        TagColor::Slate => theme.tag_palette.slate,
        TagColor::Coral => theme.tag_palette.coral,
        TagColor::Amber => theme.tag_palette.amber,
        TagColor::Mint => theme.tag_palette.mint,
        TagColor::Azure => theme.tag_palette.azure,
        TagColor::Lilac => theme.tag_palette.lilac,
    }
}

fn metadata_error_section(error: &str) -> DetailErrorSection {
    let error = error.to_ascii_lowercase();
    if error.contains("contributor") {
        DetailErrorSection::Contributors
    } else if error.contains("series") || error.contains("book number") {
        DetailErrorSection::Series
    } else if error.contains("tag") {
        DetailErrorSection::Tags
    } else if error.contains("publication date") {
        DetailErrorSection::Publication
    } else {
        DetailErrorSection::Information
    }
}

fn detail_error(editor: &BookDetailEditor, section: DetailErrorSection) -> Option<SharedString> {
    (editor.error_section == section)
        .then(|| editor.error.clone())
        .flatten()
}

fn detail_error_text(error: SharedString, theme: &PrimerTheme) -> gpui::Div {
    div()
        .text_color(theme.button.danger.foreground)
        .child(error)
}

#[allow(
    clippy::too_many_lines,
    reason = "the function declares one cohesive metadata-section hierarchy"
)]
fn detail_information_item(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.large)
        .child(detail_field_container("Title", theme).child(TextInput::new(
            "detail-title",
            "Book title",
            &editor.title,
        )))
        .child(
            detail_field_container("Language", theme)
                .child(detail_language_menu(editor, theme, cx)),
        )
        .child(
            detail_field_container("Description", theme).child(
                TextArea::new(
                    "detail-description",
                    "Book description",
                    editor
                        .description
                        .as_ref()
                        .expect("rendered description is initialized"),
                )
                .height(rems(6.)),
            ),
        )
        .child(detail_rating_field(editor, theme, cx))
        .when_some(
            detail_error(editor, DetailErrorSection::Information),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .into_any_element()
}

fn detail_publication_item(editor: &BookDetailEditor, theme: &PrimerTheme) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.large)
        .child(detail_section_heading("Publication", theme))
        .child(
            div()
                .flex()
                .gap(theme.spacing.medium)
                .child(
                    detail_field_container("Publisher", theme)
                        .flex_1()
                        .min_w_0()
                        .child(TextInput::new(
                            "detail-publisher",
                            "Publisher",
                            editor
                                .publisher
                                .as_ref()
                                .expect("rendered publisher is initialized"),
                        )),
                )
                .child(
                    detail_field_container("Publication date", theme)
                        .flex_1()
                        .min_w_0()
                        .child(TextInput::new(
                            "detail-publication-date",
                            "YYYY, YYYY-MM, or YYYY-MM-DD",
                            editor
                                .publication_date
                                .as_ref()
                                .expect("rendered publication date is initialized"),
                        )),
                ),
        )
        .when_some(
            detail_error(editor, DetailErrorSection::Publication),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .into_any_element()
}

fn detail_rating_field(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::Div {
    let rating_label = if editor.rating == BookRating::default() {
        "Unrated".to_owned()
    } else {
        format!("{} of 5", editor.rating)
    };
    detail_field_container("Rating", theme).child(
        div()
            .flex()
            .items_center()
            .gap(theme.spacing.medium)
            .child(
                StarRating::new("detail-rating", editor.rating.half_stars())
                    .disabled(editor.operation != DetailOperation::Idle)
                    .on_change(cx.listener(|this, half_stars, _, cx| {
                        this.set_detail_rating(*half_stars, cx);
                    })),
            )
            .child(
                div()
                    .text_color(theme.surface.muted_foreground)
                    .child(rating_label),
            )
            .when(editor.rating != BookRating::default(), |row| {
                row.child(
                    Button::new("clear-detail-rating", "Clear")
                        .size(ButtonSize::Small)
                        .disabled(editor.operation != DetailOperation::Idle)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_detail_rating(0, cx);
                        })),
                )
            }),
    )
}

fn detail_contributor_footer(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    div()
        .pt(theme.spacing.small)
        .flex()
        .flex_col()
        .gap(theme.spacing.small)
        .when(editor.contributors.is_empty(), |item| {
            item.child(detail_section_label("Contributors", theme))
                .child(
                    div()
                        .text_color(theme.surface.muted_foreground)
                        .child("No contributors assigned."),
                )
        })
        .when_some(
            detail_error(editor, DetailErrorSection::Contributors),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .child(
            Button::new("add-book-contributor", "Add contributor")
                .size(ButtonSize::Small)
                .disabled(editor.operation != DetailOperation::Idle)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.add_detail_contributor(window, cx);
                })),
        )
        .into_any_element()
}

#[allow(
    clippy::too_many_lines,
    reason = "the function declares one cohesive searchable-series hierarchy"
)]
fn detail_series_item(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let editing_busy = editor.operation != DetailOperation::Idle;
    let query = editor
        .series_input
        .as_ref()
        .expect("rendered series input is initialized")
        .read(cx)
        .value()
        .to_string();
    let selected_key = (!editor.curation.series.name.trim().is_empty())
        .then(|| identity_key(&editor.curation.series.name));
    let selected_series = selected_key.as_ref().map(|_| {
        EntityChip::new(
            "detail-selected-series",
            editor.curation.series.name.clone(),
        )
        .disabled(editing_busy)
        .on_remove(cx.listener(|this, _, window, cx| {
            this.remove_detail_series(window, cx);
        }))
    });
    let selected_item = selected_key.as_ref().map(|_| {
        let name = editor.curation.series.name.clone();
        if let Some(id) = editor.curation.series.existing_id {
            let series = Series {
                id,
                name: name.clone(),
            };
            ActionListItem::new("selected-detail-series", name)
                .selected(true)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_existing_detail_series(&series, window, cx);
                }))
        } else {
            let action_name = name.clone();
            ActionListItem::new("selected-detail-series", name)
                .selected(true)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.create_detail_series(action_name.clone(), window, cx);
                }))
        }
    });
    let suggestions = editor
        .series_suggestions
        .iter()
        .filter(|usage| {
            editor.curation.series.existing_id != Some(usage.series.id)
                && selected_key
                    .as_ref()
                    .is_none_or(|key| identity_key(&usage.series.name) != *key)
        })
        .map(|usage| {
            let series = usage.series.clone();
            ActionListItem::new(
                format!("series-suggestion-{}", series.id.value()),
                series.name.clone(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_existing_detail_series(&series, window, cx);
            }))
        })
        .collect::<Vec<_>>();
    let has_suggestions = selected_item.is_some() || !suggestions.is_empty();
    let normalized_query = normalize_name(NameKind::Series, &query).ok();
    let exact_match = normalized_query.as_ref().is_some_and(|query| {
        let key = identity_key(query);
        selected_key.as_ref() == Some(&key)
            || editor
                .series_suggestions
                .iter()
                .any(|usage| identity_key(&usage.series.name) == key)
    });
    let create_name = normalized_query.filter(|_| !exact_match);
    let menu_content = div()
        .flex()
        .flex_col()
        .gap(theme.spacing.small)
        .child(TextInput::new(
            "detail-series-input",
            "Add or find a series",
            editor
                .series_input
                .as_ref()
                .expect("rendered series input is initialized"),
        ))
        .when(editor.series_suggestions_loading, |content| {
            content.child(
                div()
                    .px(theme.spacing.medium)
                    .py(theme.spacing.small)
                    .text_color(theme.surface.muted_foreground)
                    .child("Finding series…"),
            )
        })
        .when(
            !editor.series_suggestions_loading && !has_suggestions,
            |content| {
                content.child(
                    div()
                        .px(theme.spacing.medium)
                        .py(theme.spacing.small)
                        .text_color(theme.surface.muted_foreground)
                        .child(if query.trim().is_empty() {
                            "No series yet. Start typing to create one."
                        } else {
                            "No existing series found."
                        }),
                )
            },
        )
        .when_some(selected_item, ParentElement::child)
        .when(!editor.series_suggestions_loading, |content| {
            content.children(suggestions)
        })
        .when_some(create_name, |content, name| {
            let action_name = name.clone();
            content.child(
                ActionListItem::new(
                    "create-detail-series",
                    format!("+  Create new series: “{name}”"),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.create_detail_series(action_name.clone(), window, cx);
                })),
            )
        });

    detail_section_container("Series", theme)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(theme.spacing.small)
                .items_center()
                .when_some(selected_series, ParentElement::child)
                .child(
                    ActionMenu::new(
                        "detail-series-menu",
                        Button::new("open-detail-series-menu", "+ Series")
                            .size(ButtonSize::Small)
                            .disabled(editing_busy),
                        menu_content,
                    )
                    .width(rems(21.))
                    .open(editor.series_menu_open)
                    .on_open_change(cx.listener(|this, open, _, cx| {
                        this.set_series_menu_open(*open, cx);
                    })),
                ),
        )
        .child(
            detail_field_container("Book number", theme).child(TextInput::new(
                "detail-series-index",
                "Book number within series",
                editor
                    .series_index
                    .as_ref()
                    .expect("rendered series index is initialized"),
            )),
        )
        .when(
            editor.series_index_availability == SeriesIndexAvailability::Checking,
            |content| {
                content.child(
                    div()
                        .text_color(theme.surface.muted_foreground)
                        .child("Checking whether this number is available…"),
                )
            },
        )
        .when(
            editor.series_index_availability == SeriesIndexAvailability::Conflict,
            |content| {
                content.child(
                    div()
                        .text_color(theme.button.danger.foreground)
                        .child("That book number is already used in this series."),
                )
            },
        )
        .when_some(
            detail_error(editor, DetailErrorSection::Series),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .into_any_element()
}

#[allow(
    clippy::too_many_lines,
    reason = "the function declares one cohesive searchable-tag hierarchy"
)]
fn detail_tags_item(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let editing_busy = editor.operation != DetailOperation::Idle;
    let tags = editor.curation.tags.iter().enumerate().map(|(index, tag)| {
        TagChip::new(
            format!("detail-tag-{index}"),
            tag.name.clone(),
            tag_color(theme, tag.color),
        )
        .disabled(editing_busy)
        .on_remove(cx.listener(move |this, _, _, cx| {
            this.remove_detail_tag(index, cx);
        }))
    });
    let menu_content = if let Some(name) = &editor.tag_creation_name {
        let colors = TagColor::ALL.into_iter().map(|color| {
            ActionListItem::new(
                format!("create-tag-color-{}", color.as_str()),
                color.to_string(),
            )
            .leading_color(tag_color(theme, color))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.create_detail_tag(color, window, cx);
            }))
        });
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.small)
            .child(
                div()
                    .px(theme.spacing.medium)
                    .py(theme.spacing.small)
                    .pb(theme.spacing.medium)
                    .border_b(theme.border.thin)
                    .border_color(theme.border.muted)
                    .text_color(theme.surface.muted_foreground)
                    .child(format!("Create “{name}”")),
            )
            .child(
                div()
                    .px(theme.spacing.medium)
                    .font_weight(theme.typography.button_weight)
                    .child("Pick a color"),
            )
            .children(colors)
            .into_any_element()
    } else {
        let query = editor
            .tag_input
            .as_ref()
            .expect("rendered tag input is initialized")
            .read(cx)
            .value()
            .to_string();
        let selected_ids = editor
            .curation
            .existing_tag_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let selected_keys = editor
            .curation
            .tags
            .iter()
            .map(|tag| identity_key(&tag.name))
            .collect::<HashSet<_>>();
        let selected_tags = editor
            .curation
            .tags
            .iter()
            .enumerate()
            .map(|(index, draft)| {
                let item = ActionListItem::new(format!("selected-tag-{index}"), draft.name.clone())
                    .leading_color(tag_color(theme, draft.color))
                    .selected(true);
                if let Some(id) = draft.existing_id {
                    let tag = Tag {
                        id,
                        name: draft.name.clone(),
                        color: draft.color,
                    };
                    item.on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_existing_detail_tag(&tag, cx);
                    }))
                } else {
                    item.on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_detail_tag(index, cx);
                    }))
                }
            })
            .collect::<Vec<_>>();
        let suggestions = editor
            .tag_suggestions
            .iter()
            .filter(|usage| !selected_ids.contains(&usage.tag.id))
            .filter(|usage| !selected_keys.contains(&identity_key(&usage.tag.name)))
            .map(|usage| {
                let tag = usage.tag.clone();
                ActionListItem::new(
                    format!("tag-suggestion-{}", tag.id.value()),
                    tag.name.clone(),
                )
                .leading_color(tag_color(theme, tag.color))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_existing_detail_tag(&tag, cx);
                }))
            })
            .collect::<Vec<_>>();
        let has_suggestions = !selected_tags.is_empty() || !suggestions.is_empty();
        let normalized_query = normalize_name(NameKind::Tag, &query).ok();
        let exact_match = normalized_query.as_ref().is_some_and(|query| {
            let key = identity_key(query);
            editor
                .curation
                .tags
                .iter()
                .any(|tag| identity_key(&tag.name) == key)
                || editor
                    .tag_suggestions
                    .iter()
                    .any(|usage| identity_key(&usage.tag.name) == key)
        });
        let create_name = normalized_query.filter(|_| !exact_match);
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.small)
            .child(TextInput::new(
                "detail-tag-input",
                "Add or find a tag",
                editor
                    .tag_input
                    .as_ref()
                    .expect("rendered tag input is initialized"),
            ))
            .when(editor.tag_suggestions_loading, |content| {
                content.child(
                    div()
                        .px(theme.spacing.medium)
                        .py(theme.spacing.small)
                        .text_color(theme.surface.muted_foreground)
                        .child("Finding tags…"),
                )
            })
            .when(
                !editor.tag_suggestions_loading && !has_suggestions,
                |content| {
                    content.child(
                        div()
                            .px(theme.spacing.medium)
                            .py(theme.spacing.small)
                            .text_color(theme.surface.muted_foreground)
                            .child(if query.trim().is_empty() {
                                "Start typing to create a tag."
                            } else {
                                "No existing tags found."
                            }),
                    )
                },
            )
            .when(!editor.tag_suggestions_loading, |content| {
                content.children(selected_tags).children(suggestions)
            })
            .when_some(create_name, |content, name| {
                let action_name = name.clone();
                content.child(
                    ActionListItem::new(
                        "create-detail-tag",
                        format!("+  Create new tag: “{name}”"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_detail_tag_creation(&action_name, cx);
                    })),
                )
            })
            .into_any_element()
    };
    detail_section_container("Tags", theme)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(theme.spacing.small)
                .items_center()
                .children(tags)
                .child(
                    ActionMenu::new(
                        "detail-tag-menu",
                        Button::new("open-detail-tag-menu", "+ Tag")
                            .size(ButtonSize::Small)
                            .disabled(editing_busy),
                        menu_content,
                    )
                    .width(rems(21.))
                    .open(editor.tag_menu_open)
                    .on_open_change(cx.listener(|this, open, _, cx| {
                        this.set_tag_menu_open(*open, cx);
                    })),
                ),
        )
        .when_some(
            detail_error(editor, DetailErrorSection::Tags),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .into_any_element()
}

fn detail_files_item(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let can_detach = editor.original.assets.len() > 1
        && !editor.dirty
        && editor.operation == DetailOperation::Idle;
    let rows = editor
        .original
        .assets
        .iter()
        .map(|asset| asset_row(asset, can_detach, theme, cx));
    detail_section_container("Files", theme)
        .children(rows)
        .child(
            Button::new(
                "add-book-asset",
                if editor.operation == DetailOperation::Assets {
                    "Updating assets…"
                } else {
                    "Add EPUB or PDF"
                },
            )
            .size(ButtonSize::Small)
            .disabled(editor.operation != DetailOperation::Idle || editor.dirty)
            .on_click(cx.listener(|this, _, window, cx| {
                this.prompt_for_detail_assets(window, cx);
            })),
        )
        .when(editor.dirty, |section| {
            section.child(
                div()
                    .text_color(theme.surface.muted_foreground)
                    .child("Save or reset metadata before changing files."),
            )
        })
        .when_some(
            detail_error(editor, DetailErrorSection::Files),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .into_any_element()
}

fn detail_library_item(
    editor: &BookDetailEditor,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    detail_section_container("Library", theme)
        .child(
            div()
                .text_color(theme.surface.muted_foreground)
                .child("Remove this entry and its cached cover. Book files stay on disk."),
        )
        .when(!editor.remove_confirmation, |section| {
            section.child(
                Button::new("remove-detail-book", "Remove from library")
                    .size(ButtonSize::Small)
                    .variant(ButtonVariant::Danger)
                    .disabled(editor.operation != DetailOperation::Idle)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.request_detail_removal(cx);
                    })),
            )
        })
        .when(editor.remove_confirmation, |section| {
            section
                .child(
                    div()
                        .font_weight(theme.typography.button_weight)
                        .child("Remove this book from the library?"),
                )
                .child(
                    div()
                        .flex()
                        .gap(theme.spacing.small)
                        .child(
                            Button::new("cancel-detail-removal", "Cancel")
                                .size(ButtonSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_detail_removal(cx);
                                })),
                        )
                        .child(
                            Button::new("confirm-detail-removal", "Remove book")
                                .size(ButtonSize::Small)
                                .variant(ButtonVariant::Danger)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_detail_removal(window, cx);
                                })),
                        ),
                )
        })
        .when_some(
            detail_error(editor, DetailErrorSection::Library),
            |content, error| content.child(detail_error_text(error, theme)),
        )
        .into_any_element()
}

fn detail_section_label(label: &'static str, theme: &PrimerTheme) -> gpui::Div {
    div()
        .mb(theme.spacing.small)
        .child(detail_section_heading(label, theme))
}

fn detail_section_heading(label: &'static str, theme: &PrimerTheme) -> gpui::Div {
    div()
        .font_weight(theme.typography.button_weight)
        .child(label)
}

#[derive(Clone, Copy)]
struct ContributorRowPresentation {
    index: usize,
    contributor_count: usize,
    editing_busy: bool,
    role_picker_open: bool,
}

fn contributor_role_picker(
    field: &ContributorField,
    editing_busy: bool,
    open: bool,
    _theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let row_id = field.row_id;
    let role_options = ContributorRole::ALL.into_iter().map(|role| {
        ActionListItem::new(
            format!("contributor-{row_id}-role-{}", role.as_str()),
            role.to_string(),
        )
        .selected(role == field.role)
        .disabled(editing_busy)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_contributor_role(row_id, role, cx);
        }))
    });
    ActionMenu::new(
        format!("contributor-{row_id}-role-menu"),
        Button::new(
            format!("contributor-{row_id}-role-picker"),
            format!("{} ▾", field.role),
        )
        .size(ButtonSize::Medium)
        .width(rems(10.))
        .truncate_label()
        .disabled(editing_busy),
        div().flex().flex_col().children(role_options),
    )
    .width(rems(10.))
    .open(open)
    .on_open_change(cx.listener(move |this, open, _, cx| {
        this.set_contributor_role_picker_open(row_id, *open, cx);
    }))
    .into_any_element()
}

fn contributor_field_row(
    field: &ContributorField,
    presentation: ContributorRowPresentation,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let row_id = field.row_id;
    let index = presentation.index;
    let editing_busy = presentation.editing_busy;
    div()
        .py(theme.spacing.small)
        .border_b(theme.border.thin)
        .border_color(theme.border.muted)
        .flex()
        .flex_col()
        .gap(theme.spacing.small)
        .child(
            div()
                .flex()
                .gap(theme.spacing.small)
                .child(div().flex_1().min_w_0().child(TextInput::new(
                    format!("contributor-{row_id}-name"),
                    format!("Contributor {} name", index + 1),
                    &field.name,
                )))
                .child(contributor_role_picker(
                    field,
                    editing_busy,
                    presentation.role_picker_open,
                    theme,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(theme.spacing.small)
                .child(
                    IconButton::new(
                        format!("contributor-{row_id}-up"),
                        "Move contributor up",
                        TablerIcon::ChevronUp,
                    )
                    .size(ButtonSize::Small)
                    .disabled(editing_busy || index == 0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_detail_contributor(row_id, -1, cx);
                    })),
                )
                .child(
                    IconButton::new(
                        format!("contributor-{row_id}-down"),
                        "Move contributor down",
                        TablerIcon::ChevronDown,
                    )
                    .size(ButtonSize::Small)
                    .disabled(editing_busy || index + 1 >= presentation.contributor_count)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_detail_contributor(row_id, 1, cx);
                    })),
                )
                .child(
                    Button::new(format!("contributor-{row_id}-remove"), "Remove")
                        .size(ButtonSize::Small)
                        .width(rems(10.))
                        .disabled(editing_busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_detail_contributor(row_id, cx);
                        })),
                ),
        )
        .into_any_element()
}

fn detail_section_container(label: &'static str, theme: &PrimerTheme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.medium)
        .child(detail_section_heading(label, theme))
}

fn detail_field_container(label: &'static str, theme: &PrimerTheme) -> gpui::Div {
    div().flex().flex_col().gap(theme.spacing.small).child(
        div()
            .text_color(theme.surface.muted_foreground)
            .child(label),
    )
}

fn asset_row(
    asset: &BookAsset,
    can_detach: bool,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let asset_id = asset.id;
    let path = asset.path.clone();
    div()
        .py(theme.spacing.small)
        .border_b(theme.border.thin)
        .border_color(theme.border.muted)
        .flex()
        .flex_col()
        .gap(theme.spacing.small)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(format!(
                    "{} · {} · {}",
                    asset.format, asset.storage, asset.health
                ))
                .child(
                    div()
                        .flex()
                        .gap(theme.spacing.small)
                        .child(
                            IconButton::new(
                                format!("reveal-asset-{}", asset.id.value()),
                                "Show file in folder",
                                TablerIcon::Eye,
                            )
                            .size(ButtonSize::Small)
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        )
                        .child(
                            Button::new(format!("detach-asset-{}", asset.id.value()), "Remove")
                                .size(ButtonSize::Small)
                                .disabled(!can_detach)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.detach_detail_asset(asset_id, window, cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

fn book_card(
    book: &LibraryBook,
    index: usize,
    selected: bool,
    selection_active: bool,
    selection_locked: bool,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> gpui::AnyElement {
    let cover = if let Some(cover) = &book.cover {
        img(Arc::clone(cover))
            .w_full()
            .h(px(BOOK_COVER_HEIGHT_PX))
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        div()
            .w_full()
            .h(px(BOOK_COVER_HEIGHT_PX))
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.surface.background)
            .text_color(theme.surface.muted_foreground)
            .text_size(theme.typography.body_size)
            .child("No cover")
            .into_any_element()
    };
    let authors = if book.summary.authors.trim().is_empty() {
        "Unknown author".to_owned()
    } else {
        book.summary.authors.clone()
    };

    let book_id = book.summary.id;
    let card = div()
        .id(format!("book-card-{}", book_id.value()))
        .w(px(BOOK_CARD_WIDTH_PX))
        .flex_none()
        .p(theme.spacing.small)
        .rounded(theme.button.radius)
        .flex()
        .flex_col()
        .text_center()
        .font_weight(theme.typography.body_weight)
        .line_height(relative(theme.typography.book_metadata_line_height))
        .relative()
        .when(selected, |card| {
            card.bg(theme.selection.background)
                .border(theme.border.thin)
                .border_color(theme.selection.border)
        })
        .when(selection_active && !selection_locked, |card| {
            card.cursor_pointer()
        })
        .on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.handle_book_click(book_id, index, event.modifiers(), window, cx);
            }),
        )
        .child(cover)
        .child(
            div()
                .w_full()
                .mt(theme.spacing.small)
                .truncate()
                .text_size(theme.typography.body_size)
                .font_weight(theme.typography.wordmark_weight)
                .child(book.summary.title.clone()),
        )
        .child(
            div()
                .w_full()
                .truncate()
                .text_color(theme.surface.muted_foreground)
                .text_size(theme.typography.body_size)
                .child(authors),
        );

    card.when(selection_active, |card| {
        card.child(book_selection_checkbox(
            book,
            index,
            selected,
            selection_locked,
            theme,
            cx,
        ))
    })
    .into_any_element()
}

fn book_selection_checkbox(
    book: &LibraryBook,
    index: usize,
    selected: bool,
    selection_locked: bool,
    theme: &PrimerTheme,
    cx: &mut Context<LecternView>,
) -> Checkbox {
    let book_id = book.summary.id;
    let checkbox_entity = cx.entity().downgrade();
    Checkbox::new(format!("book-checkbox-{}", book_id.value()))
        .checked(selected)
        .disabled(selection_locked)
        .accessibility_label(format!("Select {}", book.summary.title))
        .absolute()
        .top(theme.spacing.medium)
        .right(theme.spacing.medium)
        .size(theme.spacing.large)
        .rounded(theme.button.radius)
        .border(theme.border.thin)
        .border_color(theme.selection.border)
        .bg(if selected {
            theme.selection.check_background
        } else {
            theme.surface.background
        })
        .text_color(theme.selection.check_foreground)
        .flex()
        .items_center()
        .justify_center()
        .focus_visible(move |style| {
            style
                .border(theme.focus.width)
                .border_color(theme.focus.color)
        })
        .on_change(move |_: CheckboxState, event, window, cx| {
            cx.stop_propagation();
            _ = checkbox_entity.update(cx, |this, cx| {
                this.handle_book_click(book_id, index, event.modifiers(), window, cx);
            });
        })
        .when(selected, |checkbox| checkbox.child("✓"))
}

fn selection_status(selection: &GridSelection) -> String {
    let selected = selection.selected_count();
    if selection.is_every_matching() {
        format!("All {selected} matching selected")
    } else if selection.is_active() && selected > 0 {
        format!("{selected} selected")
    } else {
        "Select books for bulk actions".to_owned()
    }
}

fn pluralize_book(count: u64) -> &'static str {
    if count == 1 { "book" } else { "books" }
}

fn benchmark_library_books() -> Vec<LibraryBook> {
    (0..LIBRARY_PAGE_SIZE)
        .map(|index| LibraryBook {
            summary: BookSummary {
                id: BookId::new(i64::from(index) + 1),
                title: format!("Benchmark book {:03}", index + 1),
                authors: format!("Benchmark author {:03}", index % 32 + 1),
                series: None,
                series_index: None,
                has_cover: false,
                has_file_issue: false,
            },
            cover: None,
        })
        .collect()
}

fn benchmark_book_detail() -> Book {
    Book {
        id: BookId::new(1),
        title: "Benchmark book 001".to_owned(),
        authors: "Ada Author".to_owned(),
        series: Some("The Measured Shelf".to_owned()),
        contributors: vec![
            ContributorCredit {
                contributor: Contributor {
                    id: ContributorId::new(1),
                    display_name: "Ada Author".to_owned(),
                    sort_name: "Author, Ada".to_owned(),
                },
                role: ContributorRole::Author,
                position: 0,
            },
            ContributorCredit {
                contributor: Contributor {
                    id: ContributorId::new(2),
                    display_name: "Terry Translator".to_owned(),
                    sort_name: "Translator, Terry".to_owned(),
                },
                role: ContributorRole::Translator,
                position: 0,
            },
            ContributorCredit {
                contributor: Contributor {
                    id: ContributorId::new(3),
                    display_name: "Iris Illustrator".to_owned(),
                    sort_name: "Illustrator, Iris".to_owned(),
                },
                role: ContributorRole::Illustrator,
                position: 0,
            },
        ],
        series_membership: Some(SeriesMembership {
            series: Series {
                id: SeriesId::new(1),
                name: "The Measured Shelf".to_owned(),
            },
            index: Some(SeriesIndex::from_scaled(1_500_000).expect("valid benchmark index")),
        }),
        tags: vec![
            Tag {
                id: TagId::new(1),
                name: "Performance".to_owned(),
                color: TagColor::Lilac,
            },
            Tag {
                id: TagId::new(2),
                name: "Reference".to_owned(),
                color: TagColor::Azure,
            },
        ],
        publisher: Some("Lectern Press".to_owned()),
        publication_date: Some("2026-08-27".parse().expect("valid benchmark date")),
        language: Some("en".to_owned()),
        description: Some(
            "A deterministic book used to verify complete detail-panel presentation.".to_owned(),
        ),
        rating: BookRating::from_half_stars(7).expect("valid benchmark rating"),
        assets: vec![
            BookAsset {
                id: AssetId::new(1),
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
                health: AssetHealth::Available,
                path: PathBuf::from("/benchmark/Benchmark book 001.epub"),
            },
            BookAsset {
                id: AssetId::new(2),
                format: BookFormat::Pdf,
                storage: AssetStorage::Reference,
                health: AssetHealth::Available,
                path: PathBuf::from("/benchmark/Benchmark book 001.pdf"),
            },
        ],
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn explicit_selection_toggles_and_builds_a_canonical_descriptor() {
        let first = BookId::new(4);
        let second = BookId::new(2);
        let mut selection = GridSelection::default();

        selection.begin_explicit();
        selection.toggle(first, 0);
        selection.toggle(second, 1);

        assert_eq!(selection.selected_count(), 2);
        assert_eq!(selection_status(&selection), "2 selected");
        assert_eq!(
            selection.descriptor(),
            Some(BookSelection::explicit(vec![second, first]))
        );

        selection.toggle(first, 0);
        assert_eq!(selection.selected_count(), 1);
        assert!(!selection.contains(first));
    }

    #[test]
    fn all_matching_selection_tracks_exclusions_without_materializing_books() {
        let excluded = BookId::new(9);
        let query = LibraryQuery::default();
        let generation = LibraryGeneration {
            connection_changes: 3,
            data_version: 5,
        };
        let mut selection = GridSelection::default();
        selection.install_all_matching(
            query.clone(),
            SelectionSnapshot {
                matching_books: 50_000,
                generation,
            },
        );

        assert!(selection.is_every_matching());
        assert_eq!(selection_status(&selection), "All 50000 matching selected");

        selection.toggle(excluded, 8);
        assert_eq!(selection.selected_count(), 49_999);
        assert_eq!(
            selection.descriptor(),
            Some(BookSelection::all_matching(
                query,
                generation,
                vec![excluded]
            ))
        );
    }

    #[test]
    fn asset_format_detection_is_case_insensitive_and_closed() {
        assert_eq!(
            book_format_for_path(Path::new("/library/book.EPUB")),
            Some(BookFormat::Epub)
        );
        assert_eq!(
            book_format_for_path(Path::new("/library/book.Pdf")),
            Some(BookFormat::Pdf)
        );
        assert_eq!(book_format_for_path(Path::new("/library/book.mobi")), None);
        assert_eq!(book_format_for_path(Path::new("/library/book")), None);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchmarkWorkload {
    EmptyLibraryAddBooks,
    LibrarySelection,
    BookDetail,
}

struct BenchmarkRun {
    output: PathBuf,
    workload: BenchmarkWorkload,
    main_entry: Instant,
    initial_render: Option<Duration>,
    action_started: Option<Instant>,
    selection_painted: Option<Duration>,
    detail_painted: Option<Duration>,
    confirmation_started: Option<Instant>,
}

impl BenchmarkRun {
    fn finish(self) {
        assert_eq!(
            self.workload,
            BenchmarkWorkload::EmptyLibraryAddBooks,
            "Add-books completion belongs to the empty-library benchmark"
        );
        let sample = UiBenchmarkSample {
            schema_version: 1,
            workload: "empty-library-add-books",
            initial_render_ms: millis(self.initial_render.expect("initial frame was measured")),
            click_to_busy_paint_ms: millis(
                self.action_started
                    .expect("Add books action was started")
                    .elapsed(),
            ),
            peak_rss_bytes: peak_rss_bytes(),
            correctness: UiBenchmarkCorrectness {
                heading: "Your library is empty",
                explanation: "Add EPUB or PDF files to start building your library.",
                ready_button_label: "Add books",
                busy_button_label: "Adding books…",
                initial_state_presented: true,
                busy_state_presented: true,
            },
        };
        let json = serde_json::to_vec_pretty(&sample).expect("serialize GPUI benchmark sample");
        fs::write(&self.output, json).unwrap_or_else(|error| {
            panic!(
                "write GPUI benchmark sample {}: {error}",
                self.output.display()
            )
        });
    }

    fn finish_selection(
        self,
        library_total: u64,
        rendered_books: usize,
        selected_books: u64,
        confirmation_presented: bool,
    ) {
        assert_eq!(
            self.workload,
            BenchmarkWorkload::LibrarySelection,
            "selection completion belongs to the library-selection benchmark"
        );
        let mut markers = Vec::with_capacity(4);
        if selected_books == 1 {
            markers.push("compact_explicit_selection");
        }
        if self.selection_painted.is_some() {
            markers.push("selection_bar_presented");
        }
        if confirmation_presented {
            markers.push("confirmation_presented");
            markers.push("removal_copy_mentions_files_remain");
        }
        let sample = UiSelectionBenchmarkSample {
            schema_version: 1,
            workload: "library-selection",
            initial_render_ms: millis(self.initial_render.expect("initial frame was measured")),
            selection_to_paint_ms: millis(
                self.selection_painted
                    .expect("selected state was presented"),
            ),
            confirmation_to_paint_ms: millis(
                self.confirmation_started
                    .expect("confirmation action was started")
                    .elapsed(),
            ),
            peak_rss_bytes: peak_rss_bytes(),
            correctness: UiSelectionBenchmarkCorrectness {
                library_total,
                rendered_books,
                selected_books,
                markers,
            },
        };
        let json =
            serde_json::to_vec_pretty(&sample).expect("serialize GPUI selection benchmark sample");
        fs::write(&self.output, json).unwrap_or_else(|error| {
            panic!(
                "write GPUI selection benchmark sample {}: {error}",
                self.output.display()
            )
        });
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the benchmark correctness artifact keeps every observed field explicit"
    )]
    fn finish_book_detail(
        self,
        library_total: u64,
        rendered_books: usize,
        title: String,
        publication_date: String,
        rating_half_stars: u8,
        contributor_count: usize,
        tag_count: usize,
        asset_count: usize,
    ) {
        assert_eq!(
            self.workload,
            BenchmarkWorkload::BookDetail,
            "book-detail completion belongs to the book-detail benchmark"
        );
        let sample = UiBookDetailBenchmarkSample {
            schema_version: 1,
            workload: "book-detail",
            initial_render_ms: millis(self.initial_render.expect("initial frame was measured")),
            detail_to_paint_ms: millis(
                self.detail_painted
                    .expect("book-detail state was presented"),
            ),
            peak_rss_bytes: peak_rss_bytes(),
            correctness: UiBookDetailBenchmarkCorrectness {
                library_total,
                rendered_books,
                title,
                publication_date,
                rating_half_stars,
                contributor_count,
                tag_count,
                asset_count,
                markers: vec![
                    "bounded_first_page",
                    "book_detail_panel_presented",
                    "complete_metadata_fixture",
                    "publication_metadata_presented",
                    "half_star_rating_presented",
                    "multiple_assets_presented",
                ],
            },
        };
        let json = serde_json::to_vec_pretty(&sample)
            .expect("serialize GPUI book-detail benchmark sample");
        fs::write(&self.output, json).unwrap_or_else(|error| {
            panic!(
                "write GPUI book-detail benchmark sample {}: {error}",
                self.output.display()
            )
        });
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_ascii_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    kibibytes.checked_mul(1_024)
}

#[cfg(not(target_os = "linux"))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

#[derive(Serialize)]
struct UiBenchmarkSample {
    schema_version: u32,
    workload: &'static str,
    initial_render_ms: f64,
    click_to_busy_paint_ms: f64,
    peak_rss_bytes: Option<u64>,
    correctness: UiBenchmarkCorrectness,
}

#[derive(Serialize)]
struct UiBenchmarkCorrectness {
    heading: &'static str,
    explanation: &'static str,
    ready_button_label: &'static str,
    busy_button_label: &'static str,
    initial_state_presented: bool,
    busy_state_presented: bool,
}

#[derive(Serialize)]
struct UiSelectionBenchmarkSample {
    schema_version: u32,
    workload: &'static str,
    initial_render_ms: f64,
    selection_to_paint_ms: f64,
    confirmation_to_paint_ms: f64,
    peak_rss_bytes: Option<u64>,
    correctness: UiSelectionBenchmarkCorrectness,
}

#[derive(Serialize)]
struct UiSelectionBenchmarkCorrectness {
    library_total: u64,
    rendered_books: usize,
    selected_books: u64,
    markers: Vec<&'static str>,
}

#[derive(Serialize)]
struct UiBookDetailBenchmarkSample {
    schema_version: u32,
    workload: &'static str,
    initial_render_ms: f64,
    detail_to_paint_ms: f64,
    peak_rss_bytes: Option<u64>,
    correctness: UiBookDetailBenchmarkCorrectness,
}

#[derive(Serialize)]
struct UiBookDetailBenchmarkCorrectness {
    library_total: u64,
    rendered_books: usize,
    title: String,
    publication_date: String,
    rating_half_stars: u8,
    contributor_count: usize,
    tag_count: usize,
    asset_count: usize,
    markers: Vec<&'static str>,
}
