//! Local shared-review files, deterministic merge, and participant states (REVIEW-006).
//!
//! This module implements a *local* replacement for email/shared review: a
//! portable, deterministic text file that carries a [`ReviewRegistry`] plus a
//! participant roster, and a pure merge of two such files. There is no network
//! access, no filesystem I/O, and no process spawning here — the caller owns
//! the bytes and may write them wherever it likes.
//!
//! The text format is versioned and self-describing. It is built on the same
//! dependency-free [`Value`]/[`Dict`] intermediate as the rest of `pdfxi-edit`,
//! encoded as a whitespace-separated token stream with escaped strings. Equal
//! input always produces byte-identical text, and import is the exact inverse
//! of export.
//!
//! Merge keys threads by their root annotation id: threads present in only one
//! file are carried over, identical threads are collapsed, and divergent
//! threads are resolved by the newer `updated` timestamp with a deterministic
//! tie-break. Every divergence is reported as a [`MergeConflict`] so callers
//! can surface it rather than silently losing work.

use std::collections::BTreeMap;
use std::fmt;

use crate::annotation::AnnotationId;
use crate::geometry::PageIndex;
use crate::pdf::{Dict, Value};

use super::error::ReviewError;
use super::registry::ReviewRegistry;
use super::report::participants;
use super::serialize::{dict_to_registry, registry_to_dict};
use super::thread::CommentThread;

/// Version written into the header of a shared-review file.
pub const SHARED_REVIEW_VERSION: u32 = 1;

/// Header token that identifies a shared-review file.
const HEADER: &str = "PDFXI-SHARED-REVIEW";

/// Structured errors for shared-review import, export, and merge.
#[derive(Debug, Clone, PartialEq)]
pub enum ShareError {
    /// The file declared a format version this implementation does not read.
    UnsupportedVersion(u32),
    /// The two files being merged use different format versions.
    VersionMismatch {
        /// Version of the local file.
        local: u32,
        /// Version of the incoming file.
        incoming: u32,
    },
    /// The input text was empty.
    EmptyText,
    /// A required line or value was absent.
    MissingField(&'static str),
    /// A line was malformed or carried an invalid token.
    Malformed(String),
    /// Two participant records claimed the same author.
    DuplicateAuthor(String),
    /// A downstream registry conversion failed.
    Registry(ReviewError),
}

impl fmt::Display for ShareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported shared-review format version {version}")
            }
            Self::VersionMismatch { local, incoming } => write!(
                f,
                "shared-review version mismatch: local {local}, incoming {incoming}"
            ),
            Self::EmptyText => f.write_str("shared-review file is empty"),
            Self::MissingField(field) => write!(f, "shared-review file is missing `{field}`"),
            Self::Malformed(reason) => write!(f, "malformed shared-review file: {reason}"),
            Self::DuplicateAuthor(author) => {
                write!(f, "duplicate participant author `{author}`")
            }
            Self::Registry(error) => write!(f, "invalid shared review registry: {error}"),
        }
    }
}

impl std::error::Error for ShareError {}

/// Collaboration state of one participant in a shared review.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ParticipantState {
    /// Invited or assigned but not yet active.
    Pending,
    /// Has replied to or transitioned at least one thread.
    Active,
    /// Marked complete by the caller (for example, after sign-off).
    Complete,
}

impl ParticipantState {
    /// Stable lowercase token used in the file format.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Complete => "complete",
        }
    }

    /// Parses a participant-state token.
    pub fn parse_token(value: &str) -> Result<Self, ShareError> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "complete" => Ok(Self::Complete),
            other => Err(ShareError::Malformed(format!(
                "unknown participant state `{other}`"
            ))),
        }
    }
}

/// One participant's identity, state, and last-seen marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedParticipant {
    /// Author name as it appears on replies and history entries.
    pub author: String,
    /// Collaboration state of this participant.
    pub state: ParticipantState,
    /// Opaque, caller-supplied "last seen" marker (for example a timestamp).
    pub last_seen: String,
}

/// A portable local shared-review file.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedReviewFile {
    version: u32,
    origin: String,
    participants: Vec<SharedParticipant>,
    registry: ReviewRegistry,
}

/// Which side of a merge supplied the value for a divergent thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictResolution {
    /// The local (left) file won.
    KeptLocal,
    /// The incoming (right) file won.
    TookIncoming,
}

