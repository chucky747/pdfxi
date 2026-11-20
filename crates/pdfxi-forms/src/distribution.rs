//! Local distribute/collect model for form bundles and responses.
//!
//! This module is **in-memory only**: it performs no filesystem, network, or
//! other I/O. A host supplies raw FDF/XFDF strings and respondent identifiers;
//! the crate parses, merges, and reports. A blank bundle is a typed
//! [`BlankBundle`] (metadata plus a field list) with a canonical, dependency-free
//! text representation produced by [`BlankBundle::to_text`] and read back by
//! [`parse_bundle`].
//!
//! Collection merges field values across respondents into a deterministic
//! [`MergeReport`]. When respondents disagree on a field, the value from the
//! lexicographically smallest respondent id wins and a [`FieldConflict`] is
//! recorded with every differing contribution. Duplicate respondent ids are
//! collapsed deterministically (the smallest raw content wins) and reported.
//! Malformed individual responses become rejections rather than aborting the
//! whole collection, so a host can summarize partial success.
//!
//! XFDF responses are parsed without a target [`crate::Form`] (via
//! [`crate::parse_xfdf`]), so raw values are mapped conservatively: zero values
//! to [`FieldValue::Empty`], one value to [`FieldValue::Text`], and multiple
//! values to [`FieldValue::Choice`]. Hosts that know the field types can map
//! them with [`crate::XfdfDocument::to_fdf`] before calling
//! [`merge_responses`] with a pre-built [`ResponseFile`] in
//! [`ResponseFormat::Fdf`] form.
//!
//! All inputs are bounded and every report is deterministic and free of
//! `unwrap`/`panic` in non-test code.

use crate::fdf::{FdfError, FdfField, import_fdf};
use crate::field::{FieldType, FieldValue};
use crate::form::Form;
use crate::xfdf::{XfdfError, parse_xfdf};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum number of responses accepted by [`merge_responses`].
pub const MAX_RESPONSES: usize = 4096;

/// Maximum number of fields in a bundle or response.
pub const MAX_FIELDS: usize = 4096;

/// Maximum number of characters in a respondent id or metadata string.
pub const MAX_TEXT_LEN: usize = 256;

/// Maximum number of bytes in one raw response document.
pub const MAX_CONTENT_BYTES: usize = 1_048_576;

/// Metadata identifying a blank form bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleMeta {
    form_id: String,
    title: String,
    version: u32,
}

impl BundleMeta {
    /// Creates metadata, validating bounds.
    ///
    /// # Errors
    ///
    /// Returns [`DistributionError`] for empty or over-long strings.
    pub fn new(
        form_id: impl Into<String>,
        title: impl Into<String>,
        version: u32,
    ) -> Result<Self, DistributionError> {
        let form_id = form_id.into();
        let title = title.into();
        check_text(&form_id, "form id")?;
        check_text(&title, "title")?;
        Ok(Self {
            form_id,
            title,
            version,
        })
    }

    /// Returns the stable form identifier.
    pub fn form_id(&self) -> &str {
        &self.form_id
    }

    /// Returns the human-readable title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the bundle version.
    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// One field advertised in a blank bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleField {
    name: String,
    field_type: FieldType,
    label: Option<String>,
    required: bool,
}

impl BundleField {
    /// Creates a bundle field description, validating its name and label.
    ///
    /// # Errors
    ///
    /// Returns [`DistributionError`] for an empty or over-long name or label.
    pub fn new(
        name: impl Into<String>,
        field_type: FieldType,
        label: Option<String>,
        required: bool,
    ) -> Result<Self, DistributionError> {
        let name = name.into();
        check_text(&name, "field name")?;
        if let Some(label) = &label {
            check_text(label, "field label")?;
        }
        Ok(Self {
            name,
            field_type,
            label,
            required,
        })
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the advertised field type.
    pub const fn field_type(&self) -> FieldType {
        self.field_type
    }

    /// Returns the optional prompt/label.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns `true` when a response must supply this field.
    pub const fn required(&self) -> bool {
        self.required
    }
}

/// A blank form bundle: metadata plus the field list recipients should fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlankBundle {
    meta: BundleMeta,
    fields: Vec<BundleField>,
}

