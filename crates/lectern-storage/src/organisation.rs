//! Normalized curation schema, conservative migration, and projection integrity.

use std::collections::{HashMap, HashSet};

use lectern_core::{
    BookId,
    organisation::{
        BookEdit, Contributor, ContributorCredit, ContributorCreditEdit, ContributorId,
        ContributorReference, ContributorRole, ContributorUsage, ImportedContributorCredit,
        ImportedOrganisation, NameKind, Series, SeriesId, SeriesIndex, SeriesMembership,
        SeriesMembershipEdit, SeriesReference, SeriesUsage, Tag, TagId, TagReference, TagUsage,
        identity_key, normalize_name,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Result, StorageError, optional_text, sortable};

const ORGANISATION_SCHEMA: &str = r"
DROP TRIGGER books_after_insert;
DROP TRIGGER books_after_delete;
DROP TRIGGER books_after_update;
DROP TABLE books_fts;

ALTER TABLE books ADD COLUMN authors_search TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN contributors_search TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN tags_search TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN series_key TEXT;
ALTER TABLE books ADD COLUMN series_index INTEGER
    CHECK (series_index BETWEEN 0 AND 999999999999);

CREATE TABLE contributors (
    id           INTEGER PRIMARY KEY,
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 256),
    sort_name    TEXT NOT NULL CHECK (length(sort_name) BETWEEN 1 AND 256),
    identity_key TEXT NOT NULL UNIQUE CHECK (length(identity_key) > 0),
    sort_key     TEXT NOT NULL CHECK (length(sort_key) > 0)
) STRICT;
CREATE INDEX contributors_name_idx ON contributors(identity_key, id);

CREATE TABLE series_entities (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 256),
    identity_key TEXT NOT NULL UNIQUE CHECK (length(identity_key) > 0)
) STRICT;
CREATE INDEX series_entities_name_idx ON series_entities(identity_key, id);

CREATE TABLE tags (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    identity_key TEXT NOT NULL UNIQUE CHECK (length(identity_key) > 0)
) STRICT;
CREATE INDEX tags_name_idx ON tags(identity_key, id);

CREATE TABLE book_contributors (
    book_id                  INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    contributor_id           INTEGER NOT NULL REFERENCES contributors(id),
    role                     TEXT NOT NULL
                             CHECK (role IN (
                                 'author', 'editor', 'translator', 'illustrator', 'other'
                             )),
    position                 INTEGER NOT NULL CHECK (position >= 0),
    display_name_projection  TEXT NOT NULL CHECK (length(display_name_projection) > 0),
    sort_key_projection      TEXT NOT NULL CHECK (length(sort_key_projection) > 0),
    PRIMARY KEY (book_id, contributor_id, role),
    UNIQUE (book_id, role, position)
) STRICT;
CREATE INDEX book_contributors_book_role_position_idx
    ON book_contributors(book_id, role, position, contributor_id);
CREATE INDEX book_contributors_contributor_role_book_idx
    ON book_contributors(contributor_id, role, book_id);

CREATE TABLE series_memberships (
    book_id          INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    series_id        INTEGER NOT NULL REFERENCES series_entities(id),
    series_index     INTEGER CHECK (series_index BETWEEN 0 AND 999999999999),
    name_projection  TEXT NOT NULL CHECK (length(name_projection) > 0),
    key_projection   TEXT NOT NULL CHECK (length(key_projection) > 0)
) STRICT;
CREATE INDEX series_memberships_series_index_book_idx
    ON series_memberships(series_id, series_index, book_id);

CREATE TABLE book_tags (
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (book_id, tag_id)
) STRICT;
CREATE INDEX book_tags_tag_book_idx ON book_tags(tag_id, book_id);

CREATE TABLE saved_searches (
    id                INTEGER PRIMARY KEY,
    name              TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    identity_key      TEXT NOT NULL UNIQUE CHECK (length(identity_key) > 0),
    shape_version     INTEGER NOT NULL DEFAULT 1 CHECK (shape_version = 1),
    search_expression TEXT NOT NULL DEFAULT '' CHECK (length(search_expression) <= 1024),
    series_id         INTEGER REFERENCES series_entities(id),
    format            TEXT CHECK (format IN ('epub', 'pdf')),
    file_health       TEXT CHECK (
                          file_health IN ('unknown', 'available', 'missing', 'unreadable')
                      ),
    sort_order        TEXT NOT NULL DEFAULT 'title'
                      CHECK (sort_order IN ('title', 'author', 'recently_added', 'series'))
) STRICT;
CREATE INDEX saved_searches_name_idx ON saved_searches(identity_key, id);

CREATE TABLE saved_search_contributors (
    saved_search_id INTEGER NOT NULL REFERENCES saved_searches(id) ON DELETE CASCADE,
    contributor_id  INTEGER NOT NULL REFERENCES contributors(id),
    author_only     INTEGER NOT NULL CHECK (author_only IN (0, 1)),
    PRIMARY KEY (saved_search_id, contributor_id)
) STRICT;
CREATE INDEX saved_search_contributors_contributor_search_idx
    ON saved_search_contributors(contributor_id, saved_search_id);

CREATE TABLE saved_search_included_tags (
    saved_search_id INTEGER NOT NULL REFERENCES saved_searches(id) ON DELETE CASCADE,
    tag_id          INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (saved_search_id, tag_id)
) STRICT;
CREATE INDEX saved_search_included_tags_tag_search_idx
    ON saved_search_included_tags(tag_id, saved_search_id);

CREATE TABLE saved_search_excluded_tags (
    saved_search_id INTEGER NOT NULL REFERENCES saved_searches(id) ON DELETE CASCADE,
    tag_id          INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (saved_search_id, tag_id)
) STRICT;
CREATE INDEX saved_search_excluded_tags_tag_search_idx
    ON saved_search_excluded_tags(tag_id, saved_search_id);

CREATE VIRTUAL TABLE books_fts USING fts5(
    title,
    authors_search,
    contributors_search,
    series,
    publisher,
    tags_search,
    content='books',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2',
    prefix='2 3'
);

