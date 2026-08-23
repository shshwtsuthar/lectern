//! Publication discovery and import pipeline for Lectern.

use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use hayro::{
    RenderCache, RenderSettings, hayro_interpret::InterpreterSettings, hayro_syntax::Pdf, render,
    vello_cpu::color::palette::css::WHITE,
};
use image::{ImageReader, Limits, codecs::jpeg::JpegEncoder};
use lectern_core::{AssetStorage, BookAssetDraft, BookFormat, BookMetadataDraft};
use lectern_storage::{BookImport, LibraryDatabase};
use lopdf::Document;
use percent_encoding::percent_decode_str;
use quick_xml::{Reader, XmlVersion, events::BytesStart, events::Event};
use rayon::prelude::*;
use thiserror::Error;
use walkdir::WalkDir;
use zip::ZipArchive;

const CONTAINER_PATH: &str = "META-INF/container.xml";
const MAX_CONTAINER_BYTES: usize = 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_COVER_BYTES: usize = 24 * 1024 * 1024;
const MAX_COVER_DIMENSION: u32 = 8_192;
const MAX_COVER_ALLOCATION: u64 = 96 * 1024 * 1024;
const MAX_PDF_BYTES: usize = 512 * 1024 * 1024;
const THUMBNAIL_WIDTH: u32 = 320;
const THUMBNAIL_HEIGHT: u32 = 480;
const IMPORT_BATCH_SIZE: usize = 64;

