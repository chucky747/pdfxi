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
//! All inputs are bounded and deterministic; unsupported rights, empty or
//! over-long filters, and out-of-range placeholder sizes yield a typed
//! [`ReaderEnableError`].

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
    /// Returns [`ReaderEnableError`] for an empty or over-long filter, an
    /// over-long reason, or a placeholder size outside `1..=READER_MAX_PLACEHOLDER_SIZE`.
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
}
