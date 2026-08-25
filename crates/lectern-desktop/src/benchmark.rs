//! Opt-in desktop startup, scrolling, frame-time, and memory instrumentation.

use std::{
    array, env,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use lectern_core::{BookSummary, SortOrder};
use serde::Serialize;

use crate::platform::PlatformAction;

const OUTPUT_ENV: &str = "LECTERN_BENCHMARK_OUTPUT";
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(20);
const STARTUP_PHASE: usize = 0;
const IDLE_PHASE: usize = 1;
const SORT_PHASE: usize = 2;
const SCROLL_PHASE: usize = 3;
const PHASE_COUNT: usize = 4;
const SORT_SCENARIOS: [SortOrder; 3] = [
    SortOrder::Author,
    SortOrder::RecentlyAdded,
    SortOrder::Title,
];
const SORT_TO_PAINT_P95_BUDGET_NS: u64 = 50_000_000;
const ASSET_ACTION_TO_PAINT_P95_BUDGET_NS: u64 = 50_000_000;
const ASSET_ACTIONS: [PlatformAction; 2] = [PlatformAction::Open, PlatformAction::Reveal];

pub(crate) struct BenchmarkFrame {
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
    pub(crate) pixels_per_point: f32,
    pub(crate) cached_covers: usize,
    pub(crate) pending_covers: usize,
    pub(crate) missing_covers: usize,
}

#[derive(Clone, Copy)]
struct BenchmarkConfig {
    idle: Duration,
    scroll: Duration,
    scroll_warmup: Duration,
    timeout: Duration,
    scroll_pixels_per_second: f32,
    sort_iterations: usize,
    asset_action_iterations: usize,
}

impl BenchmarkConfig {
    fn from_environment() -> Result<Self, String> {
        let config = Self {
            idle: duration_from_env("LECTERN_BENCHMARK_IDLE_SECONDS", 3.0)?,
            scroll: duration_from_env("LECTERN_BENCHMARK_SCROLL_SECONDS", 15.0)?,
            scroll_warmup: duration_from_env("LECTERN_BENCHMARK_SCROLL_WARMUP_SECONDS", 1.0)?,
            timeout: duration_from_env("LECTERN_BENCHMARK_TIMEOUT_SECONDS", 120.0)?,
            scroll_pixels_per_second: number_from_env(
                "LECTERN_BENCHMARK_SCROLL_PIXELS_PER_SECOND",
                1_500.0,
            )?,
            sort_iterations: usize_from_env("LECTERN_BENCHMARK_SORT_ITERATIONS", 0)?,
            asset_action_iterations: usize_from_env(
                "LECTERN_BENCHMARK_ASSET_ACTION_ITERATIONS",
                0,
            )?,
        };
        if config.scroll.is_zero() && !config.scroll_warmup.is_zero() {
            return Err(
                "LECTERN_BENCHMARK_SCROLL_WARMUP_SECONDS must be zero when scrolling is disabled"
                    .into(),
            );
        }
        if !config.scroll.is_zero() && config.scroll_warmup >= config.scroll {
            return Err(
                "LECTERN_BENCHMARK_SCROLL_WARMUP_SECONDS must be less than scroll duration".into(),
            );
        }
        if config.timeout <= config.idle + config.scroll {
            return Err("LECTERN_BENCHMARK_TIMEOUT_SECONDS must exceed idle + scroll time".into());
        }
        if config.scroll_pixels_per_second <= 0.0 {
            return Err(
                "LECTERN_BENCHMARK_SCROLL_PIXELS_PER_SECOND must be greater than zero".into(),
            );
        }
        Ok(config)
    }
}

enum Phase {
    Startup {
        paint_frames_remaining: Option<u8>,
    },
    Idle {
        started: Instant,
    },
    Sort {
        completed: usize,
        pending: Option<PendingSort>,
    },
    AssetActions {
        completed: usize,
        pending: Option<PendingAssetAction>,
    },
    Scroll {
        started: Instant,
    },
    Finished,
}

struct PendingSort {
    sort: SortOrder,
    started: Instant,
    paint_frames_remaining: Option<u8>,
}

struct PendingAssetAction {
    action: PlatformAction,
    started: Instant,
    paint_frames_remaining: u8,
}

pub(crate) struct DesktopBenchmark {
    output_path: PathBuf,
    config: BenchmarkConfig,
    main_entry: Instant,
    phase: Phase,
    last_frame_started: Option<Instant>,
    main_entry_to_query_installed_ns: Option<u64>,
    main_entry_to_populated_library_ns: Option<u64>,
    observed_scroll_duration_ns: Option<u64>,
    ready_rss_bytes: Option<u64>,
    idle_end_rss_bytes: Option<u64>,
    library_books: u64,
    initial_page_books: usize,
    initial_page_books_with_covers: usize,
    frame_intervals_ns: Vec<u64>,
    egui_frame_intervals_ns: Vec<u64>,
    cpu_frame_times_ns: Vec<u64>,
    sort_to_paint_samples_ns: [Vec<u64>; SORT_SCENARIOS.len()],
    sort_first_book_ids: [Option<i64>; SORT_SCENARIOS.len()],
    asset_action_to_paint_samples_ns: [Vec<u64>; ASSET_ACTIONS.len()],
    validation_failure: Option<String>,
    memory: Option<PhaseMemorySampler>,
}

impl DesktopBenchmark {
    pub(crate) fn from_environment(main_entry: Instant) -> Result<Option<Self>, String> {
        let Some(output_path) = env::var_os(OUTPUT_ENV).map(PathBuf::from) else {
            return Ok(None);
        };
        let config = BenchmarkConfig::from_environment()?;
        let memory = PhaseMemorySampler::start(MEMORY_SAMPLE_INTERVAL)?;
        Ok(Some(Self {
            output_path,
            config,
            main_entry,
            phase: Phase::Startup {
                paint_frames_remaining: None,
            },
            last_frame_started: None,
            main_entry_to_query_installed_ns: None,
            main_entry_to_populated_library_ns: None,
            observed_scroll_duration_ns: None,
            ready_rss_bytes: None,
            idle_end_rss_bytes: None,
            library_books: 0,
            initial_page_books: 0,
            initial_page_books_with_covers: 0,
            frame_intervals_ns: Vec::new(),
            egui_frame_intervals_ns: Vec::new(),
            cpu_frame_times_ns: Vec::new(),
            sort_to_paint_samples_ns: array::from_fn(|_| Vec::new()),
            sort_first_book_ids: [None; SORT_SCENARIOS.len()],
            asset_action_to_paint_samples_ns: array::from_fn(|_| Vec::new()),
            validation_failure: None,
            memory: Some(memory),
        }))
    }

    pub(crate) fn library_installed(&mut self, total: u64, books: &[BookSummary], sort: SortOrder) {
        match &mut self.phase {
            Phase::Startup {
                paint_frames_remaining,
            } => {
                if total == 0 || books.is_empty() || paint_frames_remaining.is_some() {
                    return;
                }
                self.library_books = total;
                self.initial_page_books = books.len();
                self.initial_page_books_with_covers =
                    books.iter().filter(|book| book.has_cover).count();
                self.main_entry_to_query_installed_ns = elapsed_ns(self.main_entry.elapsed());
                *paint_frames_remaining = Some(1);
            }
            Phase::Sort {
                pending: Some(pending),
                ..
            } if pending.paint_frames_remaining.is_none() && pending.sort == sort => {
                if total != self.library_books || books.len() != self.initial_page_books {
                    self.validation_failure = Some(format!(
                        "sort {} returned {total} books and a {}-book page; expected {} and {}",
                        sort_name(sort),
                        books.len(),
                        self.library_books,
                        self.initial_page_books,
                    ));
                    return;
                }
                let Some(first_book_id) = books.first().map(|book| book.id.value()) else {
                    self.validation_failure =
                        Some(format!("sort {} returned an empty page", sort_name(sort)));
                    return;
                };
                let index = sort_index(sort);
                match self.sort_first_book_ids[index] {
                    Some(expected) if expected != first_book_id => {
                        self.validation_failure = Some(format!(
                            "sort {} changed its first book from {expected} to {first_book_id}",
                            sort_name(sort),
                        ));
                        return;
                    }
                    None => self.sort_first_book_ids[index] = Some(first_book_id),
                    Some(_) => {}
                }
                pending.paint_frames_remaining = Some(1);
            }
            _ => {}
        }
    }

    pub(crate) fn next_sort_request(&mut self) -> Option<SortOrder> {
        let Phase::Sort { completed, pending } = &mut self.phase else {
            return None;
        };
        if pending.is_some() || *completed >= self.config.sort_iterations * SORT_SCENARIOS.len() {
            return None;
        }
        let sort = sort_for_interaction(*completed);
        *pending = Some(PendingSort {
            sort,
            started: Instant::now(),
            paint_frames_remaining: None,
        });
        Some(sort)
    }

    pub(crate) fn next_asset_action_request(&mut self) -> Option<PlatformAction> {
        let Phase::AssetActions { completed, pending } = &mut self.phase else {
            return None;
        };
        if pending.is_some()
            || *completed >= self.config.asset_action_iterations * ASSET_ACTIONS.len()
        {
            return None;
        }
        let action = ASSET_ACTIONS[*completed % ASSET_ACTIONS.len()];
        *pending = Some(PendingAssetAction {
            action,
            started: Instant::now(),
            paint_frames_remaining: 1,
        });
        Some(action)
    }

    pub(crate) fn asset_action_dispatch_failed(&mut self) {
        self.validation_failure = Some("no-op platform action could not be queued".to_owned());
    }

    pub(crate) fn frame_started(&mut self, cpu_usage_seconds: Option<f32>, unstable_dt: f32) {
        let now = Instant::now();
        let interval = self
            .last_frame_started
            .replace(now)
            .and_then(|previous| elapsed_ns(now.duration_since(previous)));
        let Phase::Scroll { started } = self.phase else {
            return;
        };
        if now.duration_since(started) < self.config.scroll_warmup {
            return;
        }
        if let Some(interval) = interval {
            self.frame_intervals_ns.push(interval);
        }
        if unstable_dt.is_finite()
            && unstable_dt > 0.0
            && let Some(interval) = seconds_f32_ns(unstable_dt)
        {
            self.egui_frame_intervals_ns.push(interval);
        }
        if let Some(cpu_usage_seconds) = cpu_usage_seconds
            && cpu_usage_seconds.is_finite()
            && cpu_usage_seconds > 0.0
            && let Some(cpu_time) = seconds_f32_ns(cpu_usage_seconds)
        {
            self.cpu_frame_times_ns.push(cpu_time);
        }
    }

    pub(crate) fn scroll_offset(&self) -> Option<f32> {
        let Phase::Scroll { started } = self.phase else {
            return None;
        };
        Some(started.elapsed().as_secs_f32() * self.config.scroll_pixels_per_second)
    }

    pub(crate) fn frame_finished(&mut self, context: &egui::Context, frame: &BenchmarkFrame) {
        let now = Instant::now();
        let mut begin_idle = false;
        let mut begin_sort = false;
        let mut begin_asset_actions = false;
        let mut begin_scroll = false;
        let mut sort_finished = false;
        let mut asset_actions_finished = false;
        let mut finish = false;
        let mut failure = None;
        let mut observed_scroll_duration = None;
        let timed_out = now.duration_since(self.main_entry) >= self.config.timeout;

        if let Some(error) = self.validation_failure.take() {
            failure = Some(error);
        }

        match &mut self.phase {
            Phase::Startup { .. } if timed_out => {
                failure = Some("populated library was not rendered before timeout".to_owned());
            }
            Phase::Startup {
                paint_frames_remaining: Some(remaining),
            } if *remaining > 0 => {
                *remaining -= 1;
                context.request_repaint();
            }
            Phase::Startup {
                paint_frames_remaining: Some(_),
            } => begin_idle = true,
            Phase::Idle { .. } if timed_out => {
                failure = Some("desktop benchmark timed out during the idle window".to_owned());
            }
            Phase::Sort { .. } if timed_out => {
                failure = Some("desktop benchmark timed out during sort interactions".to_owned());
            }
            Phase::AssetActions { .. } if timed_out => {
                failure = Some("desktop benchmark timed out during asset actions".to_owned());
            }
            Phase::Scroll { started } if timed_out => {
                observed_scroll_duration = Some(now.duration_since(*started));
                failure = Some("desktop benchmark timed out during scrolling".to_owned());
            }
            Phase::Idle { started } if now.duration_since(*started) >= self.config.idle => {
                if self.config.sort_iterations > 0 {
                    begin_sort = true;
                } else if self.config.asset_action_iterations > 0 {
                    begin_asset_actions = true;
                } else if self.config.scroll.is_zero() {
                    self.idle_end_rss_bytes = current_rss_bytes();
                    observed_scroll_duration = Some(Duration::ZERO);
                    finish = true;
                } else {
                    begin_scroll = true;
                }
            }
            Phase::Sort { completed, pending }
                if pending
                    .as_ref()
                    .is_some_and(|pending| pending.paint_frames_remaining == Some(0)) =>
            {
                let completed_sort = pending.take().expect("completed sort is present");
                let index = sort_index(completed_sort.sort);
                if let Some(latency) = elapsed_ns(now.duration_since(completed_sort.started)) {
                    self.sort_to_paint_samples_ns[index].push(latency);
                } else {
                    failure = Some("sort-to-paint latency exceeded the supported range".to_owned());
                }
                *completed += 1;
                sort_finished = *completed >= self.config.sort_iterations * SORT_SCENARIOS.len();
                context.request_repaint();
            }
            Phase::AssetActions { completed, pending }
                if pending
                    .as_ref()
                    .is_some_and(|pending| pending.paint_frames_remaining == 0) =>
            {
                let completed_action = pending.take().expect("completed asset action is present");
                let index = asset_action_index(completed_action.action);
                if let Some(latency) = elapsed_ns(now.duration_since(completed_action.started)) {
                    self.asset_action_to_paint_samples_ns[index].push(latency);
                } else {
                    failure = Some(
                        "asset-action-to-paint latency exceeded the supported range".to_owned(),
                    );
                }
                *completed += 1;
                asset_actions_finished =
                    *completed >= self.config.asset_action_iterations * ASSET_ACTIONS.len();
                context.request_repaint();
            }
            Phase::AssetActions {
                pending: Some(pending),
                ..
            } if pending.paint_frames_remaining > 0 => {
                pending.paint_frames_remaining -= 1;
                context.request_repaint();
            }
            Phase::Sort {
                pending:
                    Some(PendingSort {
                        paint_frames_remaining: Some(remaining),
                        ..
                    }),
                ..
            } if *remaining > 0 => {
                *remaining -= 1;
                context.request_repaint();
            }
            Phase::Startup { .. }
            | Phase::Idle { .. }
            | Phase::Sort { .. }
            | Phase::AssetActions { .. } => {
                context.request_repaint_after(MEMORY_SAMPLE_INTERVAL);
            }
            Phase::Scroll { started } if now.duration_since(*started) >= self.config.scroll => {
                observed_scroll_duration = Some(now.duration_since(*started));
                finish = true;
            }
            Phase::Scroll { .. } => context.request_repaint(),
            Phase::Finished => {}
        }

        if begin_idle {
            self.main_entry_to_populated_library_ns =
                elapsed_ns(now.duration_since(self.main_entry));
            self.ready_rss_bytes = current_rss_bytes();
            if let Some(memory) = &self.memory {
                memory.record_sample(STARTUP_PHASE, self.ready_rss_bytes);
                memory.set_phase(IDLE_PHASE);
            }
            self.phase = Phase::Idle { started: now };
            context.request_repaint_after(MEMORY_SAMPLE_INTERVAL);
        } else if begin_sort {
            self.idle_end_rss_bytes = current_rss_bytes();
            if let Some(memory) = &self.memory {
                memory.record_sample(IDLE_PHASE, self.idle_end_rss_bytes);
                memory.set_phase(SORT_PHASE);
            }
            self.phase = Phase::Sort {
                completed: 0,
                pending: None,
            };
            context.request_repaint();
        } else if sort_finished {
            if self.config.asset_action_iterations > 0 {
                begin_asset_actions = true;
            } else if self.config.scroll.is_zero() {
                observed_scroll_duration = Some(Duration::ZERO);
                finish = true;
            } else {
                begin_scroll = true;
            }
        }

        if begin_asset_actions {
            if self.idle_end_rss_bytes.is_none() {
                self.idle_end_rss_bytes = current_rss_bytes();
                if let Some(memory) = &self.memory {
                    memory.record_sample(IDLE_PHASE, self.idle_end_rss_bytes);
                    memory.set_phase(SORT_PHASE);
                }
            }
            self.phase = Phase::AssetActions {
                completed: 0,
                pending: None,
            };
            context.request_repaint();
        } else if asset_actions_finished {
            if self.config.scroll.is_zero() {
                observed_scroll_duration = Some(Duration::ZERO);
                finish = true;
            } else {
                begin_scroll = true;
            }
        }

        if begin_scroll {
            if self.idle_end_rss_bytes.is_none() {
                self.idle_end_rss_bytes = current_rss_bytes();
                if let Some(memory) = &self.memory {
                    memory.record_sample(IDLE_PHASE, self.idle_end_rss_bytes);
                }
            }
            if let Some(memory) = &self.memory {
                memory.set_phase(SCROLL_PHASE);
            }
            self.phase = Phase::Scroll { started: now };
            context.request_repaint();
        } else if finish || failure.is_some() {
            self.observed_scroll_duration_ns = observed_scroll_duration.and_then(elapsed_ns);
            self.phase = Phase::Finished;
            let budget_failure = failure
                .is_none()
                .then(|| self.interaction_budget_failure())
                .flatten();
            if let Err(error) =
                self.write_result(failure.as_deref().or(budget_failure.as_deref()), frame)
            {
                eprintln!("Could not write desktop benchmark result: {error}");
            }
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn write_result(
        &mut self,
        failure: Option<&str>,
        frame: &BenchmarkFrame,
    ) -> Result<(), String> {
        let memory = self
            .memory
            .take()
            .ok_or_else(|| "memory sampler was already consumed".to_owned())?
            .finish()?;
        let result = DesktopBenchmarkResult {
            schema_version: 3,
            kind: "desktop",
            status: if failure.is_some() {
                "failed"
            } else {
                "completed"
            },
            error: failure,
            measured_at_unix_ms: unix_time_ms()?,
            build: BuildMetadata {
                version: env!("CARGO_PKG_VERSION"),
                profile: if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
                wgpu_backend_requested: env::var("WGPU_BACKEND").ok(),
                display_backend: display_backend(),
            },
            configuration: ConfigurationResult::from(self.config),
            library: LibraryResult {
                books: self.library_books,
                initial_page_books: self.initial_page_books,
                initial_page_books_with_covers: self.initial_page_books_with_covers,
            },
            startup: self
                .main_entry_to_populated_library_ns
                .map(|populated_library_ns| StartupResult {
                    main_entry_to_query_installed_ns: self.main_entry_to_query_installed_ns,
                    main_entry_to_populated_library_ns: populated_library_ns,
                    ready_rss_bytes: self.ready_rss_bytes,
                }),
            idle: IdleResult {
                duration_ns: elapsed_ns(self.config.idle),
                end_rss_bytes: self.idle_end_rss_bytes,
            },
            sort_interactions: SortInteractionsResult::new(
                self.config.sort_iterations,
                &self.sort_to_paint_samples_ns,
                self.sort_first_book_ids,
            ),
            asset_actions: AssetActionsResult::new(
                self.config.asset_action_iterations,
                &self.asset_action_to_paint_samples_ns,
            ),
            scrolling: ScrollingResult::new(
                self.config,
                self.observed_scroll_duration_ns,
                &ScrollingSamples {
                    frame_intervals: &self.frame_intervals_ns,
                    egui_unstable_dt: &self.egui_frame_intervals_ns,
                    cpu_frame_times: &self.cpu_frame_times_ns,
                },
            ),
            memory: MemoryResult {
                definition: "Linux process resident set size sampled from /proc/self/status; excludes dedicated GPU memory",
                idle_peak_window: "from the populated-library endpoint through the configured idle window; pending cover work may complete",
                sample_interval_ms: u64::try_from(MEMORY_SAMPLE_INTERVAL.as_millis())
                    .expect("memory interval fits u64"),
                process_baseline_rss_bytes: memory.baseline_bytes,
                startup_peak_rss_bytes: memory.peaks[STARTUP_PHASE],
                idle_peak_rss_bytes: memory.peaks[IDLE_PHASE],
                sort_peak_rss_bytes: memory.peaks[SORT_PHASE],
                scrolling_peak_rss_bytes: memory.peaks[SCROLL_PHASE],
            },
            final_frame: FinalFrameResult {
                viewport_width: frame.viewport_width,
                viewport_height: frame.viewport_height,
                pixels_per_point: frame.pixels_per_point,
                cached_covers: frame.cached_covers,
                pending_covers: frame.pending_covers,
                missing_covers: frame.missing_covers,
            },
        };
        write_json(&self.output_path, &result)
    }

    fn interaction_budget_failure(&self) -> Option<String> {
        interaction_budget_failure(
            "sort",
            self.config.sort_iterations,
            &self.sort_to_paint_samples_ns,
            |index| sort_name(SORT_SCENARIOS[index]),
            SORT_TO_PAINT_P95_BUDGET_NS,
        )
        .or_else(|| {
            interaction_budget_failure(
                "asset action",
                self.config.asset_action_iterations,
                &self.asset_action_to_paint_samples_ns,
                |index| asset_action_name(ASSET_ACTIONS[index]),
                ASSET_ACTION_TO_PAINT_P95_BUDGET_NS,
            )
        })
    }
}

fn interaction_budget_failure<const N: usize>(
    kind: &str,
    expected_samples: usize,
    samples_by_scenario: &[Vec<u64>; N],
    name: impl Fn(usize) -> &'static str,
    p95_budget_ns: u64,
) -> Option<String> {
    if expected_samples == 0 {
        return None;
    }
    for (index, samples) in samples_by_scenario.iter().enumerate() {
        if samples.len() != expected_samples {
            return Some(format!(
                "{kind} {} retained {} samples; expected {expected_samples}",
                name(index),
                samples.len(),
            ));
        }
        let summary = summarize_samples(samples).expect("non-empty interaction samples");
        if summary.p95_ns > p95_budget_ns {
            return Some(format!(
                "{kind} {} p95 was {:.3} ms, above the {:.3} ms budget",
                name(index),
                summary.p95_ns as f64 / 1_000_000.0,
                p95_budget_ns as f64 / 1_000_000.0,
            ));
        }
    }
    None
}

struct ScrollingSamples<'a> {
    frame_intervals: &'a [u64],
    egui_unstable_dt: &'a [u64],
    cpu_frame_times: &'a [u64],
}

impl<'a> SortInteractionsResult<'a> {
    fn new(
        iterations_per_sort: usize,
        samples: &'a [Vec<u64>; SORT_SCENARIOS.len()],
        first_book_ids: [Option<i64>; SORT_SCENARIOS.len()],
    ) -> Self {
        Self {
            endpoint: "query refresh requested through the next app frame after the sorted page was installed, ensuring one populated frame was presented",
            iterations_per_sort,
            max_p95_ns: SORT_TO_PAINT_P95_BUDGET_NS,
            scenarios: array::from_fn(|index| {
                let latency = summarize_samples(&samples[index]);
                let passed = iterations_per_sort == 0
                    || (samples[index].len() == iterations_per_sort
                        && latency
                            .as_ref()
                            .is_some_and(|summary| summary.p95_ns <= SORT_TO_PAINT_P95_BUDGET_NS));
                SortInteractionScenario {
                    name: sort_name(SORT_SCENARIOS[index]),
                    first_book_id: first_book_ids[index],
                    latency,
                    samples_ns: &samples[index],
                    passed,
                }
            }),
        }
    }
}

impl<'a> AssetActionsResult<'a> {
    fn new(iterations_per_action: usize, samples: &'a [Vec<u64>; ASSET_ACTIONS.len()]) -> Self {
        Self {
            endpoint: "request dispatch through an injected no-op platform adapter to the next fully rendered frame",
            iterations_per_action,
            max_p95_ns: ASSET_ACTION_TO_PAINT_P95_BUDGET_NS,
            scenarios: array::from_fn(|index| {
                let latency = summarize_samples(&samples[index]);
                let passed = iterations_per_action == 0
                    || (samples[index].len() == iterations_per_action
                        && latency.as_ref().is_some_and(|summary| {
                            summary.p95_ns <= ASSET_ACTION_TO_PAINT_P95_BUDGET_NS
                        }));
                AssetActionScenario {
                    name: asset_action_name(ASSET_ACTIONS[index]),
                    latency,
                    samples_ns: &samples[index],
                    passed,
                }
            }),
        }
    }
}

fn sort_for_interaction(completed: usize) -> SortOrder {
    SORT_SCENARIOS[completed % SORT_SCENARIOS.len()]
}

const fn sort_index(sort: SortOrder) -> usize {
    match sort {
        SortOrder::Author => 0,
        SortOrder::RecentlyAdded => 1,
        SortOrder::Title => 2,
        SortOrder::Series => 3,
    }
}

const fn sort_name(sort: SortOrder) -> &'static str {
    match sort {
        SortOrder::Author => "author",
        SortOrder::RecentlyAdded => "recently_added",
        SortOrder::Title => "title",
        SortOrder::Series => "series",
    }
}

const fn asset_action_index(action: PlatformAction) -> usize {
    match action {
        PlatformAction::Open => 0,
        PlatformAction::Reveal => 1,
    }
}

const fn asset_action_name(action: PlatformAction) -> &'static str {
    match action {
        PlatformAction::Open => "open",
        PlatformAction::Reveal => "reveal",
    }
}