impl BlankBundle {
    /// Builds a bundle, rejecting too many fields.
    ///
    /// # Errors
    ///
    /// Returns [`DistributionError::TooManyFields`] when `fields` exceeds
    /// [`MAX_FIELDS`].
    pub fn new(meta: BundleMeta, fields: Vec<BundleField>) -> Result<Self, DistributionError> {
        if fields.len() > MAX_FIELDS {
            return Err(DistributionError::TooManyFields {
                count: fields.len(),
                max: MAX_FIELDS,
            });
        }
        Ok(Self { meta, fields })
    }

    /// Builds a blank bundle from a [`Form`], preserving field order.
    ///
    /// # Errors
    ///
    /// Returns [`DistributionError`] for over-long field metadata or too many
    /// fields.
    pub fn from_form(meta: BundleMeta, form: &Form) -> Result<Self, DistributionError> {
        let mut fields = Vec::with_capacity(form.len());
        for field in form.fields() {
            fields.push(BundleField::new(
                field.name(),
                field.field_type(),
                None,
                field.is_required(),
            )?);
        }
        Self::new(meta, fields)
    }

    /// Returns the bundle metadata.
    pub fn meta(&self) -> &BundleMeta {
        &self.meta
    }

    /// Returns the advertised fields.
    pub fn fields(&self) -> &[BundleField] {
        &self.fields
    }

    /// Renders the bundle as a canonical, deterministic text document.
    pub fn to_text(&self) -> String {
        let mut out = String::from("PDFXI-BUNDLE-1\n");
        push_line(&mut out, "ID", &self.meta.form_id);
        push_line(&mut out, "TITLE", &self.meta.title);
        out.push_str(&format!("VERSION\t{}\n", self.meta.version));
        for field in &self.fields {
            out.push_str("FIELD\t");
            out.push_str(&escape(&field.name));
            out.push('\t');
            out.push_str(&field.field_type.to_string());
            out.push('\t');
            out.push_str(if field.required { "1" } else { "0" });
            out.push('\t');
            out.push_str(&field.label.as_deref().map(escape).unwrap_or_default());
            out.push('\n');
        }
        out
    }
}

/// Parses a canonical bundle document produced by [`BlankBundle::to_text`].
///
/// # Errors
///
/// Returns [`DistributionError::MalformedBundle`] for an unknown header, an
/// unrecognised line tag or field type, a malformed escape, or missing
/// metadata, and [`DistributionError::TooManyFields`] for oversized bundles.
pub fn parse_bundle(text: &str) -> Result<BlankBundle, DistributionError> {
    let mut lines = text.lines();
    if lines.next() != Some("PDFXI-BUNDLE-1") {
        return Err(DistributionError::MalformedBundle(
            "unknown header".to_owned(),
        ));
    }
    let mut form_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut version: Option<u32> = None;
    let mut fields = Vec::new();
    for line in lines {
        let mut parts = line.split('\t');
        let tag = parts.next().unwrap_or("");
        match tag {
            "ID" => form_id = Some(unescape(parts.next().unwrap_or(""))?),
            "TITLE" => title = Some(unescape(parts.next().unwrap_or(""))?),
            "VERSION" => {
                let raw = parts.next().unwrap_or("");
                version = Some(raw.parse::<u32>().map_err(|_| {
                    DistributionError::MalformedBundle(format!("invalid version '{raw}'"))
                })?);
            }
            "FIELD" => {
                let name = unescape(parts.next().unwrap_or(""))?;
                let type_label = parts.next().unwrap_or("");
                let field_type = parse_field_type(type_label).ok_or_else(|| {
                    DistributionError::MalformedBundle(format!("unknown field type '{type_label}'"))
                })?;
                let required = match parts.next().unwrap_or("") {
                    "0" => false,
                    "1" => true,
                    other => {
                        return Err(DistributionError::MalformedBundle(format!(
                            "invalid required flag '{other}'"
                        )));
                    }
                };
                let raw_label = parts.next().unwrap_or("");
                let label = if raw_label.is_empty() {
                    None
                } else {
                    Some(unescape(raw_label)?)
                };
                fields.push(BundleField::new(name, field_type, label, required)?);
            }
            other => {
                return Err(DistributionError::MalformedBundle(format!(
                    "unknown line tag '{other}'"
                )));
            }
        }
    }
    let meta = BundleMeta::new(
        form_id.ok_or_else(|| DistributionError::MalformedBundle("missing ID".to_owned()))?,
        title.ok_or_else(|| DistributionError::MalformedBundle("missing TITLE".to_owned()))?,
        version.ok_or_else(|| DistributionError::MalformedBundle("missing VERSION".to_owned()))?,
    )?;
    BlankBundle::new(meta, fields)
}

