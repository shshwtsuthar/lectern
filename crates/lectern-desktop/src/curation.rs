//! Pure metadata-editor state and validation.

use std::collections::{HashMap, HashSet};

use lectern_core::{
    Book, BookRating, PublicationDate,
    organisation::{
        BookEdit, BookIdentifierEdit, ContributorCreditEdit, ContributorId, ContributorReference,
        ContributorRole, Genre, IdentifierTypeId, IdentifierTypeReference, NameKind, SeriesId,
        SeriesIndex, SeriesMembershipEdit, SeriesReference, TagColor, TagId, TagReference,
        identity_key, normalize_identifier_value, normalize_name,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContributorDraft {
    pub(crate) row_id: u64,
    pub(crate) existing_id: Option<ContributorId>,
    pub(crate) name: String,
    pub(crate) sort_name: String,
    pub(crate) role: ContributorRole,
    pub(crate) confirmed_new: bool,
}

impl ContributorDraft {
    pub(crate) fn blank(row_id: u64) -> Self {
        Self {
            row_id,
            existing_id: None,
            name: String::new(),
            sort_name: String::new(),
            role: ContributorRole::Author,
            confirmed_new: false,
        }
    }

    pub(crate) fn select_existing(
        &mut self,
        id: ContributorId,
        display_name: &str,
        sort_name: &str,
    ) {
        self.existing_id = Some(id);
        display_name.clone_into(&mut self.name);
        sort_name.clone_into(&mut self.sort_name);
        self.confirmed_new = false;
    }

    pub(crate) fn confirm_new(&mut self) -> Result<(), String> {
        let name = normalize_name(NameKind::Contributor, &self.name).map_err(display_error)?;
        let sort_name = if self.sort_name.trim().is_empty() {
            name.clone()
        } else {
            normalize_name(NameKind::Contributor, &self.sort_name).map_err(display_error)?
        };
        self.existing_id = None;
        self.name = name;
        self.sort_name = sort_name;
        self.confirmed_new = true;
        Ok(())
    }

    pub(crate) fn name_edited(&mut self, previous_name: &str) {
        if self.existing_id.is_some()
            || self.sort_name.trim().is_empty()
            || self.sort_name == previous_name
        {
            self.sort_name.clone_from(&self.name);
        }
        self.existing_id = None;
        self.confirmed_new = false;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SeriesDraft {
    pub(crate) existing_id: Option<SeriesId>,
    pub(crate) name: String,
    pub(crate) index: String,
    pub(crate) confirmed_new: bool,
}

impl SeriesDraft {
    pub(crate) fn select_existing(&mut self, id: SeriesId, name: &str) {
        self.existing_id = Some(id);
        name.clone_into(&mut self.name);
        self.confirmed_new = false;
    }

    pub(crate) fn name_edited(&mut self) {
        self.existing_id = None;
        self.confirmed_new = false;
    }

    pub(crate) fn confirm_new(&mut self) -> Result<(), String> {
        self.name = normalize_name(NameKind::Series, &self.name).map_err(display_error)?;
        self.existing_id = None;
        self.confirmed_new = true;
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TagDraft {
    pub(crate) existing_id: Option<TagId>,
    pub(crate) name: String,
    pub(crate) color: TagColor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IdentifierDraft {
    pub(crate) existing_type_id: Option<IdentifierTypeId>,
    pub(crate) type_name: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BookCurationDraft {
    pub(crate) contributors: Vec<ContributorDraft>,
    pub(crate) series: SeriesDraft,
    pub(crate) tags: Vec<TagDraft>,
    pub(crate) genres: Vec<Genre>,
    pub(crate) identifiers: Vec<IdentifierDraft>,
    next_row_id: u64,
}

impl BookCurationDraft {
    pub(crate) fn from_book(book: &Book) -> Self {
        let contributors = book
            .contributors
            .iter()
            .enumerate()
            .map(|(index, credit)| ContributorDraft {
                row_id: u64::try_from(index).expect("credit count fits u64"),
                existing_id: Some(credit.contributor.id),
                name: credit.contributor.display_name.clone(),
                sort_name: credit.contributor.sort_name.clone(),
                role: credit.role,
                confirmed_new: false,
            })
            .collect::<Vec<_>>();
        let series =
            book.series_membership
                .as_ref()
                .map_or_else(SeriesDraft::default, |membership| SeriesDraft {
                    existing_id: Some(membership.series.id),
                    name: membership.series.name.clone(),
                    index: membership
                        .index
                        .map(|index| index.to_string())
                        .unwrap_or_default(),
                    confirmed_new: false,
                });
        let tags = book
            .tags
            .iter()
            .map(|tag| TagDraft {
                existing_id: Some(tag.id),
                name: tag.name.clone(),
                color: tag.color,
            })
            .collect();
        let identifiers = book
            .identifiers
            .iter()
            .map(|identifier| IdentifierDraft {
                existing_type_id: Some(identifier.identifier_type.id),
                type_name: identifier.identifier_type.name.clone(),
                value: identifier.value.clone(),
            })
            .collect();
        Self {
            next_row_id: u64::try_from(contributors.len()).expect("credit count fits u64"),
            contributors,
            series,
            tags,
            genres: book.genres.clone(),
            identifiers,
        }
    }

    pub(crate) fn add_contributor(&mut self) -> u64 {
        let row_id = self.next_row_id;
        self.next_row_id = self.next_row_id.wrapping_add(1);
        self.contributors.push(ContributorDraft::blank(row_id));
        row_id
    }

    pub(crate) fn existing_contributor_ids(&self) -> Vec<ContributorId> {
        self.contributors
            .iter()
            .filter_map(|draft| draft.existing_id)
            .collect()
    }

    pub(crate) fn existing_series_id(&self) -> Vec<SeriesId> {
        self.series.existing_id.into_iter().collect()
    }

    pub(crate) fn existing_tag_ids(&self) -> Vec<TagId> {
        self.tags
            .iter()
            .filter_map(|draft| draft.existing_id)
            .collect()
    }

    pub(crate) fn add_existing_tag(&mut self, id: TagId, name: &str, color: TagColor) -> bool {
        if self.tags.iter().any(|tag| tag.existing_id == Some(id)) {
            return false;
        }
        self.tags.push(TagDraft {
            existing_id: Some(id),
            name: name.to_owned(),
            color,
        });
        true
    }

    pub(crate) fn add_new_tag(&mut self, name: &str, color: TagColor) -> Result<bool, String> {
        let name = normalize_name(NameKind::Tag, name).map_err(display_error)?;
        let key = identity_key(&name);
        if self.tags.iter().any(|tag| identity_key(&tag.name) == key) {
            return Ok(false);
        }
        self.tags.push(TagDraft {
            existing_id: None,
            name,
            color,
        });
        Ok(true)
    }

    pub(crate) fn toggle_genre(&mut self, genre: Genre) -> bool {
        if let Some(index) = self.genres.iter().position(|selected| *selected == genre) {
            self.genres.remove(index);
            true
        } else {
            self.genres.push(genre);
            self.genres.sort_unstable();
            true
        }
    }

    #[allow(dead_code, reason = "used by the GPUI frontend")]
    pub(crate) fn add_identifier(
        &mut self,
        existing_type_id: Option<IdentifierTypeId>,
        type_name: &str,
        value: &str,
    ) -> Result<bool, String> {
        let type_name =
            normalize_name(NameKind::IdentifierType, type_name).map_err(display_error)?;
        let value = normalize_identifier_value(value)?;
        let type_key = identity_key(&type_name);
        if self.identifiers.iter().any(|identifier| {
            identity_key(&identifier.type_name) == type_key && identifier.value == value
        }) {
            return Ok(false);
        }
        self.identifiers.push(IdentifierDraft {
            existing_type_id,
            type_name,
            value,
        });
        Ok(true)
    }

    #[allow(dead_code, reason = "used by the GPUI frontend during its migration")]
    pub(crate) fn remove_tag_id(&mut self, id: TagId) -> bool {
        let previous = self.tags.len();
        self.tags.retain(|tag| tag.existing_id != Some(id));
        self.tags.len() != previous
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "canonical metadata validation keeps every edited scalar explicit"
    )]
    pub(crate) fn to_book_edit(
        &self,
        book: &Book,
        title: &str,
        publisher: &str,
        publication_date: &str,
        language: &str,
        description: &str,
        rating: BookRating,
    ) -> Result<BookEdit, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("A title is required.".into());
        }

        let mut role_positions = HashMap::<ContributorRole, u32>::new();
        let mut seen_credits = HashSet::<(String, ContributorRole)>::new();
        let mut contributors = Vec::with_capacity(self.contributors.len());
        for draft in &self.contributors {
            let contributor = if let Some(id) = draft.existing_id {
                ContributorReference::Existing(id)
            } else if draft.confirmed_new {
                ContributorReference::New {
                    display_name: normalize_name(NameKind::Contributor, &draft.name)
                        .map_err(display_error)?,
                    sort_name: normalize_name(NameKind::Contributor, &draft.sort_name)
                        .map_err(display_error)?,
                }
            } else {
                if draft.name.trim().is_empty() {
                    return Err("Complete or remove the empty contributor row.".into());
                }
                return Err(format!(
                    "Select an existing contributor or create '{}'.",
                    draft.name.trim()
                ));
            };
            let identity = match &contributor {
                ContributorReference::Existing(id) => format!("id:{}", id.value()),
                ContributorReference::New { display_name, .. } => {
                    format!("name:{}", identity_key(display_name))
                }
            };
            if !seen_credits.insert((identity, draft.role)) {
                return Err(format!(
                    "The same contributor cannot have the {} role twice.",
                    draft.role
                ));
            }
            let position = role_positions.entry(draft.role).or_default();
            contributors.push(ContributorCreditEdit {
                contributor,
                role: draft.role,
                position: *position,
            });
            *position = position.saturating_add(1);
        }

        let series = self.series_edit()?;
        let publication_date = (!publication_date.trim().is_empty())
            .then(|| publication_date.trim().parse::<PublicationDate>())
            .transpose()
            .map_err(|error| format!("Invalid publication date: {error}."))?;
        let mut tag_keys = HashSet::with_capacity(self.tags.len());
        let mut tags = Vec::with_capacity(self.tags.len());
        for tag in &self.tags {
            let name = normalize_name(NameKind::Tag, &tag.name).map_err(display_error)?;
            if !tag_keys.insert(identity_key(&name)) {
                return Err(format!("Tag '{name}' is assigned more than once."));
            }
            tags.push(tag.existing_id.map_or_else(
                || TagReference::NewColored {
                    name,
                    color: tag.color,
                },
                TagReference::Existing,
            ));
        }

        let mut identifier_keys = HashSet::with_capacity(self.identifiers.len());
        let mut identifiers = Vec::with_capacity(self.identifiers.len());
        for identifier in &self.identifiers {
            let type_name = normalize_name(NameKind::IdentifierType, &identifier.type_name)
                .map_err(display_error)?;
            let value = normalize_identifier_value(&identifier.value)?;
            if !identifier_keys.insert((identity_key(&type_name), value.clone())) {
                return Err(format!(
                    "{type_name} identifier '{value}' is assigned more than once."
                ));
            }
            identifiers.push(BookIdentifierEdit {
                identifier_type: identifier.existing_type_id.map_or_else(
                    || IdentifierTypeReference::New(type_name),
                    IdentifierTypeReference::Existing,
                ),
                value,
            });
        }

        Ok(BookEdit {
            id: book.id,
            title: title.to_owned(),
            publisher: optional_text(publisher),
            publication_date,
            language: optional_text(language),
            description: optional_text(description),
            rating,
            contributors,
            series,
            tags,
            genres: self.genres.clone(),
            identifiers,
        })
    }

    fn series_edit(&self) -> Result<Option<SeriesMembershipEdit>, String> {
        if self.series.name.trim().is_empty() {
            if self.series.index.trim().is_empty() {
                return Ok(None);
            }
            return Err("Clear the book number before removing its series.".into());
        }
        let series = if let Some(id) = self.series.existing_id {
            SeriesReference::Existing(id)
        } else if self.series.confirmed_new {
            SeriesReference::New(
                normalize_name(NameKind::Series, &self.series.name).map_err(display_error)?,
            )
        } else {
            return Err(format!(
                "Select an existing series or create '{}'.",
                self.series.name.trim()
            ));
        };
        let index = if self.series.index.trim().is_empty() {
            None
        } else {
            Some(
                self.series
                    .index
                    .trim()
                    .parse::<SeriesIndex>()
                    .map_err(display_error)?,
            )
        };
        Ok(Some(SeriesMembershipEdit { series, index }))
    }
}

fn optional_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use lectern_core::{
        AssetHealth, AssetId, AssetStorage, BookAsset, BookFormat, BookId, BookRating,
        organisation::{
            BookIdentifier, Contributor, ContributorCredit, ContributorId, ContributorRole, Genre,
            IdentifierType, IdentifierTypeId, Series, SeriesId, SeriesIndex, SeriesMembership, Tag,
            TagColor, TagId,
        },
    };

    use super::BookCurationDraft;

    fn book() -> lectern_core::Book {
        lectern_core::Book {
            id: BookId::new(1),
            title: "A Wizard of Earthsea".into(),
            authors: "Ursula K. Le Guin".into(),
            series: Some("Earthsea".into()),
            contributors: vec![ContributorCredit {
                contributor: Contributor {
                    id: ContributorId::new(2),
                    display_name: "Ursula K. Le Guin".into(),
                    sort_name: "Le Guin, Ursula K.".into(),
                },
                role: ContributorRole::Author,
                position: 0,
            }],
            series_membership: Some(SeriesMembership {
                series: Series {
                    id: SeriesId::new(3),
                    name: "Earthsea".into(),
                },
                index: Some("1.5".parse::<SeriesIndex>().unwrap()),
            }),
            tags: vec![Tag {
                id: TagId::new(4),
                name: "Fantasy".into(),
                color: TagColor::Slate,
            }],
            genres: vec![Genre::Fantasy],
            virtual_libraries: Vec::new(),
            identifiers: vec![BookIdentifier {
                identifier_type: IdentifierType {
                    id: IdentifierTypeId::new(6),
                    name: "ISBN".into(),
                },
                value: "978-0-1234-5678-9".into(),
            }],
            publisher: Some("Parnassus".into()),
            publication_date: Some("1968-09".parse().unwrap()),
            language: Some("en".into()),
            description: None,
            rating: BookRating::from_half_stars(9).unwrap(),
            assets: vec![BookAsset {
                id: AssetId::new(5),
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
                health: AssetHealth::Available,
                path: "/books/earthsea.epub".into(),
            }],
        }
    }

    #[test]
    fn loaded_curation_round_trips_to_stable_references() {
        let book = book();
        let draft = BookCurationDraft::from_book(&book);

        let edit = draft
            .to_book_edit(
                &book,
                &book.title,
                " Parnassus ",
                "1968-09",
                "en",
                "",
                book.rating,
            )
            .unwrap();

        assert_eq!(edit.contributors[0].position, 0);
        assert_eq!(edit.series.unwrap().index.unwrap().to_string(), "1.5");
        assert_eq!(edit.tags.len(), 1);
        assert_eq!(edit.genres, [Genre::Fantasy]);
        assert_eq!(edit.identifiers.len(), 1);
        assert_eq!(edit.description, None);
        assert_eq!(edit.publication_date.unwrap().to_string(), "1968-09");
        assert_eq!(edit.rating.half_stars(), 9);
    }

    #[test]
    fn new_credits_are_positioned_contiguously_within_each_role() {
        let book = book();
        let mut draft = BookCurationDraft::from_book(&book);
        let first = draft.add_contributor();
        let row = draft
            .contributors
            .iter_mut()
            .find(|row| row.row_id == first)
            .unwrap();
        row.name = "  Tehanu Editor  ".into();
        row.role = ContributorRole::Editor;
        row.confirm_new().unwrap();
        let second = draft.add_contributor();
        let row = draft
            .contributors
            .iter_mut()
            .find(|row| row.row_id == second)
            .unwrap();
        row.name = "Second Author".into();
        row.confirm_new().unwrap();

        let edit = draft
            .to_book_edit(&book, &book.title, "", "", "", "", BookRating::default())
            .unwrap();

        assert_eq!(edit.contributors[0].position, 0);
        assert_eq!(edit.contributors[1].position, 0);
        assert_eq!(edit.contributors[2].position, 1);
    }

    #[test]
    fn unresolved_identity_and_invalid_series_index_block_save() {
        let book = book();
        let mut draft = BookCurationDraft::from_book(&book);
        let row_id = draft.add_contributor();
        draft
            .contributors
            .iter_mut()
            .find(|row| row.row_id == row_id)
            .unwrap()
            .name = "Octavia Butler".into();
        assert!(
            draft
                .to_book_edit(&book, &book.title, "", "", "", "", BookRating::default(),)
                .unwrap_err()
                .contains("Select an existing contributor")
        );

        draft.contributors.pop();
        draft.series.index = "1.2.3".into();
        assert!(
            draft
                .to_book_edit(&book, &book.title, "", "", "", "", BookRating::default(),)
                .unwrap_err()
                .contains("unsigned decimal")
        );
    }

    #[test]
    fn tags_deduplicate_by_normalized_identity() {
        let mut draft = BookCurationDraft::from_book(&book());
        assert!(!draft.add_new_tag(" fantasy ", TagColor::Coral).unwrap());
        assert!(
            draft
                .add_new_tag("Science   Fiction", TagColor::Azure)
                .unwrap()
        );
        assert_eq!(draft.tags[1].name, "Science Fiction");
        assert_eq!(draft.tags[1].color, TagColor::Azure);
    }

    #[test]
    fn genres_toggle_only_fixed_catalog_values() {
        let mut draft = BookCurationDraft::from_book(&book());
        assert!(draft.toggle_genre(Genre::ScienceFiction));
        assert_eq!(draft.genres, [Genre::Fantasy, Genre::ScienceFiction]);
        assert!(draft.toggle_genre(Genre::Fantasy));
        assert_eq!(draft.genres, [Genre::ScienceFiction]);
    }

    #[test]
    fn identifiers_accept_multiple_values_and_deduplicate_exact_assignments() {
        let mut draft = BookCurationDraft::from_book(&book());
        assert!(
            draft
                .add_identifier(Some(IdentifierTypeId::new(6)), "ISBN", "978-1")
                .unwrap()
        );
        assert!(
            !draft
                .add_identifier(Some(IdentifierTypeId::new(6)), " isbn ", "978-1")
                .unwrap()
        );
        assert!(draft.add_identifier(None, "WikiData", " Q42 ").unwrap());

        let edit = draft
            .to_book_edit(&book(), "Title", "", "", "", "", BookRating::default())
            .unwrap();
        assert_eq!(edit.identifiers.len(), 3);
        assert_eq!(edit.identifiers[2].value, "Q42");
    }
}
