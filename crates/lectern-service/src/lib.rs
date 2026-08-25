//! Workflow-level application service for Lectern.
//!
//! This crate composes the publication parser and `SQLite` adapter behind the application boundary
//! defined by `lectern-core`. Frontends depend on workflows here instead of coordinating storage
//! transactions or import merge policy themselves.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use lectern_core::{
    AssetHealthReport, AssetId, BackupReport, Book, BookFormat, BookId, BookSummary,
    ImportProgress, ImportSummary, LibraryDiagnostics, LibraryPage, LibraryQuery, LibraryService,
    LibraryStats,
    organisation::{
        BookEdit, ContributorId, ContributorUsage, SeriesId, SeriesUsage, TagId, TagUsage,
    },
};
use lectern_import::{import_paths_into, validate_publication};
use lectern_storage::LibraryDatabase;
use thiserror::Error;

/// Failure returned by a composed library workflow.
#[derive(Debug, Error)]
pub enum LibraryServiceError {
    /// The requested existing library path was absent or not a regular file.
    #[error("library database does not exist: {0}")]
    LibraryMissing(PathBuf),
    /// Publication discovery, parsing, or validation failed.
    #[error(transparent)]
    Import(#[from] lectern_import::ImportError),
    /// A database or durable snapshot operation failed.
    #[error(transparent)]
    Storage(#[from] lectern_storage::StorageError),
}

/// SQLite-backed implementation of Lectern's workflow-level application boundary.
pub struct SqliteLibraryService {
    database: LibraryDatabase,
}

impl SqliteLibraryService {
    /// Opens or creates a library and applies pending schema migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened, configured, or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LibraryServiceError> {
        Ok(Self {
            database: LibraryDatabase::open(path)?,
        })
    }

    /// Opens an existing library without silently creating a new database for a read-only-looking
    /// administrative command.
    ///
    /// # Errors
    ///
    /// Returns an error when `path` is not an existing regular file or the library cannot be opened.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self, LibraryServiceError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(LibraryServiceError::LibraryMissing(path.to_path_buf()));
        }
        Self::open(path)
    }
}

impl LibraryService for SqliteLibraryService {
    type Error = LibraryServiceError;

    fn query_library(&mut self, query: &LibraryQuery) -> Result<Vec<BookSummary>, Self::Error> {
        Ok(self.database.query(query)?)
    }

    fn query_library_page(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<LibraryPage, Self::Error> {
        Ok(self.database.query_page(query, offset, limit)?)
    }

    fn query_library_window(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<BookSummary>, Self::Error> {
        Ok(self.database.query_window(query, offset, limit)?)
    }

    fn get_book(&mut self, id: BookId) -> Result<Option<Book>, Self::Error> {
        Ok(self.database.get_book(id)?)
    }

    fn update_metadata(&mut self, edit: &BookEdit) -> Result<(), Self::Error> {
        Ok(self.database.save_book_edit(edit)?)
    }

    fn autocomplete_contributors(
        &mut self,
        prefix: &str,
        selected: &[ContributorId],
        limit: u32,
    ) -> Result<Vec<ContributorUsage>, Self::Error> {
        Ok(self
            .database
            .autocomplete_contributors(prefix, selected, limit)?)
    }

    fn autocomplete_series(
        &mut self,
        prefix: &str,
        selected: &[SeriesId],
        limit: u32,
    ) -> Result<Vec<SeriesUsage>, Self::Error> {
        Ok(self.database.autocomplete_series(prefix, selected, limit)?)
    }

    fn autocomplete_tags(
        &mut self,
        prefix: &str,
        selected: &[TagId],
        limit: u32,
    ) -> Result<Vec<TagUsage>, Self::Error> {
        Ok(self.database.autocomplete_tags(prefix, selected, limit)?)
    }

    fn import_publications(
        &mut self,
        roots: &[PathBuf],
        report_progress: &mut dyn FnMut(ImportProgress),
    ) -> Result<ImportSummary, Self::Error> {
        Ok(import_paths_into(
            &mut self.database,
            roots,
            report_progress,
        )?)
    }

    fn attach_asset(
        &mut self,
        book: BookId,
        format: BookFormat,
        path: &Path,
    ) -> Result<AssetId, Self::Error> {
        validate_publication(path, format)?;
        Ok(self.database.attach_reference_asset(book, format, path)?)
    }

    fn detach_asset(&mut self, asset: AssetId) -> Result<BookId, Self::Error> {
        Ok(self.database.detach_asset(asset)?)
    }

    fn relink_asset(
        &mut self,
        asset: AssetId,
        format: BookFormat,
        replacement_path: &Path,
    ) -> Result<(), Self::Error> {
        validate_publication(replacement_path, format)?;
        Ok(self
            .database
            .relink_reference_asset(asset, replacement_path, format)?)
    }

    fn replace_asset(
        &mut self,
        asset: AssetId,
        format: BookFormat,
        replacement_path: &Path,
    ) -> Result<(), Self::Error> {
        validate_publication(replacement_path, format)?;
        Ok(self
            .database
            .replace_reference_asset(asset, replacement_path, format)?)
    }

    fn remove_book(&mut self, id: BookId) -> Result<bool, Self::Error> {
        Ok(self.database.remove_book(id)?)
    }

    fn scan_assets(&mut self) -> Result<AssetHealthReport, Self::Error> {
        Ok(self.database.rescan_reference_assets()?)
    }

    fn backup(&mut self, destination: &Path) -> Result<BackupReport, Self::Error> {
        Ok(self.database.backup(destination)?)
    }

    fn doctor(&mut self) -> Result<LibraryDiagnostics, Self::Error> {
        Ok(self.database.diagnostics()?)
    }

    fn stats(&mut self) -> Result<LibraryStats, Self::Error> {
        Ok(self.database.stats()?)
    }

    fn load_cover(&mut self, id: BookId) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.database.load_cover(id)?)
    }
}

/// Returns the database path shared by the desktop and command-line frontends.
#[must_use]
pub fn default_database_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("LECTERN_DATA_DIR") {
        return PathBuf::from(directory).join("library.sqlite3");
    }
    ProjectDirs::from("com", "Lectern", "Lectern").map_or_else(
        || PathBuf::from("lectern-library.sqlite3"),
        |directories| directories.data_dir().join("library.sqlite3"),
    )
}