/// The wire format of one collected response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    /// An FDF 1.7 document, parsed with [`import_fdf`].
    Fdf,
    /// An XFDF document, parsed with [`parse_xfdf`].
    Xfdf,
}

impl fmt::Display for ResponseFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fdf => "fdf",
            Self::Xfdf => "xfdf",
        })
    }
}

/// One candidate response supplied by the host, with its respondent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseFile {
    respondent: String,
    format: ResponseFormat,
    content: String,
}

impl ResponseFile {
    /// Creates a response candidate. Bounds are checked when it is imported.
    pub fn new(
        respondent: impl Into<String>,
        format: ResponseFormat,
        content: impl Into<String>,
    ) -> Self {
        Self {
            respondent: respondent.into(),
            format,
            content: content.into(),
        }
    }

    /// Returns the respondent identifier.
    pub fn respondent(&self) -> &str {
        &self.respondent
    }

    /// Returns the response format.
    pub const fn format(&self) -> ResponseFormat {
        self.format
    }

    /// Returns the raw document text.
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// A successfully parsed response.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedResponse {
    respondent: String,
    fields: Vec<FdfField>,
}

impl ImportedResponse {
    /// Returns the respondent identifier.
    pub fn respondent(&self) -> &str {
        &self.respondent
    }

    /// Returns the imported field values in document order.
    pub fn fields(&self) -> &[FdfField] {
        &self.fields
    }
}

/// Parses one [`ResponseFile`] into typed field values.
///
/// # Errors
///
/// Returns [`DistributionError`] for an empty or over-long respondent id, an
/// oversized document, too many fields, an empty field name, or a parse failure
/// in either supported format.
pub fn import_response_file(file: &ResponseFile) -> Result<ImportedResponse, DistributionError> {
    check_text(&file.respondent, "respondent id")?;
    if file.content.len() > MAX_CONTENT_BYTES {
        return Err(DistributionError::ContentTooLarge {
            len: file.content.len(),
            max: MAX_CONTENT_BYTES,
        });
    }
    let fields = match file.format {
        ResponseFormat::Fdf => import_fdf(&file.content)?.fields().to_vec(),
        ResponseFormat::Xfdf => {
            let document = parse_xfdf(&file.content)?;
            let mut fields = Vec::with_capacity(document.len());
            for raw in document.fields() {
                fields.push(FdfField::new(raw.name(), raw_to_value(raw.values())));
            }
            fields
        }
    };
    if fields.len() > MAX_FIELDS {
        return Err(DistributionError::TooManyFields {
            count: fields.len(),
            max: MAX_FIELDS,
        });
    }
    for field in &fields {
        if field.name().is_empty() {
            return Err(DistributionError::EmptyFieldName);
        }
    }
    Ok(ImportedResponse {
        respondent: file.respondent.clone(),
        fields,
    })
}

/// Maps raw XFDF value strings without consulting a form.
fn raw_to_value(values: &[String]) -> FieldValue {
    match values {
        [] => FieldValue::Empty,
        [single] => FieldValue::Text(single.clone()),
        many => FieldValue::Choice(many.to_vec()),
    }
}

/// One respondent's contribution to a field.
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    respondent: String,
    value: FieldValue,
}

impl Contribution {
    /// Returns the contributing respondent id.
    pub fn respondent(&self) -> &str {
        &self.respondent
    }

    /// Returns the contributed value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }
}

/// A field on which respondents supplied differing values.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldConflict {
    name: String,
    values: Vec<Contribution>,
}

impl FieldConflict {
    /// Returns the contested field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns every distinct contribution, sorted by respondent then value.
    pub fn values(&self) -> &[Contribution] {
        &self.values
    }
}

/// A merged field with a deterministic winning value.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedField {
    name: String,
    value: FieldValue,
    contributors: Vec<String>,
    conflict: bool,
}

impl MergedField {
    /// Returns the field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the deterministic winning value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }

    /// Returns the sorted respondent ids who supplied this field.
    pub fn contributors(&self) -> &[String] {
        &self.contributors
    }

    /// Returns `true` when respondents disagreed on this field.
    pub const fn has_conflict(&self) -> bool {
        self.conflict
    }
}

/// A response that could not be imported, with a typed reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedResponse {
    respondent: String,
    reason: String,
}

impl RejectedResponse {
    /// Returns the respondent id of the rejected response.
    pub fn respondent(&self) -> &str {
        &self.respondent
    }

    /// Returns the human-readable rejection reason.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// The deterministic result of collecting and merging responses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeReport {
    fields: Vec<MergedField>,
    conflicts: Vec<FieldConflict>,
    duplicates: Vec<String>,
    rejected: Vec<RejectedResponse>,
    /// Number of responses whose values entered the merge.
    accepted_count: usize,
}

impl MergeReport {
    /// Returns merged fields sorted by name.
    pub fn fields(&self) -> &[MergedField] {
        &self.fields
    }

    /// Returns conflicts sorted by field name.
    pub fn conflicts(&self) -> &[FieldConflict] {
        &self.conflicts
    }

    /// Returns respondent ids that submitted duplicate responses, sorted.
    pub fn duplicates(&self) -> &[String] {
        &self.duplicates
    }

    /// Returns rejected responses sorted by respondent id.
    pub fn rejected(&self) -> &[RejectedResponse] {
        &self.rejected
    }

    /// Returns a deterministic collection status summary.
    pub fn summary(&self) -> CollectionSummary {
        CollectionSummary {
            responses_received: self.accepted_count + self.duplicates.len() + self.rejected.len(),
            responses_accepted: self.accepted_count,
            responses_rejected: self.rejected.len(),
            duplicate_respondents: self.duplicates.len(),
            fields_merged: self.fields.len(),
            conflicts: self.conflicts.len(),
        }
    }

    /// Renders a stable, single-line status summary.
    pub fn to_text(&self) -> String {
        let summary = self.summary();
        format!(
            "collect: received={} accepted={} rejected={} duplicates={} fields={} conflicts={}",
            summary.responses_received,
            summary.responses_accepted,
            summary.responses_rejected,
            summary.duplicate_respondents,
            summary.fields_merged,
            summary.conflicts
        )
    }

    /// Adds an accepted response count; used internally during merging.
    fn note_accepted(&mut self, count: usize) {
        self.accepted_count = count;
    }
}

/// Counts summarizing a collection run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionSummary {
    /// Number of candidate responses supplied.
    pub responses_received: usize,
    /// Number of responses whose values entered the merge.
    pub responses_accepted: usize,
    /// Number of responses rejected due to parse or validation errors.
    pub responses_rejected: usize,
    /// Number of respondent ids seen more than once.
    pub duplicate_respondents: usize,
    /// Number of distinct field names merged.
    pub fields_merged: usize,
    /// Number of fields with conflicting values.
    pub conflicts: usize,
}

