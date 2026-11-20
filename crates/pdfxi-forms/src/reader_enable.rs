//! Local reader-enable compatibility metadata (not Adobe certification).
//!
//! Acrobat's "Extend features in Adobe Reader" workflow attaches a usage-rights
//! signature in a `/Perms /UR` dictionary. This module models that *shape* as a
//! purely local, typed plan: a set of [`UsageRight`] flags and a
//! [`UsageRightsSignature`] placeholder. It performs **no cryptography**, signs
//! nothing, and does not interact with Adobe or any certification authority.
//!
//! The emitted filter name is deliberately PDFXI-specific
//! ([`LOCAL_FILTER`]) and the `/Contents` entry is a fixed run of ASCII `0`
//! characters at a caller-chosen size. The output is compatibility metadata for
//! a host that wants to carry reader-enable intent alongside a form; it is
//! **not** a valid digital signature and must never be presented as Adobe
//! certification or as proof of document integrity. [`ReaderEnablePlan::is_certification`]
//! always returns `false` to make that boundary explicit in code.
//!
//! A [`FillSavePolicy`] derives explicit fill/save decisions from a rights set:
//! a form-fill flag, a save flag, and the full set of allowed operations, with
//! [`Permission`] outcomes returned by [`ReaderEnablePlan::can_fill`] and
//! [`ReaderEnablePlan::can_save`]. [`parse_rights_dictionary`] validates an
//! existing `/Perms /UR` dictionary, rejecting malformed structure, foreign
//! filters, non-placeholder `/Contents`, and unknown `/Ff` bits. A
//! [`PolicyReport`] renders those decisions deterministically.
//!
//! All inputs are bounded and deterministic; unsupported rights, empty or
//! over-long filters, foreign filters, and out-of-range placeholder sizes
//! yield a typed [`ReaderEnableError`].

use crate::acroform::PdfValue;
use std::collections::BTreeMap;
use std::fmt;

/// The local, non-Adobe filter name used by the placeholder signature.
pub const LOCAL_FILTER: &str = "PDFXI.ReaderEnable.Local";

/// Maximum number of characters in a filter or reason string.
pub const READER_MAX_TEXT_LEN: usize = 256;

/// Default size, in bytes, of the placeholder `/Contents` payload.
pub const READER_DEFAULT_PLACEHOLDER_SIZE: usize = 4096;

/// Maximum size, in bytes, of the placeholder `/Contents` payload.
pub const READER_MAX_PLACEHOLDER_SIZE: usize = 65_536;

/// A locally modelled usage right that reader-enable metadata can grant.
///
/// The bit assigned to each variant is PDFXI-local metadata. It is stable so
/// reports and dictionaries are deterministic, but it is not asserted to match
/// any proprietary Adobe flag layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageRight {
    /// Permit filling in form fields.
    FillIn,
    /// Permit saving changes to the document.
    Save,
    /// Permit adding or editing annotations.
    Annotate,
    /// Permit assembling pages (insert, delete, rotate).
    Assemble,
    /// Permit copying or extracting content.
    CopyExtract,
    /// Permit assistive-technology access.
    Accessibility,
}

impl UsageRight {
    /// All usage rights supported by this local model, in ascending bit order.
    pub const ALL: [Self; 6] = [
        Self::FillIn,
        Self::Save,
        Self::Annotate,
        Self::Assemble,
        Self::CopyExtract,
        Self::Accessibility,
    ];

    /// Returns the local bit assigned to this right.
    pub const fn bit(self) -> u32 {
        match self {
            Self::FillIn => 1 << 0,
            Self::Save => 1 << 1,
            Self::Annotate => 1 << 2,
            Self::Assemble => 1 << 3,
            Self::CopyExtract => 1 << 4,
            Self::Accessibility => 1 << 5,
        }
    }

    /// Returns a stable lowercase label for reports.
    pub const fn label(self) -> &'static str {
        match self {
            Self::FillIn => "fill-in",
            Self::Save => "save",
            Self::Annotate => "annotate",
            Self::Assemble => "assemble",
            Self::CopyExtract => "copy-extract",
            Self::Accessibility => "accessibility",
        }
    }
}

