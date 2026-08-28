//! Normalized organisation and structured-search domain values.

use std::{fmt, ops::Range, str::FromStr};

use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::{BookId, BookRating, PublicationDate};

macro_rules! stable_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(i64);

        impl $name {
            /// Creates an identifier from its database representation.
            #[must_use]
            pub const fn new(value: i64) -> Self {
                Self(value)
            }

            /// Returns the database representation of this identifier.
            #[must_use]
            pub const fn value(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

stable_id!(
    ContributorId,
    "Stable identifier for one library contributor."
);
stable_id!(SeriesId, "Stable identifier for one normalized series.");
stable_id!(TagId, "Stable identifier for one normalized tag.");
stable_id!(
    IdentifierTypeId,
    "Stable identifier for one normalized book-identifier type."
);
stable_id!(
    VirtualLibraryId,
    "Stable identifier for one user-created virtual library."
);
stable_id!(
    SavedSearchId,
    "Stable identifier for one saved library projection."
);

/// Kind of identity-bearing name accepted by the curation model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameKind {
    /// Contributor display or sort name.
    Contributor,
    /// Series display name.
    Series,
    /// Tag display name.
    Tag,
    /// Book-identifier type display name.
    IdentifierType,
    /// Saved-search display name.
    SavedSearch,
    /// Virtual-library display name.
    VirtualLibrary,
}

impl NameKind {
    /// Maximum Unicode scalar values accepted after whitespace normalization.
    #[must_use]
    pub const fn maximum_scalars(self) -> usize {
        match self {
            Self::Contributor | Self::Series => 256,
            Self::Tag | Self::IdentifierType => 64,
            Self::SavedSearch | Self::VirtualLibrary => 80,
        }
    }
}

impl fmt::Display for NameKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Contributor => "contributor name",
            Self::Series => "series name",
            Self::Tag => "tag name",
            Self::IdentifierType => "identifier type",
            Self::SavedSearch => "saved-search name",
            Self::VirtualLibrary => "virtual-library name",
        })
    }
}

/// Invalid identity-bearing display or sort name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NameValidationError {
    /// A control character appeared in the input.
    ControlCharacter {
        /// Name kind being validated.
        kind: NameKind,
        /// Zero-based Unicode scalar position of the control character.
        scalar_index: usize,
    },
    /// Whitespace normalization produced an empty name.
    Empty {
        /// Name kind being validated.
        kind: NameKind,
    },
    /// The normalized name exceeded its field-specific scalar limit.
    TooLong {
        /// Name kind being validated.
        kind: NameKind,
        /// Observed normalized Unicode scalar count.
        scalars: usize,
        /// Maximum accepted Unicode scalar count.
        maximum: usize,
    },
}

impl fmt::Display for NameValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ControlCharacter { kind, scalar_index } => {
                write!(
                    formatter,
                    "{kind} contains a control character at position {scalar_index}"
                )
            }
            Self::Empty { kind } => write!(formatter, "{kind} must not be empty"),
            Self::TooLong {
                kind,
                scalars,
                maximum,
            } => write!(
                formatter,
                "{kind} contains {scalars} Unicode scalar values; the maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for NameValidationError {}

/// Validates and whitespace-normalizes an identity-bearing display or sort name.
///
/// Leading and trailing Unicode whitespace is removed and internal Unicode whitespace runs are
/// collapsed to one ASCII space. Control characters are rejected before whitespace normalization.
///
/// # Errors
///
/// Returns an error for control characters, empty normalized input, or a field-specific length
/// violation.
pub fn normalize_name(kind: NameKind, input: &str) -> Result<String, NameValidationError> {
    let mut normalized = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut saw_non_whitespace = false;

    for (scalar_index, character) in input.chars().enumerate() {
        if character.is_control() {
            return Err(NameValidationError::ControlCharacter { kind, scalar_index });
        }
        if character.is_whitespace() {
            pending_space = saw_non_whitespace;
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        normalized.push(character);
        saw_non_whitespace = true;
    }

    if normalized.is_empty() {
        return Err(NameValidationError::Empty { kind });
    }
    let scalars = normalized.chars().count();
    let maximum = kind.maximum_scalars();
    if scalars > maximum {
        return Err(NameValidationError::TooLong {
            kind,
            scalars,
            maximum,
        });
    }
    Ok(normalized)
}

/// Produces the shared stable-identity key defined by ADR 0003.
///
/// Callers should validate the corresponding display name with [`normalize_name`] before storing
/// the key. This function applies Unicode NFKC normalization, Unicode-whitespace trimming and
/// collapse, then full non-Turkic Unicode case folding. Punctuation, diacritics, initials, and word
/// order are retained.
#[must_use]
pub fn identity_key(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut saw_non_whitespace = false;

    for character in input.nfkc() {
        if character.is_whitespace() {
            pending_space = saw_non_whitespace;
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        normalized.push(character);
        saw_non_whitespace = true;
    }

    normalized.case_fold().collect()
}

/// Ordered role held by a contributor on one book.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContributorRole {
    /// Author credit.
    Author,
    /// Editor credit.
    Editor,
    /// Translator credit.
    Translator,
    /// Illustrator credit.
    Illustrator,
    /// Any supported credit not represented by the more specific roles.
    Other,
}

impl ContributorRole {
    /// Every supported role in presentation order.
    pub const ALL: [Self; 5] = [
        Self::Author,
        Self::Editor,
        Self::Translator,
        Self::Illustrator,
        Self::Other,
    ];

    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::Editor => "editor",
            Self::Translator => "translator",
            Self::Illustrator => "illustrator",
            Self::Other => "other",
        }
    }

    /// Parses a stable lowercase storage value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "author" => Some(Self::Author),
            "editor" => Some(Self::Editor),
            "translator" => Some(Self::Translator),
            "illustrator" => Some(Self::Illustrator),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

impl fmt::Display for ContributorRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Author => "Author",
            Self::Editor => "Editor",
            Self::Translator => "Translator",
            Self::Illustrator => "Illustrator",
            Self::Other => "Other",
        })
    }
}

/// One stable contributor entity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Contributor {
    /// Stable contributor identifier.
    pub id: ContributorId,
    /// Display-ready contributor name.
    pub display_name: String,
    /// Independently editable sort name.
    pub sort_name: String,
}

/// One ordered contributor credit on a logical book.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContributorCredit {
    /// Referenced contributor.
    pub contributor: Contributor,
    /// Role held by the contributor on this book.
    pub role: ContributorRole,
    /// Zero-based position within this role.
    pub position: u32,
}

/// One ordered contributor credit obtained directly from publication metadata.
///
/// Source adapters use this shape before stable library identities have been resolved. The
/// storage boundary validates names, resolves identities, and writes every credit atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedContributorCredit {
    /// Display-ready contributor name exactly as bounded and cleaned by the source adapter.
    pub display_name: String,
    /// Role declared by the source adapter.
    pub role: ContributorRole,
    /// Zero-based position within the declared role.
    pub position: u32,
}

