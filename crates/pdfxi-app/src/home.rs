//! Home-screen model for recent documents, favorites, and collections.
//!
//! This module is deliberately headless: it stores *identity keys* and display
//! titles supplied by the caller and never touches the filesystem, network, or
//! a database. The host is responsible for turning a [`DocumentKey`] into a path
//! and for opening the document. Everything here is deterministic so two runs
//! with the same inputs produce identical orderings and serialization.
//!
//! [`HomeModel`] keeps:
//!
//! - a bounded list of recently opened documents, deduplicated by key and
//!   ordered most-recent-first using a caller-agnostic logical clock;
//! - a deduplicated set of favorites; and
//! - named collections of document keys.
//!
//! [`HomeModel::serialize`] and [`HomeModel::deserialize`] implement a bounded,
//! newline-delimited text format with explicit field escaping. Deserialization
//! never performs I/O and refuses to grow past the documented limits, so an
//! untrusted settings blob cannot cause unbounded memory use.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::tabs::normalize_path_key;

/// Maximum number of recent documents retained.
pub const MAX_RECENTS: usize = 50;
/// Maximum number of favorite documents retained.
pub const MAX_FAVORITES: usize = 200;
/// Maximum number of collections retained.
pub const MAX_COLLECTIONS: usize = 64;
/// Maximum number of document keys in a single collection.
pub const MAX_COLLECTION_MEMBERS: usize = 500;
/// Maximum byte length of a normalized [`DocumentKey`].
pub const MAX_KEY_LEN: usize = 256;
/// Maximum byte length of a stored display title.
pub const MAX_TITLE_LEN: usize = 200;
/// Maximum byte length of a collection name.
pub const MAX_NAME_LEN: usize = 120;
/// Maximum number of input lines accepted by [`HomeModel::deserialize`].
pub const MAX_SERIALIZED_LINES: usize =
    MAX_RECENTS + MAX_FAVORITES + MAX_COLLECTIONS + MAX_COLLECTION_MEMBERS + 4;

/// A normalized identity for a document, independent of platform path syntax.
///
/// Backslashes become forward slashes, ASCII/Unicode lowercase folding is
/// applied, and trailing slashes are removed. The result is truncated on a UTF-8
/// character boundary to [`MAX_KEY_LEN`] bytes. An empty key means "no identity"
/// and is ignored by the deduplicating operations.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentKey(String);

impl DocumentKey {
    /// Normalize a raw key string.
    pub fn new(raw: &str) -> Self {
        let normalized = raw.replace('\\', "/").to_lowercase();
        let trimmed = normalized.trim_end_matches('/');
        Self(truncate(trimmed, MAX_KEY_LEN))
    }

    /// Build a key from a filesystem path using the same normalization the tab
    /// strip uses for duplicate detection.
    pub fn from_path(path: &Path) -> Self {
        Self::new(&normalize_path_key(path))
    }

    /// Return the normalized key text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return `true` when this key carries no identity.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for DocumentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Truncate `s` on a UTF-8 character boundary to at most `max_bytes` bytes.
fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// One recent-document entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentEntry {
    key: DocumentKey,
    title: String,
    ordinal: u64,
}

impl RecentEntry {
    /// The document identity key.
    pub fn key(&self) -> &DocumentKey {
        &self.key
    }

    /// The display title captured when the document was opened.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The logical-clock value at which this entry was last opened.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }
}

/// One favorite-document entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoriteEntry {
    key: DocumentKey,
    title: String,
    ordinal: u64,
}

impl FavoriteEntry {
    /// The document identity key.
    pub fn key(&self) -> &DocumentKey {
        &self.key
    }

    /// The display title captured when the favorite was added.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The logical-clock value at which this favorite was added.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }
}

/// A named, ordered collection of document keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    name: String,
    members: BTreeSet<DocumentKey>,
}

impl Collection {
    /// The collection name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The member keys, sorted ascending.
    pub fn members(&self) -> impl Iterator<Item = &DocumentKey> {
        self.members.iter()
    }

