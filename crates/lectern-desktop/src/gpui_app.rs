//! Minimal GPUI application used to migrate Lectern's library journey.

use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Image, ImageFormat, KeyBinding, ObjectFit, Render, SharedString,
    StatefulInteractiveElement, StyledImage, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, relative, size,
};
use gpui_base::{
    AlertDialog, AlertDialogBackdrop, AlertDialogDescription, AlertDialogPopup, AlertDialogTitle,
    Checkbox, CheckboxState,
};
use gpui_platform::application;
use lectern_core::{
    AssetHealth, AssetStorage, Book, BookAsset, BookFormat, BookId, BookSummary, ImportSummary,
    LibraryQuery, LibraryService,
    organisation::{
        BookSelection, BulkRemovalResult, Contributor, ContributorCredit, ContributorId,
        ContributorRole, LibraryGeneration, SelectionSnapshot, Series, SeriesId, SeriesIndex,
        SeriesMembership, Tag, TagId,
    },
};
use lectern_service::{LibraryServiceError, SqliteLibraryService, default_database_path};
use lectern_ui::{Button, ButtonVariant, LecternAssets, PrimerTheme, TablerIcon, install_theme};
use serde::Serialize;

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
const BOOK_DETAIL_PANEL_WIDTH_PX: f32 = 384.0;
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

/// Runs Lectern's additive GPUI migration executable.
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
            cx.bind_keys([
                KeyBinding::new("cmd-a", SelectAllBooks, Some("LecternLibrary")),
                KeyBinding::new("ctrl-a", SelectAllBooks, Some("LecternLibrary")),
                KeyBinding::new("escape", ClearBookSelection, Some("LecternLibrary")),
            ]);
            install_theme(cx, PrimerTheme::light());
            let bounds =
                Bounds::centered(None, size(px(WINDOW_WIDTH_PX), px(WINDOW_HEIGHT_PX)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |window, cx| {
                    window.set_rem_size(px(ROOT_REM_PX));
                    cx.new(|_| LecternView::new(benchmark))
                },
            )
            .expect("open Lectern GPUI window");
            cx.activate(true);
        });
}

struct LecternView {
    database_path: PathBuf,
    library_state: LibraryState,
    library_total: u64,
    books: Vec<LibraryBook>,
    query: LibraryQuery,
    selection: GridSelection,
    selection_generation: u64,
    selection_pending: Option<PendingSelection>,
    detail_book: Option<Book>,
    removal_confirmation: Option<BulkRemovalConfirmation>,
    removing: bool,
    busy: bool,
    status: Option<SharedString>,
    initial_frame_scheduled: bool,
    benchmark: Option<BenchmarkRun>,
}

impl LecternView {
    fn new(benchmark: Option<BenchmarkRun>) -> Self {
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
            database_path: default_database_path(),
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
            detail_book: None,
            removal_confirmation: None,
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
        self.detail_book = Some(benchmark_book_detail());
        cx.notify();
        cx.on_next_frame(window, Self::benchmark_book_detail_presented);
    }