impl fmt::Display for UsageRight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Bit mask covering every right supported by this local model.
pub const SUPPORTED_RIGHT_BITS: u32 = 0b11_1111;

/// A bounded set of [`UsageRight`] flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DocumentRights(u32);

impl DocumentRights {
    /// An empty rights set.
    pub const NONE: Self = Self(0);

    /// Creates an empty set.
    pub const fn new() -> Self {
        Self(0)
    }

    /// Returns a new set with `right` added.
    #[must_use]
    pub const fn with(self, right: UsageRight) -> Self {
        Self(self.0 | right.bit())
    }

    /// Returns `true` when `right` is present.
    pub const fn contains(self, right: UsageRight) -> bool {
        (self.0 & right.bit()) != 0
    }

    /// Returns the raw local bit mask.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Builds a set from a raw bit mask, rejecting unsupported bits.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError::UnsupportedRight`] when `bits` contains a
    /// bit outside [`SUPPORTED_RIGHT_BITS`].
    pub fn from_bits(bits: u32) -> Result<Self, ReaderEnableError> {
        if bits & !SUPPORTED_RIGHT_BITS != 0 {
            return Err(ReaderEnableError::UnsupportedRight { bits });
        }
        Ok(Self(bits))
    }

    /// Returns the contained rights in ascending bit order.
    pub fn iter(self) -> impl Iterator<Item = UsageRight> {
        UsageRight::ALL
            .into_iter()
            .filter(move |right| self.contains(*right))
    }
}

/// A placeholder usage-rights signature. No cryptographic material is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRightsSignature {
    filter: String,
    placeholder_bytes: usize,
    reason: Option<String>,
}

impl UsageRightsSignature {
    /// Creates a placeholder using the local [`LOCAL_FILTER`] and the default
    /// placeholder size.
    pub fn local() -> Self {
        Self {
            filter: LOCAL_FILTER.to_owned(),
            placeholder_bytes: READER_DEFAULT_PLACEHOLDER_SIZE,
            reason: None,
        }
    }

    /// Creates a placeholder with a caller-supplied filter name.
    ///
    /// Validation happens in [`ReaderEnablePlan::validate`]; this constructor
    /// accepts any string so a host can surface a typed error at plan time.
    pub fn with_filter(filter: impl Into<String>) -> Self {
        Self {
            filter: filter.into(),
            placeholder_bytes: READER_DEFAULT_PLACEHOLDER_SIZE,
            reason: None,
        }
    }

    /// Sets the placeholder `/Contents` size in bytes.
    #[must_use]
    pub const fn with_placeholder_bytes(mut self, bytes: usize) -> Self {
        self.placeholder_bytes = bytes;
        self
    }

    /// Sets an optional reason string carried as local metadata.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Returns the filter name.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Returns the placeholder byte count.
    pub const fn placeholder_bytes(&self) -> usize {
        self.placeholder_bytes
    }

    /// Returns the optional reason.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// A deterministic, local-only reader-enable plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderEnablePlan {
    rights: DocumentRights,
    signature: UsageRightsSignature,
}

impl ReaderEnablePlan {
    /// Creates a plan granting `rights` with the default local placeholder.
    pub fn new(rights: DocumentRights) -> Self {
        Self {
            rights,
            signature: UsageRightsSignature {
                filter: LOCAL_FILTER.to_owned(),
                placeholder_bytes: READER_DEFAULT_PLACEHOLDER_SIZE,
                reason: None,
            },
        }
    }

    /// Creates a plan from a raw bit mask with the default local placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError::UnsupportedRight`] for an unsupported bit.
    pub fn from_bits(bits: u32) -> Result<Self, ReaderEnableError> {
        Ok(Self::new(DocumentRights::from_bits(bits)?))
    }

    /// Replaces the placeholder signature.
    #[must_use]
    pub fn with_signature(mut self, signature: UsageRightsSignature) -> Self {
        self.signature = signature;
        self
    }