/// Normalized organisation metadata supplied by a publication adapter.
///
/// `Some` of this value is authoritative for a newly imported book, including an empty credit
/// list. `None` retains compatibility with callers that only supply flattened author and series
/// strings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportedOrganisation {
    /// Ordered source contributor credits.
    pub contributors: Vec<ImportedContributorCredit>,
    /// Optional exact source series position.
    pub series_index: Option<SeriesIndex>,
}

/// One stable normalized series entity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Series {
    /// Stable series identifier.
    pub id: SeriesId,
    /// Display-ready series name.
    pub name: String,
}

/// One stable normalized tag entity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    /// Stable tag identifier.
    pub id: TagId,
    /// Display-ready tag name.
    pub name: String,
    /// User-selected presentation color.
    pub color: TagColor,
}

/// One genre from Lectern's fixed, built-in book classification catalog.
///
/// Genres are deliberately represented as a closed enum rather than user-owned vocabulary rows,
/// so neither frontends nor persistence adapters can create arbitrary genre values.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Genre {
    /// Art, design, architecture, and photography.
    ArtAndPhotography,
    /// Biography, autobiography, and memoir.
    BiographyAndMemoir,
    /// Business, finance, and economics.
    BusinessAndEconomics,
    /// Books primarily written for children.
    Childrens,
    /// Enduring and historically significant literature.
    Classics,
    /// Comics, manga, and graphic novels.
    ComicsAndGraphicNovels,
    /// Cooking, food writing, and wine.
    CookbooksFoodAndWine,
    /// Crime stories and mysteries.
    CrimeAndMystery,
    /// Education, study, and general reference.
    EducationAndReference,
    /// Literary and personal essays.
    Essays,
    /// Fantasy fiction.
    Fantasy,
    /// Fiction set substantially in a historical period.
    HistoricalFiction,
    /// History and historical analysis.
    History,
    /// Horror fiction.
    Horror,
    /// Humor and comedy.
    Humor,
    /// Literary fiction.
    LiteraryFiction,
    /// Poetry.
    Poetry,
    /// Politics, current affairs, and society.
    PoliticsAndSociety,
    /// Religion, theology, and spirituality.
    ReligionAndSpirituality,
    /// Romance fiction.
    Romance,
    /// Science, mathematics, technology, and nature.
    ScienceAndNature,
    /// Science fiction.
    ScienceFiction,
    /// Personal development and self-help.
    SelfHelp,
    /// Sports, games, and recreation.
    SportsAndRecreation,
    /// Thrillers and suspense fiction.
    ThrillerAndSuspense,
    /// Travel guides and travel writing.
    Travel,
    /// True crime.
    TrueCrime,
    /// Books primarily written for young adults.
    YoungAdult,
}

impl Genre {
    /// Every genre available to the metadata editor, in display order.
    pub const ALL: [Self; 28] = [
        Self::ArtAndPhotography,
        Self::BiographyAndMemoir,
        Self::BusinessAndEconomics,
        Self::Childrens,
        Self::Classics,
        Self::ComicsAndGraphicNovels,
        Self::CookbooksFoodAndWine,
        Self::CrimeAndMystery,
        Self::EducationAndReference,
        Self::Essays,
        Self::Fantasy,
        Self::HistoricalFiction,
        Self::History,
        Self::Horror,
        Self::Humor,
        Self::LiteraryFiction,
        Self::Poetry,
        Self::PoliticsAndSociety,
        Self::ReligionAndSpirituality,
        Self::Romance,
        Self::ScienceAndNature,
        Self::ScienceFiction,
        Self::SelfHelp,
        Self::SportsAndRecreation,
        Self::ThrillerAndSuspense,
        Self::Travel,
        Self::TrueCrime,
        Self::YoungAdult,
    ];

    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArtAndPhotography => "art_and_photography",
            Self::BiographyAndMemoir => "biography_and_memoir",
            Self::BusinessAndEconomics => "business_and_economics",
            Self::Childrens => "childrens",
            Self::Classics => "classics",
            Self::ComicsAndGraphicNovels => "comics_and_graphic_novels",
            Self::CookbooksFoodAndWine => "cookbooks_food_and_wine",
            Self::CrimeAndMystery => "crime_and_mystery",
            Self::EducationAndReference => "education_and_reference",
            Self::Essays => "essays",
            Self::Fantasy => "fantasy",
            Self::HistoricalFiction => "historical_fiction",
            Self::History => "history",
            Self::Horror => "horror",
            Self::Humor => "humor",
            Self::LiteraryFiction => "literary_fiction",
            Self::Poetry => "poetry",
            Self::PoliticsAndSociety => "politics_and_society",
            Self::ReligionAndSpirituality => "religion_and_spirituality",
            Self::Romance => "romance",
            Self::ScienceAndNature => "science_and_nature",
            Self::ScienceFiction => "science_fiction",
            Self::SelfHelp => "self_help",
            Self::SportsAndRecreation => "sports_and_recreation",
            Self::ThrillerAndSuspense => "thriller_and_suspense",
            Self::Travel => "travel",
            Self::TrueCrime => "true_crime",
            Self::YoungAdult => "young_adult",
        }
    }

    /// Parses a stable storage value from a library database.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|genre| genre.as_str() == value)
    }
}

impl fmt::Display for Genre {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ArtAndPhotography => "Art & Photography",
            Self::BiographyAndMemoir => "Biography & Memoir",
            Self::BusinessAndEconomics => "Business & Economics",
            Self::Childrens => "Children's",
            Self::Classics => "Classics",
            Self::ComicsAndGraphicNovels => "Comics & Graphic Novels",
            Self::CookbooksFoodAndWine => "Cookbooks, Food & Wine",
            Self::CrimeAndMystery => "Crime & Mystery",
            Self::EducationAndReference => "Education & Reference",
            Self::Essays => "Essays",
            Self::Fantasy => "Fantasy",
            Self::HistoricalFiction => "Historical Fiction",
            Self::History => "History",
            Self::Horror => "Horror",
            Self::Humor => "Humor",
            Self::LiteraryFiction => "Literary Fiction",
            Self::Poetry => "Poetry",
            Self::PoliticsAndSociety => "Politics & Society",
            Self::ReligionAndSpirituality => "Religion & Spirituality",
            Self::Romance => "Romance",
            Self::ScienceAndNature => "Science & Nature",
            Self::ScienceFiction => "Science Fiction",
            Self::SelfHelp => "Self-Help",
            Self::SportsAndRecreation => "Sports & Recreation",
            Self::ThrillerAndSuspense => "Thriller & Suspense",
            Self::Travel => "Travel",
            Self::TrueCrime => "True Crime",
            Self::YoungAdult => "Young Adult",
        })
    }
}

/// One user-created library projection backed by explicit book membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualLibrary {
    /// Stable virtual-library identifier.
    pub id: VirtualLibraryId,
    /// Display-ready unique library name.
    pub name: String,
    /// Optional user-authored description.
    pub description: Option<String>,
    /// User-selected built-in cover icon.
    pub icon: VirtualLibraryIcon,
    /// Number of canonical books currently assigned to the virtual library.
    pub books: u64,
}

