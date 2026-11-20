//! AcroForm/XFA model, fields, calculations, validation, FDF/XFDF, and flattening.
//!
//! This crate provides a dependency-free, in-memory model of an interactive PDF
//! form. The core abstractions are:
//!
//! - [`Field`] — a single widget with a type, flags, current value, default
//!   value, choice options, and optional text constraints.
//! - [`Form`] — an ordered, uniquely named collection of fields with lookup,
//!   value mutation, reset, and validation.
//! - [`Validation`] / [`ValidationError`] — the side-effect-free result of
//!   checking every field, including missing-required, invalid-choice,
//!   invalid-value-type, and invalid-text problems.
//! - [`FormFill`] / [`FillReport`] — a filling session that records net field
//!   changes in deterministic, name-sorted order.
//! - [`FillDocument`] / [`FillWorkflow`] / [`FilledDocumentPlan`] — a headless,
//!   end-to-end fill pipeline: load a parsed document model, write validated
//!   values, regenerate appearances, build a deterministic plan, serialize it
//!   with the T-0140 serializer and parse it back to verify filled `/V` values.
//!   Signature fields accept only settled workflow transitions (no bytes are
//!   written); cryptographic signing stays in `pdfxi-security`.
//! - [`FieldSpec`] / [`acroform_dictionary`] — typed creation of text,
//!   checkbox, radio, choice, pushbutton and signature fields, with widget
//!   rectangles, page association and well-formed AcroForm/widget dictionary
//!   models (page references are placeholders; radio groups use one widget).
//! - [`Validator`] / [`Rule`] / [`Format`] / [`Pattern`] — configurable
//!   validation (required, max length, numeric/currency/date, pattern, choice
//!   membership and custom [`ValidationHook`]s) with a deterministic
//!   [`ValidationSummary`].
//! - [`Calculation`] / [`CalculationOp`] — sum, product, average, min and max
//!   of named numeric fields with deterministic rounding.
//! - [`Arithmetic`] / [`CalculationPlan`] — bounded simplified arithmetic and
//!   order-independent dependency evaluation with cycle detection.
//! - [`export_fdf`] / [`import_fdf`] — round-trippable FDF 1.7 field data, plus
//!   a small [`export_xfdf`] XML subset.
//! - [`parse_xfdf`] / [`import_xfdf`] — a bounded XML reader for XFDF field data
//!   that maps names and values onto the target [`Form`]'s field types.
//! - [`read_acroform`] / [`set_text`] / [`set_checkbox`] / [`set_radio`] /
//!   [`set_choice`] — read and fill a real `pdfxi_core::Document` directly,
//!   resolving inherited `/FT`, `/T`, `/V` and `/Ff`, regenerating `/AP /N`
//!   appearances, exporting and importing XFDF, and flattening widgets into
//!   page content.
//! - [`load_acroform`] — loads a real PDF catalog `/AcroForm` dictionary into a
//!   [`Form`] through the dependency-free [`FormSource`] adapter trait.
//! - [`fill_acroform`] — applies [`FieldAssignment`]s to a concrete `/AcroForm`
//!   dictionary model, writing `/V`, `/DV`, widget `/AS` and regenerated
//!   `/AP /N` streams with a deterministic [`AcroFillReport`].
//! - [`build_appearance`] — deterministically generates a widget's appearance
//!   content stream and `/AP` dictionary model from a [`Field`], a
//!   [`WidgetAppearance`] and the current [`FieldValue`].
//! - [`flatten_page`] / [`flatten_document`] — bake widget appearances into
//!   page content, remove widget annotations, and preserve everything else.
//! - [`SignatureField`] / [`SignatureWorkflow`] / [`SignatureSlot`] — signature
//!   field creation with `/V` placeholders, locks and certification; a
//!   deterministic local signing state machine with stable status reports; and
//!   a byte-range planning boundary for a future security-layer signer. No
//!   cryptography is implemented here.
//! - [`plan_byte_range`] / [`validate_byte_range`] / [`signed_bytes`] — a
//!   deterministic `/ByteRange` contract computed from serialized bytes with a
//!   `/Contents` placeholder, plus the exact byte sequence a signer must cover.
//! - [`SignatureVerifier`] / [`StructureOnlyVerifier`] / [`SignatureLifecycle`] —
//!   a caller-supplied verification boundary (no cryptography in this crate)
//!   and a `create -> sign-request -> attach bytes -> verify -> report` state
//!   machine whose `/V` output carries caller-supplied bytes verbatim.
//! - [`TabPolicy`] / [`TabOrder`] / [`TabNavigator`] — the AcroForm `/Tabs`
//!   order (`/R`, `/C`, `/S`, `/A`), deterministic widget ordering by page, and
//!   next/previous traversal with a configurable hidden/read-only skip policy.
//! - [`ActionTrigger`] / [`SafeAction`] / [`ActionPlan`] — parsing of field
//!   `/AA` (K/F/V/C) and `/A` actions into a typed safe whitelist, with
//!   dangerous actions (JavaScript, URI, Launch, Import/Export) rejected and
//!   reported; no script is ever executed.
//! - [`serialize_form_pdf`] / [`parse_pdf`] — serialize emitted field/widget
//!   dictionaries and a page reference into a complete, loadable PDF byte
//!   stream with `/AcroForm /Fields`, page `/Annots` and corrected `/P`
//!   references, plus a bounded reader for round-trip verification.
//! - [`FieldProperties`] / [`PropertySet`] — a deterministic, name-sorted
//!   property model that validates required, max-length, numeric/currency/date,
//!   choice-membership, flag-consistency and name/tooltip constraints into the
//!   existing [`ValidationSummary`] without a UI.
//! - [`FormHint`] / [`RecognitionPlan`] / [`ConversionPlan`] — heuristic
//!   recognition of static form areas from caller-supplied label/control
//!   rectangles (no OCR), with confidence, deterministic reading order and a
//!   typed conversion into AcroForm fields.
//! - [`ReaderEnablePlan`] / [`DocumentRights`] / [`UsageRightsSignature`] — a
//!   local reader-enable compatibility metadata plan with a placeholder usage
//!   rights signature. [`FillSavePolicy`] derives `can_fill`/`can_save`
//!   decisions, [`parse_rights_dictionary`] validates existing `/Perms /UR`
//!   metadata, and [`PolicyReport`] renders a deterministic summary. This is
//!   explicitly not Adobe certification and performs no cryptography.
//! - [`BlankBundle`] / [`ResponseFile`] / [`MergeReport`] — local distribute
//!   and collect: a canonical blank-bundle export, in-memory FDF/XFDF response
//!   import, deterministic merge with conflict reporting and a status summary.
//!   No filesystem or network I/O is performed.
//!
//! Field names are fully qualified (for example `order.address.city`). Callers
//! that build fields directly supply the dotted path; [`load_acroform`] builds
//! it from the inherited `/T` partial names of a real field tree using `.` as
//! the separator. All operations are deterministic and allocate only as needed.
//!
//! # Example
//!
//! ```text
//! use pdfxi_forms::{Calculation, CalculationOp, Field, FieldValue, Form, FormFill, FieldType};
//!
//! let mut form = Form::from_fields([
//!     Field::new("a", FieldType::Text).with_value(FieldValue::number(2.0)),
//!     Field::new("b", FieldType::Text).with_value(FieldValue::number(3.0)),
//!     Field::new("total", FieldType::Text),
//! ]).unwrap();
//! Calculation::new("total", CalculationOp::Sum, ["a", "b"])
//!     .apply(&mut form).unwrap();
//! assert_eq!(form.value("total"), Ok(&FieldValue::number(5.0)));
//!
//! let mut fill = FormFill::new(form);
//! fill.set("a", FieldValue::number(10.0)).unwrap();
//! assert_eq!(fill.changes().len(), 1);
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod acrofill;
mod acroform;
mod actions;
mod appearance;
mod build;
mod calculation;
mod distribution;

