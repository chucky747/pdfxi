//! Deterministic PDF/UA structure checks decidable from a plain-data snapshot.
//!
//! This module narrows the PDF/UA-1 gap recorded for STD-009 to the rules a
//! dependency-free crate can decide: role-map coverage for custom roles,
//! alternative text for figure and form elements, tagged text markers,
//! `/StructParents` presence, unique MCIDs, and consistent parent links in
//! reading order. Everything is computed from [`crate::PreflightInput`]; no PDF
//! objects, rendering oracle, or external validator is involved. Findings carry
//! stable codes and are sorted so caller collection order cannot change them.
//!
//! A clean report means "no decidable rule fired", not certification. Full
//! semantic reading order and actual-text verification still require a human or
//! an external validator such as veraPDF running in CI.

use crate::Severity;
use crate::input::{PreflightInput, StructElementInfo};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Standard structure roles defined by PDF 1.7 (Table 9.2 and related tables).
pub const STANDARD_ROLES: &[&str] = &[
    "P",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "L",
    "LI",
    "LBody",
    "Table",
    "TR",
    "TH",
    "TD",
    "Figure",
    "Form",
    "Formula",
    "Span",
    "BibEntry",
    "Code",
    "BlockQuote",
    "Caption",
    "Document",
    "Art",
    "Sect",
    "Div",
    "Aside",
    "TOC",
    "TOCI",
    "Index",
    "NonStruct",
    "Private",
    "Note",
    "Reference",
    "FENote",
    "Ruby",
    "RB",
    "RT",
    "RP",
    "Warichu",
    "WT",
    "WP",
    "Part",
    "Abstract",
    "Keyword",
    "Procedure",
    "Sub",
    "Input",
    "Header",
    "Footer",
    "Chapter",
    "Word",
];

/// Returns `true` when `role` is a standard PDF structure role.
///
/// A single leading slash and ASCII case are ignored so callers can pass either
/// a dictionary name (`/Figure`) or a bare role (`figure`).
#[must_use]
pub fn is_standard_role(role: &str) -> bool {
    let bare = role.strip_prefix('/').unwrap_or(role);
    STANDARD_ROLES
        .iter()
        .any(|standard| standard.eq_ignore_ascii_case(bare))
}

/// A typed violation of one deterministic PDF/UA structure rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureViolation {
    /// A non-standard role has no entry in the catalog `/RoleMap`.
    MissingRoleMap {
        /// Identifier of the offending element.
        id: usize,
        /// Role as supplied by the caller.
        role: String,
    },
    /// A figure or form element has no usable `/Alt` text.
    MissingAltText {
        /// Identifier of the offending element.
        id: usize,
        /// Role as supplied by the caller.
        role: String,
    },
    /// A page contains text but no tagged marked-content sequence.
    UntaggedText {
        /// One-based page number.
        page: usize,
    },
    /// A page with text or marked content has no `/StructParents` entry.
    MissingStructParents {
        /// One-based page number.
        page: usize,
    },
    /// A page contains the same MCID more than once.
    DuplicateMcid {
        /// One-based page number.
        page: usize,
        /// The repeated marked-content identifier.
        mcid: u32,
    },
    /// An MCID has no corresponding `/StructParents` array entry.
    MissingStructParentLink {
        /// One-based page number.
        page: usize,
        /// The unlinked marked-content identifier.
        mcid: u32,
    },
    /// A `/StructParents` entry points at an element id that does not exist.
    UnknownStructParent {
        /// One-based page number.
        page: usize,
        /// The marked-content identifier being linked.
        mcid: u32,
        /// The unknown structure element id.
        id: usize,
    },
    /// Two structure elements share the same identifier.
    DuplicateStructId {
        /// The repeated identifier.
        id: usize,
    },
    /// An element's parent identifier does not exist.
    UnknownParent {
        /// Identifier of the child element.
        id: usize,
        /// The unknown parent identifier.
        parent: usize,
    },
    /// An element names itself as its parent.
    SelfParent {
        /// Identifier of the offending element.
        id: usize,
    },
    /// A parent's reading-order position is not before its child's.
    ParentAfterChild {
        /// Identifier of the child element.
        id: usize,
        /// Identifier of the parent element.
        parent: usize,
    },
}