/// Built-in cover icons available to virtual libraries.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VirtualLibraryIcon {
    /// A compact stack of books.
    #[default]
    Books,
    /// A bookmark for reading lists.
    Bookmark,
    /// A star for favorites and highlights.
    Star,
    /// A heart for personal collections.
    Heart,
    /// A mortarboard for study collections.
    Academic,
    /// A globe for place or language collections.
    World,
}

impl VirtualLibraryIcon {
    /// Every selectable icon in presentation order.
    pub const ALL: [Self; 6] = [
        Self::Books,
        Self::Bookmark,
        Self::Star,
        Self::Heart,
        Self::Academic,
        Self::World,
    ];

    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Books => "books",
            Self::Bookmark => "bookmark",
            Self::Star => "star",
            Self::Heart => "heart",
            Self::Academic => "academic",
            Self::World => "world",
        }
    }

    /// Parses a stable lowercase storage value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "books" => Some(Self::Books),
            "bookmark" => Some(Self::Bookmark),
            "star" => Some(Self::Star),
            "heart" => Some(Self::Heart),
            "academic" => Some(Self::Academic),
            "world" => Some(Self::World),
            _ => None,
        }
    }

    /// Returns the compact glyph used by the native UI.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Books => "▥",
            Self::Bookmark => "◆",
            Self::Star => "★",
            Self::Heart => "♥",
            Self::Academic => "▲",
            Self::World => "◎",
        }
    }
}

impl fmt::Display for VirtualLibraryIcon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Books => "Books",
            Self::Bookmark => "Bookmark",
            Self::Star => "Star",
            Self::Heart => "Heart",
            Self::Academic => "Academic",
            Self::World => "World",
        })
    }
}

/// Exact outcome of changing one book's virtual-library membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualLibraryMembershipResult {
    /// Refreshed virtual-library metadata and derived book count.
    pub library: VirtualLibrary,
    /// Final membership state requested by the caller.
    pub included: bool,
    /// Whether the durable relationship changed.
    pub changed: bool,
}

/// Default identifier types stored in every Lectern library.
pub const DEFAULT_IDENTIFIER_TYPES: [&str; 10] = [
    "ISBN",
    "ASIN",
    "DOI",
    "Google Books",
    "Goodreads",
    "Open Library",
    "OCLC",
    "LCCN",
    "ISSN",
    "arXiv",
];

/// One stable normalized identifier type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifierType {
    /// Stable identifier-type identity.
    pub id: IdentifierTypeId,
    /// Display-ready type name.
    pub name: String,
}

/// One identifier assigned to a logical book.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookIdentifier {
    /// Stable or newly resolved identifier type.
    pub identifier_type: IdentifierType,
    /// Display-ready identifier value.
    pub value: String,
}

/// Restrained, named colors available to library tags.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TagColor {
    /// Neutral slate, including tags created before colors were introduced.
    #[default]
    Slate,
    /// Warm coral red.
    Coral,
    /// Warm amber orange.
    Amber,
    /// Fresh mint green.
    Mint,
    /// Clear azure blue.
    Azure,
    /// Soft lilac purple.
    Lilac,
}

impl TagColor {
    /// Every selectable tag color in presentation order.
    pub const ALL: [Self; 6] = [
        Self::Coral,
        Self::Amber,
        Self::Mint,
        Self::Azure,
        Self::Lilac,
        Self::Slate,
    ];

    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Slate => "slate",
            Self::Coral => "coral",
            Self::Amber => "amber",
            Self::Mint => "mint",
            Self::Azure => "azure",
            Self::Lilac => "lilac",
        }
    }

    /// Parses a stable lowercase storage value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "slate" => Some(Self::Slate),
            "coral" => Some(Self::Coral),
            "amber" => Some(Self::Amber),
            "mint" => Some(Self::Mint),
            "azure" => Some(Self::Azure),
            "lilac" => Some(Self::Lilac),
            _ => None,
        }
    }
}

impl fmt::Display for TagColor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Slate => "Slate",
            Self::Coral => "Coral",
            Self::Amber => "Amber",
            Self::Mint => "Mint",
            Self::Azure => "Azure",
            Self::Lilac => "Lilac",
        })
    }
}

/// Existing or not-yet-persisted contributor selected in a book edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContributorReference {
    /// Reuse one stable library contributor.
    Existing(ContributorId),
    /// Create or reuse the contributor identified by these validated names.
    New {
        /// Display-ready contributor name.
        display_name: String,
        /// Independently editable sort name.
        sort_name: String,
    },
}

/// One ordered contributor credit submitted by the metadata editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContributorCreditEdit {
    /// Existing or new contributor identity.
    pub contributor: ContributorReference,
    /// Role held on this book.
    pub role: ContributorRole,
    /// Zero-based position within the selected role.
    pub position: u32,
}

/// Existing or not-yet-persisted series selected in a book edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeriesReference {
    /// Reuse one stable library series.
    Existing(SeriesId),
    /// Create or reuse the series identified by this name.
    New(String),
}

/// Optional series relation submitted by the metadata editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeriesMembershipEdit {
    /// Existing or new series identity.
    pub series: SeriesReference,
    /// Optional exact position within the series.
    pub index: Option<SeriesIndex>,
}

/// Existing or not-yet-persisted tag selected in a book edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TagReference {
    /// Reuse one stable library tag.
    Existing(TagId),
    /// Create or reuse the tag identified by this name.
    New(String),
    /// Create or reuse the tag identified by this name and color.
    NewColored {
        /// Display-ready tag name.
        name: String,
        /// Presentation color used when a new identity is created.
        color: TagColor,
    },
}

/// Existing or not-yet-persisted identifier type selected in a book edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentifierTypeReference {
    /// Reuse one stable library identifier type.
    Existing(IdentifierTypeId),
    /// Create or reuse the identifier type identified by this name.
    New(String),
}

/// One identifier submitted by the metadata editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookIdentifierEdit {
    /// Existing or new identifier-type identity.
    pub identifier_type: IdentifierTypeReference,
    /// Identifier value, preserved apart from surrounding whitespace.
    pub value: String,
}

/// Complete metadata-editor payload for one logical book.
///
/// Assets are intentionally absent: curation saves cannot detach, relink, replace, or mutate a
/// publication file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookEdit {
    /// Stable logical-book identity.
    pub id: BookId,
    /// Display title.
    pub title: String,
    /// Optional publisher.
    pub publisher: Option<String>,
    /// Optional calendar publication date.
    pub publication_date: Option<PublicationDate>,
    /// Optional publication language.
    pub language: Option<String>,
    /// Optional description or synopsis.
    pub description: Option<String>,
    /// Personal zero-to-five-star rating in exact half-star steps.
    pub rating: BookRating,
    /// Ordered contributor credits.
    pub contributors: Vec<ContributorCreditEdit>,
    /// Optional series relation and index.
    pub series: Option<SeriesMembershipEdit>,
    /// Unordered set of tags.
    pub tags: Vec<TagReference>,
    /// Unordered set selected from Lectern's fixed genre catalog.
    pub genres: Vec<Genre>,
    /// Identifiers in stable type-and-value display order.
    pub identifiers: Vec<BookIdentifierEdit>,
}