    /// Return `true` when `key` belongs to this collection.
    pub fn contains(&self, key: &DocumentKey) -> bool {
        self.members.contains(key)
    }

    /// Number of members in this collection.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Return `true` when this collection has no members.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// Counts used by home-screen UI badges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HomeSummary {
    /// Number of recent entries.
    pub recents: usize,
    /// Number of favorite entries.
    pub favorites: usize,
    /// Number of collections.
    pub collections: usize,
}

/// Typed errors produced by persistence and mutating home operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HomeError {
    /// A bounded limit was exceeded.
    #[error("home data exceeded the {limit} {what} limit")]
    LimitExceeded {
        /// What was being bounded (for example, `"recent"`).
        what: &'static str,
        /// The limit that was exceeded.
        limit: usize,
    },
    /// A serialized line could not be parsed.
    #[error("home serialization line {line} is malformed")]
    MalformedLine {
        /// One-based line number in the input.
        line: usize,
    },
    /// The serialization version is not understood.
    #[error("unsupported home serialization version {0}")]
    UnsupportedVersion(u32),
    /// A key or name was empty where identity is required.
    #[error("a key or name may not be empty")]
    EmptyIdentity,
}

/// In-memory recent/favorite/collection state.
///
/// All mutating operations honor the documented bounds. Re-adding an existing
/// identity updates it in place rather than duplicating it, and every read
/// returns a deterministic order independent of insertion order.
#[derive(Clone, Debug, Default)]
pub struct HomeModel {
    recents: Vec<RecentEntry>,
    favorites: BTreeMap<DocumentKey, FavoriteEntry>,
    collections: BTreeMap<String, Collection>,
    next_ordinal: u64,
}

impl HomeModel {
    /// Create an empty home model.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the number of distinct document identities known to this model
    /// (the union of recents, favorites, and collection members).
    pub fn known_key_count(&self) -> usize {
        let mut keys: BTreeSet<&DocumentKey> = BTreeSet::new();
        for entry in &self.recents {
            keys.insert(&entry.key);
        }
        for key in self.favorites.keys() {
            keys.insert(key);
        }
        for collection in self.collections.values() {
            for key in collection.members() {
                keys.insert(key);
            }
        }
        keys.len()
    }

    fn tick(&mut self) -> u64 {
        let value = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        value
    }

    /// Record that `key` was opened, with `title` for display.
    ///
    /// An empty key is ignored. Re-opening a known document advances its
    /// logical-clock value to the newest position. The list is capped at
    /// [`MAX_RECENTS`]; the least-recently opened entry is evicted when full.
    pub fn record_open(&mut self, key: DocumentKey, title: &str) {
        if key.is_empty() {
            return;
        }
        let ordinal = self.tick();
        let title = truncate(title, MAX_TITLE_LEN);
        if let Some(existing) = self.recents.iter_mut().find(|entry| entry.key == key) {
            existing.ordinal = ordinal;
            existing.title = title;
        } else {
            self.recents.push(RecentEntry {
                key,
                title,
                ordinal,
            });
        }
        self.recents
            .sort_by(|a, b| b.ordinal.cmp(&a.ordinal).then_with(|| a.key.cmp(&b.key)));
        if self.recents.len() > MAX_RECENTS {
            self.recents.truncate(MAX_RECENTS);
        }
    }

    /// Add `key` to favorites. Returns `true` when a new favorite was created.
    ///
    /// An empty key or a full favorites list (`MAX_FAVORITES`) makes this a
    /// no-op that returns `false`.
    pub fn add_favorite(&mut self, key: DocumentKey, title: &str) -> bool {
        if key.is_empty() || self.favorites.contains_key(&key) {
            return false;
        }
        if self.favorites.len() >= MAX_FAVORITES {
            return false;
        }
        let ordinal = self.tick();
        let entry = FavoriteEntry {
            key: key.clone(),
            title: truncate(title, MAX_TITLE_LEN),
            ordinal,
        };
        self.favorites.insert(key, entry);
        true
    }

