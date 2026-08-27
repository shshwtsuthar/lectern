//! Deterministic release-mode Kobo discovery and 120-book transfer workload.

use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use lectern_core::{AssetId, BookFormat, BookId};
use lectern_device::{
    DeviceError, DeviceManager, DeviceTransferBook, DeviceTransferSource, DuplicatePolicy,
    FormatPriority, MountedVolume, RemovableStorageProvider, SystemRemovableStorageProvider,
    TransferControl,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir_in};

const SOURCE_BYTES: usize = 1024 * 1024;
const RECONCILIATION_VOLUMES: usize = 32;

#[derive(Clone)]
struct FixtureProvider {
    volumes: Arc<Mutex<Vec<MountedVolume>>>,
}

impl RemovableStorageProvider for FixtureProvider {
    fn list_mounted_volumes(&self) -> Result<Vec<MountedVolume>, DeviceError> {
        Ok(self.volumes.lock().expect("fixture volumes").clone())
    }

    fn stable_volume_id(&self, _volume: &MountedVolume) -> Option<String> {
        Some("benchmark-kobo-volume".to_owned())
    }

    fn eject(&self, _device: &lectern_device::DeviceInfo) -> Result<(), DeviceError> {
        Err(DeviceError::Platform(
            "benchmark provider does not eject".to_owned(),
        ))
    }
}

#[derive(Serialize)]
struct BenchmarkResult {
    schema_version: u32,
    workload: &'static str,
    books: usize,
    source_bytes_per_book: usize,
    warmup_iterations: usize,
    measured_iterations: usize,
    system_enumeration: SystemEnumerationResult,
    reconciliation: ReconciliationResult,
    transfer: TransferResult,
}

#[derive(Serialize)]
struct SystemEnumerationResult {
    minimum_mounted_volumes: usize,
    maximum_mounted_volumes: usize,
    p95_ms: f64,
    samples_ns: Vec<u64>,
    correctness: Vec<&'static str>,
}

#[derive(Serialize)]
struct ReconciliationResult {
    candidate_volumes: usize,
    detected_devices: usize,
    p95_ms: f64,
    samples_ns: Vec<u64>,
    correctness: Vec<&'static str>,
}

#[derive(Serialize)]
struct TransferResult {
    transferred_books_per_iteration: usize,
    transferred_bytes_per_iteration: u64,
    p95_ms: f64,
    p05_throughput_mib_per_second: f64,
    samples_ns: Vec<u64>,
    throughput_mib_per_second: Vec<f64>,
    peak_rss_delta_bytes: Option<u64>,
    correctness: Vec<&'static str>,
}

struct Options {
    output: PathBuf,
    books: usize,
    warmup: usize,
    iterations: usize,
}