/// Contributor autocomplete or vocabulary row with a global usage count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContributorUsage {
    /// Stable contributor entity.
    pub contributor: Contributor,
    /// Number of distinct logical books crediting the contributor.
    pub books: u64,
}

/// Series autocomplete or vocabulary row with a global usage count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeriesUsage {
    /// Stable series entity.
    pub series: Series,
    /// Number of logical books in the series.
    pub books: u64,
}

/// Tag autocomplete or vocabulary row with global usage counts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagUsage {
    /// Stable tag entity.
    pub tag: Tag,
    /// Number of logical books assigned the tag.
    pub books: u64,
    /// Number of saved searches that include or exclude the tag.
    pub saved_searches: u64,
}

/// Identifier-type autocomplete row with a global assignment count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifierTypeUsage {
    /// Stable identifier-type entity.
    pub identifier_type: IdentifierType,
    /// Number of book identifiers using this type.
    pub identifiers: u64,
}

/// Maximum Unicode scalar values accepted in a book-identifier value.
pub const IDENTIFIER_VALUE_MAXIMUM_SCALARS: usize = 512;

/// Validates and trims a book-identifier value without changing its internal representation.
///
/// # Errors
///
/// Returns an error when the value is empty, contains a control character, or exceeds the
/// identifier-value limit.
pub fn normalize_identifier_value(input: &str) -> Result<String, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("identifier value must not be empty".into());
    }
    if let Some((scalar_index, _)) = value
        .chars()
        .enumerate()
        .find(|(_, character)| character.is_control())
    {
        return Err(format!(
            "identifier value contains a control character at position {scalar_index}"
        ));
    }
    let scalars = value.chars().count();
    if scalars > IDENTIFIER_VALUE_MAXIMUM_SCALARS {
        return Err(format!(
            "identifier value contains {scalars} Unicode scalar values; the maximum is {IDENTIFIER_VALUE_MAXIMUM_SCALARS}"
        ));
    }
    Ok(value.to_owned())
}

/// Connection-visible library state used to invalidate query-backed selections.
///
/// The value is intentionally opaque to frontends. It changes after a serialized write on the
/// current adapter and when another connection commits to the same library.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LibraryGeneration {
    /// Changes made through the current database connection.
    pub connection_changes: u64,
    /// `SQLite` data-version observation for commits made by other connections.
    pub data_version: u64,
}

/// Compact target set for a range or bulk operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BookSelection {
    /// Explicit stable book identities, stored sorted and without duplicates.
    Explicit(Vec<BookId>),
    /// Every book in one canonical projection except explicit stable exclusions.
    AllMatching {
        /// Complete query/filter/sort state captured when selection was established.
        query: crate::LibraryQuery,
        /// Library state captured alongside the matching count.
        generation: LibraryGeneration,
        /// Stable book identities explicitly removed from the selection.
        excluded: Vec<BookId>,
    },
}

impl BookSelection {
    /// Builds a canonical explicit selection.
    #[must_use]
    pub fn explicit(mut books: Vec<BookId>) -> Self {
        books.sort_unstable();
        books.dedup();
        Self::Explicit(books)
    }

    /// Builds a canonical all-matching selection descriptor.
    #[must_use]
    pub fn all_matching(
        query: crate::LibraryQuery,
        generation: LibraryGeneration,
        mut excluded: Vec<BookId>,
    ) -> Self {
        excluded.sort_unstable();
        excluded.dedup();
        Self::AllMatching {
            query,
            generation,
            excluded,
        }
    }
}

/// Count and invalidation token captured without loading matching book summaries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SelectionSnapshot {
    /// Number of books in the current projection.
    pub matching_books: u64,
    /// Library generation at the same read boundary as `matching_books`.
    pub generation: LibraryGeneration,
}

/// Atomic tag changes requested for one compact book selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BulkTagEdit {
    /// Existing or newly named tags to assign to every target book.
    pub add: Vec<TagReference>,
    /// Existing tags to remove from every target book.
    pub remove: Vec<TagId>,
}

/// Exact outcome of one committed bulk-tag operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BulkTagResult {
    /// Books matched by the target descriptor at transaction start.
    pub books_matched: u64,
    /// New book/tag relationships inserted.
    pub relationships_added: u64,
    /// Existing book/tag relationships removed.
    pub relationships_removed: u64,
    /// New normalized tag identities created.
    pub tags_created: u64,
}

/// Exact outcome of one atomic, committed bulk library removal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BulkRemovalResult {
    /// Logical books removed from the library.
    pub books_removed: u64,
}

/// Observed state of one tag across a target selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionTagUsage {
    /// Stable tag and global usage counts.
    pub usage: TagUsage,
    /// Number of selected books currently assigned this tag.
    pub selected_books: u64,
}

/// One durable named library projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedSearch {
    /// Stable saved-search identity.
    pub id: SavedSearchId,
    /// Display-ready unique name.
    pub name: String,
    /// Complete canonical query/filter/sort state.
    pub query: crate::LibraryQuery,
}

/// Counts presented before or after a library-wide vocabulary mutation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VocabularyMutationResult {
    /// Distinct logical books whose relationships or projections were affected.
    pub books: u64,
    /// Durable saved searches whose exact facets were affected.
    pub saved_searches: u64,
}

/// Exact fixed-point series position in millionths.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SeriesIndex(u64);

impl SeriesIndex {
    /// Fixed scale used for exact storage and comparison.
    pub const SCALE: u64 = 1_000_000;
    /// Greatest accepted scaled value (`999999.999999`).
    pub const MAX_SCALED: u64 = 999_999 * Self::SCALE + 999_999;

    /// Creates a value from its exact millionths representation.
    #[must_use]
    pub const fn from_scaled(value: u64) -> Option<Self> {
        if value <= Self::MAX_SCALED {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the exact millionths representation.
    #[must_use]
    pub const fn scaled(self) -> u64 {
        self.0
    }
}

/// Invalid exact decimal series index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeriesIndexError {
    /// The value did not use unsigned decimal syntax.
    InvalidSyntax,
    /// More than six fractional digits were supplied.
    ExcessivePrecision,
    /// The value was greater than `999999.999999`.
    OutOfRange,
}

impl fmt::Display for SeriesIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSyntax => "series index must be an unsigned decimal",
            Self::ExcessivePrecision => "series index accepts at most six fractional digits",
            Self::OutOfRange => "series index must not exceed 999999.999999",
        })
    }
}

impl std::error::Error for SeriesIndexError {}