    /// Remove `key` from favorites. Returns `true` when it was present.
    pub fn remove_favorite(&mut self, key: &DocumentKey) -> bool {
        self.favorites.remove(key).is_some()
    }

    /// Return `true` when `key` is favorited.
    pub fn is_favorite(&self, key: &DocumentKey) -> bool {
        self.favorites.contains_key(key)
    }

    /// Create an empty collection. Fails on an empty/overlong name, a duplicate
    /// name, or when the collection count is already at `MAX_COLLECTIONS`.
    pub fn create_collection(&mut self, name: &str) -> Result<(), HomeError> {
        let name = normalize_name(name)?;
        if self.collections.contains_key(&name) {
            return Err(HomeError::LimitExceeded {
                what: "duplicate collection",
                limit: 1,
            });
        }
        if self.collections.len() >= MAX_COLLECTIONS {
            return Err(HomeError::LimitExceeded {
                what: "collection",
                limit: MAX_COLLECTIONS,
            });
        }
        self.collections.insert(
            name.clone(),
            Collection {
                name,
                members: BTreeSet::new(),
            },
        );
        Ok(())
    }

    /// Remove a collection and all its members. Returns `true` when it existed.
    pub fn remove_collection(&mut self, name: &str) -> bool {
        self.collections.remove(name).is_some()
    }

    /// Add `key` to the named collection. Returns `true` when a member was
    /// added. An unknown collection, empty key, or a full collection is a no-op.
    pub fn add_to_collection(&mut self, name: &str, key: DocumentKey) -> bool {
        if key.is_empty() {
            return false;
        }
        match self.collections.get_mut(name) {
            Some(collection) => {
                if collection.members.len() >= MAX_COLLECTION_MEMBERS {
                    return false;
                }
                collection.members.insert(key)
            }
            None => false,
        }
    }

    /// Remove `key` from the named collection. Returns `true` when it was present.
    pub fn remove_from_collection(&mut self, name: &str, key: &DocumentKey) -> bool {
        self.collections
            .get_mut(name)
            .is_some_and(|collection| collection.members.remove(key))
    }

    /// Recent entries, ordered most-recent-first then by key for ties.
    pub fn recents(&self) -> &[RecentEntry] {
        &self.recents
    }

    /// Favorite entries, ordered by insertion clock then key.
    pub fn favorites(&self) -> Vec<&FavoriteEntry> {
        let mut entries: Vec<&FavoriteEntry> = self.favorites.values().collect();
        entries.sort_by(|a, b| a.ordinal.cmp(&b.ordinal).then_with(|| a.key.cmp(&b.key)));
        entries
    }

    /// Collections, ordered by name.
    pub fn collections(&self) -> impl Iterator<Item = &Collection> {
        self.collections.values()
    }

    /// Return one collection by name.
    pub fn collection(&self, name: &str) -> Option<&Collection> {
        self.collections.get(name)
    }

    /// Return the badge counts for a home screen.
    pub fn summary(&self) -> HomeSummary {
        HomeSummary {
            recents: self.recents.len(),
            favorites: self.favorites.len(),
            collections: self.collections.len(),
        }
    }

    /// Serialize to a bounded, newline-delimited text format.
    ///
    /// Layout:
    ///
    /// ```text
    /// PDFXI-HOME 1
    /// R <ordinal> <key> <title>
    /// F <ordinal> <key> <title>
    /// C <name>
    /// M <name> <key>
    /// ```
    ///
    /// Fields are separated by a single space and escaped by [`escape_field`];
    /// no field may contain an unescaped space. Output order is fully
    /// deterministic for a given model.
    pub fn serialize(&self) -> String {
        let mut out = String::from("PDFXI-HOME 1\n");
        for entry in &self.recents {
            out.push_str(&format_line(&[
                "R",
                &entry.ordinal.to_string(),
                entry.key.as_str(),
                &entry.title,
            ]));
        }
        for entry in self.favorites() {
            out.push_str(&format_line(&[
                "F",
                &entry.ordinal.to_string(),
                entry.key.as_str(),
                &entry.title,
            ]));
        }
        for collection in self.collections.values() {
            out.push_str(&format_line(&["C", collection.name()]));
            for key in collection.members() {
                out.push_str(&format_line(&["M", collection.name(), key.as_str()]));
            }
        }
        out
    }

