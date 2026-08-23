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
use lectern_core::BookSummary;
use serde::Serialize;

const OUTPUT_ENV: &str = "LECTERN_BENCHMARK_OUTPUT";
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(20);
const STARTUP_PHASE: usize = 0;
const IDLE_PHASE: usize = 1;
const SCROLL_PHASE: usize = 2;
const PHASE_COUNT: usize = 3;

pub(crate) struct BenchmarkFrame {
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
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
    Startup { paint_frames_remaining: Option<u8> },
    Idle { started: Instant },
    Scroll { started: Instant },
    Finished,
}

pub(crate) struct DesktopBenchmark {
    output_path: PathBuf,
    config: BenchmarkConfig,
    process_started: Instant,
    phase: Phase,
    last_frame_started: Option<Instant>,
    query_installed_ns: Option<u64>,
    populated_ns: Option<u64>,
    ready_rss_bytes: Option<u64>,
    idle_end_rss_bytes: Option<u64>,
    library_books: usize,
    books_with_covers: usize,
    frame_intervals_ns: Vec<u64>,
    egui_frame_intervals_ns: Vec<u64>,
    cpu_frame_times_ns: Vec<u64>,
    memory: Option<PhaseMemorySampler>,
}

impl DesktopBenchmark {
    pub(crate) fn from_environment(process_started: Instant) -> Result<Option<Self>, String> {
        let Some(output_path) = env::var_os(OUTPUT_ENV).map(PathBuf::from) else {
            return Ok(None);
        };
        let config = BenchmarkConfig::from_environment()?;
        let memory = PhaseMemorySampler::start(MEMORY_SAMPLE_INTERVAL)?;
        Ok(Some(Self {
            output_path,
            config,
            process_started,
            phase: Phase::Startup {
                paint_frames_remaining: None,
            },
            last_frame_started: None,
            query_installed_ns: None,
            populated_ns: None,
            ready_rss_bytes: None,
            idle_end_rss_bytes: None,
            library_books: 0,
            books_with_covers: 0,
            frame_intervals_ns: Vec::new(),
            egui_frame_intervals_ns: Vec::new(),
            cpu_frame_times_ns: Vec::new(),
            memory: Some(memory),
        }))
    }