impl<'a> ScrollingResult<'a> {
    fn new(
        config: BenchmarkConfig,
        observed_duration_ns: Option<u64>,
        samples: &ScrollingSamples<'a>,
    ) -> Self {
        let configured_measured_duration = config.scroll.saturating_sub(config.scroll_warmup);
        let observed_duration = observed_duration_ns.map(Duration::from_nanos);
        let observed_measured_duration =
            observed_duration.map(|duration| duration.saturating_sub(config.scroll_warmup));
        Self {
            configured_duration_ns: elapsed_ns(config.scroll),
            observed_duration_ns,
            configured_measured_duration_ns: elapsed_ns(configured_measured_duration),
            observed_measured_duration_ns: observed_measured_duration.and_then(elapsed_ns),
            configured_speed_pixels_per_second: config.scroll_pixels_per_second,
            configured_distance_pixels: scroll_distance(config.scroll, config),
            observed_distance_pixels: observed_duration
                .map(|duration| scroll_distance(duration, config)),
            configured_measured_distance_pixels: scroll_distance(
                configured_measured_duration,
                config,
            ),
            observed_measured_distance_pixels: observed_measured_duration
                .map(|duration| scroll_distance(duration, config)),
            frame_interval: summarize_samples(samples.frame_intervals),
            egui_unstable_dt: summarize_samples(samples.egui_unstable_dt),
            cpu_frame_time: summarize_samples(samples.cpu_frame_times),
            frame_intervals_ns: samples.frame_intervals,
            egui_unstable_dt_ns: samples.egui_unstable_dt,
            cpu_frame_times_ns: samples.cpu_frame_times,
        }
    }
}