    /// Returns the granted rights.
    pub const fn rights(&self) -> DocumentRights {
        self.rights
    }

    /// Returns the placeholder signature.
    pub fn signature(&self) -> &UsageRightsSignature {
        &self.signature
    }

    /// Always `false`: this plan carries local metadata, never a certification.
    pub const fn is_certification(&self) -> bool {
        false
    }

    /// Validates the plan's bounded inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError`] for an empty, over-long, or foreign
    /// (non-local) filter, an over-long reason, or a placeholder size outside
    /// `1..=READER_MAX_PLACEHOLDER_SIZE`.
    pub fn validate(&self) -> Result<(), ReaderEnableError> {
        let filter = &self.signature.filter;
        if filter.is_empty() {
            return Err(ReaderEnableError::EmptyFilter);
        }
        if filter.chars().count() > READER_MAX_TEXT_LEN {
            return Err(ReaderEnableError::FilterTooLong {
                len: filter.chars().count(),
                max: READER_MAX_TEXT_LEN,
            });
        }
        if let Some(reason) = &self.signature.reason
            && reason.chars().count() > READER_MAX_TEXT_LEN
        {
            return Err(ReaderEnableError::ReasonTooLong {
                len: reason.chars().count(),
                max: READER_MAX_TEXT_LEN,
            });
        }
        if filter != LOCAL_FILTER {
            return Err(ReaderEnableError::NonLocalFilter {
                filter: filter.clone(),
            });
        }
        if self.signature.placeholder_bytes == 0
            || self.signature.placeholder_bytes > READER_MAX_PLACEHOLDER_SIZE
        {
            return Err(ReaderEnableError::InvalidPlaceholderSize {
                size: self.signature.placeholder_bytes,
                max: READER_MAX_PLACEHOLDER_SIZE,
            });
        }
        Ok(())
    }

    /// Emits a `/Perms /UR` dictionary *model* containing the local flags and
    /// placeholder contents.
    ///
    /// The `/Contents` value is a run of ASCII `0` characters of the configured
    /// size. It is intentionally not a cryptographic signature.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError`] when [`ReaderEnablePlan::validate`] fails.
    pub fn to_dictionary(&self) -> Result<PdfValue, ReaderEnableError> {
        self.validate()?;
        let mut ur = BTreeMap::new();
        ur.insert(
            "Ff".to_owned(),
            PdfValue::Number(f64::from(self.rights.bits())),
        );
        ur.insert(
            "F".to_owned(),
            PdfValue::Name(self.signature.filter.clone()),
        );
        ur.insert(
            "Contents".to_owned(),
            PdfValue::Text("0".repeat(self.signature.placeholder_bytes)),
        );
        if let Some(reason) = &self.signature.reason {
            ur.insert("Reason".to_owned(), PdfValue::Text(reason.clone()));
        }
        let mut perms = BTreeMap::new();
        perms.insert("UR".to_owned(), PdfValue::Dictionary(ur));
        let mut root = BTreeMap::new();
        root.insert("Perms".to_owned(), PdfValue::Dictionary(perms));
        Ok(PdfValue::Dictionary(root))
    }

    /// Returns a deterministic, human-readable report of the plan.
    pub fn report(&self) -> RightsReport {
        let rights: Vec<String> = self
            .rights
            .iter()
            .map(|right| right.label().to_owned())
            .collect();
        RightsReport {
            rights,
            filter: self.signature.filter.clone(),
            placeholder_bytes: self.signature.placeholder_bytes,
            certified: false,
        }
    }

    /// Derives the fill-and-save policy from this plan's rights.
    pub const fn policy(&self) -> FillSavePolicy {
        FillSavePolicy::from_rights(self.rights)
    }

    /// Returns the fill decision after validating the plan.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError`] when [`ReaderEnablePlan::validate`] fails,
    /// so a malformed, foreign, or out-of-bounds plan never grants a decision.
    pub fn can_fill(&self) -> Result<Permission, ReaderEnableError> {
        self.validate()?;
        Ok(self.policy().can_fill())
    }

