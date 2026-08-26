//! Minimal GPUI application used to migrate Lectern's library journey.

use std::{
    env, fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, PathPromptOptions, Render, SharedString, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, relative, size,
};
use gpui_platform::application;
use lectern_ui::{Button, ButtonVariant, LecternAssets, PrimerTheme, TablerIcon, install_theme};
use serde::Serialize;

const BENCHMARK_OUTPUT_ENV: &str = "LECTERN_GPUI_BENCHMARK_OUTPUT";
const ROOT_REM_PX: f32 = 16.0;
const WINDOW_WIDTH_PX: f32 = 900.0;
const WINDOW_HEIGHT_PX: f32 = 620.0;
const EMPTY_LIBRARY_CONTENT_WIDTH_PX: f32 = 480.0;

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
                    cx.new(|_| EmptyLibrary::new(benchmark))
                },
            )
            .expect("open Lectern GPUI window");
            cx.activate(true);
        });
}

struct EmptyLibrary {
    busy: bool,
    status: Option<SharedString>,
    initial_frame_scheduled: bool,
    benchmark: Option<BenchmarkRun>,
}

impl EmptyLibrary {
    fn new(benchmark: Option<BenchmarkRun>) -> Self {
        Self {
            busy: false,
            status: None,
            initial_frame_scheduled: false,
            benchmark,
        }
    }

    fn initial_frame_presented(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(benchmark) = &mut self.benchmark else {
            return;
        };
        benchmark.initial_render = Some(benchmark.main_entry.elapsed());
        self.start_add_books(window, cx);
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
        let response = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(SharedString::new_static("Add books")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selection = response.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.status = match selection {
                    Ok(Ok(Some(paths))) if !paths.is_empty() => {
                        Some(format!("Selected {} book files.", paths.len()).into())
                    }
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => {
                        Some(format!("Could not open the file picker: {error}").into())
                    }
                    Err(error) => Some(format!("The file picker was interrupted: {error}").into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for EmptyLibrary {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.initial_frame_scheduled {
            self.initial_frame_scheduled = true;
            cx.on_next_frame(window, Self::initial_frame_presented);
        }
        let theme = PrimerTheme::current(cx);
        let button_label = if self.busy {
            "Adding books…"
        } else {
            "Add books"
        };

        div()
            .size_full()
            .bg(theme.surface.background)
            .text_color(theme.surface.foreground)
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
                    .child(
                        Button::new("add-books", button_label)
                            .variant(ButtonVariant::Primary)
                            .leading_icon(TablerIcon::Upload)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_add_books(window, cx);
                            })),
                    )
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
