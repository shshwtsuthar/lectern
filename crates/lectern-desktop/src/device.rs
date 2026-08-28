use std::{collections::HashSet, path::Path};

use lectern_core::{
    AssetStorage, LibraryService,
    organisation::{BookSelection, LibraryGeneration},
};
use lectern_device::{DeviceTransferBook, DeviceTransferSource};
use lectern_service::SqliteLibraryService;

const SELECTION_PAGE_SIZE: u32 = 512;

pub(crate) fn load_transfer_books(
    database_path: &Path,
    selection: &BookSelection,
) -> Result<Vec<DeviceTransferBook>, String> {
    let mut service =
        SqliteLibraryService::open(database_path).map_err(|error| error.to_string())?;
    let ids = resolve_selection(&mut service, selection)?;
    let mut books = Vec::with_capacity(ids.len());
    for id in ids {
        let book = service
            .get_book(id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("Book {id} is no longer in the library."))?;
        let sources = book
            .assets
            .iter()
            .filter(|asset| asset.storage == AssetStorage::Reference)
            .map(|asset| DeviceTransferSource {
                asset_id: asset.id,
                format: asset.format,
                path: asset.path.clone(),
            })
            .collect();
        books.push(DeviceTransferBook {
            book_id: book.id,
            title: book.title,
            authors: book.authors,
            sources,
        });
    }
    Ok(books)
}

fn resolve_selection(
    service: &mut SqliteLibraryService,
    selection: &BookSelection,
) -> Result<Vec<lectern_core::BookId>, String> {
    match selection {
        BookSelection::Explicit(books) => Ok(books.clone()),
        BookSelection::AllMatching {
            query,
            generation,
            excluded,
        } => {
            validate_generation(service, query, *generation)?;
            let excluded = excluded.iter().copied().collect::<HashSet<_>>();
            let mut ids = Vec::new();
            let mut offset = 0_u64;
            loop {
                let page = service
                    .query_library_ids_window(query, offset, SELECTION_PAGE_SIZE)
                    .map_err(|error| error.to_string())?;
                let page_len = page.len();
                ids.extend(page.into_iter().filter(|id| !excluded.contains(id)));
                if page_len < SELECTION_PAGE_SIZE as usize {
                    break;
                }
                offset = offset
                    .checked_add(u64::from(SELECTION_PAGE_SIZE))
                    .ok_or_else(|| "Selection offset overflowed.".to_owned())?;
            }
            Ok(ids)
        }
    }
}

fn validate_generation(
    service: &mut SqliteLibraryService,
    query: &lectern_core::LibraryQuery,
    expected: LibraryGeneration,
) -> Result<(), String> {
    let current = service
        .selection_snapshot(query)
        .map_err(|error| error.to_string())?;
    if current.generation == expected {
        Ok(())
    } else {
        Err(
            "The library changed after this selection. Select the books again before sending."
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write as _, path::Path};

    use lectern_core::{LibraryQuery, LibraryService, organisation::BookSelection};
    use lectern_service::SqliteLibraryService;
    use tempfile::tempdir;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::load_transfer_books;

    #[test]
    fn resolves_explicit_assets_and_rejects_a_stale_query_selection() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("library.sqlite3");
        let first_path = directory.path().join("first.epub");
        create_epub(&first_path, "First");
        let mut service = SqliteLibraryService::open(&database_path).unwrap();
        service
            .import_publications(std::slice::from_ref(&first_path), &mut |_| {})
            .unwrap();
        let query = LibraryQuery::default();
        let first_id = service.query_library(&query).unwrap()[0].id;
        let snapshot = service.selection_snapshot(&query).unwrap();
        drop(service);

        let books = load_transfer_books(
            &database_path,
            &BookSelection::explicit(vec![first_id, first_id]),
        )
        .unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].sources.len(), 1);
        assert_eq!(books[0].sources[0].path, first_path);

        let second_path = directory.path().join("second.epub");
        create_epub(&second_path, "Second");
        let mut service = SqliteLibraryService::open(&database_path).unwrap();
        service
            .import_publications(&[second_path], &mut |_| {})
            .unwrap();
        drop(service);
        let stale = BookSelection::all_matching(query, snapshot.generation, Vec::new());
        assert!(load_transfer_books(&database_path, &stale).is_err());
    }

    fn create_epub(path: &Path, title: &str) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let compressed =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("mimetype", stored).unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        writer
            .start_file("META-INF/container.xml", compressed)
            .unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?>
                <container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
                  <rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles>
                </container>"#,
            )
            .unwrap();
        writer.start_file("OEBPS/content.opf", compressed).unwrap();
        writer
            .write_all(
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
                    <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
                      <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                        <dc:title>{title}</dc:title><dc:creator>Author</dc:creator>
                      </metadata><manifest/></package>"#
                )
                .as_bytes(),
            )
            .unwrap();
        writer.finish().unwrap();
    }
}