    /// Returns the save decision after validating the plan.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError`] when [`ReaderEnablePlan::validate`] fails.
    pub fn can_save(&self) -> Result<Permission, ReaderEnableError> {
        self.validate()?;
        Ok(self.policy().can_save())
    }

    /// Returns a deterministic fill/save policy report after validating.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError`] when [`ReaderEnablePlan::validate`] fails.
    pub fn policy_report(&self) -> Result<PolicyReport, ReaderEnableError> {
        self.validate()?;
        Ok(self.policy().report())
    }

    /// Parses and validates an existing `/Perms /UR` reader-enable dictionary.
    ///
    /// # Errors
    ///
    /// Returns [`ReaderEnableError`] when the dictionary is malformed, carries
    /// a foreign filter or non-placeholder contents, or grants unknown rights.
    pub fn from_dictionary(value: &PdfValue) -> Result<Self, ReaderEnableError> {
        parse_rights_dictionary(value)
    }
}

/// A deterministic report describing a [`ReaderEnablePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RightsReport {
    rights: Vec<String>,
    filter: String,
    placeholder_bytes: usize,
    certified: bool,
}

impl RightsReport {
    /// Returns the granted right labels in ascending bit order.
    pub fn rights(&self) -> &[String] {
        &self.rights
    }

    /// Returns the local filter name.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Returns the placeholder byte count.
    pub const fn placeholder_bytes(&self) -> usize {
        self.placeholder_bytes
    }

    /// Always `false`: no Adobe certification is implied.
    pub const fn certified(&self) -> bool {
        self.certified
    }

    /// Renders a stable, single-line summary.
    pub fn to_text(&self) -> String {
        format!(
            "reader-enable: rights=[{}] filter={} placeholder={} certified={}",
            self.rights.join(","),
            self.filter,
            self.placeholder_bytes,
            self.certified
        )
    }
}