CREATE TRIGGER books_after_insert AFTER INSERT ON books BEGIN
    INSERT INTO books_fts(
        rowid, title, authors_search, contributors_search, series, publisher, tags_search
    ) VALUES (
        new.id, new.title, new.authors_search, new.contributors_search,
        new.series, new.publisher, new.tags_search
    );
END;

CREATE TRIGGER books_after_delete AFTER DELETE ON books BEGIN
    INSERT INTO books_fts(
        books_fts, rowid, title, authors_search, contributors_search,
        series, publisher, tags_search
    ) VALUES (
        'delete', old.id, old.title, old.authors_search, old.contributors_search,
        old.series, old.publisher, old.tags_search
    );
END;

CREATE TRIGGER books_after_update
AFTER UPDATE OF title, authors_search, contributors_search, series, publisher, tags_search ON books
WHEN old.title IS NOT new.title
  OR old.authors_search IS NOT new.authors_search
  OR old.contributors_search IS NOT new.contributors_search
  OR old.series IS NOT new.series
  OR old.publisher IS NOT new.publisher
  OR old.tags_search IS NOT new.tags_search
BEGIN
    INSERT INTO books_fts(
        books_fts, rowid, title, authors_search, contributors_search,
        series, publisher, tags_search
    ) VALUES (
        'delete', old.id, old.title, old.authors_search, old.contributors_search,
        old.series, old.publisher, old.tags_search
    );
    INSERT INTO books_fts(
        rowid, title, authors_search, contributors_search, series, publisher, tags_search
    ) VALUES (
        new.id, new.title, new.authors_search, new.contributors_search,
        new.series, new.publisher, new.tags_search
    );
END;
";