impl FromStr for SeriesIndex {
    type Err = SeriesIndexError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(SeriesIndexError::InvalidSyntax);
        }
        let (integer, fractional) = value
            .split_once('.')
            .map_or((value, None), |(integer, fractional)| {
                (integer, Some(fractional))
            });
        if integer.is_empty()
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || fractional.is_some_and(|digits| {
                digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Err(SeriesIndexError::InvalidSyntax);
        }
        let fractional = fractional.unwrap_or_default();
        if fractional.len() > 6 {
            return Err(SeriesIndexError::ExcessivePrecision);
        }
        let integer = integer
            .parse::<u64>()
            .map_err(|_| SeriesIndexError::OutOfRange)?;
        if integer > 999_999 {
            return Err(SeriesIndexError::OutOfRange);
        }
        let fractional_value = if fractional.is_empty() {
            0
        } else {
            fractional
                .parse::<u64>()
                .map_err(|_| SeriesIndexError::InvalidSyntax)?
                * 10_u64.pow(6 - u32::try_from(fractional.len()).expect("length is at most six"))
        };
        Self::from_scaled(integer * Self::SCALE + fractional_value)
            .ok_or(SeriesIndexError::OutOfRange)
    }
}

impl fmt::Display for SeriesIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let integer = self.0 / Self::SCALE;
        let fractional = self.0 % Self::SCALE;
        if fractional == 0 {
            return integer.fmt(formatter);
        }
        let mut digits = format!("{fractional:06}");
        while digits.ends_with('0') {
            digits.pop();
        }
        write!(formatter, "{integer}.{digits}")
    }
}

/// Optional normalized series relation on one logical book.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeriesMembership {
    /// Referenced series.
    pub series: Series,
    /// Optional exact position within the series.
    pub index: Option<SeriesIndex>,
}

/// Exact contributor facet stored in a canonical library projection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContributorFacet {
    /// Required contributor identity.
    pub contributor: ContributorId,
    /// Whether only Author-role credits satisfy this facet.
    pub author_only: bool,
}

/// Invalid combination of exact facet values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetError {
    /// A tag appeared in both include and exclude sets.
    IncludedAndExcluded(TagId),
}

impl fmt::Display for FacetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncludedAndExcluded(tag) => {
                write!(formatter, "tag {tag} cannot be both included and excluded")
            }
        }
    }
}

impl std::error::Error for FacetError {}

/// Canonical exact facets combined conjunctively with a structured search.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExactFacets {
    /// Required contributors, each optionally restricted to Author credits.
    pub contributors: Vec<ContributorFacet>,
    /// Optional required series.
    pub series: Option<SeriesId>,
    /// Tags every result must have.
    pub included_tags: Vec<TagId>,
    /// Tags every result must not have.
    pub excluded_tags: Vec<TagId>,
}

impl ExactFacets {
    /// Canonicalizes facet ordering, removes duplicates, and enforces disjoint tag sets.
    ///
    /// # Errors
    ///
    /// Returns an error when the same stable tag identity is both included and excluded.
    pub fn new(
        mut contributors: Vec<ContributorFacet>,
        series: Option<SeriesId>,
        mut included_tags: Vec<TagId>,
        mut excluded_tags: Vec<TagId>,
    ) -> Result<Self, FacetError> {
        contributors.sort_unstable();
        contributors.dedup();
        included_tags.sort_unstable();
        included_tags.dedup();
        excluded_tags.sort_unstable();
        excluded_tags.dedup();
        if let Some(tag) = first_intersection(&included_tags, &excluded_tags) {
            return Err(FacetError::IncludedAndExcluded(tag));
        }
        Ok(Self {
            contributors,
            series,
            included_tags,
            excluded_tags,
        })
    }

    /// Moves a tag into the include set while keeping include and exclude disjoint.
    pub fn include_tag(&mut self, tag: TagId) {
        remove_sorted(&mut self.excluded_tags, &tag);
        insert_sorted(&mut self.included_tags, tag);
    }

    /// Moves a tag into the exclude set while keeping include and exclude disjoint.
    pub fn exclude_tag(&mut self, tag: TagId) {
        remove_sorted(&mut self.included_tags, &tag);
        insert_sorted(&mut self.excluded_tags, tag);
    }
}

fn first_intersection(left: &[TagId], right: &[TagId]) -> Option<TagId> {
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => return Some(left[left_index]),
        }
    }
    None
}

fn insert_sorted<T: Copy + Ord>(values: &mut Vec<T>, value: T) {
    if let Err(index) = values.binary_search(&value) {
        values.insert(index, value);
    }
}

fn remove_sorted<T: Ord>(values: &mut Vec<T>, value: &T) {
    if let Ok(index) = values.binary_search(value) {
        values.remove(index);
    }
}

/// Text matching behavior for a structured-search clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextMatch {
    /// Unicode-aware prefix term.
    Prefix(String),
    /// Adjacent ordered phrase.
    Phrase(String),
}

impl TextMatch {
    /// Returns the clause text without its syntactic quoting.
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Prefix(value) | Self::Phrase(value) => value,
        }
    }
}

/// One safe conjunctive structured-search clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchClause {
    /// Text across title, contributors, series, publisher, and tags.
    Any(TextMatch),
    /// Title-only text.
    Title(TextMatch),
    /// Author-role contributor text.
    Author(TextMatch),
    /// Contributor text in any role.
    Contributor(TextMatch),
    /// Series-name text.
    Series(TextMatch),
    /// Tag-name text.
    Tag(TextMatch),
    /// Publisher text.
    Publisher(TextMatch),
    /// Exact case-insensitive language value.
    Language(String),
    /// Required attached publication format.
    Format(crate::BookFormat),
    /// Required last-observed file-health state.
    File(crate::AssetHealth),
}

/// Bounded, parsed structured-search expression.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchExpression {
    clauses: Vec<SearchClause>,
}

impl SearchExpression {
    /// Maximum accepted UTF-8 byte length.
    pub const MAX_BYTES: usize = 1_024;
    /// Maximum accepted conjunctive clause count.
    pub const MAX_CLAUSES: usize = 32;

    /// Parses safe field syntax without accepting raw FTS operators.
    ///
    /// # Errors
    ///
    /// Returns an error with a UTF-8 byte span for malformed syntax, unsupported fields or
    /// operators, invalid enum values, or bounded-input violations.
    pub fn parse(input: &str) -> Result<Self, SearchParseError> {
        SearchParser::new(input).parse()
    }

    /// Returns the ordered conjunctive clauses.
    #[must_use]
    pub fn clauses(&self) -> &[SearchClause] {
        &self.clauses
    }