fn scroll_distance(duration: Duration, config: BenchmarkConfig) -> f64 {
    duration.as_secs_f64() * f64::from(config.scroll_pixels_per_second)
}

#[derive(Serialize)]
struct DesktopBenchmarkResult<'a> {
    schema_version: u32,
    kind: &'static str,
    status: &'static str,
    error: Option<&'a str>,
    measured_at_unix_ms: u128,
    build: BuildMetadata,
    configuration: ConfigurationResult,
    library: LibraryResult,
    startup: Option<StartupResult>,
    idle: IdleResult,
    sort_interactions: SortInteractionsResult<'a>,
    asset_actions: AssetActionsResult<'a>,
    scrolling: ScrollingResult<'a>,
    memory: MemoryResult,
    final_frame: FinalFrameResult,
}

#[derive(Serialize)]
struct BuildMetadata {
    version: &'static str,
    profile: &'static str,
    wgpu_backend_requested: Option<String>,
    display_backend: &'static str,
}

#[derive(Serialize)]
struct ConfigurationResult {
    idle_duration_ns: Option<u64>,
    scroll_duration_ns: Option<u64>,
    scroll_warmup_ns: Option<u64>,
    timeout_ns: Option<u64>,
    scroll_pixels_per_second: f32,
    sort_iterations: usize,
    asset_action_iterations: usize,
}