    fn benchmark_book_detail_presented(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let book = self
            .detail_book
            .as_ref()
            .expect("book-detail benchmark has a presented book");
        let title = book.title.clone();
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
        }
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
            .items_center()
            .justify_center()
            .child(
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
        let cards = self
            .books
            .iter()
            .enumerate()
            .map(|(index, book)| {
                book_card(
                    book,
                    index,
                    self.selection.contains(book.summary.id),
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
            .child(
                div()
                    .flex_none()
                    .h(px(TOP_BAR_HEIGHT_PX))
                    .flex()
                    .items_center()
                    .justify_between()
                    .p(theme.spacing.small)
                    .border_b(theme.border.thin)
                    .border_color(theme.border.muted)
                    .child(
                        div()
                            .text_size(theme.typography.title_size)
                            .font_weight(theme.typography.title_weight)
                            .child("Lectern"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(theme.spacing.small)
                            .child(
                                Button::new("begin-selection", "Select books")
                                    .disabled(selection_active || selection_locked)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.begin_selection(cx);
                                    })),
                            )
                            .child(self.add_books_button("library-add-books", cx)),
                    ),
            )
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
                            .when_some(self.status.clone(), |content, status| {
                                content.child(
                                    div()
                                        .text_color(theme.surface.muted_foreground)
                                        .text_size(theme.typography.body_size)
                                        .child(status),
                                )
                            })
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
                    .when_some(self.detail_book.as_ref(), |body, book| {
                        body.child(book_detail_panel(book, theme))
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
                    .text_color(theme.surface.muted_foreground)
                    .text_size(theme.typography.body_size)
                    .child(book_count),
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

        div()
            .size_full()
            .bg(theme.surface.background)
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
            .child(content)
            .when(self.removal_confirmation.is_some(), |root| {
                root.child(self.bulk_removal_dialog(&theme, cx))
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

#[expect(
    clippy::too_many_lines,
    reason = "one declarative detail skeleton keeps benchmark coverage visibly representative"
)]
fn book_detail_panel(book: &Book, theme: &PrimerTheme) -> gpui::AnyElement {
    let contributor_rows = book.contributors.iter().map(|credit| {
        detail_summary_row(
            credit.role.to_string(),
            credit.contributor.display_name.clone(),
            theme,
        )
    });
    let tag_rows = book.tags.iter().map(|tag| {
        div()
            .px(theme.spacing.small)
            .py(theme.spacing.small)
            .rounded(theme.button.radius)
            .border(theme.border.thin)
            .border_color(theme.border.muted)
            .child(tag.name.clone())
    });
    let asset_rows = book.assets.iter().map(|asset| {
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
                    .child(Button::new(
                        format!("detail-open-asset-{}", asset.id.value()),
                        "Open",
                    )),
            )
            .child(
                div()
                    .truncate()
                    .text_color(theme.surface.muted_foreground)
                    .child(asset.path.to_string_lossy().into_owned()),
            )
    });

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
                .p(theme.spacing.extra_large)
                .border_b(theme.border.thin)
                .border_color(theme.border.muted)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(theme.typography.title_size)
                        .font_weight(theme.typography.title_weight)
                        .child("Book details"),
                )
                .child(Button::new("close-book-detail", "Close")),
        )
        .child(
            div()
                .id("book-detail-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(theme.spacing.extra_large)
                .flex()
                .flex_col()
                .gap(theme.spacing.extra_large)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.spacing.small)
                        .child(
                            Button::new("save-book-detail", "Save").variant(ButtonVariant::Primary),
                        )
                        .child(Button::new("reset-book-detail", "Reset")),
                )
                .child(detail_section("Title", book.title.clone(), theme))
                .child(
                    detail_section_container("Contributors", theme)
                        .children(contributor_rows)
                        .child(Button::new("add-book-contributor", "Add contributor")),
                )
                .child(detail_section(
                    "Series",
                    book.series_membership.as_ref().map_or_else(
                        || "Not in a series".to_owned(),
                        |membership| {
                            membership.index.map_or_else(
                                || membership.series.name.clone(),
                                |index| format!("{} · Book {index}", membership.series.name),
                            )
                        },
                    ),
                    theme,
                ))
                .child(
                    detail_section_container("Tags", theme).child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(theme.spacing.small)
                            .children(tag_rows),
                    ),
                )
                .child(detail_section(
                    "Publisher",
                    book.publisher
                        .clone()
                        .unwrap_or_else(|| "Not set".to_owned()),
                    theme,
                ))
                .child(detail_section(
                    "Language",
                    book.language
                        .clone()
                        .unwrap_or_else(|| "Not set".to_owned()),
                    theme,
                ))
                .child(detail_section(
                    "Description",
                    book.description
                        .clone()
                        .unwrap_or_else(|| "Not set".to_owned()),
                    theme,
                ))
                .child(
                    detail_section_container("Files", theme)
                        .children(asset_rows)
                        .child(Button::new("add-book-asset", "Add EPUB or PDF")),
                )
                .child(
                    detail_section_container("Library", theme)
                        .child(div().text_color(theme.surface.muted_foreground).child(
                            "Remove this entry and its cached cover. Book files stay on disk.",
                        ))
                        .child(
                            Button::new("remove-detail-book", "Remove from library")
                                .variant(ButtonVariant::Danger),
                        ),
                ),
        )
        .into_any_element()
}

fn detail_section(
    label: &'static str,
    value: impl Into<SharedString>,
    theme: &PrimerTheme,
) -> gpui::Div {
    detail_section_container(label, theme).child(value.into())
}

fn detail_section_container(label: &'static str, theme: &PrimerTheme) -> gpui::Div {
    div().flex().flex_col().gap(theme.spacing.small).child(
        div()
            .font_weight(theme.typography.button_weight)
            .child(label),
    )
}

fn detail_summary_row(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    theme: &PrimerTheme,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(theme.spacing.medium)
        .child(
            div()
                .text_color(theme.surface.muted_foreground)
                .child(label.into()),
        )
        .child(div().truncate().child(value.into()))
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
        .gap(theme.spacing.small)
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
                .truncate()
                .text_size(theme.typography.body_size)
                .font_weight(theme.typography.button_weight)
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
            },
            Tag {
                id: TagId::new(2),
                name: "Reference".to_owned(),
            },
        ],
        publisher: Some("Lectern Press".to_owned()),
        language: Some("English".to_owned()),
        description: Some(
            "A deterministic book used to verify complete detail-panel presentation.".to_owned(),
        ),
        assets: vec![
            BookAsset {
                id: lectern_core::AssetId::new(1),
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
                health: AssetHealth::Available,
                path: PathBuf::from("/benchmark/Benchmark book 001.epub"),
            },
            BookAsset {
                id: lectern_core::AssetId::new(2),
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

    fn finish_book_detail(
        self,
        library_total: u64,
        rendered_books: usize,
        title: String,
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
                contributor_count,
                tag_count,
                asset_count,
                markers: vec![
                    "bounded_first_page",
                    "book_detail_panel_presented",
                    "complete_metadata_fixture",
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
    contributor_count: usize,
    tag_count: usize,
    asset_count: usize,
    markers: Vec<&'static str>,
}