    /// Returns a stable canonical expression suitable for saved searches.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut output = String::new();
        for clause in &self.clauses {
            if !output.is_empty() {
                output.push(' ');
            }
            match clause {
                SearchClause::Any(value) => push_text_match(&mut output, None, value),
                SearchClause::Title(value) => push_text_match(&mut output, Some("title"), value),
                SearchClause::Author(value) => push_text_match(&mut output, Some("author"), value),
                SearchClause::Contributor(value) => {
                    push_text_match(&mut output, Some("contributor"), value);
                }
                SearchClause::Series(value) => {
                    push_text_match(&mut output, Some("series"), value);
                }
                SearchClause::Tag(value) => push_text_match(&mut output, Some("tag"), value),
                SearchClause::Publisher(value) => {
                    push_text_match(&mut output, Some("publisher"), value);
                }
                SearchClause::Language(value) => {
                    output.push_str("language:");
                    output.push_str(value);
                }
                SearchClause::Format(value) => {
                    output.push_str("format:");
                    output.push_str(value.as_str());
                }
                SearchClause::File(value) => {
                    output.push_str("file:");
                    output.push_str(match value {
                        crate::AssetHealth::Unknown => "unchecked",
                        crate::AssetHealth::Available => "available",
                        crate::AssetHealth::Missing => "missing",
                        crate::AssetHealth::Unreadable => "unreadable",
                    });
                }
            }
        }
        output
    }
}

impl FromStr for SearchExpression {
    type Err = SearchParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

fn push_text_match(output: &mut String, field: Option<&str>, value: &TextMatch) {
    if let Some(field) = field {
        output.push_str(field);
        output.push(':');
    }
    match value {
        TextMatch::Prefix(value) => output.push_str(value),
        TextMatch::Phrase(value) => {
            output.push('"');
            for character in value.chars() {
                if matches!(character, '"' | '\\') {
                    output.push('\\');
                }
                output.push(character);
            }
            output.push('"');
        }
    }
}

/// Structured-search syntax error category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchParseErrorKind {
    /// The expression exceeded 1,024 UTF-8 bytes.
    TooLong,
    /// The expression exceeded 32 clauses.
    TooManyClauses,
    /// A quoted value was not terminated.
    UnmatchedQuote,
    /// A quoted value used an unsupported escape.
    InvalidEscape,
    /// A field did not have a value.
    EmptyValue,
    /// A field name is not supported.
    UnknownField(String),
    /// An exact enum field used an unsupported value.
    InvalidValue {
        /// Canonical field name.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
    /// Raw Boolean, grouping, wildcard, or unary syntax was supplied.
    UnsupportedOperator,
    /// A quote or other delimiter appeared in an invalid position.
    UnexpectedCharacter,
}

/// Structured-search syntax error with a UTF-8 byte source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchParseError {
    /// Error category.
    pub kind: SearchParseErrorKind,
    /// Half-open UTF-8 byte range in the original expression.
    pub span: Range<usize>,
}

impl fmt::Display for SearchParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match &self.kind {
            SearchParseErrorKind::TooLong => "search expression exceeds 1,024 UTF-8 bytes".into(),
            SearchParseErrorKind::TooManyClauses => {
                "search expression contains more than 32 clauses".into()
            }
            SearchParseErrorKind::UnmatchedQuote => "search phrase has no closing quote".into(),
            SearchParseErrorKind::InvalidEscape => {
                "quoted values only support \\\" and \\\\ escapes".into()
            }
            SearchParseErrorKind::EmptyValue => "search field has an empty value".into(),
            SearchParseErrorKind::UnknownField(field) => {
                format!("unknown search field {field:?}")
            }
            SearchParseErrorKind::InvalidValue { field, value } => {
                format!("invalid {field}: value {value:?}")
            }
            SearchParseErrorKind::UnsupportedOperator => {
                "raw Boolean, grouping, wildcard, and unary operators are unsupported".into()
            }
            SearchParseErrorKind::UnexpectedCharacter => {
                "unexpected character in search clause".into()
            }
        };
        write!(
            formatter,
            "{description} at bytes {}..{}",
            self.span.start, self.span.end
        )
    }
}

impl std::error::Error for SearchParseError {}

struct SearchParser<'a> {
    input: &'a str,
    position: usize,
    clauses: Vec<SearchClause>,
}