/// Converts flattened version-five metadata without reading any publication file.
pub(super) fn migrate_v5_to_v6(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(ORGANISATION_SCHEMA)?;
    // External-content FTS delete commands require an existing row. Seed the new index before
    // projection updates begin, then rebuild once more after every normalized relation is ready.
    transaction.execute("INSERT INTO books_fts(books_fts) VALUES ('rebuild')", [])?;

    let books = {
        let mut statement =
            transaction.prepare("SELECT id, authors, series FROM books ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut contributor_ids = HashMap::<String, i64>::new();
    let mut series_ids = HashMap::<String, i64>::new();
    let mut insert_contributor = transaction.prepare(
        "INSERT INTO contributors(display_name, sort_name, identity_key, sort_key) \
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    let mut insert_credit = transaction.prepare(
        "INSERT INTO book_contributors( \
             book_id, contributor_id, role, position, \
             display_name_projection, sort_key_projection \
         ) VALUES (?1, ?2, 'author', 0, ?3, ?4)",
    )?;
    let mut insert_series =
        transaction.prepare("INSERT INTO series_entities(name, identity_key) VALUES (?1, ?2)")?;
    let mut insert_membership = transaction.prepare(
        "INSERT INTO series_memberships( \
             book_id, series_id, series_index, name_projection, key_projection \
         ) VALUES (?1, ?2, NULL, ?3, ?4)",
    )?;
    let mut update_projection = transaction.prepare(
        "UPDATE books SET \
             authors_search = ?2, contributors_search = ?2, sort_authors = ?3, \
             tags_search = '', series_key = ?4, series_index = NULL \
         WHERE id = ?1",
    )?;

    for (book_id, legacy_authors, legacy_series) in books {
        let mut author_sort_key = String::new();
        if !legacy_authors.trim().is_empty() {
            let display_name = validate_legacy_name(NameKind::Contributor, &legacy_authors)?;
            let key = identity_key(&display_name);
            author_sort_key.clone_from(&key);
            let contributor_id = if let Some(id) = contributor_ids.get(&key) {
                *id
            } else {
                insert_contributor.execute(params![display_name, display_name, key, key])?;
                let id = transaction.last_insert_rowid();
                contributor_ids.insert(key.clone(), id);
                id
            };
            insert_credit.execute(params![
                book_id,
                contributor_id,
                legacy_authors,
                author_sort_key,
            ])?;
        }

        let mut series_key = None::<String>;
        if let Some(legacy_series) = legacy_series.as_deref()
            && !legacy_series.trim().is_empty()
        {
            let display_name = validate_legacy_name(NameKind::Series, legacy_series)?;
            let key = identity_key(&display_name);
            let series_id = if let Some(id) = series_ids.get(&key) {
                *id
            } else {
                insert_series.execute(params![display_name, key])?;
                let id = transaction.last_insert_rowid();
                series_ids.insert(key.clone(), id);
                id
            };
            insert_membership.execute(params![book_id, series_id, legacy_series, key])?;
            series_key = Some(key);
        }

        update_projection.execute(params![
            book_id,
            legacy_authors,
            author_sort_key,
            series_key,
        ])?;
    }

    drop(update_projection);
    drop(insert_membership);
    drop(insert_series);
    drop(insert_credit);
    drop(insert_contributor);
    transaction.execute("INSERT INTO books_fts(books_fts) VALUES ('rebuild')", [])?;
    Ok(())
}

fn validate_legacy_name(kind: NameKind, value: &str) -> Result<String> {
    normalize_name(kind, value).map_err(|error| {
        StorageError::Integrity(format!("legacy {kind} cannot be normalized: {error}"))
    })
}

/// Reconciles the compatibility flattened author/series input with normalized relations.
///
/// This remains deliberately conservative until format adapters supply ordered creator records:
/// the complete author string becomes one Author credit and is never split on punctuation.
pub(super) fn replace_flattened_organisation(
    transaction: &Transaction<'_>,
    book_id: i64,
    authors: &str,
    series: Option<&str>,
) -> Result<()> {
    transaction.execute(
        "DELETE FROM book_contributors WHERE book_id = ?1",
        [book_id],
    )?;
    if !authors.trim().is_empty() {
        let incoming_display = normalize_user_name(NameKind::Contributor, authors)?;
        let incoming_key = identity_key(&incoming_display);
        let (contributor_id, display_name, sort_key) = transaction.query_row(
            "INSERT INTO contributors(display_name, sort_name, identity_key, sort_key) \
             VALUES (?1, ?1, ?2, ?2) \
             ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
             RETURNING id, display_name, sort_key",
            params![incoming_display, incoming_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        transaction.execute(
            "INSERT INTO book_contributors( \
                 book_id, contributor_id, role, position, \
                 display_name_projection, sort_key_projection \
             ) VALUES (?1, ?2, 'author', 0, ?3, ?4)",
            params![book_id, contributor_id, display_name, sort_key],
        )?;
    }

    transaction.execute(
        "DELETE FROM series_memberships WHERE book_id = ?1",
        [book_id],
    )?;
    if let Some(series) = series
        && !series.trim().is_empty()
    {
        let incoming_name = normalize_user_name(NameKind::Series, series)?;
        let incoming_key = identity_key(&incoming_name);
        let (series_id, name, key) = transaction.query_row(
            "INSERT INTO series_entities(name, identity_key) VALUES (?1, ?2) \
             ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
             RETURNING id, name, identity_key",
            params![incoming_name, incoming_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        transaction.execute(
            "INSERT INTO series_memberships( \
                 book_id, series_id, series_index, name_projection, key_projection \
             ) VALUES (?1, ?2, NULL, ?3, ?4)",
            params![book_id, series_id, name, key],
        )?;
    }

    rebuild_book_projection(transaction, book_id)
}

/// Reconciles publication-derived organization for a newly imported or explicitly replaced book.
///
/// Adapters that preserve creator boundaries supply normalized source credits. Compatibility
/// callers retain the conservative complete-string behavior.
pub(super) fn replace_imported_organisation(
    transaction: &Transaction<'_>,
    book_id: i64,
    authors: &str,
    series: Option<&str>,
    imported: Option<&ImportedOrganisation>,
) -> Result<()> {
    let Some(imported) = imported else {
        return replace_flattened_organisation(transaction, book_id, authors, series);
    };
    validate_imported_credit_positions(&imported.contributors)?;

    transaction.execute(
        "DELETE FROM book_contributors WHERE book_id = ?1",
        [book_id],
    )?;
    let mut unique_credits = HashSet::with_capacity(imported.contributors.len());
    let mut insert_credit = transaction.prepare(
        "INSERT INTO book_contributors( \
             book_id, contributor_id, role, position, \
             display_name_projection, sort_key_projection \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for credit in &imported.contributors {
        let incoming_display = normalize_user_name(NameKind::Contributor, &credit.display_name)?;
        let incoming_key = identity_key(&incoming_display);
        let (contributor_id, display_name, sort_key) = transaction.query_row(
            "INSERT INTO contributors(display_name, sort_name, identity_key, sort_key) \
             VALUES (?1, ?1, ?2, ?2) \
             ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
             RETURNING id, display_name, sort_key",
            params![incoming_display, incoming_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        if !unique_credits.insert((contributor_id, credit.role)) {
            return Err(StorageError::InvalidCuration(format!(
                "contributor {contributor_id} appears more than once as {}",
                credit.role
            )));
        }
        insert_credit.execute(params![
            book_id,
            contributor_id,
            credit.role.as_str(),
            credit.position,
            display_name,
            sort_key,
        ])?;
    }
    drop(insert_credit);

    transaction.execute(
        "DELETE FROM series_memberships WHERE book_id = ?1",
        [book_id],
    )?;
    if let Some(series) = series
        && !series.trim().is_empty()
    {
        let incoming_name = normalize_user_name(NameKind::Series, series)?;
        let incoming_key = identity_key(&incoming_name);
        let (series_id, name, key) = transaction.query_row(
            "INSERT INTO series_entities(name, identity_key) VALUES (?1, ?2) \
             ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
             RETURNING id, name, identity_key",
            params![incoming_name, incoming_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let series_index = imported
            .series_index
            .map(|index| i64::try_from(index.scaled()).expect("valid series index fits in SQLite"));
        transaction.execute(
            "INSERT INTO series_memberships( \
                 book_id, series_id, series_index, name_projection, key_projection \
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![book_id, series_id, series_index, name, key],
        )?;
    }

    rebuild_book_projection(transaction, book_id)
}

fn validate_imported_credit_positions(credits: &[ImportedContributorCredit]) -> Result<()> {
    for role in ContributorRole::ALL {
        let mut positions = credits
            .iter()
            .filter(|credit| credit.role == role)
            .map(|credit| credit.position)
            .collect::<Vec<_>>();
        positions.sort_unstable();
        if positions
            .iter()
            .enumerate()
            .any(|(expected, observed)| usize::try_from(*observed) != Ok(expected))
        {
            return Err(StorageError::InvalidCuration(format!(
                "{role} imported credit positions must be contiguous from zero"
            )));
        }
    }
    Ok(())
}

fn normalize_user_name(kind: NameKind, value: &str) -> Result<String> {
    normalize_name(kind, value).map_err(|error| StorageError::InvalidCuration(error.to_string()))
}

/// Loads authoritative contributor, series, and tag records for one complete book.
pub(super) fn load_book_curation(
    connection: &Connection,
    book_id: BookId,
) -> Result<(Vec<ContributorCredit>, Option<SeriesMembership>, Vec<Tag>)> {
    let contributors = {
        let mut statement = connection.prepare_cached(
            "SELECT c.id, c.display_name, c.sort_name, bc.role, bc.position \
             FROM book_contributors bc \
             JOIN contributors c ON c.id = bc.contributor_id \
             WHERE bc.book_id = ?1 \
             ORDER BY \
                 CASE bc.role \
                     WHEN 'author' THEN 0 WHEN 'editor' THEN 1 WHEN 'translator' THEN 2 \
                     WHEN 'illustrator' THEN 3 ELSE 4 \
                 END, bc.position, c.id",
        )?;
        let rows = statement.query_map([book_id.value()], |row| {
            let role = row.get::<_, String>(3)?;
            let role = ContributorRole::parse(&role).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    format!("invalid contributor role {role:?}").into(),
                )
            })?;
            Ok(ContributorCredit {
                contributor: Contributor {
                    id: ContributorId::new(row.get(0)?),
                    display_name: row.get(1)?,
                    sort_name: row.get(2)?,
                },
                role,
                position: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let series = connection
        .query_row(
            "SELECT s.id, s.name, sm.series_index \
             FROM series_memberships sm \
             JOIN series_entities s ON s.id = sm.series_id \
             WHERE sm.book_id = ?1",
            [book_id.value()],
            |row| {
                let index = series_index_from_database(row.get(2)?, 2)?;
                Ok(SeriesMembership {
                    series: Series {
                        id: SeriesId::new(row.get(0)?),
                        name: row.get(1)?,
                    },
                    index,
                })
            },
        )
        .optional()?;

    let tags = {
        let mut statement = connection.prepare_cached(
            "SELECT t.id, t.name FROM book_tags bt \
             JOIN tags t ON t.id = bt.tag_id \
             WHERE bt.book_id = ?1 ORDER BY t.identity_key, t.id",
        )?;
        let rows = statement.query_map([book_id.value()], |row| {
            Ok(Tag {
                id: TagId::new(row.get(0)?),
                name: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    Ok((contributors, series, tags))
}

/// Atomically stores ordinary metadata and every authoritative curation relationship.
pub(super) fn save_book_edit(transaction: &Transaction<'_>, edit: &BookEdit) -> Result<()> {
    validate_credit_positions(&edit.contributors)?;

    let changed = transaction.execute(
        "UPDATE books SET title = ?1, sort_title = ?2, \
         publisher = ?3, language = ?4, description = ?5, modified_at = unixepoch() \
         WHERE id = ?6",
        params![
            edit.title.trim(),
            sortable(&edit.title),
            optional_text(edit.publisher.as_deref()),
            optional_text(edit.language.as_deref()),
            optional_text(edit.description.as_deref()),
            edit.id.value(),
        ],
    )?;
    if changed == 0 {
        return Err(StorageError::BookNotFound(edit.id));
    }

    let resolved_credits = edit
        .contributors
        .iter()
        .map(|credit| resolve_credit(transaction, credit))
        .collect::<Result<Vec<_>>>()?;
    let mut unique_credits = HashSet::with_capacity(resolved_credits.len());
    for (contributor, role, _, _, _) in &resolved_credits {
        if !unique_credits.insert((*contributor, *role)) {
            return Err(StorageError::InvalidCuration(format!(
                "contributor {contributor} appears more than once as {role}"
            )));
        }
    }

    let resolved_series = edit
        .series
        .as_ref()
        .map(|membership| resolve_series(transaction, membership))
        .transpose()?;
    let mut resolved_tags = edit
        .tags
        .iter()
        .map(|tag| resolve_tag(transaction, tag))
        .collect::<Result<Vec<_>>>()?;
    resolved_tags.sort_unstable_by_key(|(id, _, _)| *id);
    resolved_tags.dedup_by_key(|(id, _, _)| *id);

    transaction.execute(
        "DELETE FROM book_contributors WHERE book_id = ?1",
        [edit.id.value()],
    )?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO book_contributors( \
                 book_id, contributor_id, role, position, \
                 display_name_projection, sort_key_projection \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (id, role, position, display_name, sort_key) in resolved_credits {
            insert.execute(params![
                edit.id.value(),
                id.value(),
                role.as_str(),
                position,
                display_name,
                sort_key,
            ])?;
        }
    }

    transaction.execute(
        "DELETE FROM series_memberships WHERE book_id = ?1",
        [edit.id.value()],
    )?;
    if let Some((series_id, name, key, index)) = resolved_series {
        transaction.execute(
            "INSERT INTO series_memberships( \
                 book_id, series_id, series_index, name_projection, key_projection \
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                edit.id.value(),
                series_id.value(),
                index.map(|value| {
                    i64::try_from(value.scaled()).expect("valid series index fits in SQLite")
                }),
                name,
                key,
            ],
        )?;
    }

    transaction.execute(
        "DELETE FROM book_tags WHERE book_id = ?1",
        [edit.id.value()],
    )?;
    {
        let mut insert =
            transaction.prepare("INSERT INTO book_tags(book_id, tag_id) VALUES (?1, ?2)")?;
        for (tag_id, _, _) in resolved_tags {
            insert.execute(params![edit.id.value(), tag_id.value()])?;
        }
    }

    rebuild_book_projection(transaction, edit.id.value())
}

fn validate_credit_positions(credits: &[ContributorCreditEdit]) -> Result<()> {
    for role in ContributorRole::ALL {
        let mut positions = credits
            .iter()
            .filter(|credit| credit.role == role)
            .map(|credit| credit.position)
            .collect::<Vec<_>>();
        positions.sort_unstable();
        if positions
            .iter()
            .enumerate()
            .any(|(expected, observed)| usize::try_from(*observed) != Ok(expected))
        {
            return Err(StorageError::InvalidCuration(format!(
                "{role} credit positions must be contiguous from zero"
            )));
        }
    }
    Ok(())
}

fn resolve_credit(
    transaction: &Transaction<'_>,
    credit: &ContributorCreditEdit,
) -> Result<(ContributorId, ContributorRole, u32, String, String)> {
    let (id, display_name, sort_key) = match &credit.contributor {
        ContributorReference::Existing(id) => transaction
            .query_row(
                "SELECT display_name, sort_key FROM contributors WHERE id = ?1",
                [id.value()],
                |row| Ok((*id, row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::InvalidCuration(format!("contributor {id} does not exist"))
            })?,
        ContributorReference::New {
            display_name,
            sort_name,
        } => {
            let display_name = normalize_user_name(NameKind::Contributor, display_name)?;
            let sort_name = normalize_user_name(NameKind::Contributor, sort_name)?;
            let key = identity_key(&display_name);
            let sort_key = identity_key(&sort_name);
            transaction.query_row(
                "INSERT INTO contributors(display_name, sort_name, identity_key, sort_key) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
                 RETURNING id, display_name, sort_key",
                params![display_name, sort_name, key, sort_key],
                |row| Ok((ContributorId::new(row.get(0)?), row.get(1)?, row.get(2)?)),
            )?
        }
    };
    Ok((id, credit.role, credit.position, display_name, sort_key))
}

fn resolve_series(
    transaction: &Transaction<'_>,
    membership: &SeriesMembershipEdit,
) -> Result<(SeriesId, String, String, Option<SeriesIndex>)> {
    let (id, name, key) = match &membership.series {
        SeriesReference::Existing(id) => transaction
            .query_row(
                "SELECT name, identity_key FROM series_entities WHERE id = ?1",
                [id.value()],
                |row| Ok((*id, row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StorageError::InvalidCuration(format!("series {id} does not exist")))?,
        SeriesReference::New(name) => {
            let name = normalize_user_name(NameKind::Series, name)?;
            let key = identity_key(&name);
            transaction.query_row(
                "INSERT INTO series_entities(name, identity_key) VALUES (?1, ?2) \
                 ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
                 RETURNING id, name, identity_key",
                params![name, key],
                |row| Ok((SeriesId::new(row.get(0)?), row.get(1)?, row.get(2)?)),
            )?
        }
    };
    Ok((id, name, key, membership.index))
}

fn resolve_tag(
    transaction: &Transaction<'_>,
    tag: &TagReference,
) -> Result<(TagId, String, String)> {
    match tag {
        TagReference::Existing(id) => transaction
            .query_row(
                "SELECT name, identity_key FROM tags WHERE id = ?1",
                [id.value()],
                |row| Ok((*id, row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StorageError::InvalidCuration(format!("tag {id} does not exist"))),
        TagReference::New(name) => {
            let name = normalize_user_name(NameKind::Tag, name)?;
            let key = identity_key(&name);
            transaction
                .query_row(
                    "INSERT INTO tags(name, identity_key) VALUES (?1, ?2) \
                 ON CONFLICT(identity_key) DO UPDATE SET identity_key = excluded.identity_key \
                 RETURNING id, name, identity_key",
                    params![name, key],
                    |row| Ok((TagId::new(row.get(0)?), row.get(1)?, row.get(2)?)),
                )
                .map_err(Into::into)
        }
    }
}

/// Returns selected contributors first, followed by bounded identity-prefix matches.
pub(super) fn autocomplete_contributors(
    connection: &Connection,
    prefix: &str,
    selected: &[ContributorId],
    limit: u32,
) -> Result<Vec<ContributorUsage>> {
    let limit = usize::try_from(limit.min(50)).expect("limit is at most fifty");
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut results = Vec::with_capacity(limit);
    let mut seen = HashSet::with_capacity(selected.len());
    let mut selected_statement = connection.prepare_cached(
        "SELECT c.display_name, c.sort_name, count(DISTINCT bc.book_id) \
         FROM contributors c LEFT JOIN book_contributors bc ON bc.contributor_id = c.id \
         WHERE c.id = ?1 GROUP BY c.id",
    )?;
    for id in selected.iter().copied().take(limit) {
        if !seen.insert(id) {
            continue;
        }
        if let Some(entry) = selected_statement
            .query_row([id.value()], |row| contributor_usage_row(id, row))
            .optional()?
        {
            results.push(entry);
        }
    }
    if results.len() == limit {
        return Ok(results);
    }

    let (lower, upper) = prefix_bounds(NameKind::Contributor, prefix)?;
    let candidate_limit = i64::try_from(limit + seen.len()).unwrap_or(i64::MAX);
    let mut statement = connection.prepare_cached(
        "SELECT c.id, c.display_name, c.sort_name, count(DISTINCT bc.book_id) \
         FROM contributors c LEFT JOIN book_contributors bc ON bc.contributor_id = c.id \
         WHERE c.identity_key >= ?1 AND c.identity_key < ?2 \
         GROUP BY c.id ORDER BY c.identity_key, c.id LIMIT ?3",
    )?;
    let rows = statement.query_map(params![lower, upper, candidate_limit], |row| {
        let id = ContributorId::new(row.get(0)?);
        contributor_usage_row_offset(id, row, 1)
    })?;
    for row in rows {
        let entry = row?;
        if seen.insert(entry.contributor.id) {
            results.push(entry);
            if results.len() == limit {
                break;
            }
        }
    }
    Ok(results)
}

/// Returns selected series first, followed by bounded identity-prefix matches.
pub(super) fn autocomplete_series(
    connection: &Connection,
    prefix: &str,
    selected: &[SeriesId],
    limit: u32,
) -> Result<Vec<SeriesUsage>> {
    let limit = usize::try_from(limit.min(50)).expect("limit is at most fifty");
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut results = Vec::with_capacity(limit);
    let mut seen = HashSet::with_capacity(selected.len());
    let mut selected_statement = connection.prepare_cached(
        "SELECT s.name, count(sm.book_id) FROM series_entities s \
         LEFT JOIN series_memberships sm ON sm.series_id = s.id \
         WHERE s.id = ?1 GROUP BY s.id",
    )?;
    for id in selected.iter().copied().take(limit) {
        if !seen.insert(id) {
            continue;
        }
        if let Some((name, books)) = selected_statement
            .query_row([id.value()], |row| Ok((row.get(0)?, row.get::<_, i64>(1)?)))
            .optional()?
        {
            results.push(SeriesUsage {
                series: Series { id, name },
                books: checked_count(books)?,
            });
        }
    }
    if results.len() == limit {
        return Ok(results);
    }

    let (lower, upper) = prefix_bounds(NameKind::Series, prefix)?;
    let candidate_limit = i64::try_from(limit + seen.len()).unwrap_or(i64::MAX);
    let mut statement = connection.prepare_cached(
        "SELECT s.id, s.name, count(sm.book_id) FROM series_entities s \
         LEFT JOIN series_memberships sm ON sm.series_id = s.id \
         WHERE s.identity_key >= ?1 AND s.identity_key < ?2 \
         GROUP BY s.id ORDER BY s.identity_key, s.id LIMIT ?3",
    )?;
    let rows = statement.query_map(params![lower, upper, candidate_limit], |row| {
        Ok((
            SeriesId::new(row.get(0)?),
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (id, name, books) = row?;
        if seen.insert(id) {
            results.push(SeriesUsage {
                series: Series { id, name },
                books: checked_count(books)?,
            });
            if results.len() == limit {
                break;
            }
        }
    }
    Ok(results)
}

/// Returns selected tags first, followed by bounded identity-prefix matches.
pub(super) fn autocomplete_tags(
    connection: &Connection,
    prefix: &str,
    selected: &[TagId],
    limit: u32,
) -> Result<Vec<TagUsage>> {
    let limit = usize::try_from(limit.min(50)).expect("limit is at most fifty");
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut results = Vec::with_capacity(limit);
    let mut seen = HashSet::with_capacity(selected.len());
    let sql = "SELECT t.name, count(DISTINCT bt.book_id), \
                      count(DISTINCT sti.saved_search_id) + count(DISTINCT ste.saved_search_id) \
               FROM tags t LEFT JOIN book_tags bt ON bt.tag_id = t.id \
               LEFT JOIN saved_search_included_tags sti ON sti.tag_id = t.id \
               LEFT JOIN saved_search_excluded_tags ste ON ste.tag_id = t.id \
               WHERE t.id = ?1 GROUP BY t.id";
    let mut selected_statement = connection.prepare_cached(sql)?;
    for id in selected.iter().copied().take(limit) {
        if !seen.insert(id) {
            continue;
        }
        if let Some((name, books, searches)) = selected_statement
            .query_row([id.value()], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
            })
            .optional()?
        {
            results.push(TagUsage {
                tag: Tag { id, name },
                books: checked_count(books)?,
                saved_searches: checked_count(searches)?,
            });
        }
    }
    if results.len() == limit {
        return Ok(results);
    }

    let (lower, upper) = prefix_bounds(NameKind::Tag, prefix)?;
    let candidate_limit = i64::try_from(limit + seen.len()).unwrap_or(i64::MAX);
    let mut statement = connection.prepare_cached(
        "SELECT t.id, t.name, count(DISTINCT bt.book_id), \
                count(DISTINCT sti.saved_search_id) + count(DISTINCT ste.saved_search_id) \
         FROM tags t LEFT JOIN book_tags bt ON bt.tag_id = t.id \
         LEFT JOIN saved_search_included_tags sti ON sti.tag_id = t.id \
         LEFT JOIN saved_search_excluded_tags ste ON ste.tag_id = t.id \
         WHERE t.identity_key >= ?1 AND t.identity_key < ?2 \
         GROUP BY t.id ORDER BY t.identity_key, t.id LIMIT ?3",
    )?;
    let rows = statement.query_map(params![lower, upper, candidate_limit], |row| {
        Ok((
            TagId::new(row.get(0)?),
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (id, name, books, searches) = row?;
        if seen.insert(id) {
            results.push(TagUsage {
                tag: Tag { id, name },
                books: checked_count(books)?,
                saved_searches: checked_count(searches)?,
            });
            if results.len() == limit {
                break;
            }
        }
    }
    Ok(results)
}

fn contributor_usage_row(
    id: ContributorId,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ContributorUsage> {
    contributor_usage_row_offset(id, row, 0)
}

fn contributor_usage_row_offset(
    id: ContributorId,
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<ContributorUsage> {
    let count = row.get::<_, i64>(offset + 2)?;
    let books = u64::try_from(count)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(offset + 2, count))?;
    Ok(ContributorUsage {
        contributor: Contributor {
            id,
            display_name: row.get(offset)?,
            sort_name: row.get(offset + 1)?,
        },
        books,
    })
}

fn prefix_bounds(kind: NameKind, prefix: &str) -> Result<(String, String)> {
    if let Some((scalar_index, _)) = prefix
        .chars()
        .enumerate()
        .find(|(_, character)| character.is_control())
    {
        return Err(StorageError::InvalidCuration(format!(
            "{kind} prefix contains a control character at position {scalar_index}"
        )));
    }
    if prefix.chars().count() > kind.maximum_scalars() {
        return Err(StorageError::InvalidCuration(format!(
            "{kind} prefix exceeds {} Unicode scalar values",
            kind.maximum_scalars()
        )));
    }
    let lower = identity_key(prefix);
    let mut upper = lower.clone();
    upper.push(char::MAX);
    Ok((lower, upper))
}

fn checked_count(count: i64) -> Result<u64> {
    u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))
}

pub(super) fn series_index_from_database(
    value: Option<i64>,
    column: usize,
) -> rusqlite::Result<Option<SeriesIndex>> {
    value
        .map(|value| {
            let scaled = u64::try_from(value)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))?;
            SeriesIndex::from_scaled(scaled)
                .ok_or(rusqlite::Error::IntegralValueOutOfRange(column, value))
        })
        .transpose()
}

/// Rebuilds every bounded browser and FTS projection for one logical book from database state.
pub(super) fn rebuild_book_projection(transaction: &Transaction<'_>, book_id: i64) -> Result<()> {
    let credits = load_projected_credits(transaction, book_id)?;

    let author_credits = credits
        .iter()
        .filter(|(role, _, _)| role == "author")
        .collect::<Vec<_>>();
    let authors_search = author_credits
        .iter()
        .map(|(_, display, _)| display.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let contributors_search = credits
        .iter()
        .map(|(_, display, _)| display.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let (authors, sort_authors) = if let Some((_, _, sort_key)) = author_credits.first() {
        (
            author_credits
                .iter()
                .map(|(_, display, _)| display.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            (*sort_key).clone(),
        )
    } else if let Some((role, display, sort_key)) = credits.first() {
        (
            format!("{display} ({})", role_display(role)),
            sort_key.clone(),
        )
    } else {
        (String::new(), String::new())
    };

    let membership = transaction
        .query_row(
            "SELECT name_projection, key_projection, series_index \
             FROM series_memberships WHERE book_id = ?1",
            [book_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    let (series, series_key, series_index) = membership
        .map_or((None, None, None), |(name, key, index)| {
            (Some(name), Some(key), index)
        });

    let tags_search = {
        let mut statement = transaction.prepare(
            "SELECT t.name FROM book_tags bt \
             JOIN tags t ON t.id = bt.tag_id \
             WHERE bt.book_id = ?1 ORDER BY t.identity_key, t.id",
        )?;
        let rows = statement.query_map([book_id], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?.join(" ")
    };

    let changed = transaction.execute(
        "UPDATE books SET \
             authors = ?2, authors_search = ?3, contributors_search = ?4, \
             sort_authors = ?5, series = ?6, series_key = ?7, series_index = ?8, \
             tags_search = ?9 \
         WHERE id = ?1",
        params![
            book_id,
            authors,
            authors_search,
            contributors_search,
            sort_authors,
            series,
            series_key,
            series_index,
            tags_search,
        ],
    )?;
    if changed == 0 {
        return Err(StorageError::Integrity(format!(
            "cannot rebuild projection for absent book {book_id}"
        )));
    }
    Ok(())
}

fn load_projected_credits(
    transaction: &Transaction<'_>,
    book_id: i64,
) -> Result<Vec<(String, String, String)>> {
    let mut statement = transaction.prepare(
        "SELECT role, display_name_projection, sort_key_projection \
         FROM book_contributors WHERE book_id = ?1 \
         ORDER BY \
             CASE role \
                 WHEN 'author' THEN 0 \
                 WHEN 'editor' THEN 1 \
                 WHEN 'translator' THEN 2 \
                 WHEN 'illustrator' THEN 3 \
                 ELSE 4 \
             END, \
             position, contributor_id",
    )?;
    let rows = statement.query_map([book_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn role_display(role: &str) -> &'static str {
    match role {
        "author" => "Author",
        "editor" => "Editor",
        "translator" => "Translator",
        "illustrator" => "Illustrator",
        _ => "Other",
    }
}

/// Verifies normalized relationships, derived projections, and identity keys.
pub(super) fn validate_organisation_schema(transaction: &Transaction<'_>) -> Result<()> {
    validate_identity_keys(transaction, "contributors", "display_name", "identity_key")?;
    validate_identity_keys(transaction, "series_entities", "name", "identity_key")?;
    validate_identity_keys(transaction, "tags", "name", "identity_key")?;
    validate_identity_keys(transaction, "saved_searches", "name", "identity_key")?;

    let invalid_positions = transaction.query_row(
        "SELECT EXISTS( \
             SELECT 1 FROM book_contributors \
             GROUP BY book_id, role \
             HAVING min(position) <> 0 OR max(position) + 1 <> count(*) \
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if invalid_positions {
        return Err(StorageError::Integrity(
            "contributor positions are not contiguous within a role".into(),
        ));
    }

    let overlapping_saved_tags = transaction.query_row(
        "SELECT EXISTS( \
             SELECT 1 FROM saved_search_included_tags included \
             JOIN saved_search_excluded_tags excluded \
               ON excluded.saved_search_id = included.saved_search_id \
              AND excluded.tag_id = included.tag_id \
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if overlapping_saved_tags {
        return Err(StorageError::Integrity(
            "a saved search both includes and excludes the same tag".into(),
        ));
    }

    let stale_projection =
        transaction.query_row(PROJECTION_INTEGRITY_QUERY, [], |row| row.get::<_, bool>(0))?;
    if stale_projection {
        return Err(StorageError::Integrity(
            "normalized curation projections are stale".into(),
        ));
    }
    Ok(())
}

fn validate_identity_keys(
    transaction: &Transaction<'_>,
    table: &str,
    display_column: &str,
    key_column: &str,
) -> Result<()> {
    let sql = format!("SELECT {display_column}, {key_column} FROM {table} ORDER BY id");
    let mut statement = transaction.prepare(&sql)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let display = row.get::<_, String>(0)?;
        let stored = row.get::<_, String>(1)?;
        if identity_key(&display) != stored {
            return Err(StorageError::Integrity(format!(
                "{table} contains a stale identity key"
            )));
        }
    }
    Ok(())
}

const PROJECTION_INTEGRITY_QUERY: &str = r"
WITH
author_ordered AS (
    SELECT book_id, display_name_projection, sort_key_projection
    FROM book_contributors
    WHERE role = 'author'
    ORDER BY book_id, position, contributor_id
),
author_projection AS (
    SELECT
        book_id,
        group_concat(display_name_projection, ', ') AS display_line,
        group_concat(display_name_projection, ' ') AS search_text
    FROM author_ordered
    GROUP BY book_id
),
first_author AS (
    SELECT book_id, sort_key_projection
    FROM (
        SELECT
            book_id,
            sort_key_projection,
            row_number() OVER (
                PARTITION BY book_id ORDER BY position, contributor_id
            ) AS credit_number
        FROM book_contributors
        WHERE role = 'author'
    )
    WHERE credit_number = 1
),
contributor_ordered AS (
    SELECT
        book_id,
        display_name_projection,
        sort_key_projection,
        role,
        row_number() OVER (
            PARTITION BY book_id
            ORDER BY
                CASE role
                    WHEN 'author' THEN 0
                    WHEN 'editor' THEN 1
                    WHEN 'translator' THEN 2
                    WHEN 'illustrator' THEN 3
                    ELSE 4
                END,
                position,
                contributor_id
        ) AS credit_number
    FROM book_contributors
),
contributor_projection AS (
    SELECT book_id, group_concat(display_name_projection, ' ') AS search_text
    FROM contributor_ordered
    GROUP BY book_id
),
first_contributor AS (
    SELECT book_id, display_name_projection, sort_key_projection, role
    FROM contributor_ordered
    WHERE credit_number = 1
),
tag_ordered AS (
    SELECT bt.book_id, t.name
    FROM book_tags bt
    JOIN tags t ON t.id = bt.tag_id
    ORDER BY bt.book_id, t.identity_key, t.id
),
tag_projection AS (
    SELECT book_id, group_concat(name, ' ') AS search_text
    FROM tag_ordered
    GROUP BY book_id
)
SELECT EXISTS(
    SELECT 1
    FROM books b
    LEFT JOIN author_projection authors ON authors.book_id = b.id
    LEFT JOIN first_author first_author ON first_author.book_id = b.id
    LEFT JOIN contributor_projection contributors ON contributors.book_id = b.id
    LEFT JOIN first_contributor first_contributor ON first_contributor.book_id = b.id
    LEFT JOIN series_memberships membership ON membership.book_id = b.id
    LEFT JOIN tag_projection tags ON tags.book_id = b.id
    WHERE b.authors IS NOT CASE
              WHEN authors.display_line IS NOT NULL THEN authors.display_line
              WHEN first_contributor.book_id IS NOT NULL THEN
                  first_contributor.display_name_projection || ' (' ||
                  CASE first_contributor.role
                      WHEN 'author' THEN 'Author'
                      WHEN 'editor' THEN 'Editor'
                      WHEN 'translator' THEN 'Translator'
                      WHEN 'illustrator' THEN 'Illustrator'
                      ELSE 'Other'
                  END || ')'
              ELSE ''
          END
       OR b.authors_search IS NOT coalesce(authors.search_text, '')
       OR b.contributors_search IS NOT coalesce(contributors.search_text, '')
       OR b.sort_authors IS NOT coalesce(
              first_author.sort_key_projection,
              first_contributor.sort_key_projection,
              ''
          )
       OR b.series IS NOT membership.name_projection
       OR b.series_key IS NOT membership.key_projection
       OR b.series_index IS NOT membership.series_index
       OR b.tags_search IS NOT coalesce(tags.search_text, '')
)
";

#[cfg(test)]
mod tests {
    use lectern_core::organisation::identity_key;
    use rusqlite::Connection;

    use super::super::{SCHEMA, SCHEMA_VERSION, initialize_schema_transaction};

    fn version_five_library() -> Connection {
        let connection = Connection::open_in_memory().expect("open version-five fixture");
        connection.execute_batch(SCHEMA).expect("create v5 schema");
        for (id, authors, series) in [
            (7, "Le Guin, Ursula & Charles Vess", Some("Earthsea")),
            (8, "Le Guin, Ursula & Charles Vess", Some("Earthsea")),
            (9, "Octavia E. Butler", None),
        ] {
            connection
                .execute(
                    "INSERT INTO books( \
                         id, title, sort_title, authors, sort_authors, series \
                     ) VALUES (?1, ?2, ?2, ?3, ?3, ?4)",
                    rusqlite::params![id, format!("Book {id}"), authors, series],
                )
                .expect("insert v5 book");
            connection
                .execute(
                    "INSERT INTO book_assets(id, book_id, format, path) \
                     VALUES (?1, ?1, 'epub', CAST(?2 AS BLOB))",
                    rusqlite::params![id, format!("/books/{id}.epub")],
                )
                .expect("insert v5 asset");
        }
        connection
            .execute(
                "INSERT INTO book_covers(book_id, jpeg) VALUES (7, x'010203')",
                [],
            )
            .expect("insert v5 cover");
        connection
            .pragma_update(None, "user_version", 5)
            .expect("mark v5 schema");
        connection
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn migration_preserves_flattened_projection_and_stable_library_data() {
        let mut connection = version_five_library();
        initialize_schema_transaction(&mut connection).expect("migrate v5 library");

        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read schema version");
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM contributors", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM book_contributors", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            3
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM series_entities", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM series_memberships", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM tags", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM saved_searches", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );

        let projection = connection
            .query_row(
                "SELECT id, authors, authors_search, contributors_search, series, \
                        has_cover \
                 FROM books WHERE id = 7",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(projection.0, 7);
        assert_eq!(projection.1, "Le Guin, Ursula & Charles Vess");
        assert_eq!(projection.2, projection.1);
        assert_eq!(projection.3, projection.1);
        assert_eq!(projection.4.as_deref(), Some("Earthsea"));
        assert!(projection.5);
        assert_eq!(
            connection
                .query_row("SELECT id FROM book_assets WHERE book_id = 7", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            7
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT identity_key FROM contributors WHERE display_name = ?1",
                    ["Le Guin, Ursula & Charles Vess"],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            identity_key("Le Guin, Ursula & Charles Vess")
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM books_fts WHERE books_fts MATCH '\"Charles\"*'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn migration_installs_covering_relationship_indexes() {
        let mut connection = version_five_library();
        initialize_schema_transaction(&mut connection).expect("migrate v5 library");
        for index in [
            "book_contributors_book_role_position_idx",
            "book_contributors_contributor_role_book_idx",
            "series_memberships_series_index_book_idx",
            "book_tags_tag_book_idx",
        ] {
            let exists = connection
                .query_row(
                    "SELECT EXISTS( \
                         SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1 \
                     )",
                    [index],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap();
            assert!(exists, "missing {index}");
        }
    }

    #[test]
    fn invalid_legacy_name_rolls_back_the_entire_schema_upgrade() {
        let mut connection = version_five_library();
        connection
            .execute(
                "UPDATE books SET authors = 'bad' || char(10) || 'name' WHERE id = 7",
                [],
            )
            .expect("inject invalid legacy author");

        assert!(initialize_schema_transaction(&mut connection).is_err());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let normalized_table_exists = connection
            .query_row(
                "SELECT EXISTS( \
                     SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'contributors' \
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(!normalized_table_exists);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM books_fts WHERE books_fts MATCH 'Book'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
    }
}