impl StructureViolation {
    /// Returns the stable finding-code suffix for this violation.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingRoleMap { .. } => "ROLE_MAP_MISSING",
            Self::MissingAltText { .. } => "ALT_TEXT_MISSING",
            Self::UntaggedText { .. } => "UNTAGGED_TEXT",
            Self::MissingStructParents { .. } => "NO_STRUCT_PARENTS",
            Self::DuplicateMcid { .. } => "DUPLICATE_MCID",
            Self::MissingStructParentLink { .. } => "MISSING_STRUCT_PARENT_LINK",
            Self::UnknownStructParent { .. } => "UNKNOWN_STRUCT_PARENT",
            Self::DuplicateStructId { .. } => "DUPLICATE_STRUCT_ID",
            Self::UnknownParent { .. } => "UNKNOWN_PARENT",
            Self::SelfParent { .. } => "SELF_PARENT",
            Self::ParentAfterChild { .. } => "READING_ORDER_PARENT_AFTER_CHILD",
        }
    }

    /// Every structure violation blocks PDF/UA conformance.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        Severity::Error
    }

    /// Returns an actionable, human-readable explanation.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MissingRoleMap { id, role } => format!(
                "structure element {id} uses non-standard role '{role}' without a /RoleMap entry"
            ),
            Self::MissingAltText { id, role } => format!(
                "structure element {id} has role '{role}' but no /Alt text; supply an alternative"
            ),
            Self::UntaggedText { page } => {
                format!("page {page} has text outside tagged marked content")
            }
            Self::MissingStructParents { page } => format!(
                "page {page} has tagged content but no /StructParents entry in the page dictionary"
            ),
            Self::DuplicateMcid { page, mcid } => {
                format!("page {page} reuses MCID {mcid} in its marked-content sequence")
            }
            Self::MissingStructParentLink { page, mcid } => format!(
                "page {page} MCID {mcid} has no /StructParents entry for its structure parent"
            ),
            Self::UnknownStructParent { page, mcid, id } => {
                format!("page {page} MCID {mcid} points at unknown structure element id {id}")
            }
            Self::DuplicateStructId { id } => {
                format!("structure element id {id} is used more than once")
            }
            Self::UnknownParent { id, parent } => {
                format!("structure element {id} names unknown parent id {parent}")
            }
            Self::SelfParent { id } => {
                format!("structure element {id} names itself as its parent")
            }
            Self::ParentAfterChild { id, parent } => {
                format!("structure element {id} precedes its parent {parent} in reading order")
            }
        }
    }

    /// Returns a stable location string for the violation.
    #[must_use]
    pub fn location(&self) -> String {
        match self {
            Self::UntaggedText { page }
            | Self::MissingStructParents { page }
            | Self::DuplicateMcid { page, .. }
            | Self::MissingStructParentLink { page, .. }
            | Self::UnknownStructParent { page, .. } => format!("page {page}"),
            Self::MissingRoleMap { id, .. }
            | Self::MissingAltText { id, .. }
            | Self::DuplicateStructId { id }
            | Self::SelfParent { id } => format!("struct element {id}"),
            Self::UnknownParent { id, .. } | Self::ParentAfterChild { id, .. } => {
                format!("struct element {id}")
            }
        }
    }
}

impl fmt::Display for StructureViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for StructureViolation {}