/// Failure returned while discovering, parsing, or persisting publications.
#[derive(Debug, Error)]
pub enum ImportError {
    /// A filesystem operation failed.
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// An archive could not be opened or read.
    #[error("invalid EPUB archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    /// Directory traversal failed.
    #[error("library discovery failed: {0}")]
    Discovery(#[from] walkdir::Error),
    /// Package XML was malformed or used an unsupported encoding.
    #[error("invalid EPUB XML: {0}")]
    Xml(String),
    /// A required publication structure was absent.
    #[error("invalid EPUB: {0}")]
    InvalidPublication(&'static str),
    /// A PDF document could not be decoded.
    #[error("invalid PDF: {0}")]
    Pdf(#[from] lopdf::Error),
    /// A required PDF structure was absent or unsupported.
    #[error("invalid PDF: {0}")]
    InvalidPdf(&'static str),
    /// A PDF exceeded the defensive in-memory parsing limit.
    #[error("PDF exceeds the {limit}-byte import limit ({size} bytes)")]
    PdfTooLarge {
        /// Actual file size in bytes.
        size: u64,
        /// Maximum accepted file size in bytes.
        limit: usize,
    },
    /// A directly parsed path did not use a supported extension.
    #[error("unsupported publication format: {0}")]
    UnsupportedFormat(PathBuf),
    /// An archive member exceeded a defensive size limit.
    #[error("EPUB entry {name} exceeds the {limit}-byte limit")]
    EntryTooLarge {
        /// Archive member name.
        name: String,
        /// Maximum accepted uncompressed size.
        limit: usize,
    },
    /// An archive path attempted to escape the publication root.
    #[error("unsafe EPUB archive path: {0}")]
    UnsafeArchivePath(String),
    /// Cover image decoding or thumbnail encoding failed.
    #[error("invalid cover image: {0}")]
    Image(#[from] image::ImageError),
    /// Imported metadata could not be stored.
    #[error("library update failed: {0}")]
    Storage(#[from] lectern_storage::StorageError),
}

/// Result type returned by import operations.
pub type Result<T> = std::result::Result<T, ImportError>;

/// One publication that could not be imported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportFailure {
    /// Source path that failed.
    pub path: PathBuf,
    /// Human-readable cause.
    pub message: String,
}

/// Monotonic progress emitted by an import job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportProgress {
    /// Number of supported publication files found before parsing began.
    pub discovered: usize,
    /// Number of files parsed or rejected so far.
    pub processed: usize,
    /// Number of files committed to the library.
    pub imported: usize,
    /// Number of files that could not be parsed.
    pub failed: usize,
}

/// Final outcome of a completed import job.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    /// Number of supported publication files found.
    pub discovered: usize,
    /// Number of files committed to the library.
    pub imported: usize,
    /// Number of files that could not be parsed.
    pub failed: usize,
    /// Per-file parse failures.
    pub failures: Vec<ImportFailure>,
}

/// Recursively discovers supported publications below `roots` without following symlinks.
///
/// # Errors
///
/// Returns an error when a directory cannot be traversed.
pub fn discover_publications(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut publications = Vec::new();

    for root in roots {
        if root.is_file() {
            if format_for_path(root).is_some() {
                publications.push(root.clone());
            }
            continue;
        }
        if !root.is_dir() {
            continue;
        }

        for entry in WalkDir::new(root).follow_links(false) {
            let entry = entry?;
            if entry.file_type().is_file() && format_for_path(entry.path()).is_some() {
                publications.push(entry.into_path());
            }
        }
    }

    publications.sort_unstable();
    publications.dedup();
    Ok(publications)
}

/// Imports discovered EPUB and PDF files into a persistent Lectern library.
///
/// Parsing and thumbnail generation run in parallel. Successful records are committed in bounded
/// transactions, while malformed publications are isolated and reported in the final summary.
///
/// # Errors
///
/// Returns an error when discovery cannot finish or the library database cannot be opened or
/// updated. Individual malformed publications are returned in [`ImportSummary::failures`].
pub fn import_paths(
    database_path: impl AsRef<Path>,
    roots: &[PathBuf],
    mut report_progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary> {
    let publications = discover_publications(roots)?;
    let discovered = publications.len();
    let mut progress = ImportProgress {
        discovered,
        ..ImportProgress::default()
    };
    report_progress(progress);

    let mut database = LibraryDatabase::open(database_path)?;
    let mut failures = Vec::new();

    for batch in publications.chunks(IMPORT_BATCH_SIZE) {
        let parsed = batch
            .par_iter()
            .map(|path| (path, parse_publication(path)))
            .collect::<Vec<_>>();
        let mut records = Vec::with_capacity(parsed.len());

        for (path, result) in parsed {
            match result {
                Ok(record) => records.push(record),
                Err(error) => failures.push(ImportFailure {
                    path: path.clone(),
                    message: error.to_string(),
                }),
            }
        }

        if !records.is_empty() {
            database.import_books(&records)?;
        }

        progress.processed += batch.len();
        progress.imported += records.len();
        progress.failed = failures.len();
        report_progress(progress);
    }

    Ok(ImportSummary {
        discovered,
        imported: progress.imported,
        failed: failures.len(),
        failures,
    })
}

/// Parses one supported publication and prepares it for storage.
///
/// # Errors
///
/// Returns an error when the path has an unsupported extension or the selected parser cannot
/// read the publication.
pub fn parse_publication(path: impl AsRef<Path>) -> Result<BookImport> {
    let path = path.as_ref();
    match format_for_path(path) {
        Some(BookFormat::Epub) => parse_epub(path),
        Some(BookFormat::Pdf) => parse_pdf(path),
        None => Err(ImportError::UnsupportedFormat(path.to_path_buf())),
    }
}

/// Parses metadata and a bounded cover thumbnail from one EPUB publication.
///
/// A corrupt or unsupported cover is ignored so otherwise valid book metadata remains importable.
///
/// # Errors
///
/// Returns an error when the file is not a readable EPUB, its container/package metadata is
/// malformed, or a required archive path is unsafe.
pub fn parse_epub(path: impl AsRef<Path>) -> Result<BookImport> {
    let path = path.as_ref();
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let container = read_entry(&mut archive, CONTAINER_PATH, MAX_CONTAINER_BYTES)?;
    let package_path = parse_container(&container)?;
    let package_path = normalize_archive_path(None, &package_path)?;
    let package = read_entry(&mut archive, &package_path, MAX_PACKAGE_BYTES)?;
    let metadata = parse_package(&package)?;

    let cover_thumbnail = metadata
        .cover_href()
        .and_then(|href| extract_cover(&mut archive, &package_path, href).ok());
    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .map_or_else(|| "Untitled".to_owned(), clean_text);

    Ok(BookImport {
        book: BookMetadataDraft {
            title: metadata.title.unwrap_or(fallback_title),
            authors: metadata.creators.join(", "),
            series: metadata.series,
            publisher: metadata.publisher,
            language: metadata.language,
            description: metadata.description,
        },
        assets: vec![BookAssetDraft {
            format: BookFormat::Epub,
            storage: AssetStorage::Reference,
            path: path.to_path_buf(),
        }],
        cover_thumbnail,
    })
}

/// Parses standard document metadata and a first-page thumbnail from one PDF publication.
///
/// A page that cannot be rendered is imported without a thumbnail so readable metadata is not
/// lost. Password-protected documents are currently rejected because Lectern has no password
/// prompt yet.
///
/// # Errors
///
/// Returns an error when the file is too large for bounded in-memory parsing, is not a readable
/// PDF, contains no pages, or is password protected.
pub fn parse_pdf(path: impl AsRef<Path>) -> Result<BookImport> {
    let path = path.as_ref();
    let bytes = read_bounded_file(path, MAX_PDF_BYTES)?;
    let metadata = Document::load_metadata_mem(&bytes)?;
    if metadata.encrypted {
        return Err(ImportError::InvalidPdf(
            "password-protected documents are not supported",
        ));
    }
    if metadata.page_count == 0 {
        return Err(ImportError::InvalidPdf("document contains no pages"));
    }

    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .map_or_else(|| "Untitled".to_owned(), clean_text);
    let title = clean_optional(metadata.title).unwrap_or(fallback_title);
    let authors = clean_optional(metadata.author).unwrap_or_default();
    let description = clean_optional(metadata.subject);
    let cover_thumbnail = render_pdf_cover(bytes);

    Ok(BookImport {
        book: BookMetadataDraft {
            title,
            authors,
            series: None,
            publisher: None,
            language: None,
            description,
        },
        assets: vec![BookAssetDraft {
            format: BookFormat::Pdf,
            storage: AssetStorage::Reference,
            path: path.to_path_buf(),
        }],
        cover_thumbnail,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextField {
    Title,
    Creator,
    Language,
    Publisher,
    Description,
    Series,
}

impl TextField {
    const fn end_name(self) -> &'static [u8] {
        match self {
            Self::Title => b"title",
            Self::Creator => b"creator",
            Self::Language => b"language",
            Self::Publisher => b"publisher",
            Self::Description => b"description",
            Self::Series => b"meta",
        }
    }
}

#[derive(Debug)]
struct Capture {
    field: TextField,
    text: String,
}

#[derive(Debug, Default)]
struct PackageMetadata {
    title: Option<String>,
    creators: Vec<String>,
    series: Option<String>,
    publisher: Option<String>,
    language: Option<String>,
    description: Option<String>,
    legacy_cover_id: Option<String>,
    manifest: Vec<ManifestItem>,
}

impl PackageMetadata {
    fn accept(&mut self, field: TextField, value: &str) {
        let value = match field {
            TextField::Description => clean_description(value),
            _ => clean_text(value),
        };
        if value.is_empty() {
            return;
        }

        match field {
            TextField::Title => {
                self.title.get_or_insert(value);
            }
            TextField::Creator => {
                if !self.creators.contains(&value) {
                    self.creators.push(value);
                }
            }
            TextField::Language => {
                self.language.get_or_insert(value);
            }
            TextField::Publisher => {
                self.publisher.get_or_insert(value);
            }
            TextField::Description => {
                self.description.get_or_insert(value);
            }
            TextField::Series => {
                self.series.get_or_insert(value);
            }
        }
    }

    fn cover_href(&self) -> Option<&str> {
        self.manifest
            .iter()
            .find(|item| {
                item.properties
                    .split_whitespace()
                    .any(|value| value == "cover-image")
            })
            .or_else(|| {
                self.legacy_cover_id
                    .as_ref()
                    .and_then(|cover_id| self.manifest.iter().find(|item| item.id == *cover_id))
            })
            .or_else(|| {
                self.manifest.iter().find(|item| {
                    item.media_type.starts_with("image/")
                        && (item.id.to_ascii_lowercase().contains("cover")
                            || item.href.to_ascii_lowercase().contains("cover"))
                })
            })
            .map(|item| item.href.as_str())
    }
}

#[derive(Debug)]
struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
    properties: String,
}

fn parse_container(xml: &[u8]) -> Result<String> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == b"rootfile" =>
            {
                if let Some(path) = attribute(&element, b"full-path")? {
                    return Ok(path);
                }
            }
            Event::Eof => {
                return Err(ImportError::InvalidPublication(
                    "container.xml does not name a package document",
                ));
            }
            _ => {}
        }
    }
}

fn parse_package(xml: &[u8]) -> Result<PackageMetadata> {
    let mut reader = Reader::from_reader(xml);
    let mut metadata = PackageMetadata::default();
    let mut capture = None;

    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) => {
                if capture.is_some() {
                    append_separator(&mut capture);
                } else {
                    capture = inspect_element(&element, &mut metadata)?;
                }
            }
            Event::Empty(element) => {
                inspect_element(&element, &mut metadata)?;
                append_separator(&mut capture);
            }
            Event::Text(text) => {
                if let Some(active) = &mut capture {
                    let decoded = text.decode().map_err(xml_error)?;
                    let decoded = quick_xml::escape::unescape(&decoded).map_err(xml_error)?;
                    active.text.push_str(&decoded);
                }
            }
            Event::CData(text) => {
                if let Some(active) = &mut capture {
                    active.text.push_str(&text.decode().map_err(xml_error)?);
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(active) = &mut capture {
                    if let Some(character) = reference.resolve_char_ref().map_err(xml_error)? {
                        active.text.push(character);
                    } else {
                        let name = reference.decode().map_err(xml_error)?;
                        if let Some(value) = quick_xml::escape::resolve_predefined_entity(&name) {
                            active.text.push_str(value);
                        } else {
                            active.text.push('&');
                            active.text.push_str(&name);
                            active.text.push(';');
                        }
                    }
                }
            }
            Event::End(element) => {
                let should_finish = capture
                    .as_ref()
                    .is_some_and(|active| element.local_name().as_ref() == active.field.end_name());
                if should_finish {
                    if let Some(active) = capture.take() {
                        metadata.accept(active.field, &active.text);
                    }
                } else {
                    append_separator(&mut capture);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(metadata)
}

fn inspect_element(
    element: &BytesStart<'_>,
    metadata: &mut PackageMetadata,
) -> Result<Option<Capture>> {
    let name = element.local_name();
    let field = match name.as_ref() {
        b"title" => Some(TextField::Title),
        b"creator" => Some(TextField::Creator),
        b"language" => Some(TextField::Language),
        b"publisher" => Some(TextField::Publisher),
        b"description" => Some(TextField::Description),
        b"meta" => inspect_meta(element, metadata)?,
        b"item" => {
            if let (Some(id), Some(href)) =
                (attribute(element, b"id")?, attribute(element, b"href")?)
            {
                metadata.manifest.push(ManifestItem {
                    id,
                    href,
                    media_type: attribute(element, b"media-type")?.unwrap_or_default(),
                    properties: attribute(element, b"properties")?.unwrap_or_default(),
                });
            }
            None
        }
        _ => None,
    };
    Ok(field.map(|field| Capture {
        field,
        text: String::new(),
    }))
}

fn inspect_meta(
    element: &BytesStart<'_>,
    metadata: &mut PackageMetadata,
) -> Result<Option<TextField>> {
    let name = attribute(element, b"name")?;
    let content = attribute(element, b"content")?;
    match name.as_deref() {
        Some("calibre:series") => {
            if let Some(value) = content {
                metadata.accept(TextField::Series, &value);
            }
        }
        Some("cover") => metadata.legacy_cover_id = content,
        _ => {}
    }

    let property = attribute(element, b"property")?;
    Ok((property.as_deref() == Some("belongs-to-collection")).then_some(TextField::Series))
}

fn attribute(element: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>> {
    for result in element.attributes().with_checks(false) {
        let attribute = result.map_err(xml_error)?;
        if attribute.key.local_name().as_ref() == name {
            return attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, element.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(xml_error);
        }
    }
    Ok(None)
}

fn append_separator(capture: &mut Option<Capture>) {
    if let Some(active) = capture
        && !active.text.ends_with(char::is_whitespace)
    {
        active.text.push(' ');
    }
}

fn extract_cover(
    archive: &mut ZipArchive<File>,
    package_path: &str,
    href: &str,
) -> Result<Vec<u8>> {
    let cover_path = normalize_archive_path(Some(package_path), href)?;
    let bytes = read_entry(archive, &cover_path, MAX_COVER_BYTES)?;
    encode_thumbnail(&bytes)
}

fn render_pdf_cover(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let pdf = Pdf::new(bytes).ok()?;
    let page = pdf.pages().first()?;
    let (width, height) = page.render_dimensions();
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }

    let scale = (320.0 / width).min(480.0 / height);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }

    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
            ..RenderSettings::default()
        },
    );
    let png = pixmap.into_png().ok()?;
    encode_thumbnail(&png).ok()
}

fn encode_thumbnail(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_COVER_DIMENSION);
    limits.max_image_height = Some(MAX_COVER_DIMENSION);
    limits.max_alloc = Some(MAX_COVER_ALLOCATION);
    reader.limits(limits);
    let thumbnail = reader
        .decode()?
        .thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 84).encode_image(&thumbnail)?;
    Ok(encoded)
}

fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    if size > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(ImportError::PdfTooLarge { size, limit });
    }

    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(limit).min(limit));
    file.by_ref()
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ImportError::PdfTooLarge {
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(bytes)
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str, limit: usize) -> Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    if entry.size() > limit_u64 {
        return Err(ImportError::EntryTooLarge {
            name: name.to_owned(),
            limit,
        });
    }

    let capacity = usize::try_from(entry.size()).unwrap_or(limit).min(limit);
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .by_ref()
        .take(limit_u64.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ImportError::EntryTooLarge {
            name: name.to_owned(),
            limit,
        });
    }
    Ok(bytes)
}

fn normalize_archive_path(base_file: Option<&str>, path: &str) -> Result<String> {
    let decoded = percent_decode_str(path.split('#').next().unwrap_or(path)).decode_utf8_lossy();
    if decoded.starts_with('/') || decoded.contains('\\') || decoded.contains('\0') {
        return Err(ImportError::UnsafeArchivePath(decoded.into_owned()));
    }

    let mut parts = base_file
        .and_then(|base| base.rsplit_once('/').map(|(parent, _)| parent))
        .map_or_else(Vec::new, |parent| {
            parent.split('/').map(str::to_owned).collect()
        });

    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(ImportError::UnsafeArchivePath(decoded.into_owned()));
                }
            }
            value => parts.push(value.to_owned()),
        }
    }

    if parts.is_empty() {
        return Err(ImportError::UnsafeArchivePath(decoded.into_owned()));
    }
    Ok(parts.join("/"))
}