impl From<BenchmarkConfig> for ConfigurationResult {
    fn from(config: BenchmarkConfig) -> Self {
        Self {
            idle_duration_ns: elapsed_ns(config.idle),
            scroll_duration_ns: elapsed_ns(config.scroll),
            scroll_warmup_ns: elapsed_ns(config.scroll_warmup),
            timeout_ns: elapsed_ns(config.timeout),
            scroll_pixels_per_second: config.scroll_pixels_per_second,
            sort_iterations: config.sort_iterations,
            asset_action_iterations: config.asset_action_iterations,
        }
    }
}

#[derive(Serialize)]
struct LibraryResult {
    books: u64,
    initial_page_books: usize,
    initial_page_books_with_covers: usize,
}

#[derive(Serialize)]
struct StartupResult {
    main_entry_to_query_installed_ns: Option<u64>,
    main_entry_to_populated_library_ns: u64,
    ready_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct IdleResult {
    duration_ns: Option<u64>,
    end_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct SortInteractionsResult<'a> {
    endpoint: &'static str,
    iterations_per_sort: usize,
    max_p95_ns: u64,
    scenarios: [SortInteractionScenario<'a>; SORT_SCENARIOS.len()],
}

#[derive(Serialize)]
struct SortInteractionScenario<'a> {
    name: &'static str,
    first_book_id: Option<i64>,
    latency: Option<SampleSummary>,
    samples_ns: &'a [u64],
    passed: bool,
}

#[derive(Serialize)]
struct AssetActionsResult<'a> {
    endpoint: &'static str,
    iterations_per_action: usize,
    max_p95_ns: u64,
    scenarios: [AssetActionScenario<'a>; ASSET_ACTIONS.len()],
}

#[derive(Serialize)]
struct AssetActionScenario<'a> {
    name: &'static str,
    latency: Option<SampleSummary>,
    samples_ns: &'a [u64],
    passed: bool,
}