/// Runs every decidable PDF/UA structure check over `input`.
///
/// The result is independent of the order of the input vectors: pages are
/// visited by page number, element ids are resolved through a canonical map,
/// and violations are sorted by code, location, and message.
#[must_use]
pub fn check_structure(input: &PreflightInput) -> Vec<StructureViolation> {
    let mut out = Vec::new();
    let all_ids: BTreeSet<usize> = input.struct_elements.iter().map(|item| item.id).collect();
    let mut sorted_ids: Vec<usize> = input.struct_elements.iter().map(|item| item.id).collect();
    sorted_ids.sort_unstable();
    let mut reported_duplicate_id = None;
    for pair in sorted_ids.windows(2) {
        if pair[0] == pair[1] && reported_duplicate_id != Some(pair[0]) {
            out.push(StructureViolation::DuplicateStructId { id: pair[0] });
            reported_duplicate_id = Some(pair[0]);
        }
    }

    // Canonical id map: with duplicate ids, the lowest reading-order element
    // wins, so parent resolution never depends on caller vector order.
    let mut ordered: Vec<&StructElementInfo> = input.struct_elements.iter().collect();
    ordered.sort_by(|a, b| {
        (a.id, a.reading_order, a.role.as_str(), a.is_artifact).cmp(&(
            b.id,
            b.reading_order,
            b.role.as_str(),
            b.is_artifact,
        ))
    });
    let mut by_id: BTreeMap<usize, &StructElementInfo> = BTreeMap::new();
    for element in ordered {
        by_id.entry(element.id).or_insert(element);
    }

    let mut pages: Vec<_> = input.page_structure.iter().collect();
    pages.sort_by_key(|page| page.page_number);
    for page in pages {
        let number = page.page_number;
        if !page.has_struct_parents && (page.has_text || !page.mcids.is_empty()) {
            out.push(StructureViolation::MissingStructParents { page: number });
        }
        if page.has_text && !page.has_tagged_text {
            out.push(StructureViolation::UntaggedText { page: number });
        }

        let mut mcids = page.mcids.clone();
        mcids.sort_unstable();
        let mut reported_duplicate = None;
        for pair in mcids.windows(2) {
            if pair[0] == pair[1] && reported_duplicate != Some(pair[0]) {
                out.push(StructureViolation::DuplicateMcid {
                    page: number,
                    mcid: pair[0],
                });
                reported_duplicate = Some(pair[0]);
            }
        }

        if page.has_struct_parents {
            for &mcid in &page.mcids {
                let index = mcid as usize;
                match page.struct_parents.get(index) {
                    None => {
                        out.push(StructureViolation::MissingStructParentLink { page: number, mcid })
                    }
                    Some(&id) if !all_ids.contains(&id) => {
                        out.push(StructureViolation::UnknownStructParent {
                            page: number,
                            mcid,
                            id,
                        });
                    }
                    Some(_) => {}
                }
            }
        }
    }

    for element in &input.struct_elements {
        let bare = element.role.strip_prefix('/').unwrap_or(&element.role);
        let in_role_map = input.role_map_roles.iter().any(|role| {
            role.strip_prefix('/')
                .unwrap_or(role)
                .eq_ignore_ascii_case(bare)
        });
        if !is_standard_role(bare) && !in_role_map {
            out.push(StructureViolation::MissingRoleMap {
                id: element.id,
                role: element.role.clone(),
            });
        }

        let needs_alt = !element.is_artifact
            && (bare.eq_ignore_ascii_case("Figure") || bare.eq_ignore_ascii_case("Form"));
        if needs_alt
            && element
                .alt_text
                .as_ref()
                .is_none_or(|text| text.trim().is_empty())
        {
            out.push(StructureViolation::MissingAltText {
                id: element.id,
                role: element.role.clone(),
            });
        }

        match element.parent {
            Some(parent) if parent == element.id => {
                out.push(StructureViolation::SelfParent { id: element.id });
            }
            Some(parent) => match by_id.get(&parent) {
                None => out.push(StructureViolation::UnknownParent {
                    id: element.id,
                    parent,
                }),
                Some(parent_element) if parent_element.reading_order >= element.reading_order => {
                    out.push(StructureViolation::ParentAfterChild {
                        id: element.id,
                        parent,
                    });
                }
                Some(_) => {}
            },
            None => {}
        }
    }

    out.sort_by(|a, b| {
        (a.code(), a.location(), a.message()).cmp(&(b.code(), b.location(), b.message()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PageStructureInfo;

    fn element(
        id: usize,
        role: &str,
        parent: Option<usize>,
        reading_order: usize,
    ) -> StructElementInfo {
        StructElementInfo {
            id,
            role: role.to_owned(),
            alt_text: None,
            is_artifact: false,
            parent,
            reading_order,
        }
    }

    fn page(number: usize) -> PageStructureInfo {
        PageStructureInfo {
            page_number: number,
            has_struct_parents: true,
            has_text: true,
            has_tagged_text: true,
            mcids: vec![0],
            struct_parents: vec![1],
        }
    }

    fn clean_input() -> PreflightInput {
        let mut paragraph = element(1, "P", Some(0), 1);
        paragraph.alt_text = None;
        PreflightInput {
            struct_elements: vec![element(0, "Document", None, 0), paragraph],
            page_structure: vec![page(1)],
            ..Default::default()
        }
    }

    #[test]
    fn standard_roles_are_recognised_case_insensitively() {
        assert!(is_standard_role("Figure"));
        assert!(is_standard_role("/figure"));
        assert!(is_standard_role("P"));
        assert!(!is_standard_role("CustomWidget"));
        assert!(!is_standard_role(""));
    }

    #[test]
    fn clean_structure_has_no_violations() {
        assert!(check_structure(&clean_input()).is_empty());
    }

    #[test]
    fn missing_role_map_is_typed_and_actionable() {
        let mut input = clean_input();
        input.struct_elements[1].role = "/CustomWidget".to_owned();
        let violations = check_structure(&input);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].code(), "ROLE_MAP_MISSING");
        assert_eq!(violations[0].severity(), Severity::Error);
        assert_eq!(violations[0].location(), "struct element 1");
        assert!(violations[0].message().contains("CustomWidget"));
    }

    #[test]
    fn role_map_entry_suppresses_custom_role_finding() {
        let mut input = clean_input();
        input.struct_elements[1].role = "CustomWidget".to_owned();
        input.role_map_roles = vec!["/customwidget".to_owned()];
        assert!(check_structure(&input).is_empty());
    }

    #[test]
    fn missing_alt_text_for_figures_and_forms_is_reported() {
        let mut input = clean_input();
        input.struct_elements.push(element(2, "Figure", Some(0), 2));
        input.struct_elements.push(element(3, "/Form", Some(0), 3));
        let violations = check_structure(&input);
        let alt: Vec<_> = violations
            .iter()
            .filter(|item| item.code() == "ALT_TEXT_MISSING")
            .collect();
        assert_eq!(alt.len(), 2);
        assert_eq!(alt[0].location(), "struct element 2");
        assert_eq!(alt[1].location(), "struct element 3");
    }

    #[test]
    fn artifact_and_present_alt_text_are_accepted() {
        let mut input = clean_input();
        let mut figure = element(2, "Figure", Some(0), 2);
        figure.alt_text = Some("a blue circle".to_owned());
        input.struct_elements.push(figure);
        let mut decoration = element(3, "Figure", Some(0), 3);
        decoration.is_artifact = true;
        input.struct_elements.push(decoration);
        let mut blank = element(4, "Form", Some(0), 4);
        blank.alt_text = Some("   ".to_owned());
        input.struct_elements.push(blank);
        let violations = check_structure(&input);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].code(), "ALT_TEXT_MISSING");
        assert_eq!(violations[0].location(), "struct element 4");
    }

    #[test]
    fn untagged_text_and_missing_struct_parents_are_reported() {
        let mut input = clean_input();
        input.page_structure[0].has_tagged_text = false;
        input.page_structure[0].has_struct_parents = false;
        let violations = check_structure(&input);
        let codes: Vec<&str> = violations.iter().map(StructureViolation::code).collect();
        assert_eq!(codes, vec!["NO_STRUCT_PARENTS", "UNTAGGED_TEXT"]);
        for violation in &violations {
            assert_eq!(violation.location(), "page 1");
        }
    }

    #[test]
    fn duplicate_mcid_is_reported_once_per_value() {
        let mut input = clean_input();
        input.page_structure[0].mcids = vec![2, 2, 2, 1];
        input.page_structure[0].struct_parents = vec![1, 1, 1];
        let violations = check_structure(&input);
        let duplicates: Vec<_> = violations
            .iter()
            .filter(|item| item.code() == "DUPLICATE_MCID")
            .collect();
        assert_eq!(duplicates.len(), 1);
        assert!(duplicates[0].message().contains("MCID 2"));
    }

    #[test]
    fn struct_parent_links_are_checked() {
        let mut input = clean_input();
        input.page_structure[0].mcids = vec![0, 3];
        input.page_structure[0].struct_parents = vec![99];
        let violations = check_structure(&input);
        let codes: Vec<&str> = violations.iter().map(StructureViolation::code).collect();
        assert_eq!(
            codes,
            vec!["MISSING_STRUCT_PARENT_LINK", "UNKNOWN_STRUCT_PARENT"]
        );
    }

    #[test]
    fn parent_links_and_reading_order_are_checked() {
        let mut input = clean_input();
        input.struct_elements[1].parent = Some(42);
        input.struct_elements.push(element(2, "P", Some(2), 2));
        input.struct_elements.push(element(3, "P", Some(1), 0));
        let violations = check_structure(&input);
        let codes: Vec<&str> = violations.iter().map(StructureViolation::code).collect();
        assert_eq!(
            codes,
            vec![
                "READING_ORDER_PARENT_AFTER_CHILD",
                "SELF_PARENT",
                "UNKNOWN_PARENT"
            ]
        );
    }

    #[test]
    fn duplicate_struct_id_is_typed() {
        let mut input = clean_input();
        input.struct_elements.push(element(1, "P", Some(0), 2));
        let violations = check_structure(&input);
        assert!(
            violations
                .iter()
                .any(|item| item.code() == "DUPLICATE_STRUCT_ID"
                    && item.location() == "struct element 1")
        );
    }

    #[test]
    fn violations_are_sorted_and_order_independent() {
        let mut input = clean_input();
        input.page_structure[0].has_tagged_text = false;
        input.struct_elements[1].role = "CustomWidget".to_owned();
        input.struct_elements.push(element(2, "Figure", Some(0), 2));
        let forward = check_structure(&input);
        let mut reversed = input.clone();
        reversed.struct_elements.reverse();
        reversed.page_structure.reverse();
        assert_eq!(forward, check_structure(&reversed));
        let keys: Vec<_> = forward
            .iter()
            .map(|item| (item.code(), item.location(), item.message()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn violation_display_contains_code_and_message() {
        let input = clean_input();
        let mut input = input;
        input.struct_elements[1].role = "Widget".to_owned();
        let text = check_structure(&input)[0].to_string();
        assert!(text.starts_with("ROLE_MAP_MISSING: "));
    }
}