fn format_for_path(path: &Path) -> Option<BookFormat> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("epub") {
        Some(BookFormat::Epub)
    } else if extension.eq_ignore_ascii_case("pdf") {
        Some(BookFormat::Pdf)
    } else {
        None
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| clean_text(&value))
        .filter(|value| !value.is_empty())
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_description(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut inside_tag = false;
    for character in value.chars() {
        match character {
            '<' => {
                inside_tag = true;
                plain.push(' ');
            }
            '>' => inside_tag = false,
            _ if !inside_tag => plain.push(character),
            _ => {}
        }
    }
    clean_text(&plain)
}

fn xml_error(error: impl std::fmt::Display) -> ImportError {
    ImportError::Xml(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Write,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use image::{DynamicImage, Rgb, RgbImage, codecs::jpeg::JpegEncoder};
    use lectern_core::{BookFormat, LibraryQuery, SortOrder};
    use lectern_storage::LibraryDatabase;
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::{
        ImportProgress, discover_publications, import_paths, parse_epub, parse_pdf,
        parse_publication,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "lectern-import-{label}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test directory");
        }
    }

    fn create_epub(path: &Path, title: &str) {
        let file = File::create(path).expect("create EPUB");
        let mut writer = ZipWriter::new(file);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let compressed =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        writer
            .start_file("mimetype", stored)
            .expect("start mimetype");
        writer
            .write_all(b"application/epub+zip")
            .expect("write mimetype");
        writer
            .start_file("META-INF/container.xml", compressed)
            .expect("start container");
        writer
            .write_all(
                br#"<?xml version="1.0"?>
                <container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
                  <rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles>
                </container>"#,
            )
            .expect("write container");
        writer
            .start_file("OEBPS/content.opf", compressed)
            .expect("start package");
        let package = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
              <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                <dc:title>{title} &amp; Dust</dc:title>
                <dc:creator>Ursula K. Le Guin</dc:creator>
                <dc:language>en</dc:language>
                <dc:publisher>Earthsea Press</dc:publisher>
                <dc:description>&lt;p&gt;A wizard's journey.&lt;/p&gt;</dc:description>
                <meta property="belongs-to-collection">Earthsea</meta>
              </metadata>
              <manifest>
                <item id="cover" href="images/cover%20art.jpg" media-type="image/jpeg"
                      properties="cover-image"/>
              </manifest>
            </package>"#
        );
        writer.write_all(package.as_bytes()).expect("write package");
        writer
            .start_file("OEBPS/images/cover art.jpg", compressed)
            .expect("start cover");
        writer.write_all(&cover_jpeg()).expect("write cover");
        writer.finish().expect("finish EPUB");
    }

    fn cover_jpeg() -> Vec<u8> {
        let cover = DynamicImage::ImageRgb8(RgbImage::from_pixel(40, 60, Rgb([24, 80, 140])));
        let mut encoded = Vec::new();
        JpegEncoder::new_with_quality(&mut encoded, 90)
            .encode_image(&cover)
            .expect("encode test cover");
        encoded
    }

    fn create_pdf(path: &Path, title: &str) {
        let mut document = Document::with_version("1.5");
        let page_tree_id = document.new_object_id();
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new("rg", vec![0.09.into(), 0.31.into(), 0.55.into()]),
                Operation::new("re", vec![0.into(), 0.into(), 300.into(), 450.into()]),
                Operation::new("f", vec![]),
                Operation::new("Q", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("encode PDF content"),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => page_tree_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 450.into()],
            "Resources" => dictionary! {},
        });
        document.objects.insert(
            page_tree_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => page_tree_id,
        });
        let info_id = document.add_object(dictionary! {
            "Title" => Object::string_literal(title),
            "Author" => Object::string_literal("Octavia E. Butler"),
            "Subject" => Object::string_literal("A field guide to many worlds."),
        });
        document.trailer.set("Root", catalog_id);
        document.trailer.set("Info", info_id);
        document.save(path).expect("save PDF");
    }

    #[test]
    fn parses_metadata_and_cover() {
        let directory = TestDirectory::new("parse");
        let path = directory.0.join("wizard.epub");
        create_epub(&path, "A Wizard");

        let record = parse_epub(&path).expect("parse EPUB");

        assert_eq!(record.book.title, "A Wizard & Dust");
        assert_eq!(record.book.authors, "Ursula K. Le Guin");
        assert_eq!(record.book.series.as_deref(), Some("Earthsea"));
        assert_eq!(record.book.publisher.as_deref(), Some("Earthsea Press"));
        assert_eq!(record.book.language.as_deref(), Some("en"));
        assert_eq!(
            record.book.description.as_deref(),
            Some("A wizard's journey.")
        );
        assert!(record.cover_thumbnail.is_some());
    }

    #[test]
    fn parses_pdf_metadata_and_first_page_cover() {
        let directory = TestDirectory::new("parse-pdf");
        let path = directory.0.join("field-guide.pdf");
        create_pdf(&path, "Field Guide");

        let record = parse_pdf(&path).expect("parse PDF");

        assert_eq!(record.book.title, "Field Guide");
        assert_eq!(record.book.authors, "Octavia E. Butler");
        assert_eq!(
            record.book.description.as_deref(),
            Some("A field guide to many worlds.")
        );
        assert_eq!(record.assets.len(), 1);
        assert_eq!(record.assets[0].format, BookFormat::Pdf);
        let cover = record.cover_thumbnail.expect("render first page");
        let image = image::load_from_memory(&cover).expect("decode rendered cover");
        assert!(image.width() <= super::THUMBNAIL_WIDTH);
        assert!(image.height() <= super::THUMBNAIL_HEIGHT);
    }

    #[test]
    fn dispatches_publications_by_case_insensitive_extension() {
        let directory = TestDirectory::new("dispatch");
        let path = directory.0.join("guide.PDF");
        create_pdf(&path, "Guide");

        let record = parse_publication(&path).expect("parse publication");

        assert_eq!(record.assets.len(), 1);
        assert_eq!(record.assets[0].format, BookFormat::Pdf);
    }

    #[test]
    fn discovers_supported_publications_recursively_and_case_insensitively() {
        let directory = TestDirectory::new("discover");
        let nested = directory.0.join("nested");
        fs::create_dir(&nested).expect("create nested directory");
        create_epub(&directory.0.join("one.epub"), "One");
        create_epub(&nested.join("two.EPUB"), "Two");
        create_pdf(&nested.join("three.PDF"), "Three");
        File::create(directory.0.join("notes.txt")).expect("create unrelated file");

        let discovered = discover_publications(std::slice::from_ref(&directory.0))
            .expect("discover publications");

        assert_eq!(discovered.len(), 3);
        assert!(discovered[0] < discovered[1]);
    }

    #[test]
    fn imports_valid_books_and_isolates_bad_archives() {
        let directory = TestDirectory::new("pipeline");
        let valid = directory.0.join("valid.epub");
        let valid_pdf = directory.0.join("valid.pdf");
        let invalid = directory.0.join("invalid.epub");
        let database_path = directory.0.join("library.sqlite3");
        create_epub(&valid, "The Dispossessed");
        create_pdf(&valid_pdf, "Parable of the Sower");
        File::create(&invalid)
            .expect("create invalid EPUB")
            .write_all(b"not a zip")
            .expect("write invalid EPUB");
        let mut updates = Vec::<ImportProgress>::new();

        let summary = import_paths(
            &database_path,
            std::slice::from_ref(&directory.0),
            |progress| updates.push(progress),
        )
        .expect("run import");

        assert_eq!(summary.discovered, 3);
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(updates.last().map(|progress| progress.processed), Some(3));
        let database = LibraryDatabase::open(&database_path).expect("open imported library");
        let books = database
            .query(&LibraryQuery {
                search: "Dispossessed".into(),
                sort: SortOrder::Title,
                ..LibraryQuery::default()
            })
            .expect("query imported books");
        assert_eq!(books.len(), 1);
        let pdfs = database
            .query(&LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            })
            .expect("query imported PDFs");
        assert_eq!(pdfs.len(), 1);
        assert_eq!(pdfs[0].title, "Parable of the Sower");
    }
}