#[derive(Serialize)]
struct ScrollingResult<'a> {
    configured_duration_ns: Option<u64>,
    observed_duration_ns: Option<u64>,
    configured_measured_duration_ns: Option<u64>,
    observed_measured_duration_ns: Option<u64>,
    configured_speed_pixels_per_second: f32,
    configured_distance_pixels: f64,
    observed_distance_pixels: Option<f64>,
    configured_measured_distance_pixels: f64,
    observed_measured_distance_pixels: Option<f64>,
    frame_interval: Option<SampleSummary>,
    egui_unstable_dt: Option<SampleSummary>,
    cpu_frame_time: Option<SampleSummary>,
    frame_intervals_ns: &'a [u64],
    egui_unstable_dt_ns: &'a [u64],
    cpu_frame_times_ns: &'a [u64],
}

#[derive(Serialize)]
struct SampleSummary {
    count: usize,
    min_ns: u64,
    mean_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
}

#[derive(Serialize)]
struct MemoryResult {
    definition: &'static str,
    idle_peak_window: &'static str,
    sample_interval_ms: u64,
    process_baseline_rss_bytes: Option<u64>,
    startup_peak_rss_bytes: Option<u64>,
    idle_peak_rss_bytes: Option<u64>,
    sort_peak_rss_bytes: Option<u64>,
    scrolling_peak_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct FinalFrameResult {
    viewport_width: f32,
    viewport_height: f32,
    pixels_per_point: f32,
    cached_covers: usize,
    pending_covers: usize,
    missing_covers: usize,
}

struct MemorySamples {
    baseline_bytes: Option<u64>,
    peaks: [Option<u64>; PHASE_COUNT],
}

struct PhaseMemorySampler {
    phase: Arc<AtomicUsize>,
    peaks: Arc<[AtomicU64; PHASE_COUNT]>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    baseline_bytes: Option<u64>,
}

impl PhaseMemorySampler {
    fn start(interval: Duration) -> Result<Self, String> {
        let baseline_bytes = current_rss_bytes();
        let phase = Arc::new(AtomicUsize::new(STARTUP_PHASE));
        let peaks = Arc::new(array::from_fn(|_| AtomicU64::new(0)));
        if let Some(baseline) = baseline_bytes {
            peaks[STARTUP_PHASE].store(baseline, Ordering::Relaxed);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_phase = Arc::clone(&phase);
        let thread_peaks = Arc::clone(&peaks);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("lectern-desktop-benchmark-memory".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    if let Some(rss) = current_rss_bytes() {
                        let phase = thread_phase.load(Ordering::Relaxed).min(PHASE_COUNT - 1);
                        thread_peaks[phase].fetch_max(rss, Ordering::Relaxed);
                    }
                    thread::sleep(interval);
                }
            })
            .map_err(display_error)?;
        Ok(Self {
            phase,
            peaks,
            stop,
            thread: Some(thread),
            baseline_bytes,
        })
    }