/// An error produced while building or validating a reader-enable plan.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReaderEnableError {
    /// The bit mask requested a right this local model does not support.
    #[error("unsupported usage-right bits: 0x{bits:08x}")]
    UnsupportedRight {
        /// The rejected bit mask.
        bits: u32,
    },
    /// The filter name was empty.
    #[error("usage-rights filter must not be empty")]
    EmptyFilter,
    /// The filter name exceeded [`READER_MAX_TEXT_LEN`].
    #[error("usage-rights filter is too long ({len} > {max})")]
    FilterTooLong {
        /// The observed length.
        len: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// The reason string exceeded [`READER_MAX_TEXT_LEN`].
    #[error("usage-rights reason is too long ({len} > {max})")]
    ReasonTooLong {
        /// The observed length.
        len: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// The placeholder size was zero or exceeded [`READER_MAX_PLACEHOLDER_SIZE`].
    #[error("placeholder byte count {size} is outside 1..={max}")]
    InvalidPlaceholderSize {
        /// The requested size.
        size: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// An existing `/Perms /UR` dictionary was structurally malformed.
    #[error("malformed usage-rights dictionary: {detail}")]
    MalformedDictionary {
        /// A short, stable description of the structural problem.
        detail: &'static str,
    },
    /// The `/Ff` usage-rights flag was not a finite, non-negative integer.
    #[error("invalid /Ff usage-rights flag value: {value}")]
    InvalidFf {
        /// The rejected value, formatted for display.
        value: String,
    },
    /// The filter was not the PDFXI local filter, so a foreign certification
    /// claim is refused.
    #[error("foreign usage-rights filter rejected: {filter}")]
    NonLocalFilter {
        /// The foreign filter name.
        filter: String,
    },
    /// `/Contents` was not a local placeholder (a run of ASCII `0`).
    #[error("/Contents is not a local placeholder signature")]
    NonPlaceholderContent,
}

/// The outcome of a fill or save permission decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// The local policy allows the operation.
    Allowed,
    /// The local policy denies the operation.
    Denied,
}

impl Permission {
    /// Returns `true` for [`Permission::Allowed`].
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// A fill-and-save policy derived from a local [`DocumentRights`] set.
///
/// The policy carries three pieces of state: the set of allowed operations,
/// an explicit form-fill flag (set by [`UsageRight::FillIn`]), and an explicit
/// save flag (set by [`UsageRight::Save`]). It is a pure, deterministic
/// function of a rights set and performs no I/O or cryptography.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillSavePolicy {
    operations: DocumentRights,
    form_fill: bool,
    save: bool,
}

impl FillSavePolicy {
    /// Derives a fill-and-save policy from a rights set.
    pub const fn from_rights(rights: DocumentRights) -> Self {
        Self {
            form_fill: rights.contains(UsageRight::FillIn),
            save: rights.contains(UsageRight::Save),
            operations: rights,
        }
    }

    /// Returns the full set of allowed operations.
    pub const fn allowed_operations(&self) -> DocumentRights {
        self.operations
    }

    /// Returns the explicit form-fill flag.
    pub const fn form_fill_flag(&self) -> bool {
        self.form_fill
    }

    /// Returns the explicit save flag.
    pub const fn save_flag(&self) -> bool {
        self.save
    }

    /// Returns `true` when `right` is among the allowed operations.
    pub const fn allows(&self, right: UsageRight) -> bool {
        self.operations.contains(right)
    }

    /// Returns the form-fill decision.
    pub const fn can_fill(&self) -> Permission {
        if self.form_fill {
            Permission::Allowed
        } else {
            Permission::Denied
        }
    }

    /// Returns the save decision.
    pub const fn can_save(&self) -> Permission {
        if self.save {
            Permission::Allowed
        } else {
            Permission::Denied
        }
    }

    /// Renders a deterministic report of this policy.
    pub fn report(&self) -> PolicyReport {
        let operations: Vec<String> = self
            .operations
            .iter()
            .map(|right| right.label().to_owned())
            .collect();
        PolicyReport {
            can_fill: self.form_fill,
            can_save: self.save,
            operations,
            filter: LOCAL_FILTER.to_owned(),
            certified: false,
        }
    }
}

/// A deterministic report describing a [`FillSavePolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReport {
    can_fill: bool,
    can_save: bool,
    operations: Vec<String>,
    filter: String,
    certified: bool,
}

impl PolicyReport {
    /// Returns `true` when the form-fill decision is allowed.
    pub const fn can_fill(&self) -> bool {
        self.can_fill
    }

    /// Returns `true` when the save decision is allowed.
    pub const fn can_save(&self) -> bool {
        self.can_save
    }

    /// Returns the allowed operation labels in ascending bit order.
    pub fn operations(&self) -> &[String] {
        &self.operations
    }

    /// Returns the local filter name.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Always `false`: no Adobe certification is implied.
    pub const fn certified(&self) -> bool {
        self.certified
    }

    /// Renders a stable, single-line summary.
    pub fn to_text(&self) -> String {
        format!(
            "fill-save-policy: fill={} save={} operations=[{}] filter={} certified={}",
            self.can_fill,
            self.can_save,
            self.operations.join(","),
            self.filter,
            self.certified
        )
    }
}