/// Reading and filling a real `pdfxi_core::Document`.
///
/// The module keeps the explicit constructors [`document_form::read_acroform`],
/// [`document_form::set_text`], [`document_form::export_xfdf`] and
/// [`document_form::import_xfdf`]. The crate root re-exports the setter and
/// model names directly and exposes the two XFDF entry points under
/// `*_document` names because [`export_xfdf`] and [`import_xfdf`] already denote
/// the dependency-free [`Form`] API.
pub mod document_form;
mod fdf;
mod field;
mod fill;
mod fillworkflow;
mod flatten;
mod form;
mod properties;
mod propertyedit;
mod reader_enable;
mod recognition;
mod rules;
mod serialize;
mod signature;
mod signatureplan;
mod signing;
mod taborder;
mod validation;
mod xfdf;

pub use acrofill::{
    AcroFillError, AcroFillReport, FieldAssignment, FilledField, RejectedField, SkipReason,
    SkippedField, fill_acroform,
};
pub use acroform::{
    AcroFormLoad, FormSource, LoadError, LoadedFieldInfo, LoadedForm, ObjectRef, PdfValue,
    load_acroform,
};
pub use actions::{
    ActionCollector, ActionError, ActionPlan, ActionReport, ActionSite, ActionTrigger,
    FieldActions, PlannedAction, SafeAction, UnsupportedAction, parse_action, parse_field_actions,
};
pub use appearance::{
    AVERAGE_WIDTH, Alignment, AppearanceDictionary, AppearanceError, AppearanceStream, BorderStyle,
    DEFAULT_FONT_RESOURCE, FontSpec, HORIZONTAL_PADDING, LINE_HEIGHT_FACTOR, NARROW_WIDTH,
    NormalAppearance, RgbColor, STATE_OFF, STATE_ON, TextLayout, TextLine, VERTICAL_PADDING,
    WIDE_WIDTH, WidgetAppearance, WidgetRect, build_appearance, button_state_names,
    check_mark_content, escape_literal_string, format_number, layout_text,
};
pub use build::{CreationError, FieldSpec, MAX_OPTIONS, acroform_dictionary};
pub use calculation::{
    Arithmetic, ArithmeticCalculation, Calculation, CalculationError, CalculationOp,
    CalculationPlan, CalculationReport, CalculationResult, CalculationStep, MAX_PLAN_STEPS,
    run_calculations,
};
pub use distribution::{
    BlankBundle, BundleField, BundleMeta, CollectionSummary, Contribution, DistributionError,
    FieldConflict, ImportedResponse, MAX_CONTENT_BYTES, MAX_FIELDS, MAX_RESPONSES, MAX_TEXT_LEN,
    MergeReport, MergedField, RejectedResponse, ResponseFile, ResponseFormat, import_response_file,
    merge_responses, parse_bundle,
};
pub use document_form::{
    AcroFormError, AcroFormModel, FlattenFormReport, FormField, FormFieldKind, FormRect,
    FormWidget, export_xfdf as export_xfdf_document, flatten, import_xfdf as import_xfdf_document,
    needs_appearances, read_acroform, regenerate_appearances, set_checkbox, set_choice, set_radio,
    set_text,
};
pub use fdf::{FdfDocument, FdfError, FdfField, export_fdf, export_xfdf, import_fdf};
pub use field::{Field, FieldFlags, FieldType, FieldValue, TextConstraint, ValueKind};
pub use fill::{ChangedField, FillReport, FormFill};
pub use fillworkflow::{
    FillDocument, FillWorkflow, FillWorkflowError, FilledDocument, FilledDocumentPlan,
    SignaturePlanRecord, Verification,
};
pub use flatten::{
    AppearancePolicy, FlatDocument, FlatPage, FlattenError, FlattenedDocument, FlattenedPage,
    PageAnnotation, StoredAppearance, flatten_document, flatten_page,
};
pub use form::{Form, FormError};
pub use properties::{FieldProperties, MAX_NAME_LEN, MAX_TOOLTIP_LEN, PropertyError, PropertySet};
pub use propertyedit::{
    EditError, EditOperation, EditPlan, EditReport, EditSnapshot, FieldDictionarySet,
    PropertyDescriptor, PropertyEdit, PropertyEditor, PropertyKey, PropertyValue,
    applicable_properties,
};
pub use reader_enable::{
    DocumentRights, FillSavePolicy, LOCAL_FILTER, Permission, PolicyReport,
    READER_DEFAULT_PLACEHOLDER_SIZE, READER_MAX_PLACEHOLDER_SIZE, READER_MAX_TEXT_LEN,
    ReaderEnableError, ReaderEnablePlan, RightsReport, SUPPORTED_RIGHT_BITS, UsageRight,
    UsageRightsSignature, parse_rights_dictionary,
};
pub use recognition::{
    ConversionPlan, FormHint, HintKind, LABEL_PROXIMITY, MAX_HINTS, RECOGNITION_MAX_TEXT_LEN,
    RecognitionError, RecognitionPlan, RecognizedField, recognize,
};
pub use rules::{
    CurrencyFormat, DateLayout, Format, MAX_PATTERN_LEN, MAX_PATTERN_TEXT_LEN, Pattern,
    PatternError, Rule, ValidationHook, Validator,
};
pub use serialize::{
    MAX_OBJECT_NUMBER, MAX_OBJECTS, ParseError, PdfDocument, SerializeError, parse_pdf,
    serialize_form_pdf,
};
pub use signature::{
    ByteRangePlan, CertPermission, Certification, DEFAULT_FILTER, DEFAULT_PLACEHOLDER_SIZE,
    DEFAULT_SUBFILTER, LockAction, LockScope, MAX_PLACEHOLDER_SIZE, Participant, ParticipantRole,
    SigFilter, SignatureDictionary, SignatureError, SignatureField, SignatureGuard,
    SignatureGuardError, SignatureLock, SignatureSlot, SignatureStatus, SignatureWorkflow,
    SubFilter, Transition, TransitionKind, WorkflowError, WorkflowReport,
};
pub use signatureplan::{
    ContractError, PlaceholderContract, SignatureFieldPlanEntry, SignaturePlan, SignaturePlanError,
    SignaturePlanReport, workflow_report_lines,
};
pub use signing::{
    Authenticity, ByteRangeError, LifecycleError, LifecycleReport, SignatureLifecycle,
    SignatureVerifier, StructureOnlyVerifier, VerificationError, VerificationOutcome,
    VerificationRequest, plan_byte_range, signed_bytes, validate_byte_range,
    workflow_transition_lines,
};
pub use taborder::{
    NavPolicy, TabNavigator, TabOrder, TabPolicy, TabWidget, parse_tabs, tabs_from_acroform,
};
pub use validation::{Validation, ValidationError, ValidationSummary};
pub use xfdf::{XFDF_NAMESPACE, XfdfDocument, XfdfError, XfdfField, import_xfdf, parse_xfdf};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-forms"
}