    fn set_phase(&self, phase: usize) {
        self.phase.store(phase, Ordering::Relaxed);
        self.record_sample(phase, current_rss_bytes());
    }

    fn record_sample(&self, phase: usize, rss: Option<u64>) {
        if let Some(rss) = rss {
            self.peaks[phase].fetch_max(rss, Ordering::Relaxed);
        }
    }

    fn finish(mut self) -> Result<MemorySamples, String> {
        let phase = self.phase.load(Ordering::Relaxed).min(PHASE_COUNT - 1);
        self.record_sample(phase, current_rss_bytes());
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "desktop memory sampler thread panicked".to_owned())?;
        }
        let peaks = array::from_fn(|phase| {
            let peak = self.peaks[phase].load(Ordering::Relaxed);
            (peak > 0).then_some(peak)
        });
        Ok(MemorySamples {
            baseline_bytes: self.baseline_bytes,
            peaks,
        })
    }
}

fn summarize_samples(samples: &[u64]) -> Option<SampleSummary> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let total = sorted
        .iter()
        .map(|&sample| u128::from(sample))
        .sum::<u128>();
    let count = u128::try_from(sorted.len()).expect("sample count fits u128");
    Some(SampleSummary {
        count: sorted.len(),
        min_ns: sorted[0],
        mean_ns: u64::try_from(total / count).expect("mean sample fits u64"),
        p50_ns: nearest_rank(&sorted, 50),
        p95_ns: nearest_rank(&sorted, 95),
        p99_ns: nearest_rank(&sorted, 99),
        max_ns: *sorted.last().expect("non-empty samples"),
    })
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn duration_from_env(name: &str, default_seconds: f32) -> Result<Duration, String> {
    number_from_env(name, default_seconds).and_then(|seconds| {
        if seconds < 0.0 {
            Err(format!("{name} must be non-negative"))
        } else {
            Ok(Duration::from_secs_f32(seconds))
        }
    })
}