fn main() {
    let options = parse_options().unwrap_or_else(|error| {
        eprintln!("error: {error}");
        process::exit(2);
    });
    if let Err(error) = run(&options) {
        eprintln!("error: {error}");
        process::exit(2);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the benchmark keeps fixture construction, measurement, and correctness together"
)]
fn run(options: &Options) -> Result<(), String> {
    let output_parent = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent).map_err(|error| error.to_string())?;
    let system_provider = SystemRemovableStorageProvider;
    let mut system_samples = Vec::with_capacity(100);
    let mut system_volume_counts = Vec::with_capacity(110);
    for iteration in 0..110 {
        let started = Instant::now();
        let volumes = system_provider
            .list_mounted_volumes()
            .map_err(|error| error.to_string())?;
        let elapsed = u64::try_from(started.elapsed().as_nanos())
            .map_err(|error| format!("system enumeration duration overflowed: {error}"))?;
        system_volume_counts.push(volumes.len());
        if iteration >= 10 {
            system_samples.push(elapsed);
        }
    }
    let fixture = tempdir_in(output_parent).map_err(|error| error.to_string())?;
    let sources_root = fixture.path().join("sources");
    fs::create_dir(&sources_root).map_err(|error| error.to_string())?;
    let source_payload = vec![0xA5_u8; SOURCE_BYTES];
    let mut books = Vec::with_capacity(options.books);
    for index in 0..options.books {
        let path = sources_root.join(format!("source-{index:04}.epub"));
        fs::write(&path, &source_payload).map_err(|error| error.to_string())?;
        books.push(DeviceTransferBook {
            book_id: BookId::new(i64::try_from(index + 1).map_err(|error| error.to_string())?),
            title: format!("Benchmark book {index:04}"),
            authors: format!("Benchmark author {:02}", index % 12),
            sources: vec![DeviceTransferSource {
                asset_id: AssetId::new(
                    i64::try_from(index + 10_000).map_err(|error| error.to_string())?,
                ),
                format: BookFormat::Epub,
                path,
            }],
        });
    }

    let ordinary_root = fixture.path().join("ordinary-volumes");
    fs::create_dir(&ordinary_root).map_err(|error| error.to_string())?;
    let mut volumes = Vec::with_capacity(RECONCILIATION_VOLUMES);
    for index in 0..(RECONCILIATION_VOLUMES - 1) {
        let directory = ordinary_root.join(format!("volume-{index:02}"));
        fs::create_dir(&directory).map_err(|error| error.to_string())?;
        volumes.push(volume(&directory, format!("USB-{index:02}")));
    }
    let initial_kobo = create_kobo_mount(fixture.path())?;
    volumes.push(volume(initial_kobo.path(), "KOBOeReader".to_owned()));
    let provider = FixtureProvider {
        volumes: Arc::new(Mutex::new(volumes)),
    };
    let manager = DeviceManager::new(provider.clone(), fixture.path().join("history.json"));

    let mut reconciliation_samples = Vec::with_capacity(100);
    for iteration in 0..110 {
        let started = Instant::now();
        let result = manager.reconcile().map_err(|error| error.to_string())?;
        let elapsed = u64::try_from(started.elapsed().as_nanos())
            .map_err(|error| format!("reconciliation duration overflowed: {error}"))?;
        if result.devices.len() != 1 {
            return Err(format!(
                "reconciliation detected {} devices instead of one",
                result.devices.len()
            ));
        }
        if iteration >= 10 {
            reconciliation_samples.push(elapsed);
        }
    }

    let rss_before = peak_rss_bytes();
    for _ in 0..options.warmup {
        run_transfer_iteration(&manager, &provider, fixture.path(), &books)?;
    }
    let mut transfer_samples = Vec::with_capacity(options.iterations);
    let mut throughput = Vec::with_capacity(options.iterations);
    let transfer_bytes = u64::try_from(options.books)
        .map_err(|error| error.to_string())?
        .saturating_mul(u64::try_from(SOURCE_BYTES).expect("source bytes fit u64"));
    for _ in 0..options.iterations {
        let started = Instant::now();
        run_transfer_iteration(&manager, &provider, fixture.path(), &books)?;
        let elapsed = started.elapsed();
        transfer_samples.push(
            u64::try_from(elapsed.as_nanos())
                .map_err(|error| format!("transfer duration overflowed: {error}"))?,
        );
        let transfer_mib = u32::try_from(transfer_bytes / (1024 * 1024))
            .map_err(|error| format!("transfer MiB overflowed: {error}"))?;
        throughput.push(f64::from(transfer_mib) / elapsed.as_secs_f64());
    }
    let peak_rss_delta_bytes = rss_before
        .zip(peak_rss_bytes())
        .map(|(before, after)| after.saturating_sub(before));
    let result = BenchmarkResult {
        schema_version: 1,
        workload: "kobo-device-v1",
        books: options.books,
        source_bytes_per_book: SOURCE_BYTES,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        system_enumeration: SystemEnumerationResult {
            minimum_mounted_volumes: system_volume_counts.iter().copied().min().unwrap_or(0),
            maximum_mounted_volumes: system_volume_counts.iter().copied().max().unwrap_or(0),
            p95_ms: Duration::from_nanos(percentile_ns(&system_samples, 95)).as_secs_f64() * 1000.0,
            samples_ns: system_samples,
            correctness: vec![
                "production_volume_provider_exercised",
                "stable_system_sample_count",
            ],
        },
        reconciliation: ReconciliationResult {
            candidate_volumes: RECONCILIATION_VOLUMES,
            detected_devices: 1,
            p95_ms: Duration::from_nanos(percentile_ns(&reconciliation_samples, 95)).as_secs_f64()
                * 1000.0,
            samples_ns: reconciliation_samples,
            correctness: vec![
                "ordinary_volumes_rejected",
                "single_marker_volume_detected",
                "stable_reconciliation_count",
            ],
        },
        transfer: TransferResult {
            transferred_books_per_iteration: options.books,
            transferred_bytes_per_iteration: transfer_bytes,
            p95_ms: Duration::from_nanos(percentile_ns(&transfer_samples, 95)).as_secs_f64()
                * 1000.0,
            p05_throughput_mib_per_second: percentile_f64(&throughput, 5),
            samples_ns: transfer_samples,
            throughput_mib_per_second: throughput,
            peak_rss_delta_bytes,
            correctness: vec![
                "all_books_transferred",
                "exact_source_hashes_preserved",
                "no_partial_files_retained",
                "history_reconciled",
            ],
        },
    };
    let bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    let mut output = File::create(&options.output).map_err(|error| error.to_string())?;
    output
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())?;
    Ok(())
}