/// Merges a set of responses deterministically.
///
/// Each response is imported; failures are recorded in [`MergeReport::rejected`]
/// rather than aborting. Candidates are then sorted by respondent id and raw
/// content so that duplicate respondent ids collapse to the smallest content
/// and the winner of any value conflict is stable under input reordering.
///
/// # Errors
///
/// Returns [`DistributionError::TooManyResponses`] when more than
/// [`MAX_RESPONSES`] candidates are supplied.
pub fn merge_responses(files: &[ResponseFile]) -> Result<MergeReport, DistributionError> {
    if files.len() > MAX_RESPONSES {
        return Err(DistributionError::TooManyResponses {
            count: files.len(),
            max: MAX_RESPONSES,
        });
    }

    // Canonical input order makes duplicate resolution and rejection ordering
    // independent of the order in which the host supplied the responses.
    let mut ordered: Vec<&ResponseFile> = files.iter().collect();
    ordered.sort_by(|a, b| {
        a.respondent
            .cmp(&b.respondent)
            .then_with(|| a.content.cmp(&b.content))
    });

    let mut imported: Vec<ImportedResponse> = Vec::new();
    let mut rejected: Vec<RejectedResponse> = Vec::new();
    for file in ordered {
        match import_response_file(file) {
            Ok(response) => imported.push(response),
            Err(error) => rejected.push(RejectedResponse {
                respondent: file.respondent.clone(),
                reason: error.to_string(),
            }),
        }
    }

    // Deterministic ordering independent of caller order.
    imported.sort_by(|a, b| a.respondent.cmp(&b.respondent));
    let mut chosen: Vec<ImportedResponse> = Vec::new();
    let mut duplicates: Vec<String> = Vec::new();
    for response in imported {
        if let Some(existing) = chosen.last()
            && existing.respondent == response.respondent
        {
            duplicates.push(response.respondent.clone());
            continue;
        }
        chosen.push(response);
    }

    let mut by_field: BTreeMap<String, Vec<Contribution>> = BTreeMap::new();
    for response in &chosen {
        for field in &response.fields {
            by_field
                .entry(field.name().to_owned())
                .or_default()
                .push(Contribution {
                    respondent: response.respondent.clone(),
                    value: field.value().clone(),
                });
        }
    }

    let mut fields = Vec::with_capacity(by_field.len());
    let mut conflicts = Vec::new();
    for (name, mut contributions) in by_field {
        contributions.sort_by(|a, b| {
            a.respondent
                .cmp(&b.respondent)
                .then_with(|| value_key(&a.value).cmp(&value_key(&b.value)))
        });
        let winner = contributions
            .first()
            .map(|contribution| contribution.value.clone())
            .unwrap_or_default();
        let has_conflict = contributions
            .iter()
            .any(|contribution| contribution.value != winner);
        let mut contributors: Vec<String> = contributions
            .iter()
            .map(|contribution| contribution.respondent.clone())
            .collect();
        contributors.dedup();
        if has_conflict {
            let mut distinct: Vec<Contribution> = Vec::new();
            for contribution in &contributions {
                let key = (
                    contribution.respondent.as_str(),
                    value_key(&contribution.value),
                );
                if !distinct.iter().any(|existing| {
                    existing.respondent == contribution.respondent
                        && value_key(&existing.value) == key.1
                }) {
                    distinct.push(contribution.clone());
                }
            }
            conflicts.push(FieldConflict {
                name: name.clone(),
                values: distinct,
            });
        }
        fields.push(MergedField {
            name,
            value: winner,
            contributors,
            conflict: has_conflict,
        });
    }

    duplicates.sort();
    duplicates.dedup();
    rejected.sort_by(|a, b| a.respondent.cmp(&b.respondent));
    let mut report = MergeReport {
        fields,
        conflicts,
        duplicates,
        rejected,
        ..MergeReport::default()
    };
    report.note_accepted(chosen.len());
    Ok(report)
}

/// Returns an orderable string key for a value, used only for deterministic
/// sorting and conflict de-duplication.
fn value_key(value: &FieldValue) -> String {
    match value {
        FieldValue::Empty => "0:".to_owned(),
        FieldValue::Text(text) => format!("1:{text}"),
        FieldValue::Bool(flag) => format!("2:{flag}"),
        FieldValue::Choice(values) => format!("3:{}", values.join("\u{1f}")),
        FieldValue::Number(number) => format!("4:{:016x}", number.to_bits()),
    }
}