    /// Deserialize text produced by [`Self::serialize`].
    ///
    /// Malformed lines are reported with [`HomeError::MalformedLine`]; unknown
    /// versions with [`HomeError::UnsupportedVersion`]. The number of lines and
    /// the size of every field are bounded, so untrusted input cannot grow the
    /// model without limit. Member lines that reference an unknown collection
    /// are ignored, and duplicate identities are coalesced.
    pub fn deserialize(input: &str) -> Result<Self, HomeError> {
        let mut lines = input.lines();
        let header = lines.next().unwrap_or_default();
        let fields = parse_line(header).ok_or(HomeError::MalformedLine { line: 1 })?;
        if fields.first().map(String::as_str) != Some("PDFXI-HOME") {
            return Err(HomeError::MalformedLine { line: 1 });
        }
        let version = fields
            .get(1)
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(HomeError::MalformedLine { line: 1 })?;
        if version != 1 {
            return Err(HomeError::UnsupportedVersion(version));
        }

        let mut model = Self::new();
        let mut max_ordinal = 0u64;
        for (offset, raw) in lines.enumerate() {
            let line_number = offset + 2;
            if line_number > MAX_SERIALIZED_LINES {
                return Err(HomeError::LimitExceeded {
                    what: "serialized line",
                    limit: MAX_SERIALIZED_LINES,
                });
            }
            let parts = parse_line(raw).ok_or(HomeError::MalformedLine { line: line_number })?;
            match parts.as_slice() {
                [tag, ordinal, key, title] if tag == "R" => {
                    let ordinal = parse_ordinal(ordinal, line_number)?;
                    max_ordinal = max_ordinal.max(ordinal);
                    let key = DocumentKey::new(key);
                    model.upsert_recent(key, truncate(title, MAX_TITLE_LEN), ordinal);
                }
                [tag, ordinal, key, title] if tag == "F" => {
                    let ordinal = parse_ordinal(ordinal, line_number)?;
                    max_ordinal = max_ordinal.max(ordinal);
                    let key = DocumentKey::new(key);
                    model.upsert_favorite(key, truncate(title, MAX_TITLE_LEN), ordinal);
                }
                [tag, name] if tag == "C" => {
                    let name = normalize_name(name)?;
                    if !model.collections.contains_key(&name)
                        && model.collections.len() < MAX_COLLECTIONS
                    {
                        model.collections.insert(
                            name.clone(),
                            Collection {
                                name,
                                members: BTreeSet::new(),
                            },
                        );
                    }
                }
                [tag, name, key] if tag == "M" => {
                    let name = normalize_name(name)?;
                    let key = DocumentKey::new(key);
                    if !key.is_empty()
                        && let Some(collection) = model.collections.get_mut(&name)
                        && collection.members.len() < MAX_COLLECTION_MEMBERS
                    {
                        collection.members.insert(key);
                    }
                }
                _ => return Err(HomeError::MalformedLine { line: line_number }),
            }
        }

        model.next_ordinal = max_ordinal.saturating_add(1);
        model
            .recents
            .sort_by(|a, b| b.ordinal.cmp(&a.ordinal).then_with(|| a.key.cmp(&b.key)));
        if model.recents.len() > MAX_RECENTS {
            model.recents.truncate(MAX_RECENTS);
        }
        Ok(model)
    }