/// One thread whose local and incoming versions diverged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeConflict {
    /// Root annotation keying the divergent thread.
    pub root: AnnotationId,
    /// `updated` marker of the local version.
    pub local_updated: String,
    /// `updated` marker of the incoming version.
    pub incoming_updated: String,
    /// Which version the deterministic rule selected.
    pub resolution: ConflictResolution,
}

/// Result of merging two shared-review files.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeResult {
    /// The merged file, with origin `merged`.
    pub file: SharedReviewFile,
    /// Divergent threads that were resolved by the deterministic rule.
    pub conflicts: Vec<MergeConflict>,
    /// Threads carried over from the local file only.
    pub added_from_local: usize,
    /// Threads carried over from the incoming file only.
    pub added_from_incoming: usize,
    /// Threads that were byte-identical in both files.
    pub identical: usize,
}

impl SharedReviewFile {
    /// Builds a shared-review file from an origin label and a registry.
    ///
    /// The participant roster is derived from the registry: anyone who replied
    /// or transitioned a thread is [`ParticipantState::Active`], while an
    /// assignee with no other activity is [`ParticipantState::Pending`]. The
    /// derivation is sorted by author, so it is deterministic.
    pub fn new(origin: impl Into<String>, registry: ReviewRegistry) -> Self {
        let participants = derive_roster(&registry);
        Self {
            version: SHARED_REVIEW_VERSION,
            origin: origin.into(),
            participants,
            registry,
        }
    }

    /// Format version recorded in this file.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Origin label of this file.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Participant roster carried by this file.
    pub fn participants(&self) -> &[SharedParticipant] {
        &self.participants
    }

    /// Review registry carried by this file.
    pub fn registry(&self) -> &ReviewRegistry {
        &self.registry
    }

    /// Mutable access to the review registry carried by this file.
    pub fn registry_mut(&mut self) -> &mut ReviewRegistry {
        &mut self.registry
    }

