use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use directories::ProjectDirs;
use eframe::egui::{self, Align, Color32, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};
use lectern_core::{Book, BookFormat, BookId, BookSummary, LibraryQuery, SortOrder};
use lectern_import::{ImportProgress, ImportSummary};
use lectern_storage::LibraryDatabase;

use crate::{
    benchmark::{BenchmarkFrame, DesktopBenchmark},
    workers::{DecodedCover, ImportRequest, QueryRequest, WorkerEvent, WorkerSet},
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

struct CachedCover {
    texture: egui::TextureHandle,
    last_used: u64,
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
    books: Vec<BookSummary>,
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
        let mut app = Self {
            database_path,
            workers,
            query: LibraryQuery::default(),
            query_generation: 0,
            query_pending: false,
            books: Vec::new(),
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
            editor_loading: None,
            editor: None,
            benchmark,
        };
        app.refresh_library();
        app
    }

    fn refresh_library(&mut self) {
        self.query_generation = self.query_generation.wrapping_add(1);
        self.query_pending = self.workers.query(QueryRequest {
            generation: self.query_generation,
            query: self.query.clone(),
        });
        if !self.query_pending {
            "Library query worker is unavailable".clone_into(&mut self.status);
        }
    }

    fn poll_workers(&mut self, context: &egui::Context) {
        while let Some(event) = self.workers.next_event() {
            match event {
                WorkerEvent::QueryFinished { generation, result }
                    if generation == self.query_generation =>
                {
                    self.query_pending = false;
                    match result {
                        Ok(books) => {
                            let recovered = self.status.starts_with("Library query failed:");
                            self.books = books;
                            if let Some(benchmark) = &mut self.benchmark {
                                benchmark.library_installed(&self.books);
                            }
                            if self.selected.is_some_and(|selected| {
                                !self.books.iter().any(|book| book.id == selected)
                            }) {
                                self.clear_selection();
                            }
                            if recovered {
                                "Library ready".clone_into(&mut self.status);
                            }
                        }
                        Err(error) => self.status = format!("Library query failed: {error}"),
                    }
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
                WorkerEvent::BookSaved { book, result } => match result {
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
                },
                WorkerEvent::QueryFinished { .. } | WorkerEvent::BookLoaded { .. } => {}
                WorkerEvent::Error(error) => self.status = format!("Background worker: {error}"),
            }
        }
        self.evict_covers();
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

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut add_books = false;
        let mut add_folder = false;
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Lectern").size(26.0).strong());
            ui.label(
                RichText::new(format!("{} books", self.books.len()))
                    .color(MUTED)
                    .size(13.0),
            );
            if self.query_pending {
                ui.spinner();
            }
            if self.importing {
                ui.label(RichText::new("Importing…").color(ACCENT).size(12.0));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                add_folder = ui
                    .add_enabled(!self.importing, egui::Button::new("Add folder"))
                    .clicked();
                add_books = ui
                    .add_enabled(!self.importing, egui::Button::new("Add books"))
                    .clicked();
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

    fn start_import(&mut self, roots: Vec<PathBuf>) {
        if roots.is_empty() {
            return;
        }
        if self.importing {
            "An import is already running".clone_into(&mut self.status);
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

    fn select_book(&mut self, id: BookId) {
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
        self.selected = None;
        self.editor_loading = None;
        self.editor = None;
    }

    fn metadata_panel(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut reset = false;
        let mut save = false;
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
                let actions = metadata_form(ui, editor);
                save = actions.0;
                reset = actions.1;
            });

        save |= ui.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::S,
            ))
        });
        if close {
            self.clear_selection();
        } else if reset {
            if let Some(editor) = &mut self.editor {
                *editor = BookEditor::new(editor.original.clone());
            }
        } else if save {
            self.save_editor();
        }
    }

    fn save_editor(&mut self) {
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

    fn library(&mut self, ui: &mut egui::Ui) {
        if self.query_pending && self.books.is_empty() {
            centered_message(
                ui,
                "Opening your library…",
                "Reading the local index.",
                true,
            );
            return;
        }
        if self.books.is_empty() {
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
        let row_count = self.books.len().div_ceil(columns);
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
            for row in visible_rows {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = CARD_GAP;
                    for column in 0..columns {
                        let index = row * columns + column;
                        let Some(book) = self.books.get(index).cloned() else {
                            break;
                        };
                        ui.allocate_ui_with_layout(
                            Vec2::new(CARD_WIDTH, CARD_HEIGHT),
                            egui::Layout::top_down(Align::Center),
                            |ui| self.book_card(ui, &book),
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
            ui.add_sized(
                COVER_SIZE,
                egui::Image::from_texture(&cover.texture).corner_radius(7),
            );
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

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            if self.importing {
                ui.spinner();
            }
            ui.label(RichText::new(&self.status).color(MUTED).size(12.0));
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(self.database_path.display().to_string())
                        .color(Color32::from_rgb(105, 114, 127))
                        .size(11.0),
                );
            });
        });
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

fn metadata_text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.add_space(4.0);
    ui.label(RichText::new(label).strong());
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
}

fn metadata_form(ui: &mut egui::Ui, editor: &mut BookEditor) -> (bool, bool) {
    let mut save = false;
    let mut reset = false;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            metadata_text_field(ui, "Title", &mut editor.title);
            if editor.title.trim().is_empty() {
                ui.label(RichText::new("A title is required.").color(Color32::LIGHT_RED));
            }
            metadata_text_field(ui, "Authors", &mut editor.authors);
            metadata_text_field(ui, "Series", &mut editor.series);
            metadata_text_field(ui, "Publisher", &mut editor.publisher);
            metadata_text_field(ui, "Language", &mut editor.language);

            ui.add_space(4.0);
            ui.label(RichText::new("Description").strong());
            ui.add(
                egui::TextEdit::multiline(&mut editor.description)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Files").strong());
            for asset in &editor.original.assets {
                ui.label(
                    RichText::new(format!("{} · {}", asset.format, asset.storage))
                        .color(MUTED)
                        .size(12.0),
                );
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

            if let Some(error) = &editor.error {
                ui.add_space(8.0);
                ui.label(RichText::new(error).color(Color32::LIGHT_RED));
            }

            ui.add_space(12.0);
            let changed = editor.changed();
            ui.horizontal(|ui| {
                let button_text = if editor.saving { "Saving…" } else { "Save" };
                save = ui
                    .add_enabled(editor.can_save(), egui::Button::new(button_text))
                    .on_hover_text("Save metadata (Ctrl/Cmd-S)")
                    .clicked();
                reset = ui
                    .add_enabled(changed && !editor.saving, egui::Button::new("Reset"))
                    .clicked();
                if editor.saving {
                    ui.spinner();
                }
            });
        });
    (save, reset)
}

fn optional_metadata(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lectern_core::{AssetId, AssetStorage, Book, BookAsset, BookFormat, BookId};
    use lectern_import::ImportProgress;

    use super::{BookEditor, CARD_GAP, CARD_WIDTH, column_count, import_status};

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
    fn metadata_editor_normalizes_saved_values() {
        let book = Book {
            id: BookId::new(7),
            title: "Dune".into(),
            authors: "Frank Herbert".into(),
            series: Some("Dune".into()),
            publisher: None,
            language: Some("en".into()),
            description: None,
            assets: vec![BookAsset {
                id: AssetId::new(11),
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
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