impl<'a> SearchParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            position: 0,
            clauses: Vec::new(),
        }
    }

    fn parse(mut self) -> Result<SearchExpression, SearchParseError> {
        if self.input.len() > SearchExpression::MAX_BYTES {
            return Err(Self::error(
                SearchParseErrorKind::TooLong,
                SearchExpression::MAX_BYTES..self.input.len(),
            ));
        }
        loop {
            self.skip_whitespace();
            if self.position == self.input.len() {
                break;
            }
            if self.clauses.len() == SearchExpression::MAX_CLAUSES {
                return Err(Self::error(
                    SearchParseErrorKind::TooManyClauses,
                    self.position..self.input.len(),
                ));
            }
            let clause = self.parse_clause()?;
            self.clauses.push(clause);
        }
        Ok(SearchExpression {
            clauses: self.clauses,
        })
    }

    fn parse_clause(&mut self) -> Result<SearchClause, SearchParseError> {
        let start = self.position;
        if self.peek() == Some('"') {
            let value = self.parse_quoted()?;
            self.require_boundary()?;
            return Ok(SearchClause::Any(TextMatch::Phrase(value)));
        }

        let head_end = self
            .scan_until(|character| character.is_whitespace() || matches!(character, ':' | '"'));
        if head_end == start {
            return Err(Self::error(
                SearchParseErrorKind::UnexpectedCharacter,
                start..self.next_position(start),
            ));
        }
        let head = &self.input[start..head_end];
        self.position = head_end;
        if self.peek() != Some(':') {
            Self::validate_unquoted(head, start)?;
            self.require_boundary()?;
            return Ok(SearchClause::Any(TextMatch::Prefix(head.to_owned())));
        }

        self.position += 1;
        let field = head.to_ascii_lowercase();
        if !matches!(
            field.as_str(),
            "title"
                | "author"
                | "contributor"
                | "series"
                | "tag"
                | "publisher"
                | "language"
                | "format"
                | "file"
        ) {
            return Err(Self::error(
                SearchParseErrorKind::UnknownField(head.to_owned()),
                start..head_end,
            ));
        }
        if self.position == self.input.len() || self.peek().is_some_and(char::is_whitespace) {
            return Err(Self::error(
                SearchParseErrorKind::EmptyValue,
                self.position..self.position,
            ));
        }
        let value_start = self.position;
        let (value, phrase) = if self.peek() == Some('"') {
            (self.parse_quoted()?, true)
        } else {
            let end = self.scan_until(char::is_whitespace);
            let value = self.input[value_start..end].to_owned();
            self.position = end;
            Self::validate_unquoted(&value, value_start)?;
            (value, false)
        };
        self.require_boundary()?;
        Self::build_field_clause(&field, value, phrase, value_start..self.position)
    }

    fn build_field_clause(
        field: &str,
        value: String,
        phrase: bool,
        span: Range<usize>,
    ) -> Result<SearchClause, SearchParseError> {
        let text = || {
            if phrase {
                TextMatch::Phrase(value.clone())
            } else {
                TextMatch::Prefix(value.clone())
            }
        };
        let clause = match field {
            "title" => SearchClause::Title(text()),
            "author" => SearchClause::Author(text()),
            "contributor" => SearchClause::Contributor(text()),
            "series" => SearchClause::Series(text()),
            "tag" => SearchClause::Tag(text()),
            "publisher" => SearchClause::Publisher(text()),
            "language" => {
                if value.chars().any(char::is_whitespace) {
                    return Err(Self::error(
                        SearchParseErrorKind::InvalidValue {
                            field: "language",
                            value,
                        },
                        span,
                    ));
                }
                SearchClause::Language(value.to_ascii_lowercase())
            }
            "format" => {
                let canonical = value.to_ascii_lowercase();
                let format = crate::BookFormat::parse(&canonical).ok_or_else(|| {
                    Self::error(
                        SearchParseErrorKind::InvalidValue {
                            field: "format",
                            value,
                        },
                        span,
                    )
                })?;
                SearchClause::Format(format)
            }
            "file" => {
                let canonical = value.to_ascii_lowercase();
                let health = match canonical.as_str() {
                    "available" => crate::AssetHealth::Available,
                    "missing" => crate::AssetHealth::Missing,
                    "unreadable" => crate::AssetHealth::Unreadable,
                    "unchecked" => crate::AssetHealth::Unknown,
                    _ => {
                        return Err(Self::error(
                            SearchParseErrorKind::InvalidValue {
                                field: "file",
                                value,
                            },
                            span,
                        ));
                    }
                };
                SearchClause::File(health)
            }
            _ => unreachable!("field was validated before value parsing"),
        };
        Ok(clause)
    }

    fn parse_quoted(&mut self) -> Result<String, SearchParseError> {
        let quote = self.position;
        self.position += 1;
        let mut value = String::new();
        let mut pending_space = false;
        let mut saw_non_whitespace = false;
        while let Some(character) = self.peek() {
            if character == '"' {
                self.position += 1;
                if value.is_empty() {
                    return Err(Self::error(
                        SearchParseErrorKind::EmptyValue,
                        quote..self.position,
                    ));
                }
                return Ok(value);
            }
            if character == '\\' {
                let escape = self.position;
                self.position += 1;
                let Some(escaped) = self.peek() else {
                    return Err(Self::error(
                        SearchParseErrorKind::UnmatchedQuote,
                        quote..self.input.len(),
                    ));
                };
                if !matches!(escaped, '"' | '\\') {
                    return Err(Self::error(
                        SearchParseErrorKind::InvalidEscape,
                        escape..self.next_position(self.position),
                    ));
                }
                if pending_space {
                    value.push(' ');
                    pending_space = false;
                }
                value.push(escaped);
                saw_non_whitespace = true;
                self.position += escaped.len_utf8();
                continue;
            }
            if character.is_whitespace() {
                pending_space = saw_non_whitespace;
                self.position += character.len_utf8();
                continue;
            }
            if pending_space {
                value.push(' ');
                pending_space = false;
            }
            value.push(character);
            saw_non_whitespace = true;
            self.position += character.len_utf8();
        }
        Err(Self::error(
            SearchParseErrorKind::UnmatchedQuote,
            quote..self.input.len(),
        ))
    }

    fn validate_unquoted(value: &str, start: usize) -> Result<(), SearchParseError> {
        if value.eq_ignore_ascii_case("or")
            || value.eq_ignore_ascii_case("and")
            || value.eq_ignore_ascii_case("not")
            || value.starts_with('-')
            || value.contains(['*', '(', ')'])
        {
            return Err(Self::error(
                SearchParseErrorKind::UnsupportedOperator,
                start..start + value.len(),
            ));
        }
        if let Some(relative) = value.find('"') {
            return Err(Self::error(
                SearchParseErrorKind::UnexpectedCharacter,
                start + relative..start + relative + 1,
            ));
        }
        Ok(())
    }

    fn require_boundary(&self) -> Result<(), SearchParseError> {
        if self.position < self.input.len() && !self.peek().is_some_and(char::is_whitespace) {
            return Err(Self::error(
                SearchParseErrorKind::UnexpectedCharacter,
                self.position..self.next_position(self.position),
            ));
        }
        Ok(())
    }

    fn scan_until(&self, predicate: impl Fn(char) -> bool) -> usize {
        let mut position = self.position;
        while let Some(character) = self.input[position..].chars().next() {
            if predicate(character) {
                break;
            }
            position += character.len_utf8();
        }
        position
    }

    fn skip_whitespace(&mut self) {
        while let Some(character) = self.peek() {
            if !character.is_whitespace() {
                break;
            }
            self.position += character.len_utf8();
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.position..].chars().next()
    }

    fn next_position(&self, position: usize) -> usize {
        self.input[position..]
            .chars()
            .next()
            .map_or(position, |character| position + character.len_utf8())
    }

    fn error(kind: SearchParseErrorKind, span: Range<usize>) -> SearchParseError {
        SearchParseError { kind, span }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContributorFacet, ContributorId, ExactFacets, FacetError, NameKind, NameValidationError,
        SearchClause, SearchExpression, SearchParseErrorKind, SeriesId, SeriesIndex,
        SeriesIndexError, TagId, TextMatch, identity_key, normalize_identifier_value,
        normalize_name,
    };
    use crate::{AssetHealth, BookFormat};

    #[test]
    fn stable_identifiers_round_trip_storage_values() {
        assert_eq!(ContributorId::new(7).value(), 7);
        assert_eq!(SeriesId::new(8).to_string(), "8");
        assert_eq!(TagId::new(9).value(), 9);
        assert_eq!(super::SavedSearchId::new(10).to_string(), "10");
        assert_eq!(super::VirtualLibraryId::new(11).value(), 11);
    }

    #[test]
    fn fixed_genres_round_trip_storage_values() {
        let mut storage_values = std::collections::HashSet::new();
        for genre in super::Genre::ALL {
            assert_eq!(super::Genre::parse(genre.as_str()), Some(genre));
            assert!(storage_values.insert(genre.as_str()));
            assert!(!genre.to_string().is_empty());
        }
        assert_eq!(storage_values.len(), 28);
        assert_eq!(super::Genre::parse("made_up_genre"), None);
    }

    #[test]
    fn identity_keys_follow_nfkc_whitespace_and_full_case_folding() {
        assert_eq!(
            identity_key("  Science\u{a0}\u{2003}Fiction  "),
            "science fiction"
        );
        assert_eq!(identity_key("Ｓｃｉｅｎｃｅ"), "science");
        assert_eq!(identity_key("Maße"), "masse");
        assert_ne!(
            identity_key("Ursula Le Guin"),
            identity_key("Ursula K. Le Guin")
        );
        assert_ne!(identity_key("Jose"), identity_key("José"));
    }

    #[test]
    fn name_validation_is_shared_bounded_and_control_safe() {
        assert_eq!(
            normalize_name(NameKind::Tag, "  Science\u{a0}  Fiction  ").unwrap(),
            "Science Fiction"
        );
        assert_eq!(
            normalize_name(NameKind::Tag, "\nscience").unwrap_err(),
            NameValidationError::ControlCharacter {
                kind: NameKind::Tag,
                scalar_index: 0,
            }
        );
        assert_eq!(
            normalize_name(NameKind::SavedSearch, " \u{2003} ").unwrap_err(),
            NameValidationError::Empty {
                kind: NameKind::SavedSearch,
            }
        );
        assert!(matches!(
            normalize_name(NameKind::Tag, &"x".repeat(65)),
            Err(NameValidationError::TooLong { maximum: 64, .. })
        ));
        assert!(normalize_name(NameKind::VirtualLibrary, &"x".repeat(80)).is_ok());
        assert!(matches!(
            normalize_name(NameKind::VirtualLibrary, &"x".repeat(81)),
            Err(NameValidationError::TooLong { maximum: 80, .. })
        ));
    }

    #[test]
    fn virtual_library_icons_round_trip_storage_values() {
        for icon in super::VirtualLibraryIcon::ALL {
            assert_eq!(super::VirtualLibraryIcon::parse(icon.as_str()), Some(icon));
            assert!(!icon.glyph().is_empty());
        }
    }

    #[test]
    fn identifier_values_preserve_content_and_reject_unsafe_bounds() {
        assert_eq!(
            normalize_identifier_value("  978-0-1234-5678-9  ").unwrap(),
            "978-0-1234-5678-9"
        );
        assert!(normalize_identifier_value(" \t ").is_err());
        assert!(normalize_identifier_value("doi\nvalue").is_err());
        assert!(normalize_identifier_value(&"x".repeat(513)).is_err());
    }

    #[test]
    fn series_indices_are_exact_bounded_and_canonical() {
        for (input, scaled, display) in [
            ("0", 0, "0"),
            ("1.2", 1_200_000, "1.2"),
            ("000001.230000", 1_230_000, "1.23"),
            ("999999.999999", SeriesIndex::MAX_SCALED, "999999.999999"),
        ] {
            let index = input.parse::<SeriesIndex>().unwrap();
            assert_eq!(index.scaled(), scaled);
            assert_eq!(index.to_string(), display);
        }
        assert_eq!(
            "1.0000001".parse::<SeriesIndex>(),
            Err(SeriesIndexError::ExcessivePrecision)
        );
        assert_eq!(
            "1000000".parse::<SeriesIndex>(),
            Err(SeriesIndexError::OutOfRange)
        );
        for invalid in ["", ".1", "1.", "+1", "-1", "1e2", " 1"] {
            assert_eq!(
                invalid.parse::<SeriesIndex>(),
                Err(SeriesIndexError::InvalidSyntax),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn exact_facets_are_sorted_unique_and_disjoint() {
        let facets = ExactFacets::new(
            vec![
                ContributorFacet {
                    contributor: ContributorId::new(2),
                    author_only: false,
                },
                ContributorFacet {
                    contributor: ContributorId::new(1),
                    author_only: true,
                },
                ContributorFacet {
                    contributor: ContributorId::new(2),
                    author_only: false,
                },
            ],
            Some(SeriesId::new(4)),
            vec![TagId::new(3), TagId::new(1), TagId::new(3)],
            vec![TagId::new(4)],
        )
        .unwrap();
        assert_eq!(facets.contributors.len(), 2);
        assert_eq!(facets.included_tags, [TagId::new(1), TagId::new(3)]);
        assert_eq!(facets.excluded_tags, [TagId::new(4)]);

        assert_eq!(
            ExactFacets::new(Vec::new(), None, vec![TagId::new(2)], vec![TagId::new(2)]),
            Err(FacetError::IncludedAndExcluded(TagId::new(2)))
        );

        let mut moving = ExactFacets::default();
        moving.exclude_tag(TagId::new(7));
        moving.include_tag(TagId::new(7));
        assert_eq!(moving.included_tags, [TagId::new(7)]);
        assert!(moving.excluded_tags.is_empty());
    }

    #[test]
    fn structured_search_parses_every_supported_clause() {
        let input = concat!(
            "dune \"left hand\" title:foundation author:le ",
            "contributor:\"ursula le guin\" series:earthsea ",
            "tag:\"science fiction\" publisher:ace language:EN ",
            "format:EPUB format:pdf file:unchecked"
        );
        let expression = SearchExpression::parse(input).unwrap();
        assert_eq!(expression.clauses().len(), 12);
        assert_eq!(
            expression.clauses()[0],
            SearchClause::Any(TextMatch::Prefix("dune".into()))
        );
        assert_eq!(
            expression.clauses()[1],
            SearchClause::Any(TextMatch::Phrase("left hand".into()))
        );
        assert_eq!(expression.clauses()[8], SearchClause::Language("en".into()));
        assert_eq!(
            expression.clauses()[9],
            SearchClause::Format(BookFormat::Epub)
        );
        assert_eq!(
            expression.clauses()[10],
            SearchClause::Format(BookFormat::Pdf)
        );
        assert_eq!(
            expression.clauses()[11],
            SearchClause::File(AssetHealth::Unknown)
        );
        assert_eq!(expression.canonical(), input.to_ascii_lowercase());
    }

    #[test]
    fn quoted_search_values_unescape_and_canonicalize() {
        let expression = SearchExpression::parse(r#"title:"a\\b \"quoted\" title""#).unwrap();
        assert_eq!(
            expression.clauses(),
            [SearchClause::Title(TextMatch::Phrase(
                "a\\b \"quoted\" title".into()
            ))]
        );
        assert_eq!(expression.canonical(), r#"title:"a\\b \"quoted\" title""#);
    }

    #[test]
    fn structured_search_rejects_raw_syntax_and_reports_byte_spans() {
        for input in ["dune OR earthsea", "NOT dune", "-dune", "dune*", "(dune)"] {
            assert!(matches!(
                SearchExpression::parse(input),
                Err(super::SearchParseError {
                    kind: SearchParseErrorKind::UnsupportedOperator,
                    ..
                })
            ));
        }
        let unknown = SearchExpression::parse("writer:le").unwrap_err();
        assert_eq!(
            unknown.kind,
            SearchParseErrorKind::UnknownField("writer".into())
        );
        assert_eq!(unknown.span, 0..6);
        let unmatched = SearchExpression::parse("tag:\"science").unwrap_err();
        assert_eq!(unmatched.kind, SearchParseErrorKind::UnmatchedQuote);
        assert_eq!(unmatched.span, 4..12);
        let invalid_enum = SearchExpression::parse("format:mobi").unwrap_err();
        assert!(matches!(
            invalid_enum.kind,
            SearchParseErrorKind::InvalidValue {
                field: "format",
                ..
            }
        ));
    }

    #[test]
    fn structured_search_enforces_byte_and_clause_bounds() {
        let long = "é".repeat(513);
        let error = SearchExpression::parse(&long).unwrap_err();
        assert_eq!(error.kind, SearchParseErrorKind::TooLong);
        assert_eq!(error.span, 1024..1026);

        let clauses = std::iter::repeat_n("term", 33)
            .collect::<Vec<_>>()
            .join(" ");
        let error = SearchExpression::parse(&clauses).unwrap_err();
        assert_eq!(error.kind, SearchParseErrorKind::TooManyClauses);
        assert_eq!(&clauses[error.span], "term");
    }
}