    /// Sets the origin label.
    pub fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = origin.into();
        self
    }

    /// Replaces the participant roster.
    pub fn with_participants(mut self, participants: Vec<SharedParticipant>) -> Self {
        self.participants = participants;
        self
    }

    /// Sets or inserts a participant's collaboration state and last-seen marker.
    ///
    /// Participants stay sorted by author after the update.
    pub fn set_participant_state(
        &mut self,
        author: impl Into<String>,
        state: ParticipantState,
        last_seen: impl Into<String>,
    ) {
        let author = author.into();
        if let Some(existing) = self.participants.iter_mut().find(|p| p.author == author) {
            existing.state = state;
            existing.last_seen = last_seen.into();
        } else {
            self.participants.push(SharedParticipant {
                author,
                state,
                last_seen: last_seen.into(),
            });
        }
        self.participants.sort_by(|a, b| a.author.cmp(&b.author));
    }

    /// Serializes this file to deterministic, portable text.
    pub fn to_text(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("{HEADER} {}", self.version));
        lines.push(format!("origin {}", escape(&self.origin)));
        lines.push(format!("participants {}", self.participants.len()));
        for participant in &self.participants {
            lines.push(format!(
                "participant {} {} s{}",
                escape(&participant.author),
                participant.state.as_str(),
                escape(&participant.last_seen)
            ));
        }
        let mut tokens = Vec::new();
        encode_value(
            &Value::Dictionary(registry_to_dict(&self.registry)),
            &mut tokens,
        );
        lines.push(format!("registry {}", tokens.join(" ")));
        lines.push(String::new());
        lines.join("\n")
    }

    /// Parses a file produced by [`Self::to_text`].
    pub fn from_text(text: &str) -> Result<Self, ShareError> {
        if text.trim().is_empty() {
            return Err(ShareError::EmptyText);
        }
        let mut version = None;
        let mut origin = None;
        let mut expected = None;
        let mut participants = Vec::new();
        let mut registry_dict = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix(HEADER) {
                let value = rest.trim();
                version =
                    Some(value.parse::<u32>().map_err(|_| {
                        ShareError::Malformed(format!("invalid version `{value}`"))
                    })?);
            } else if let Some(rest) = line.strip_prefix("origin ") {
                origin = Some(unescape(rest)?);
            } else if let Some(rest) = line.strip_prefix("participants ") {
                expected = Some(rest.parse::<usize>().map_err(|_| {
                    ShareError::Malformed(format!("invalid participant count `{rest}`"))
                })?);
            } else if let Some(rest) = line.strip_prefix("participant ") {
                let fields: Vec<&str> = rest.split(' ').collect();
                if fields.len() != 3 {
                    return Err(ShareError::Malformed(
                        "participant line needs author, state, and last-seen".to_string(),
                    ));
                }
                let author = unescape(fields[0])?;
                let state = ParticipantState::parse_token(fields[1])?;
                let last_seen = match fields[2].as_bytes().first() {
                    Some(b's') => unescape(&fields[2][1..])?,
                    _ => {
                        return Err(ShareError::Malformed(format!(
                            "last-seen `{}` must be a string token",
                            fields[2]
                        )));
                    }
                };
                participants.push(SharedParticipant {
                    author,
                    state,
                    last_seen,
                });
            } else if let Some(rest) = line.strip_prefix("registry ") {
                let tokens: Vec<&str> = rest.split(' ').filter(|t| !t.is_empty()).collect();
                let mut index = 0;
                let value = decode_value(&tokens, &mut index)?;
                if index != tokens.len() {
                    return Err(ShareError::Malformed(
                        "trailing tokens after the registry value".to_string(),
                    ));
                }
                match value {
                    Value::Dictionary(dict) => registry_dict = Some(dict),
                    _ => {
                        return Err(ShareError::Malformed(
                            "registry value must be a dictionary".to_string(),
                        ));
                    }
                }
            } else {
                return Err(ShareError::Malformed(format!(
                    "unknown line `{}`",
                    line.chars().take(32).collect::<String>()
                )));
            }
        }

        let version = version.ok_or(ShareError::MissingField("header"))?;
        if version != SHARED_REVIEW_VERSION {
            return Err(ShareError::UnsupportedVersion(version));
        }
        let origin = origin.ok_or(ShareError::MissingField("origin"))?;
        let expected = expected.ok_or(ShareError::MissingField("participants"))?;
        let registry_dict = registry_dict.ok_or(ShareError::MissingField("registry"))?;
        if expected != participants.len() {
            return Err(ShareError::Malformed(format!(
                "declared {expected} participants but found {}",
                participants.len()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for participant in &participants {
            if !seen.insert(participant.author.as_str()) {
                return Err(ShareError::DuplicateAuthor(participant.author.clone()));
            }
        }
        let registry = dict_to_registry(&registry_dict).map_err(ShareError::Registry)?;
        Ok(Self {
            version,
            origin,
            participants,
            registry,
        })
    }
}

/// Derives a deterministic participant roster from a registry.
pub fn derive_roster(registry: &ReviewRegistry) -> Vec<SharedParticipant> {
    participants(registry)
        .into_iter()
        .map(|summary| SharedParticipant {
            author: summary.author,
            state: if summary.reply_count > 0 || summary.transition_count > 0 {
                ParticipantState::Active
            } else {
                ParticipantState::Pending
            },
            last_seen: String::new(),
        })
        .collect()
}

/// Merges two shared-review files deterministically (REVIEW-006).
///
/// Threads keyed by a root annotation present in only one file are carried
/// over. Identical threads are collapsed. Divergent threads are resolved by
/// the lexicographically newer `updated` marker; equal markers fall back to a
/// stable debug canonical form. New thread ids are assigned in root order so
/// the result does not depend on id collisions between independent files.
pub fn merge_shared(
    local: &SharedReviewFile,
    incoming: &SharedReviewFile,
) -> Result<MergeResult, ShareError> {
    if local.version != incoming.version {
        return Err(ShareError::VersionMismatch {
            local: local.version,
            incoming: incoming.version,
        });
    }

    let local_threads = index_threads(&local.registry);
    let incoming_threads = index_threads(&incoming.registry);
    let mut roots: Vec<AnnotationId> = local_threads
        .keys()
        .chain(incoming_threads.keys())
        .copied()
        .collect();
    roots.sort();
    roots.dedup();

    let mut chosen: Vec<(CommentThread, PageIndex)> = Vec::new();
    let mut conflicts = Vec::new();
    let mut added_from_local = 0;
    let mut added_from_incoming = 0;
    let mut identical = 0;

    for root in roots {
        let left = local_threads.get(&root);
        let right = incoming_threads.get(&root);
        match (left, right) {
            (Some((local_thread, local_page)), Some((incoming_thread, incoming_page))) => {
                if local_thread == incoming_thread && local_page == incoming_page {
                    chosen.push((local_thread.clone(), *local_page));
                    identical += 1;
                    continue;
                }
                let (winner, winner_page, resolution) =
                    if local_thread.updated() > incoming_thread.updated() {
                        (
                            local_thread.clone(),
                            *local_page,
                            ConflictResolution::KeptLocal,
                        )
                    } else if local_thread.updated() < incoming_thread.updated() {
                        (
                            incoming_thread.clone(),
                            *incoming_page,
                            ConflictResolution::TookIncoming,
                        )
                    } else {
                        // Deterministic tie-break on a canonical debug form.
                        let left_form = format!("{:?}", (local_thread, local_page));
                        let right_form = format!("{:?}", (incoming_thread, incoming_page));
                        if left_form <= right_form {
                            (
                                local_thread.clone(),
                                *local_page,
                                ConflictResolution::KeptLocal,
                            )
                        } else {
                            (
                                incoming_thread.clone(),
                                *incoming_page,
                                ConflictResolution::TookIncoming,
                            )
                        }
                    };
                conflicts.push(MergeConflict {
                    root,
                    local_updated: local_thread.updated().to_string(),
                    incoming_updated: incoming_thread.updated().to_string(),
                    resolution,
                });
                chosen.push((winner, winner_page));
            }
            (Some((local_thread, local_page)), None) => {
                chosen.push((local_thread.clone(), *local_page));
                added_from_local += 1;
            }
            (None, Some((incoming_thread, incoming_page))) => {
                chosen.push((incoming_thread.clone(), *incoming_page));
                added_from_incoming += 1;
            }
            (None, None) => {}
        }
    }

    // Rebuild with fresh sequential thread ids in root order so independent
    // files whose ids collide can still be merged.
    let renumbered: Vec<(CommentThread, PageIndex)> = chosen
        .into_iter()
        .enumerate()
        .map(|(index, (thread, page))| {
            let fresh = CommentThread::from_parts(
                super::ThreadId::new(index as u64),
                thread.root(),
                thread.replies().to_vec(),
                thread.state().clone(),
            )
            .with_extra(thread.extra().to_vec());
            (fresh, page)
        })
        .collect();

    let mut extra = local.registry.extra().to_vec();
    for (key, value) in incoming.registry.extra() {
        if !extra.iter().any(|(existing, _)| existing == key) {
            extra.push((key.clone(), value.clone()));
        }
    }
    let mut registry =
        ReviewRegistry::from_parsed(renumbered, extra).map_err(ShareError::Registry)?;
    registry.reindex();

    let mut roster: BTreeMap<String, SharedParticipant> = BTreeMap::new();
    for participant in local
        .participants
        .iter()
        .chain(incoming.participants.iter())
    {
        let entry = roster
            .entry(participant.author.clone())
            .or_insert_with(|| participant.clone());
        if participant.state > entry.state {
            entry.state = participant.state;
        }
        if participant.last_seen > entry.last_seen {
            entry.last_seen = participant.last_seen.clone();
        }
    }
    let participants: Vec<SharedParticipant> = roster.into_values().collect();
    let file = SharedReviewFile::new("merged", registry).with_participants(participants);

    Ok(MergeResult {
        file,
        conflicts,
        added_from_local,
        added_from_incoming,
        identical,
    })
}

fn index_threads(registry: &ReviewRegistry) -> BTreeMap<AnnotationId, (CommentThread, PageIndex)> {
    registry
        .threads()
        .map(|thread| {
            let page = registry
                .page_of(thread.root())
                .unwrap_or_else(|| PageIndex::new(0));
            (thread.root(), (thread.clone(), page))
        })
        .collect()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ' ' => out.push_str("\\s"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape(text: &str) -> Result<String, ShareError> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let next = chars
                .next()
                .ok_or_else(|| ShareError::Malformed("dangling escape".to_string()))?;
            match next {
                '\\' => out.push('\\'),
                's' => out.push(' '),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                other => {
                    return Err(ShareError::Malformed(format!("unknown escape `\\{other}`")));
                }
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

fn encode_value(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Null => out.push("null".to_string()),
        Value::Bool(true) => out.push("true".to_string()),
        Value::Bool(false) => out.push("false".to_string()),
        Value::Int(number) => out.push(format!("i{number}")),
        Value::Real(number) => out.push(format!("r{number:?}")),
        Value::Name(name) => out.push(format!("n{}", escape(name))),
        Value::Text(text) => out.push(format!("s{}", escape(text))),
        Value::Array(items) => {
            out.push("A".to_string());
            for item in items {
                encode_value(item, out);
            }
            out.push("Z".to_string());
        }
        Value::Dictionary(dict) => {
            out.push("D".to_string());
            for (key, value) in dict {
                out.push(format!("s{}", escape(key)));
                encode_value(value, out);
            }
            out.push("E".to_string());
        }
    }
}

fn decode_value(tokens: &[&str], index: &mut usize) -> Result<Value, ShareError> {
    let token = tokens
        .get(*index)
        .ok_or(ShareError::MissingField("value"))?;
    *index += 1;
    match *token {
        "null" => Ok(Value::Null),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        "A" => {
            let mut items = Vec::new();
            loop {
                let next = tokens
                    .get(*index)
                    .ok_or_else(|| ShareError::Malformed("unterminated array".to_string()))?;
                if *next == "Z" {
                    *index += 1;
                    break;
                }
                items.push(decode_value(tokens, index)?);
            }
            Ok(Value::Array(items))
        }
        "D" => {
            let mut dict = Dict::new();
            loop {
                let next = tokens
                    .get(*index)
                    .ok_or_else(|| ShareError::Malformed("unterminated dictionary".to_string()))?;
                if *next == "E" {
                    *index += 1;
                    break;
                }
                let key_token = *next;
                *index += 1;
                let key = match key_token.as_bytes().first() {
                    Some(b's') => unescape(&key_token[1..])?,
                    _ => {
                        return Err(ShareError::Malformed(format!(
                            "dictionary key `{key_token}` must be text"
                        )));
                    }
                };
                let value = decode_value(tokens, index)?;
                dict.insert(key, value);
            }
            Ok(Value::Dictionary(dict))
        }
        "Z" | "E" => Err(ShareError::Malformed(format!(
            "unexpected closing token `{token}`"
        ))),
        other => match other.as_bytes().first() {
            Some(b'i') => other[1..]
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| ShareError::Malformed(format!("invalid integer `{other}`"))),
            Some(b'r') => other[1..]
                .parse::<f64>()
                .map(Value::Real)
                .map_err(|_| ShareError::Malformed(format!("invalid real `{other}`"))),
            Some(b'n') => unescape(&other[1..]).map(Value::Name),
            Some(b's') => unescape(&other[1..]).map(Value::Text),
            _ => Err(ShareError::Malformed(format!("unknown token `{other}`"))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Annotation, AnnotationKind};
    use crate::geometry::Rect;
    use crate::review::ReviewStatus;

    fn annotation(id: u64, page: usize) -> Annotation {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0).expect("valid rect");
        Annotation::new(
            AnnotationId::new(id),
            AnnotationKind::Note,
            PageIndex::new(page),
            rect,
            "author",
            "body",
        )
        .expect("valid annotation")
    }

    fn registry(ids: &[u64], updated: &str) -> ReviewRegistry {
        let annotations: Vec<Annotation> = ids
            .iter()
            .map(|id| annotation(*id, (*id % 2) as usize))
            .collect();
        let mut registry = ReviewRegistry::from_annotations(annotations.iter(), updated)
            .expect("registry from annotations");
        for id in ids {
            registry
                .add_reply(
                    AnnotationId::new(*id),
                    "alice",
                    "t1",
                    "note",
                    ReviewStatus::Open,
                )
                .expect("reply");
        }
        registry
    }

    fn file(origin: &str, ids: &[u64], updated: &str) -> SharedReviewFile {
        SharedReviewFile::new(origin, registry(ids, updated))
    }

    #[test]
    fn export_import_round_trip_is_byte_stable() {
        let original = file("reviewer-a", &[3, 1, 2], "t0");
        let text = original.to_text();
        assert_eq!(text, original.to_text());
        let restored = SharedReviewFile::from_text(&text).expect("import");
        assert_eq!(restored, original);
        assert_eq!(restored.to_text(), text);
        assert!(text.starts_with("PDFXI-SHARED-REVIEW 1\n"));
    }

    #[test]
    fn escaping_preserves_spaces_tabs_and_newlines() {
        let mut original = file("origin with spaces", &[1], "t\n0");
        original.set_participant_state("author\tx", ParticipantState::Complete, "line\nmarker");
        let text = original.to_text();
        let restored = SharedReviewFile::from_text(&text).expect("import");
        assert_eq!(restored.origin(), "origin with spaces");
        assert_eq!(restored.registry(), original.registry());
        assert_eq!(restored.participants(), original.participants());
    }

    #[test]
    fn disjoint_files_merge_all_threads_deterministically() {
        let left = file("a", &[1, 2], "t0");
        let right = file("b", &[3, 4], "t0");
        let result = merge_shared(&left, &right).expect("merge");
        assert_eq!(result.added_from_local, 2);
        assert_eq!(result.added_from_incoming, 2);
        assert_eq!(result.identical, 0);
        assert!(result.conflicts.is_empty());
        assert_eq!(result.file.registry().len(), 4);
        assert_eq!(result.file.origin(), "merged");
        assert_eq!(merge_shared(&left, &right).expect("merge again"), result);
    }

    #[test]
    fn identical_threads_are_collapsed_without_conflicts() {
        let left = file("a", &[1, 2], "t0");
        let right = file("b", &[1, 2], "t0");
        let result = merge_shared(&left, &right).expect("merge");
        assert_eq!(result.identical, 2);
        assert!(result.conflicts.is_empty());
        assert_eq!(result.file.registry().len(), 2);
    }

    #[test]
    fn divergent_threads_report_conflicts_and_pick_newer_deterministically() {
        let mut left = file("a", &[1], "t0");
        let mut right = file("b", &[1], "t0");
        left.registry_mut()
            .add_reply(
                AnnotationId::new(1),
                "bob",
                "t9",
                "newer",
                ReviewStatus::Open,
            )
            .expect("newer reply");
        right
            .registry_mut()
            .add_reply(
                AnnotationId::new(1),
                "carol",
                "t1",
                "older",
                ReviewStatus::Open,
            )
            .expect("older reply");
        // Give the left thread the newer `updated` marker.
        left.registry_mut()
            .transition(
                AnnotationId::new(1),
                ReviewStatus::Resolved,
                "bob",
                "t10",
                "done",
            )
            .expect("transition");

        let result = merge_shared(&left, &right).expect("merge");
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(
            result.conflicts[0].resolution,
            ConflictResolution::KeptLocal
        );
        let merged = result.file.registry();
        let thread = merged.thread(AnnotationId::new(1)).expect("thread");
        assert!(thread.has_reply_by("bob"));
        // Swapping operands flips the chosen side but keeps the conflict count.
        let mirrored = merge_shared(&right, &left).expect("merge mirrored");
        assert_eq!(mirrored.conflicts.len(), 1);
        assert_eq!(
            mirrored.conflicts[0].resolution,
            ConflictResolution::TookIncoming
        );
    }

    #[test]
    fn participant_states_merge_by_highest_priority() {
        let mut left = file("a", &[1], "t0");
        left.set_participant_state("dave", ParticipantState::Pending, "t1");
        let mut right = file("b", &[2], "t0");
        right.set_participant_state("dave", ParticipantState::Complete, "t2");
        right.set_participant_state("erin", ParticipantState::Active, "t0");
        let result = merge_shared(&left, &right).expect("merge");
        let roster = result.file.participants();
        let dave = roster.iter().find(|p| p.author == "dave").expect("dave");
        assert_eq!(dave.state, ParticipantState::Complete);
        assert_eq!(dave.last_seen, "t2");
        let authors: Vec<&str> = roster.iter().map(|p| p.author.as_str()).collect();
        let mut sorted = authors.clone();
        sorted.sort();
        assert_eq!(authors, sorted);
        assert!(roster.iter().any(|p| p.author == "erin"));
    }

    #[test]
    fn malformed_files_and_version_mismatches_are_typed() {
        assert_eq!(SharedReviewFile::from_text(""), Err(ShareError::EmptyText));
        let text = file("a", &[1], "t0").to_text();
        let unsupported = text.replacen("PDFXI-SHARED-REVIEW 1", "PDFXI-SHARED-REVIEW 9", 1);
        assert_eq!(
            SharedReviewFile::from_text(&unsupported),
            Err(ShareError::UnsupportedVersion(9))
        );
        let truncated = text.lines().take(2).collect::<Vec<&str>>().join("\n");
        assert!(matches!(
            SharedReviewFile::from_text(&truncated),
            Err(ShareError::MissingField(_))
        ));
        assert!(matches!(
            SharedReviewFile::from_text(&text.replacen("origin ", "bogus ", 1)),
            Err(ShareError::Malformed(_))
        ));
    }
}