    pub(crate) fn library_installed(&mut self, books: &[BookSummary]) {
        let Phase::Startup {
            paint_frames_remaining,
        } = &mut self.phase
        else {
            return;
        };
        if books.is_empty() || paint_frames_remaining.is_some() {
            return;
        }
        self.library_books = books.len();
        self.books_with_covers = books.iter().filter(|book| book.has_cover).count();
        self.query_installed_ns = elapsed_ns(self.process_started.elapsed());
        *paint_frames_remaining = Some(1);
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
        let mut begin_scroll = false;
        let mut finish = false;
        let mut failure = None;

        match &mut self.phase {
            Phase::Startup {
                paint_frames_remaining: Some(remaining),
            } if *remaining > 0 => {
                *remaining -= 1;
                context.request_repaint();
            }
            Phase::Startup {
                paint_frames_remaining: Some(_),
            } => begin_idle = true,
            Phase::Startup { .. }
                if now.duration_since(self.process_started) >= self.config.timeout =>
            {
                failure = Some("populated library was not rendered before timeout".to_owned());
            }
            Phase::Idle { started } if now.duration_since(*started) >= self.config.idle => {
                if self.config.scroll.is_zero() {
                    self.idle_end_rss_bytes = current_rss_bytes();
                    finish = true;
                } else {
                    begin_scroll = true;
                }
            }
            Phase::Startup { .. } | Phase::Idle { .. } => {
                context.request_repaint_after(MEMORY_SAMPLE_INTERVAL);
            }
            Phase::Scroll { started } if now.duration_since(*started) >= self.config.scroll => {
                finish = true;
            }
            Phase::Scroll { .. } => context.request_repaint(),
            Phase::Finished => {}
        }

        if begin_idle {
            self.populated_ns = elapsed_ns(now.duration_since(self.process_started));
            self.ready_rss_bytes = current_rss_bytes();
            if let Some(memory) = &self.memory {
                memory.record_sample(STARTUP_PHASE, self.ready_rss_bytes);
                memory.set_phase(IDLE_PHASE);
            }
            self.phase = Phase::Idle { started: now };
            context.request_repaint_after(MEMORY_SAMPLE_INTERVAL);
        } else if begin_scroll {
            self.idle_end_rss_bytes = current_rss_bytes();
            if let Some(memory) = &self.memory {
                memory.record_sample(IDLE_PHASE, self.idle_end_rss_bytes);
                memory.set_phase(SCROLL_PHASE);
            }
            self.phase = Phase::Scroll { started: now };
            context.request_repaint();
        } else if finish || failure.is_some() {
            self.phase = Phase::Finished;
            if let Err(error) = self.write_result(failure.as_deref(), frame) {
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
            schema_version: 1,
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
                books_with_covers: self.books_with_covers,
            },
            startup: self.populated_ns.map(|populated_ns| StartupResult {
                query_installed_ns: self.query_installed_ns,
                populated_library_ns: populated_ns,
                ready_rss_bytes: self.ready_rss_bytes,
            }),
            idle: IdleResult {
                duration_ns: elapsed_ns(self.config.idle),
                end_rss_bytes: self.idle_end_rss_bytes,
            },
            scrolling: ScrollingResult {
                measured_duration_ns: elapsed_ns(
                    self.config.scroll.saturating_sub(self.config.scroll_warmup),
                ),
                configured_speed_pixels_per_second: self.config.scroll_pixels_per_second,
                configured_distance_pixels: self.config.scroll.as_secs_f32()
                    * self.config.scroll_pixels_per_second,
                frame_interval: summarize_samples(&self.frame_intervals_ns),
                egui_unstable_dt: summarize_samples(&self.egui_frame_intervals_ns),
                cpu_frame_time: summarize_samples(&self.cpu_frame_times_ns),
                frame_intervals_ns: &self.frame_intervals_ns,
                egui_unstable_dt_ns: &self.egui_frame_intervals_ns,
                cpu_frame_times_ns: &self.cpu_frame_times_ns,
            },
            memory: MemoryResult {
                definition: "Linux process resident set size sampled from /proc/self/status; excludes dedicated GPU memory",
                sample_interval_ms: u64::try_from(MEMORY_SAMPLE_INTERVAL.as_millis())
                    .expect("memory interval fits u64"),
                process_baseline_rss_bytes: memory.baseline_bytes,
                startup_peak_rss_bytes: memory.peaks[STARTUP_PHASE],
                idle_peak_rss_bytes: memory.peaks[IDLE_PHASE],
                scrolling_peak_rss_bytes: memory.peaks[SCROLL_PHASE],
            },
            final_frame: FinalFrameResult {
                viewport_width: frame.viewport_width,
                viewport_height: frame.viewport_height,
                cached_covers: frame.cached_covers,
                pending_covers: frame.pending_covers,
                missing_covers: frame.missing_covers,
            },
        };
        write_json(&self.output_path, &result)
    }
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
}

impl From<BenchmarkConfig> for ConfigurationResult {
    fn from(config: BenchmarkConfig) -> Self {
        Self {
            idle_duration_ns: elapsed_ns(config.idle),
            scroll_duration_ns: elapsed_ns(config.scroll),
            scroll_warmup_ns: elapsed_ns(config.scroll_warmup),
            timeout_ns: elapsed_ns(config.timeout),
            scroll_pixels_per_second: config.scroll_pixels_per_second,
        }
    }
}

#[derive(Serialize)]
struct LibraryResult {
    books: usize,
    books_with_covers: usize,
}

#[derive(Serialize)]
struct StartupResult {
    query_installed_ns: Option<u64>,
    populated_library_ns: u64,
    ready_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct IdleResult {
    duration_ns: Option<u64>,
    end_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct ScrollingResult<'a> {
    measured_duration_ns: Option<u64>,
    configured_speed_pixels_per_second: f32,
    configured_distance_pixels: f32,
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
    sample_interval_ms: u64,
    process_baseline_rss_bytes: Option<u64>,
    startup_peak_rss_bytes: Option<u64>,
    idle_peak_rss_bytes: Option<u64>,
    scrolling_peak_rss_bytes: Option<u64>,
}

#[derive(Serialize)]
struct FinalFrameResult {
    viewport_width: f32,
    viewport_height: f32,
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
    let file = File::create(path).map_err(display_error)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value).map_err(display_error)?;
    writer.write_all(b"\n").map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{nearest_rank, summarize_samples};

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
}
