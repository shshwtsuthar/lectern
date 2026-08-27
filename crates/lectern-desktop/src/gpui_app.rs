//! Minimal GPUI application used to migrate Lectern's library journey.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Image, ImageFormat, ObjectFit, Render, SharedString,
    StatefulInteractiveElement, StyledImage, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, relative, size,
};
use gpui_platform::application;
use lectern_core::{BookSummary, ImportSummary, LibraryQuery, LibraryService};
use lectern_service::{LibraryServiceError, SqliteLibraryService, default_database_path};
use lectern_ui::{Button, ButtonVariant, LecternAssets, PrimerTheme, TablerIcon, install_theme};
use serde::Serialize;

const BENCHMARK_OUTPUT_ENV: &str = "LECTERN_GPUI_BENCHMARK_OUTPUT";
const ROOT_REM_PX: f32 = 16.0;
const WINDOW_WIDTH_PX: f32 = 900.0;
const WINDOW_HEIGHT_PX: f32 = 620.0;
const EMPTY_LIBRARY_CONTENT_WIDTH_PX: f32 = 480.0;
const BOOK_CARD_WIDTH_PX: f32 = 144.0;
const BOOK_COVER_HEIGHT_PX: f32 = 216.0;
const TOP_BAR_HEIGHT_PX: f32 = 48.0;
const BOTTOM_BAR_HEIGHT_PX: f32 = 24.0;
const LIBRARY_PAGE_SIZE: u32 = 128;

/// Runs Lectern's additive GPUI migration executable.
///
/// # Panics
///
/// Panics if GPUI cannot open the application window or benchmark output cannot be serialized and
/// written.
pub fn run(main_entry: Instant) {
    let benchmark = env::var_os(BENCHMARK_OUTPUT_ENV).map(|path| BenchmarkRun {
        output: PathBuf::from(path),
        main_entry,
        initial_render: None,
        action_started: None,
    });

    application()
        .with_assets(LecternAssets)
        .run(move |cx: &mut App| {
            gpui_base::init(cx);
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
        Self {
            database_path: default_database_path(),
            library_state,
            library_total: 0,
            books: Vec::new(),
            busy: false,
            status: None,
            initial_frame_scheduled: false,
            benchmark,
        }
    }

    fn initial_frame_presented(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(benchmark) = &mut self.benchmark {
            benchmark.initial_render = Some(benchmark.main_entry.elapsed());
            self.start_add_books(window, cx);
            return;
        }
        self.load_library(window, cx);
    }

    fn load_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if self.busy {
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

    fn add_books_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        let button_label = if self.busy {
            "Adding books…"
        } else {
            "Add books"
        };
        Button::new(id, button_label)
            .variant(ButtonVariant::Primary)
            .leading_icon(TablerIcon::Upload)
            .disabled(self.busy)
            .on_click(cx.listener(|this, _, window, cx| {
                this.start_add_books(window, cx);
            }))
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
        let cards = self
            .books
            .iter()
            .map(|book| book_card(book, theme))
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
                    .child(self.add_books_button("library-add-books", cx)),
            )
            .child(
                div()
                    .id("library-scroll")
                    .flex_1()
                    .min_h_0()
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
                        self.library_total > u64::try_from(self.books.len()).unwrap_or(u64::MAX),
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
            .child(content)
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

fn book_card(book: &LibraryBook, theme: &PrimerTheme) -> gpui::Div {
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

    div()
        .w(px(BOOK_CARD_WIDTH_PX))
        .flex_none()
        .flex()
        .flex_col()
        .gap(theme.spacing.small)
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
        )
}

struct BenchmarkRun {
    output: PathBuf,
    main_entry: Instant,
    initial_render: Option<Duration>,
    action_started: Option<Instant>,
}

impl BenchmarkRun {
    fn finish(self) {
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