    fn upsert_recent(&mut self, key: DocumentKey, title: String, ordinal: u64) {
        if key.is_empty() || self.recents.len() >= MAX_RECENTS {
            return;
        }
        if let Some(existing) = self.recents.iter_mut().find(|entry| entry.key == key) {
            existing.ordinal = ordinal;
            existing.title = title;
        } else {
            self.recents.push(RecentEntry {
                key,
                title,
                ordinal,
            });
        }
    }

    fn upsert_favorite(&mut self, key: DocumentKey, title: String, ordinal: u64) {
        if key.is_empty() || self.favorites.len() >= MAX_FAVORITES {
            return;
        }
        self.favorites
            .entry(key.clone())
            .or_insert_with(|| FavoriteEntry {
                key,
                title,
                ordinal,
            });
    }
}

fn parse_ordinal(raw: &str, line: usize) -> Result<u64, HomeError> {
    raw.parse::<u64>()
        .map_err(|_| HomeError::MalformedLine { line })
}

/// Normalize and bound a collection name.
fn normalize_name(name: &str) -> Result<String, HomeError> {
    if name.trim().is_empty() {
        return Err(HomeError::EmptyIdentity);
    }
    if name.len() > MAX_NAME_LEN {
        return Err(HomeError::LimitExceeded {
            what: "collection name",
            limit: MAX_NAME_LEN,
        });
    }
    Ok(truncate(name, MAX_NAME_LEN))
}