fn run_transfer_iteration(
    manager: &DeviceManager<FixtureProvider>,
    provider: &FixtureProvider,
    fixture_root: &Path,
    books: &[DeviceTransferBook],
) -> Result<(), String> {
    let mount = create_kobo_mount(fixture_root)?;
    *provider.volumes.lock().expect("fixture volumes") =
        vec![volume(mount.path(), "KOBOeReader".to_owned())];
    let reconciled = manager.reconcile().map_err(|error| error.to_string())?;
    let device = reconciled
        .devices
        .first()
        .ok_or_else(|| "benchmark Kobo was not detected".to_owned())?;
    let plan = manager
        .plan_transfer(&device.id, books, &FormatPriority::default())
        .map_err(|error| error.to_string())?;
    if !plan.failures.is_empty() || plan.items.len() != books.len() {
        return Err("transfer preflight did not retain every book".to_owned());
    }
    let outcome = manager
        .transfer(&plan, DuplicatePolicy::Skip, |_| TransferControl::Continue)
        .map_err(|error| error.to_string())?;
    if outcome.transferred_count() != books.len()
        || !outcome.failures.is_empty()
        || outcome.history_error.is_some()
    {
        return Err("transfer outcome failed correctness reconciliation".to_owned());
    }
    for (book, item) in books.iter().zip(&plan.items) {
        let source = &book.sources[0].path;
        let destination = device.mount_path.join(&item.relative_path);
        if file_hash(source)? != file_hash(&destination)? {
            return Err(format!(
                "hash mismatch for {}",
                item.relative_path.display()
            ));
        }
    }
    if walk_partial_files(&device.mount_path)? != 0 {
        return Err("partial transfer files remained on the device".to_owned());
    }
    let listed = manager
        .list_books(&device.id)
        .map_err(|error| error.to_string())?;
    if listed.len() != books.len() || listed.iter().any(|book| !book.managed_by_lectern) {
        return Err("device listing did not reconcile transfer history".to_owned());
    }
    drop(mount);
    Ok(())
}

fn create_kobo_mount(parent: &Path) -> Result<TempDir, String> {
    let mount = tempdir_in(parent).map_err(|error| error.to_string())?;
    fs::create_dir(mount.path().join(".kobo")).map_err(|error| error.to_string())?;
    fs::write(
        mount.path().join(".kobo/version"),
        "BENCHMARK-SERIAL-001,4.41.0\n",
    )
    .map_err(|error| error.to_string())?;
    Ok(mount)
}

fn volume(path: &Path, name: String) -> MountedVolume {
    MountedVolume {
        name: OsString::from(name),
        mount_path: path.to_path_buf(),
        file_system: OsString::from("vfat"),
        total_bytes: 4 * 1024 * 1024 * 1024,
        free_bytes: 3 * 1024 * 1024 * 1024,
        removable: true,
    }
}

fn file_hash(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut hash = Sha256::new();
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().to_vec())
}

fn walk_partial_files(root: &Path) -> Result<usize, String> {
    let mut partials = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".lectern-transfer-")
            {
                partials += 1;
            }
        }
    }
    Ok(partials)
}

fn percentile_ns(samples: &[u64], percentile: usize) -> u64 {
    let mut values = samples.to_vec();
    values.sort_unstable();
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values[index]
}

fn percentile_f64(samples: &[f64], percentile: usize) -> f64 {
    let mut values = samples.to_vec();
    values.sort_by(f64::total_cmp);
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values[index]
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    kibibytes.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

fn parse_options() -> Result<Options, String> {
    let mut arguments = env::args_os().skip(1);
    let mut output = None;
    let mut books = 120;
    let mut warmup = 1;
    let mut iterations = 10;
    while let Some(argument) = arguments.next() {
        match argument.to_string_lossy().as_ref() {
            "--output" => output = arguments.next().map(PathBuf::from),
            "--books" => books = parse_usize(arguments.next(), "--books")?,
            "--warmup" => warmup = parse_usize(arguments.next(), "--warmup")?,
            "--iterations" => iterations = parse_usize(arguments.next(), "--iterations")?,
            unknown => return Err(format!("unknown argument {unknown}")),
        }
    }
    let output = output.ok_or_else(|| "--output is required".to_owned())?;
    if books == 0 || iterations == 0 {
        return Err("books and iterations must be greater than zero".to_owned());
    }
    Ok(Options {
        output,
        books,
        warmup,
        iterations,
    })
}

fn parse_usize(value: Option<OsString>, option: &str) -> Result<usize, String> {
    value
        .ok_or_else(|| format!("{option} requires a value"))?
        .to_string_lossy()
        .parse::<usize>()
        .map_err(|error| format!("invalid {option}: {error}"))
}