fn number_from_env(name: &str, default: f32) -> Result<f32, String> {
    let Some(value) = env::var_os(name) else {
        return Ok(default);
    };
    let value = value
        .into_string()
        .map_err(|value| format!("{name} is not valid UTF-8: {}", value.display()))?;
    let parsed = value
        .parse::<f32>()
        .map_err(|_| format!("{name} expects a number, got '{value}'"))?;
    if !parsed.is_finite() {
        return Err(format!("{name} must be finite"));
    }
    Ok(parsed)
}

fn usize_from_env(name: &str, default: usize) -> Result<usize, String> {
    let Some(value) = env::var_os(name) else {
        return Ok(default);
    };
    let value = value
        .into_string()
        .map_err(|value| format!("{name} is not valid UTF-8: {}", value.display()))?;
    value
        .parse::<usize>()
        .map_err(|_| format!("{name} expects a non-negative integer, got '{value}'"))
}

fn seconds_f32_ns(seconds: f32) -> Option<u64> {
    elapsed_ns(Duration::from_secs_f32(seconds))
}

fn elapsed_ns(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_nanos()).ok()
}

fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kilobytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1_024)
}

fn display_backend() -> &'static str {
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        "wayland"
    } else if env::var_os("DISPLAY").is_some() {
        "x11"
    } else {
        "unknown"
    }
}