/// Validates and parses an existing `/Perms /UR` reader-enable dictionary.
///
/// The returned [`ReaderEnablePlan`] reproduces the rights and placeholder
/// metadata found in `value`. Foreign filters, non-placeholder `/Contents`,
/// malformed structure, and unknown `/Ff` bits are rejected with typed errors.
/// Nothing is signed or verified cryptographically.
///
/// # Errors
///
/// Returns [`ReaderEnableError`] when the dictionary is malformed, carries a
/// foreign filter or non-placeholder contents, has an invalid `/Ff` value, or
/// grants rights this local model does not support.
pub fn parse_rights_dictionary(value: &PdfValue) -> Result<ReaderEnablePlan, ReaderEnableError> {
    let root = value
        .as_dict()
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "catalog must be a dictionary",
        })?;
    let perms = root
        .get("Perms")
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "missing /Perms",
        })?
        .as_dict()
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "/Perms must be a dictionary",
        })?;
    let ur = perms
        .get("UR")
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "missing /UR",
        })?
        .as_dict()
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "/UR must be a dictionary",
        })?;

    let ff_value = ur.get("Ff").ok_or(ReaderEnableError::MalformedDictionary {
        detail: "missing /Ff",
    })?;
    let ff_number = match ff_value {
        PdfValue::Number(number) => *number,
        _ => {
            return Err(ReaderEnableError::InvalidFf {
                value: "not a number".to_owned(),
            });
        }
    };
    if !ff_number.is_finite()
        || ff_number < 0.0
        || ff_number.fract() != 0.0
        || ff_number > u32::MAX as f64
    {
        return Err(ReaderEnableError::InvalidFf {
            value: ff_number.to_string(),
        });
    }
    let rights = DocumentRights::from_bits(ff_number as u32)?;

    let filter = ur
        .get("F")
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "missing /F",
        })?
        .as_name()
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "/F must be a name",
        })?
        .to_owned();

    let contents = ur
        .get("Contents")
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "missing /Contents",
        })?
        .as_text()
        .ok_or(ReaderEnableError::MalformedDictionary {
            detail: "/Contents must be a string",
        })?;
    if contents.is_empty() || !contents.bytes().all(|byte| byte == b'0') {
        return Err(ReaderEnableError::NonPlaceholderContent);
    }
    if contents.len() > READER_MAX_PLACEHOLDER_SIZE {
        return Err(ReaderEnableError::InvalidPlaceholderSize {
            size: contents.len(),
            max: READER_MAX_PLACEHOLDER_SIZE,
        });
    }

    let reason = match ur.get("Reason") {
        Some(value) => Some(
            value
                .as_text()
                .ok_or(ReaderEnableError::MalformedDictionary {
                    detail: "/Reason must be a string",
                })?
                .to_owned(),
        ),
        None => None,
    };

    let signature = UsageRightsSignature {
        filter,
        placeholder_bytes: contents.len(),
        reason,
    };
    let plan = ReaderEnablePlan { rights, signature };
    plan.validate()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_emits_local_usage_rights_metadata() {
        let rights = DocumentRights::new()
            .with(UsageRight::FillIn)
            .with(UsageRight::Save);
        let plan = ReaderEnablePlan::new(rights);
        let dict = plan.to_dictionary().unwrap();
        let perms = dict.as_dict().unwrap();
        let ur = perms.get("Perms").unwrap().as_dict().unwrap();
        let ur = ur.get("UR").unwrap().as_dict().unwrap();

        assert_eq!(ur.get("Ff"), Some(&PdfValue::Number(3.0)));
        assert_eq!(ur.get("F"), Some(&PdfValue::Name(LOCAL_FILTER.to_owned())));
        assert!(
            matches!(ur.get("Contents"), Some(PdfValue::Text(s)) if s.len() == READER_DEFAULT_PLACEHOLDER_SIZE)
        );
        assert!(!plan.is_certification());
    }

    #[test]
    fn report_is_deterministic_and_not_certified() {
        let plan = ReaderEnablePlan::new(
            DocumentRights::new()
                .with(UsageRight::Accessibility)
                .with(UsageRight::FillIn),
        )
        .with_signature(UsageRightsSignature::local().with_placeholder_bytes(8));
        let report = plan.report();
        assert_eq!(report.rights(), ["fill-in", "accessibility"]);
        assert!(!report.certified());
        assert_eq!(
            report.to_text(),
            "reader-enable: rights=[fill-in,accessibility] filter=PDFXI.ReaderEnable.Local placeholder=8 certified=false"
        );
        assert_eq!(plan.report(), report);
    }

    #[test]
    fn unsupported_right_bits_are_a_typed_error() {
        assert_eq!(
            DocumentRights::from_bits(1 << 20),
            Err(ReaderEnableError::UnsupportedRight { bits: 1 << 20 })
        );
        assert!(ReaderEnablePlan::from_bits(SUPPORTED_RIGHT_BITS).is_ok());
        assert!(matches!(
            ReaderEnablePlan::from_bits(u32::MAX),
            Err(ReaderEnableError::UnsupportedRight { .. })
        ));
    }

    #[test]
    fn invalid_placeholder_and_filter_are_typed_errors() {
        let empty = ReaderEnablePlan::new(DocumentRights::NONE)
            .with_signature(UsageRightsSignature::with_filter(""));
        assert_eq!(empty.validate(), Err(ReaderEnableError::EmptyFilter));

        let zero = ReaderEnablePlan::new(DocumentRights::NONE)
            .with_signature(UsageRightsSignature::local().with_placeholder_bytes(0));
        assert_eq!(
            zero.validate(),
            Err(ReaderEnableError::InvalidPlaceholderSize {
                size: 0,
                max: READER_MAX_PLACEHOLDER_SIZE
            })
        );

        let too_big = ReaderEnablePlan::new(DocumentRights::NONE).with_signature(
            UsageRightsSignature::local().with_placeholder_bytes(READER_MAX_PLACEHOLDER_SIZE + 1),
        );
        assert!(matches!(
            too_big.to_dictionary(),
            Err(ReaderEnableError::InvalidPlaceholderSize { .. })
        ));

        let long_reason = "r".repeat(READER_MAX_TEXT_LEN + 1);
        let reason = ReaderEnablePlan::new(DocumentRights::NONE)
            .with_signature(UsageRightsSignature::local().with_reason(long_reason));
        assert!(matches!(
            reason.validate(),
            Err(ReaderEnableError::ReasonTooLong { .. })
        ));
    }

    #[test]
    fn bit_round_trip_preserves_every_supported_right() {
        let rights = DocumentRights::from_bits(SUPPORTED_RIGHT_BITS).unwrap();
        for right in UsageRight::ALL {
            assert!(rights.contains(right));
        }
        assert_eq!(rights.bits(), SUPPORTED_RIGHT_BITS);
        assert_eq!(DocumentRights::NONE.iter().count(), 0);
    }

    fn dictionary_with(flags: f64, filter: &str, contents: &str) -> PdfValue {
        let mut ur = BTreeMap::new();
        ur.insert("Ff".to_owned(), PdfValue::Number(flags));
        ur.insert("F".to_owned(), PdfValue::Name(filter.to_owned()));
        ur.insert("Contents".to_owned(), PdfValue::Text(contents.to_owned()));
        let mut perms = BTreeMap::new();
        perms.insert("UR".to_owned(), PdfValue::Dictionary(ur));
        let mut root = BTreeMap::new();
        root.insert("Perms".to_owned(), PdfValue::Dictionary(perms));
        PdfValue::Dictionary(root)
    }

    #[test]
    fn rights_round_trip_through_dictionary() {
        let rights = DocumentRights::new()
            .with(UsageRight::FillIn)
            .with(UsageRight::Save)
            .with(UsageRight::Accessibility);
        let plan = ReaderEnablePlan::new(rights);
        let dict = plan.to_dictionary().unwrap();
        let parsed = parse_rights_dictionary(&dict).unwrap();
        assert_eq!(parsed.rights(), rights);
        assert_eq!(parsed.signature(), plan.signature());
        assert_eq!(parsed, plan);
    }

    #[test]
    fn can_fill_allowed_when_fill_in_granted() {
        let plan = ReaderEnablePlan::new(DocumentRights::new().with(UsageRight::FillIn));
        assert_eq!(plan.can_fill(), Ok(Permission::Allowed));
        assert!(plan.can_fill().unwrap().is_allowed());
    }

    #[test]
    fn can_fill_denied_when_fill_in_absent() {
        let plan = ReaderEnablePlan::new(DocumentRights::new().with(UsageRight::Save));
        assert_eq!(plan.can_fill(), Ok(Permission::Denied));
    }

    #[test]
    fn can_save_allowed_when_save_granted() {
        let plan = ReaderEnablePlan::new(DocumentRights::new().with(UsageRight::Save));
        assert_eq!(plan.can_save(), Ok(Permission::Allowed));
    }

    #[test]
    fn can_save_denied_when_save_absent() {
        let plan = ReaderEnablePlan::new(DocumentRights::new().with(UsageRight::FillIn));
        assert_eq!(plan.can_save(), Ok(Permission::Denied));
    }

    #[test]
    fn unknown_right_bits_in_dictionary_are_a_typed_error() {
        let unknown = 1 << 12;
        let dict = dictionary_with(
            f64::from(SUPPORTED_RIGHT_BITS | unknown),
            LOCAL_FILTER,
            "00",
        );
        assert_eq!(
            parse_rights_dictionary(&dict),
            Err(ReaderEnableError::UnsupportedRight {
                bits: SUPPORTED_RIGHT_BITS | unknown,
            })
        );
    }

    #[test]
    fn placeholder_contents_validated_on_parse() {
        let dict = dictionary_with(0.0, LOCAL_FILTER, "SIgned");
        assert_eq!(
            parse_rights_dictionary(&dict),
            Err(ReaderEnableError::NonPlaceholderContent)
        );
        let empty = dictionary_with(0.0, LOCAL_FILTER, "");
        assert_eq!(
            parse_rights_dictionary(&empty),
            Err(ReaderEnableError::NonPlaceholderContent)
        );
    }

    #[test]
    fn policy_report_is_deterministic() {
        let plan = ReaderEnablePlan::new(
            DocumentRights::new()
                .with(UsageRight::Accessibility)
                .with(UsageRight::FillIn),
        );
        let first = plan.policy_report().unwrap();
        let second = plan.policy_report().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.operations(), ["fill-in", "accessibility"]);
        assert!(first.can_fill());
        assert!(!first.can_save());
        assert!(!first.certified());
    }

    #[test]
    fn malformed_rights_dictionary_is_a_typed_error() {
        assert_eq!(
            parse_rights_dictionary(&PdfValue::Null),
            Err(ReaderEnableError::MalformedDictionary {
                detail: "catalog must be a dictionary",
            })
        );
        let empty = PdfValue::Dictionary(BTreeMap::new());
        assert_eq!(
            parse_rights_dictionary(&empty),
            Err(ReaderEnableError::MalformedDictionary {
                detail: "missing /Perms"
            })
        );
        let mut root = BTreeMap::new();
        root.insert("Perms".to_owned(), PdfValue::Dictionary(BTreeMap::new()));
        let no_ur = PdfValue::Dictionary(root);
        assert_eq!(
            parse_rights_dictionary(&no_ur),
            Err(ReaderEnableError::MalformedDictionary {
                detail: "missing /UR"
            })
        );
    }

    #[test]
    fn policy_report_wording_is_stable() {
        let plan = ReaderEnablePlan::new(
            DocumentRights::new()
                .with(UsageRight::FillIn)
                .with(UsageRight::Save),
        );
        assert_eq!(
            plan.policy_report().unwrap().to_text(),
            "fill-save-policy: fill=true save=true operations=[fill-in,save] filter=PDFXI.ReaderEnable.Local certified=false"
        );
    }

    #[test]
    fn foreign_filter_is_rejected_without_a_certification_claim() {
        let plan = ReaderEnablePlan::new(DocumentRights::NONE)
            .with_signature(UsageRightsSignature::with_filter("Adobe.ReaderEnable"));
        assert_eq!(
            plan.can_fill().unwrap_err(),
            ReaderEnableError::NonLocalFilter {
                filter: "Adobe.ReaderEnable".to_owned(),
            }
        );
        assert!(!plan.is_certification());
    }

    #[test]
    fn invalid_ff_flag_is_a_typed_error() {
        let negative = dictionary_with(-1.0, LOCAL_FILTER, "0");
        assert!(matches!(
            parse_rights_dictionary(&negative),
            Err(ReaderEnableError::InvalidFf { .. })
        ));
        let fractional = dictionary_with(1.5, LOCAL_FILTER, "0");
        assert!(matches!(
            parse_rights_dictionary(&fractional),
            Err(ReaderEnableError::InvalidFf { .. })
        ));
    }
}