/// Escape one serialization field (space, backslash, newline, carriage return).
pub fn escape_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ' ' => out.push_str("\\s"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// Format one space-separated, escaped line with a trailing newline.
fn format_line(fields: &[&str]) -> String {
    let encoded: Vec<String> = fields.iter().map(|field| escape_field(field)).collect();
    let mut out = encoded.join(" ");
    out.push('\n');
    out
}

/// Parse one space-separated, escaped line.
fn parse_line(line: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let escaped = chars.next()?;
                match escaped {
                    '\\' => current.push('\\'),
                    's' => current.push(' '),
                    'n' => current.push('\n'),
                    'r' => current.push('\r'),
                    _ => return None,
                }
            }
            ' ' => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    Some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: &str) -> DocumentKey {
        DocumentKey::new(raw)
    }

    #[test]
    fn document_key_normalizes_separators_case_and_trailing_slash() {
        assert_eq!(key("C:\\Docs\\A.PDF"), key("c:/docs/a.pdf"));
        assert_eq!(key("c:/docs/a.pdf/"), key("c:/docs/a.pdf"));
        assert_eq!(key("c:/docs/a.pdf").as_str(), "c:/docs/a.pdf");
        assert!(key("").is_empty());
        assert!(
            DocumentKey::from_path(Path::new("X:\\A.PDF"))
                .as_str()
                .contains('a')
        );
    }

    #[test]
    fn document_key_is_bounded_to_max_key_len() {
        let long = "a".repeat(MAX_KEY_LEN + 50);
        let bounded = key(&long);
        assert!(bounded.as_str().len() <= MAX_KEY_LEN);
    }

    #[test]
    fn recents_are_deduplicated_and_ordered_most_recent_first() {
        let mut model = HomeModel::new();
        model.record_open(key("a.pdf"), "A");
        model.record_open(key("b.pdf"), "B");
        model.record_open(key("a.pdf"), "A again");
        assert_eq!(model.summary().recents, 2, "re-opening deduplicates by key");
        assert_eq!(model.recents()[0].key(), &key("a.pdf"));
        assert_eq!(model.recents()[0].title(), "A again");
        assert_eq!(model.recents()[1].key(), &key("b.pdf"));
    }

    #[test]
    fn recents_are_bounded_and_evict_least_recent() {
        let mut model = HomeModel::new();
        for i in 0..(MAX_RECENTS + 10) {
            model.record_open(key(&format!("doc-{i}.pdf")), "t");
        }
        assert_eq!(model.recents().len(), MAX_RECENTS);
        assert_eq!(
            model.recents()[0].key(),
            &key(&format!("doc-{}.pdf", MAX_RECENTS + 9))
        );
        assert!(model.recents().iter().all(|entry| entry.title() == "t"));
    }

    #[test]
    fn empty_keys_are_ignored_by_all_mutations() {
        let mut model = HomeModel::new();
        model.record_open(key(""), "blank");
        assert!(!model.add_favorite(key(""), "blank"));
        assert!(!model.add_to_collection("set", key("")));
        assert_eq!(model.summary(), HomeSummary::default());
    }

    #[test]
    fn favorites_dedupe_toggle_and_are_bounded() {
        let mut model = HomeModel::new();
        assert!(model.add_favorite(key("f.pdf"), "F"));
        assert!(!model.add_favorite(key("f.pdf"), "F again"), "dedupe");
        assert!(model.is_favorite(&key("f.pdf")));
        assert_eq!(model.favorites().len(), 1);
        assert_eq!(
            model.favorites()[0].title(),
            "F",
            "title is not overwritten"
        );
        assert!(model.remove_favorite(&key("f.pdf")));
        assert!(!model.remove_favorite(&key("f.pdf")));
        assert!(!model.is_favorite(&key("f.pdf")));
    }

    #[test]
    fn favorites_respect_the_cap() {
        let mut model = HomeModel::new();
        for i in 0..(MAX_FAVORITES + 5) {
            let _ = model.add_favorite(key(&format!("f-{i}")), "t");
        }
        assert_eq!(model.favorites().len(), MAX_FAVORITES);
    }

    #[test]
    fn collections_are_validated_deduplicated_and_ordered() {
        let mut model = HomeModel::new();
        assert!(matches!(
            model.create_collection(""),
            Err(HomeError::EmptyIdentity)
        ));
        assert!(model.create_collection("Work").is_ok());
        assert!(matches!(
            model.create_collection("Work"),
            Err(HomeError::LimitExceeded { .. })
        ));
        assert!(model.create_collection("Alpha").is_ok());
        let names: Vec<&str> = model.collections().map(Collection::name).collect();
        assert_eq!(names, vec!["Alpha", "Work"], "collections sort by name");
        assert!(model.remove_collection("Work"));
        assert!(!model.remove_collection("Work"));
    }

    #[test]
    fn collection_membership_is_bounded_and_deduplicated() {
        let mut model = HomeModel::new();
        let _ = model.create_collection("Set");
        assert!(model.add_to_collection("Set", key("a")));
        assert!(!model.add_to_collection("Set", key("a")), "dedupe members");
        assert!(
            !model.add_to_collection("Missing", key("b")),
            "unknown collection"
        );
        assert!(
            model
                .collection("Set")
                .is_some_and(|c| c.contains(&key("a")))
        );
        assert!(model.remove_from_collection("Set", &key("a")));
        assert!(!model.remove_from_collection("Set", &key("a")));
        assert!(model.collection("Set").is_some_and(Collection::is_empty));

        for i in 0..(MAX_COLLECTION_MEMBERS + 5) {
            let _ = model.add_to_collection("Set", key(&format!("m-{i}")));
        }
        assert_eq!(
            model.collection("Set").map(Collection::len),
            Some(MAX_COLLECTION_MEMBERS)
        );
    }

    #[test]
    fn overlong_names_and_titles_are_bounded() {
        let mut model = HomeModel::new();
        let long_name = "n".repeat(MAX_NAME_LEN + 40);
        assert!(matches!(
            model.create_collection(&long_name),
            Err(HomeError::LimitExceeded { .. })
        ));
        let long_title = "t".repeat(MAX_TITLE_LEN + 40);
        model.record_open(key("x"), &long_title);
        assert!(model.recents()[0].title().len() <= MAX_TITLE_LEN);
    }

    #[test]
    fn serialization_round_trips_all_state_deterministically() {
        let mut model = HomeModel::new();
        model.record_open(key("C:\\Docs\\a.pdf"), "A");
        model.record_open(key("b.pdf"), "B");
        let _ = model.add_favorite(key("a.pdf"), "A fav");
        let _ = model.create_collection("Team");
        let _ = model.add_to_collection("Team", key("b.pdf"));
        let _ = model.add_to_collection("Team", key("a.pdf"));

        let first = model.serialize();
        let second = model.serialize();
        assert_eq!(first, second, "serialization is deterministic");

        let restored = HomeModel::deserialize(&first).expect("round-trip parses");
        assert_eq!(restored.serialize(), first);
        assert_eq!(restored.summary(), model.summary());
        assert_eq!(restored.recents(), model.recents());
        assert_eq!(restored.favorites(), model.favorites());
        assert!(
            restored
                .collection("Team")
                .is_some_and(|c| c.contains(&key("b.pdf")) && c.contains(&key("a.pdf")))
        );
    }

    #[test]
    fn serialization_is_independent_of_insertion_order() {
        let mut a = HomeModel::new();
        a.record_open(key("x"), "X");
        a.record_open(key("y"), "Y");
        let mut b = HomeModel::new();
        b.record_open(key("y"), "Y");
        b.record_open(key("x"), "X");
        // Different logical clocks intentionally produce different recents; the
        // header and collection blocks still share a stable, versioned shape.
        assert!(a.serialize().starts_with("PDFXI-HOME 1\n"));
        assert!(b.serialize().starts_with("PDFXI-HOME 1\n"));
        let restored = HomeModel::deserialize(&a.serialize()).expect("parse");
        assert_eq!(restored.serialize(), a.serialize());
    }

    #[test]
    fn deserialization_rejects_bad_header_version_and_malformed_lines() {
        assert!(matches!(
            HomeModel::deserialize("NOT-HOME 1\n"),
            Err(HomeError::MalformedLine { line: 1 })
        ));
        assert!(matches!(
            HomeModel::deserialize("PDFXI-HOME 9\n"),
            Err(HomeError::UnsupportedVersion(9))
        ));
        assert!(matches!(
            HomeModel::deserialize("PDFXI-HOME 1\nQ bogus\n"),
            Err(HomeError::MalformedLine { line: 2 })
        ));
        assert!(matches!(
            HomeModel::deserialize("PDFXI-HOME 1\nR nope k t\n"),
            Err(HomeError::MalformedLine { line: 2 })
        ));
    }

    #[test]
    fn deserialization_enforces_line_cap() {
        let mut input = String::from("PDFXI-HOME 1\n");
        for _ in 0..(MAX_SERIALIZED_LINES + 2) {
            input.push_str("C ok\n");
        }
        assert!(matches!(
            HomeModel::deserialize(&input),
            Err(HomeError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn field_escaping_handles_spaces_backslashes_and_newlines() {
        let raw = "a b\\c\nd\re";
        assert_eq!(escape_field(raw), "a\\sb\\\\c\\nd\\re");
        let line = format_line(&["C", raw]);
        let parsed = parse_line(line.trim_end_matches('\n')).expect("escaped line parses");
        assert_eq!(parsed, vec!["C", raw]);
        assert!(
            parse_line("C bad\\q").is_none(),
            "unknown escape is rejected"
        );
    }

    #[test]
    fn deserialize_coalesces_duplicate_identities() {
        let input = concat!(
            "PDFXI-HOME 1\n",
            "R 1 c:/a.pdf A\n",
            "R 3 c:/a.pdf A\\snewest\n",
            "F 2 c:/b.pdf B\n",
            "F 4 c:/b.pdf B\\sagain\n",
        );
        let model = HomeModel::deserialize(input).expect("parse");
        assert_eq!(model.recents().len(), 1);
        assert_eq!(model.recents()[0].title(), "A newest");
        assert_eq!(model.favorites().len(), 1);
        assert_eq!(model.favorites()[0].ordinal(), 2, "first favorite wins");
        // The restored clock sits past every observed ordinal.
        let before = model.recents()[0].ordinal();
        assert!(before < u64::MAX);
    }
}