fn unix_time_ms() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(display_error)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
    let mut temporary_name = path.as_os_str().to_os_string();
    temporary_name.push(".tmp");
    let temporary_path = PathBuf::from(temporary_name);
    let file = File::create(&temporary_path).map_err(display_error)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value).map_err(display_error)?;
    writer.write_all(b"\n").map_err(display_error)?;
    writer.flush().map_err(display_error)?;
    drop(writer);
    fs::rename(&temporary_path, path).map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        BenchmarkConfig, ScrollingResult, ScrollingSamples, nearest_rank, sort_for_interaction,
        summarize_samples,
    };
    use lectern_core::SortOrder;

    #[test]
    fn summarizes_frame_samples_with_nearest_rank_percentiles() {
        let samples = (1..=100).collect::<Vec<_>>();
        let summary = summarize_samples(&samples).expect("sample summary");

        assert_eq!(summary.count, 100);
        assert_eq!(summary.mean_ns, 50);
        assert_eq!(summary.p50_ns, 50);
        assert_eq!(summary.p95_ns, 95);
        assert_eq!(summary.p99_ns, 99);
    }

    #[test]
    fn nearest_rank_handles_small_samples() {
        assert_eq!(nearest_rank(&[7], 99), 7);
        assert_eq!(nearest_rank(&[4, 9], 50), 4);
        assert_eq!(nearest_rank(&[4, 9], 99), 9);
    }

    #[test]
    fn sort_interactions_cycle_without_repeating_the_current_order() {
        assert_eq!(sort_for_interaction(0), SortOrder::Author);
        assert_eq!(sort_for_interaction(1), SortOrder::RecentlyAdded);
        assert_eq!(sort_for_interaction(2), SortOrder::Title);
        assert_eq!(sort_for_interaction(3), SortOrder::Author);
    }

    #[test]
    fn scrolling_reports_configured_and_observed_windows() {
        let config = BenchmarkConfig {
            idle: Duration::from_secs(3),
            scroll: Duration::from_secs(10),
            scroll_warmup: Duration::from_secs(2),
            timeout: Duration::from_secs(30),
            scroll_pixels_per_second: 1_500.0,
            sort_iterations: 40,
            asset_action_iterations: 40,
        };
        let result = ScrollingResult::new(
            config,
            Some(10_100_000_000),
            &ScrollingSamples {
                frame_intervals: &[],
                egui_unstable_dt: &[],
                cpu_frame_times: &[],
            },
        );

        assert_eq!(result.configured_duration_ns, Some(10_000_000_000));
        assert_eq!(result.observed_duration_ns, Some(10_100_000_000));
        assert_eq!(result.configured_measured_duration_ns, Some(8_000_000_000));
        assert_eq!(result.observed_measured_duration_ns, Some(8_100_000_000));
        assert!((result.configured_distance_pixels - 15_000.0).abs() < f64::EPSILON);
        assert!(
            (result.observed_distance_pixels.expect("observed distance") - 15_150.0).abs()
                < f64::EPSILON
        );
        assert!((result.configured_measured_distance_pixels - 12_000.0).abs() < f64::EPSILON);
        assert!(
            (result
                .observed_measured_distance_pixels
                .expect("observed measured distance")
                - 12_150.0)
                .abs()
                < f64::EPSILON
        );
    }
}
