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
        BookEdit, BookSelection, BulkRemovalResult, BulkTagEdit, BulkTagResult, ContributorId,
        ContributorUsage, SavedSearch, SavedSearchId, SelectionSnapshot, SelectionTagUsage,
        SeriesId, SeriesIndex, SeriesUsage, TagId, TagUsage, VocabularyMutationResult,
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

    fn selection_snapshot(
        &mut self,
        query: &LibraryQuery,
    ) -> Result<SelectionSnapshot, Self::Error> {
        Ok(self.database.selection_snapshot(query)?)
    }

    fn query_library_ids_window(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<BookId>, Self::Error> {
        Ok(self.database.query_ids_window(query, offset, limit)?)
    }

    fn selection_tag_usage(
        &mut self,
        selection: &BookSelection,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<SelectionTagUsage>, Self::Error> {
        Ok(self
            .database
            .selection_tag_usage(selection, offset, limit)?)
    }

    fn apply_bulk_tags(
        &mut self,
        selection: &BookSelection,
        edit: &BulkTagEdit,
    ) -> Result<BulkTagResult, Self::Error> {
        Ok(self.database.apply_bulk_tags(selection, edit)?)
    }

    fn remove_books(
        &mut self,
        selection: &BookSelection,
    ) -> Result<BulkRemovalResult, Self::Error> {
        Ok(self.database.remove_books(selection)?)
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

    fn series_index_is_available(
        &mut self,
        series: SeriesId,
        index: SeriesIndex,
        excluding_book: BookId,
    ) -> Result<bool, Self::Error> {
        Ok(self
            .database
            .series_index_is_available(series, index, excluding_book)?)
    }

    fn autocomplete_tags(
        &mut self,
        prefix: &str,
        selected: &[TagId],
        limit: u32,
    ) -> Result<Vec<TagUsage>, Self::Error> {
        Ok(self.database.autocomplete_tags(prefix, selected, limit)?)
    }

    fn search_contributors(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<ContributorUsage>, Self::Error> {
        Ok(self.database.search_contributors(prefix, offset, limit)?)
    }

    fn search_series(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<SeriesUsage>, Self::Error> {
        Ok(self.database.search_series(prefix, offset, limit)?)
    }

    fn search_tags(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<TagUsage>, Self::Error> {
        Ok(self.database.search_tags(prefix, offset, limit)?)
    }

    fn list_saved_searches(&mut self) -> Result<Vec<SavedSearch>, Self::Error> {
        Ok(self.database.list_saved_searches()?)
    }

    fn search_saved_searches(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<SavedSearch>, Self::Error> {
        Ok(self.database.search_saved_searches(prefix, offset, limit)?)
    }

    fn create_saved_search(
        &mut self,
        name: &str,
        query: &LibraryQuery,
    ) -> Result<SavedSearchId, Self::Error> {
        Ok(self.database.create_saved_search(name, query)?)
    }

    fn update_saved_search(
        &mut self,
        id: SavedSearchId,
        query: &LibraryQuery,
    ) -> Result<(), Self::Error> {
        Ok(self.database.update_saved_search(id, query)?)
    }

    fn rename_saved_search(&mut self, id: SavedSearchId, name: &str) -> Result<(), Self::Error> {
        Ok(self.database.rename_saved_search(id, name)?)
    }

    fn delete_saved_search(&mut self, id: SavedSearchId) -> Result<bool, Self::Error> {
        Ok(self.database.delete_saved_search(id)?)
    }

    fn contributor_mutation_impact(
        &mut self,
        id: ContributorId,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.contributor_mutation_impact(id)?)
    }

    fn series_mutation_impact(
        &mut self,
        id: SeriesId,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.series_mutation_impact(id)?)
    }

    fn tag_mutation_impact(&mut self, id: TagId) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.tag_mutation_impact(id)?)
    }

    fn rename_contributor(
        &mut self,
        id: ContributorId,
        display_name: &str,
        sort_name: &str,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self
            .database
            .rename_contributor(id, display_name, sort_name)?)
    }

    fn merge_contributors(
        &mut self,
        source: ContributorId,
        target: ContributorId,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.merge_contributors(source, target)?)
    }

    fn delete_contributor(&mut self, id: ContributorId) -> Result<(), Self::Error> {
        Ok(self.database.delete_contributor(id)?)
    }

    fn rename_series(
        &mut self,
        id: SeriesId,
        name: &str,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.rename_series(id, name)?)
    }

    fn merge_series(
        &mut self,
        source: SeriesId,
        target: SeriesId,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.merge_series(source, target)?)
    }

    fn delete_series(&mut self, id: SeriesId) -> Result<(), Self::Error> {
        Ok(self.database.delete_series(id)?)
    }

    fn rename_tag(
        &mut self,
        id: TagId,
        name: &str,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.rename_tag(id, name)?)
    }

    fn merge_tags(
        &mut self,
        source: TagId,
        target: TagId,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.merge_tags(source, target)?)
    }

    fn delete_tag(
        &mut self,
        id: TagId,
        confirmed: VocabularyMutationResult,
    ) -> Result<VocabularyMutationResult, Self::Error> {
        Ok(self.database.delete_tag(id, confirmed)?)
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
