use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use directories::ProjectDirs;
use eframe::egui::{self, Align, Color32, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};
use lectern_core::{BookFormat, BookId, BookSummary, LibraryQuery, SortOrder};
use lectern_storage::LibraryDatabase;

use crate::workers::{DecodedCover, QueryRequest, WorkerEvent, WorkerSet};

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
}

impl LecternApp {
    pub(crate) fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
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
                            self.books = books;
                            if self.selected.is_some_and(|selected| {
                                !self.books.iter().any(|book| book.id == selected)
                            }) {
                                self.selected = None;
                            }
                            "Library ready".clone_into(&mut self.status);
                        }
                        Err(error) => self.status = format!("Library query failed: {error}"),
                    }
                }
                WorkerEvent::QueryFinished { .. } => {}
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
        ui.horizontal_centered(|ui| {
            ui.heading(RichText::new("Lectern").size(26.0).strong());
            ui.label(
                RichText::new(format!("{} books", self.books.len()))
                    .color(MUTED)
                    .size(13.0),
            );
            if self.query_pending {
                ui.spinner();
            }
        });
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
                    "Drop EPUB files here to start building it.",
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
        egui::ScrollArea::vertical()
            .id_salt("library-grid")
            .auto_shrink([false, false])
            .show_rows(ui, CARD_HEIGHT, row_count, |ui, visible_rows| {
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
            self.selected = Some(book.id);
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_number = self.frame_number.wrapping_add(1);
        self.poll_workers(ui.ctx());

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
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ui, |ui| self.library(ui));
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

#[cfg(test)]
mod tests {
    use super::{CARD_GAP, CARD_WIDTH, column_count};

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
}