/// An error produced while exporting a bundle or importing responses.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DistributionError {
    /// More than [`MAX_RESPONSES`] candidates were supplied.
    #[error("too many responses: {count} exceeds maximum {max}")]
    TooManyResponses {
        /// The number supplied.
        count: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// More than [`MAX_FIELDS`] fields were supplied.
    #[error("too many fields: {count} exceeds maximum {max}")]
    TooManyFields {
        /// The number supplied.
        count: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// A respondent id or metadata string was empty.
    #[error("{0} must not be empty")]
    EmptyText(&'static str),
    /// A respondent id or metadata string was too long.
    #[error("{what} is too long ({len} > {max})")]
    TextTooLong {
        /// What the string describes.
        what: &'static str,
        /// The observed length.
        len: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// A raw response exceeded [`MAX_CONTENT_BYTES`].
    #[error("response content is too large ({len} > {max})")]
    ContentTooLarge {
        /// The observed byte length.
        len: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// An imported response contained an unnamed field.
    #[error("imported response contains a field with an empty name")]
    EmptyFieldName,
    /// A bundle document was structurally invalid.
    #[error("malformed bundle: {0}")]
    MalformedBundle(String),
    /// FDF parsing failed.
    #[error("FDF import failed: {0}")]
    Fdf(#[from] FdfError),
    /// XFDF parsing failed.
    #[error("XFDF import failed: {0}")]
    Xfdf(#[from] XfdfError),
}

/// Validates a bounded free-text string used by bundles and responses.
fn check_text(text: &str, what: &'static str) -> Result<(), DistributionError> {
    if text.is_empty() {
        return Err(DistributionError::EmptyText(what));
    }
    if text.chars().count() > MAX_TEXT_LEN {
        return Err(DistributionError::TextTooLong {
            what,
            len: text.chars().count(),
            max: MAX_TEXT_LEN,
        });
    }
    Ok(())
}

/// Appends an escaped `KEY\tVALUE\n` line to `out`.
fn push_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push('\t');
    out.push_str(&escape(value));
    out.push('\n');
}

/// Escapes backslash, tab and newline so values stay on one line.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Reverses [`escape`].
fn unescape(value: &str) -> Result<String, DistributionError> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    return Err(DistributionError::MalformedBundle(format!(
                        "invalid escape '\\{other}'"
                    )));
                }
                None => {
                    return Err(DistributionError::MalformedBundle(
                        "trailing backslash".to_owned(),
                    ));
                }
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

/// Maps a stable field-type label back to its enum variant.
fn parse_field_type(label: &str) -> Option<FieldType> {
    Some(match label {
        "text" => FieldType::Text,
        "checkbox" => FieldType::CheckBox,
        "radio" => FieldType::Radio,
        "choice" => FieldType::Choice,
        "button" => FieldType::Button,
        "signature" => FieldType::Signature,
        "unknown" => FieldType::Unknown,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::FieldFlags;
    use crate::form::Form;

    fn meta() -> BundleMeta {
        BundleMeta::new("order-form", "Order", 3).unwrap()
    }

    fn form() -> Form {
        Form::from_fields([
            crate::Field::new("customer", FieldType::Text).with_flags(FieldFlags::REQUIRED),
            crate::Field::new("agree", FieldType::CheckBox),
        ])
        .unwrap()
    }

    fn fdf(respondent: &str, assignments: &[(&str, FieldValue)]) -> ResponseFile {
        let mut body = String::from("%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [\n");
        for (name, value) in assignments {
            body.push_str("<< /T (");
            body.push_str(name);
            body.push_str(") /V (");
            match value {
                FieldValue::Text(text) => body.push_str(text),
                FieldValue::Bool(flag) => body.push_str(if *flag { "On" } else { "Off" }),
                _ => {}
            }
            body.push_str(") >>\n");
        }
        body.push_str("] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
        ResponseFile::new(respondent, ResponseFormat::Fdf, body)
    }

    #[test]
    fn blank_bundle_exports_and_round_trips() {
        let bundle = BlankBundle::from_form(meta(), &form()).unwrap();
        let text = bundle.to_text();
        assert!(text.starts_with("PDFXI-BUNDLE-1\n"));
        assert!(text.contains("FIELD\tcustomer\ttext\t1\t"));
        let parsed = parse_bundle(&text).unwrap();
        assert_eq!(parsed, bundle);
        assert_eq!(parsed.meta().form_id(), "order-form");
        assert_eq!(parsed.fields()[0].field_type(), FieldType::Text);
        assert!(parsed.fields()[0].required());
    }

    #[test]
    fn bundle_text_escapes_special_characters() {
        let bundle = BlankBundle::new(
            BundleMeta::new("id\t1", "A\nB", 1).unwrap(),
            vec![
                BundleField::new(
                    "name\\x",
                    FieldType::Text,
                    Some("tab\there".to_owned()),
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let text = bundle.to_text();
        assert!(text.contains("ID\tid\\t1\n"));
        assert!(text.contains("TITLE\tA\\nB\n"));
        assert_eq!(parse_bundle(&text), Ok(bundle));
    }

    #[test]
    fn imports_fdf_and_xfdf_responses() {
        let fdf = fdf("alice", &[("customer", FieldValue::Text("Ada".into()))]);
        let imported = import_response_file(&fdf).unwrap();
        assert_eq!(imported.respondent(), "alice");
        assert_eq!(imported.fields()[0].name(), "customer");
        assert_eq!(
            imported.fields()[0].value(),
            &FieldValue::Text("Ada".into())
        );

        let xfdf_text = "<?xml version=\"1.0\"?>\n<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields><field name=\"color\"><value>red</value><value>blue</value></field></fields></xfdf>";
        let xfdf = ResponseFile::new("bob", ResponseFormat::Xfdf, xfdf_text);
        let imported = import_response_file(&xfdf).unwrap();
        assert_eq!(
            imported.fields()[0].value(),
            &FieldValue::Choice(vec!["red".into(), "blue".into()])
        );
    }

    #[test]
    fn duplicate_responses_collapse_deterministically() {
        let a = fdf("alice", &[("customer", FieldValue::Text("1".into()))]);
        let b = fdf("alice", &[("customer", FieldValue::Text("2".into()))]);
        let forward = merge_responses(&[a.clone(), b.clone()]).unwrap();
        let reverse = merge_responses(&[b, a]).unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(forward.duplicates(), ["alice"]);
        assert_eq!(forward.summary().duplicate_respondents, 1);
        assert_eq!(forward.fields()[0].value(), &FieldValue::Text("1".into()));
    }

    #[test]
    fn conflicts_are_reported_with_all_contributions() {
        let a = fdf("alice", &[("customer", FieldValue::Text("1".into()))]);
        let b = fdf("bob", &[("customer", FieldValue::Text("2".into()))]);
        let report = merge_responses(&[b, a]).unwrap();
        assert_eq!(report.conflicts().len(), 1);
        assert_eq!(report.conflicts()[0].name(), "customer");
        assert_eq!(report.conflicts()[0].values().len(), 2);
        assert_eq!(report.fields()[0].value(), &FieldValue::Text("1".into()));
        assert!(report.fields()[0].has_conflict());
        assert_eq!(report.fields()[0].contributors(), ["alice", "bob"]);
    }

    #[test]
    fn status_summary_counts_rejected_responses() {
        let good = fdf("alice", &[("customer", FieldValue::Text("ok".into()))]);
        let bad = ResponseFile::new("bob", ResponseFormat::Fdf, "not an fdf");
        let report = merge_responses(&[good, bad]).unwrap();
        let summary = report.summary();
        assert_eq!(summary.responses_received, 2);
        assert_eq!(summary.responses_accepted, 1);
        assert_eq!(summary.responses_rejected, 1);
        assert_eq!(summary.fields_merged, 1);
        assert_eq!(summary.conflicts, 0);
        assert_eq!(report.rejected()[0].respondent(), "bob");
        assert!(report.to_text().contains("rejected=1"));
    }

    #[test]
    fn merge_is_deterministic_under_input_reordering() {
        let a = fdf("alice", &[("customer", FieldValue::Text("A".into()))]);
        let b = fdf("bob", &[("agree", FieldValue::Bool(true))]);
        let c = fdf("carol", &[("customer", FieldValue::Text("A".into()))]);
        let first = merge_responses(&[a.clone(), b.clone(), c.clone()]).unwrap();
        let second = merge_responses(&[c, b, a]).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.fields().len(), 2);
        assert!(!first.fields()[0].has_conflict());
    }

    #[test]
    fn invalid_inputs_are_typed_errors() {
        let empty_id = ResponseFile::new("", ResponseFormat::Fdf, "%FDF-1.7");
        assert_eq!(
            import_response_file(&empty_id),
            Err(DistributionError::EmptyText("respondent id"))
        );

        let long_id = ResponseFile::new("x".repeat(MAX_TEXT_LEN + 1), ResponseFormat::Fdf, "x");
        assert!(matches!(
            import_response_file(&long_id),
            Err(DistributionError::TextTooLong { .. })
        ));

        let huge = ResponseFile::new("a", ResponseFormat::Fdf, "x".repeat(MAX_CONTENT_BYTES + 1));
        assert!(matches!(
            import_response_file(&huge),
            Err(DistributionError::ContentTooLarge { .. })
        ));

        let many = vec![fdf("a", &[]); MAX_RESPONSES + 1];
        assert!(matches!(
            merge_responses(&many),
            Err(DistributionError::TooManyResponses { .. })
        ));

        assert!(parse_bundle("NOT-A-BUNDLE").is_err());
        assert!(BundleMeta::new("", "t", 1).is_err());
    }
}
