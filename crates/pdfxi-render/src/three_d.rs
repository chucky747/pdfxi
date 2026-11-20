//! 3D annotation model (feature MEDIA-003).
//!
//! This module models the *data* of a PDF 3D annotation without decoding or
//! displaying any 3D content. It parses a small, owned intermediate dictionary
//! that mirrors the annotation's `/3D` stream reference, its collection of
//! named 3D views, the default-view metadata, the activation dictionary, and
//! the animation flag. It validates the structure with typed errors and bounds
//! every variable-length input, so hostile PDFs cannot make it allocate without
//! limit.
//!
//! # Backend limits (honest delegation)
//!
//! [`ThreeDAnnotation`] is a plan, not a renderer. It never decodes U3D or PRC
//! geometry, never opens a 3D graphics context, and never animates anything.
//! [`ThreeDAnnotation::resolve_plan`] produces a [`ThreeDPlan`] whose
//! [`ThreeDPlan::requirements`] state exactly which capabilities a native 3D
//! backend must supply. A stream whose format is not U3D or PRC is reported as
//! [`ThreeDFormat::Unknown`] rather than silently accepted: this crate does not
//! claim to view formats it cannot name.
//!
//! The operations here are pure and bounded. They perform no filesystem,
//! wall-clock, randomness or native-library access, add no dependency, and need
//! no cancellation token because a single parse or plan resolution is short.

/// Maximum number of named 3D views in a single annotation.
pub const MAX_VIEW_COUNT: usize = 64;
/// Maximum byte length of a view name or stream-reference key.
pub const MAX_NAME_BYTES: usize = 128;

/// Structured errors produced while parsing a 3D annotation dictionary.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ThreeDError {
    /// The `/3D` stream reference was empty.
    #[error("3D stream reference must not be empty")]
    EmptyStreamReference,
    /// A stream-reference key exceeded the byte-length cap.
    #[error("3D stream reference length {len} exceeds the {limit}-byte limit")]
    StreamReferenceTooLong {
        /// Actual byte length of the key.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// The annotation carried no 3D views.
    #[error("3D annotation must declare at least one view")]
    NoViews,
    /// The annotation declared more views than the cap allows.
    #[error("3D view count {count} exceeds the {limit}-view limit")]
    TooManyViews {
        /// Number of views supplied.
        count: usize,
        /// Enforced view limit.
        limit: usize,
    },
    /// A view name was empty.
    #[error("3D view name must not be empty")]
    EmptyViewName,
    /// A view name exceeded the byte-length cap.
    #[error("3D view name length {len} exceeds the {limit}-byte limit")]
    ViewNameTooLong {
        /// Actual byte length of the name.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// Two views shared the same name.
    #[error("duplicate 3D view name '{0}'")]
    DuplicateViewName(String),
    /// The named default view was not among the declared views.
    #[error("default 3D view '{0}' is not declared")]
    DefaultViewNotFound(String),
    /// The activation-event code was not recognised.
    #[error("unknown 3D activation event code '{0}'")]
    UnknownActivationEvent(String),
    /// The activation-state code was not recognised.
    #[error("unknown 3D activation state code '{0}'")]
    UnknownActivationState(String),
}

/// The event that causes a 3D annotation to become active.
///
/// The variants correspond to the PDF activation-event codes `PV`, `PI`, `PO`
/// and `PC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationEvent {
    /// The page containing the annotation becomes visible (`PV`).
    PageVisible,
    /// The page containing the annotation becomes invisible (`PI`).
    PageInvisible,
    /// The document is opened (`PO`).
    DocumentOpened,
    /// The document is closed (`PC`).
    DocumentClosed,
}

impl ActivationEvent {
    /// Parse a PDF activation-event code.
    ///
    /// Returns [`ThreeDError::UnknownActivationEvent`] for any other code.
    pub fn parse(code: &str) -> Result<Self, ThreeDError> {
        match code {
            "PV" => Ok(Self::PageVisible),
            "PI" => Ok(Self::PageInvisible),
            "PO" => Ok(Self::DocumentOpened),
            "PC" => Ok(Self::DocumentClosed),
            other => Err(ThreeDError::UnknownActivationEvent(other.to_owned())),
        }
    }

    /// Return the canonical PDF code for this event.
    pub const fn code(self) -> &'static str {
        match self {
            Self::PageVisible => "PV",
            Self::PageInvisible => "PI",
            Self::DocumentOpened => "PO",
            Self::DocumentClosed => "PC",
        }
    }
}

/// The activation state requested for a 3D annotation.
///
/// The variants correspond to the PDF activation-state codes `L`, `F` and `U`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivationState {
    /// The 3D content is live and fully instantiated (`L`).
    Live,
    /// The 3D content is instantiated and fitted to the annotation (`F`).
    Fit,
    /// The 3D content is uninstantiated and inert (`U`). This is the default.
    #[default]
    Uninstantiated,
}

impl ActivationState {
    /// Parse a PDF activation-state code.
    ///
    /// Returns [`ThreeDError::UnknownActivationState`] for any other code.
    pub fn parse(code: &str) -> Result<Self, ThreeDError> {
        match code {
            "L" => Ok(Self::Live),
            "F" => Ok(Self::Fit),
            "U" => Ok(Self::Uninstantiated),
            other => Err(ThreeDError::UnknownActivationState(other.to_owned())),
        }
    }

    /// Return the canonical PDF code for this state.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Live => "L",
            Self::Fit => "F",
            Self::Uninstantiated => "U",
        }
    }
}

/// The payload format advertised by a `/3D` stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreeDFormat {
    /// Universal 3D.
    U3d,
    /// PRC (Product Representation Compact).
    Prc,
    /// A format this crate does not recognise, kept verbatim for honest
    /// reporting to a backend.
    Unknown(String),
}

impl ThreeDFormat {
    /// Parse a format name. Comparison is case-insensitive; unknown names are
    /// preserved rather than rejected so a caller can report them honestly.
    pub fn parse(name: &str) -> Self {
        if name.eq_ignore_ascii_case("U3D") {
            Self::U3d
        } else if name.eq_ignore_ascii_case("PRC") {
            Self::Prc
        } else {
            Self::Unknown(name.to_owned())
        }
    }

    /// Return `true` only when the format is one this model names.
    pub fn is_known(&self) -> bool {
        matches!(self, Self::U3d | Self::Prc)
    }

    /// Return a stable token for this format, preserving unknown names as a
    /// prefixed, escaped value so they remain distinguishable and honest.
    pub fn code(&self) -> String {
        match self {
            Self::U3d => "u3d".to_owned(),
            Self::Prc => "prc".to_owned(),
            Self::Unknown(name) => {
                let mut out = String::from("unknown:");
                push_field(&mut out, name);
                out
            }
        }
    }
}

/// A typed reference to the `/3D` stream that carries the 3D payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreeDStreamRef {
    /// Reference by a bounded name key.
    Named(String),
    /// Reference by PDF object number and generation.
    Object {
        /// The indirect object number.
        number: u32,
        /// The indirect object generation.
        generation: u16,
    },
}

impl ThreeDStreamRef {
    /// Validate the reference's bounded inputs.
    fn validate(&self) -> Result<(), ThreeDError> {
        if let Self::Named(key) = self {
            if key.is_empty() {
                return Err(ThreeDError::EmptyStreamReference);
            }
            if key.len() > MAX_NAME_BYTES {
                return Err(ThreeDError::StreamReferenceTooLong {
                    len: key.len(),
                    limit: MAX_NAME_BYTES,
                });
            }
        }
        Ok(())
    }

    /// Return a byte-stable descriptor distinguishing named from object
    /// references, with the name escaped.
    pub fn descriptor(&self) -> String {
        match self {
            Self::Named(key) => {
                let mut out = String::from("named:");
                push_field(&mut out, key);
                out
            }
            Self::Object { number, generation } => format!("object:{number}:{generation}"),
        }
    }
}

/// How the active 3D view is selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewSelection {
    /// Use the view marked as default (the first view when none is marked).
    Default,
    /// Use the view with this name.
    Named(String),
    /// Use the view at this zero-based index (clamped when out of range).
    Indexed(usize),
}

/// One named 3D view declared by the annotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreeDView {
    name: String,
    is_default: bool,
}

impl ThreeDView {
    /// Return this view's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return `true` when this view is the annotation's default view.
    pub fn is_default(&self) -> bool {
        self.is_default
    }
}

/// Owned intermediate form of a 3D annotation dictionary.
///
/// Field names mirror the relevant PDF keys; this crate does not depend on a
/// PDF object parser, so a caller extracts these values and hands them here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreeDDictionary {
    /// The `/3D` stream reference.
    pub stream: ThreeDStreamRef,
    /// The stream's format name (`U3D`, `PRC`, or another name).
    pub format: String,
    /// Names of the declared 3D views, in document order.
    pub views: Vec<String>,
    /// The name of the default view, or `None` to use the first view.
    pub default_view: Option<String>,
    /// Activation-event code (`PV`, `PI`, `PO`, `PC`).
    pub activation_event: String,
    /// Activation-state code (`L`, `F`, `U`).
    pub activation_state: String,
    /// Whether the 3D content should animate once active.
    pub animate: bool,
}

/// A validated 3D annotation model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreeDAnnotation {
    stream: ThreeDStreamRef,
    format: ThreeDFormat,
    views: Vec<ThreeDView>,
    selection: ViewSelection,
    activation_event: ActivationEvent,
    activation_state: ActivationState,
    animate: bool,
}

impl ThreeDAnnotation {
    /// Validate and build a 3D annotation from an intermediate dictionary.
    pub fn parse(dict: ThreeDDictionary) -> Result<Self, ThreeDError> {
        dict.stream.validate()?;

        if dict.views.is_empty() {
            return Err(ThreeDError::NoViews);
        }
        if dict.views.len() > MAX_VIEW_COUNT {
            return Err(ThreeDError::TooManyViews {
                count: dict.views.len(),
                limit: MAX_VIEW_COUNT,
            });
        }

        let mut seen: Vec<&str> = Vec::with_capacity(dict.views.len());
        for name in &dict.views {
            validate_name(name)?;
            if seen.contains(&name.as_str()) {
                return Err(ThreeDError::DuplicateViewName(name.clone()));
            }
            seen.push(name.as_str());
        }

        let (selection, default_index) = match &dict.default_view {
            Some(name) => {
                validate_name(name)?;
                let index = dict
                    .views
                    .iter()
                    .position(|candidate| candidate == name)
                    .ok_or_else(|| ThreeDError::DefaultViewNotFound(name.clone()))?;
                (ViewSelection::Named(name.clone()), index)
            }
            None => (ViewSelection::Default, 0),
        };

        let views = dict
            .views
            .into_iter()
            .enumerate()
            .map(|(index, name)| ThreeDView {
                name,
                is_default: index == default_index,
            })
            .collect();

        Ok(Self {
            stream: dict.stream,
            format: ThreeDFormat::parse(&dict.format),
            views,
            selection,
            activation_event: ActivationEvent::parse(&dict.activation_event)?,
            activation_state: ActivationState::parse(&dict.activation_state)?,
            animate: dict.animate,
        })
    }

    /// Return the `/3D` stream reference.
    pub fn stream(&self) -> &ThreeDStreamRef {
        &self.stream
    }

    /// Return the advertised stream format.
    pub fn format(&self) -> &ThreeDFormat {
        &self.format
    }

    /// Return the declared views in document order.
    pub fn views(&self) -> &[ThreeDView] {
        &self.views
    }

    /// Return the configured view selection.
    pub fn selection(&self) -> &ViewSelection {
        &self.selection
    }

    /// Return the activation event.
    pub fn activation_event(&self) -> ActivationEvent {
        self.activation_event
    }

    /// Return the activation state.
    pub fn activation_state(&self) -> ActivationState {
        self.activation_state
    }

    /// Return the animation flag.
    pub fn animate(&self) -> bool {
        self.animate
    }

    /// Resolve the zero-based index of the active view.
    ///
    /// The result is deterministic and always in `0..views.len()`. Named and
    /// indexed selections were validated or clamped during parsing, so this
    /// method never fails.
    pub fn active_view_index(&self) -> usize {
        if self.views.is_empty() {
            return 0;
        }
        let last = self.views.len() - 1;
        match &self.selection {
            ViewSelection::Default => self
                .views
                .iter()
                .position(ThreeDView::is_default)
                .unwrap_or(0),
            ViewSelection::Named(name) => self
                .views
                .iter()
                .position(|view| view.name() == name)
                .unwrap_or(0),
            ViewSelection::Indexed(index) => (*index).min(last),
        }
    }

    /// Return the active view, or `None` only if the annotation has no views.
    pub fn active_view(&self) -> Option<&ThreeDView> {
        self.views.get(self.active_view_index())
    }

    /// Produce the typed backend plan described in the module documentation.
    pub fn resolve_plan(&self) -> ThreeDPlan {
        let mut requirements = vec![BackendRequirement::ThreeDDecoder(self.format.clone())];
        if self.animate {
            requirements.push(BackendRequirement::Animation);
        }
        if self.views.len() > 1 {
            requirements.push(BackendRequirement::ViewActivation);
        }
        ThreeDPlan {
            stream: self.stream.clone(),
            format: self.format.clone(),
            active_view_index: self.active_view_index(),
            activation_event: self.activation_event,
            activation_state: self.activation_state,
            animate: self.animate,
            requirements,
        }
    }
}

/// A capability a native 3D backend must provide to realise a [`ThreeDPlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendRequirement {
    /// Decode the payload in the given format.
    ThreeDDecoder(ThreeDFormat),
    /// Advance 3D animation frames while the annotation is active.
    Animation,
    /// Switch between the annotation's named views.
    ViewActivation,
}

impl BackendRequirement {
    /// Return a stable token for this requirement in a plan descriptor.
    pub fn code(&self) -> String {
        match self {
            Self::ThreeDDecoder(format) => format!("decoder:{}", format.code()),
            Self::Animation => "animation".to_owned(),
            Self::ViewActivation => "view-activation".to_owned(),
        }
    }
}

/// The deterministic plan a 3D backend must realise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreeDPlan {
    stream: ThreeDStreamRef,
    format: ThreeDFormat,
    active_view_index: usize,
    activation_event: ActivationEvent,
    activation_state: ActivationState,
    animate: bool,
    requirements: Vec<BackendRequirement>,
}

impl ThreeDPlan {
    /// Return the `/3D` stream reference to decode.
    pub fn stream(&self) -> &ThreeDStreamRef {
        &self.stream
    }

    /// Return the advertised stream format.
    pub fn format(&self) -> &ThreeDFormat {
        &self.format
    }

    /// Return the zero-based index of the view to show on activation.
    pub fn active_view_index(&self) -> usize {
        self.active_view_index
    }

    /// Return the activation event.
    pub fn activation_event(&self) -> ActivationEvent {
        self.activation_event
    }

    /// Return the activation state.
    pub fn activation_state(&self) -> ActivationState {
        self.activation_state
    }

    /// Return the animation flag.
    pub fn animate(&self) -> bool {
        self.animate
    }

    /// Return the capabilities a backend must supply, in stable order.
    pub fn requirements(&self) -> &[BackendRequirement] {
        &self.requirements
    }

    /// Return `true` when a native decoder is required at all.
    pub fn needs_backend(&self) -> bool {
        true
    }

    /// Return `true` when the payload format is one this model names.
    pub fn is_supported_format(&self) -> bool {
        self.format.is_known()
    }

    /// Return a byte-stable, single-line descriptor of this plan.
    ///
    /// Fields appear in a fixed order and text is escaped, so equal plans give
    /// an identical string. Unknown formats are preserved via
    /// [`ThreeDFormat::code`] rather than collapsed, keeping the report honest.
    pub fn descriptor(&self) -> String {
        let mut out = String::from("three-d-plan/v1|stream=");
        out.push_str(&self.stream.descriptor());
        out.push_str("|format=");
        out.push_str(&self.format.code());
        out.push_str("|view=");
        out.push_str(&self.active_view_index.to_string());
        out.push_str("|event=");
        out.push_str(self.activation_event.code());
        out.push_str("|state=");
        out.push_str(self.activation_state.code());
        out.push_str("|animate=");
        out.push_str(if self.animate { "1" } else { "0" });
        out.push_str("|requirements=");
        for (index, requirement) in self.requirements.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&requirement.code());
        }
        out
    }
}

/// Append `value` to `out`, escaping the descriptor's delimiter characters so
/// distinct field values can never produce the same descriptor string.
fn push_field(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            ',' => out.push_str("\\,"),
            _ => out.push(ch),
        }
    }
}

fn validate_name(name: &str) -> Result<(), ThreeDError> {
    if name.is_empty() {
        return Err(ThreeDError::EmptyViewName);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(ThreeDError::ViewNameTooLong {
            len: name.len(),
            limit: MAX_NAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
fn sample_dictionary() -> ThreeDDictionary {
    ThreeDDictionary {
        stream: ThreeDStreamRef::Named("CadModel".to_owned()),
        format: "U3D".to_owned(),
        views: vec!["front".to_owned(), "side".to_owned(), "top".to_owned()],
        default_view: Some("side".to_owned()),
        activation_event: "PV".to_owned(),
        activation_state: "L".to_owned(),
        animate: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_three_d_dictionary() {
        let annotation = ThreeDAnnotation::parse(sample_dictionary()).expect("valid dictionary");
        assert_eq!(
            annotation.stream(),
            &ThreeDStreamRef::Named("CadModel".to_owned())
        );
        assert_eq!(annotation.format(), &ThreeDFormat::U3d);
        assert_eq!(annotation.views().len(), 3);
        assert_eq!(annotation.activation_event(), ActivationEvent::PageVisible);
        assert_eq!(annotation.activation_state(), ActivationState::Live);
        assert!(annotation.animate());
    }

    #[test]
    fn view_selection_resolves_named_and_default_views() {
        let named = ThreeDAnnotation::parse(sample_dictionary()).expect("named selection");
        assert_eq!(named.selection(), &ViewSelection::Named("side".to_owned()));
        assert_eq!(named.active_view_index(), 1);
        assert_eq!(named.active_view().map(ThreeDView::name), Some("side"));
        assert!(named.views()[1].is_default());
        assert!(!named.views()[0].is_default());

        let mut default_dict = sample_dictionary();
        default_dict.default_view = None;
        let default = ThreeDAnnotation::parse(default_dict).expect("default selection");
        assert_eq!(default.selection(), &ViewSelection::Default);
        assert_eq!(default.active_view_index(), 0);
        assert_eq!(default.active_view().map(ThreeDView::name), Some("front"));
    }

    #[test]
    fn indexed_selection_is_clamped_to_the_view_range() {
        let mut annotation = ThreeDAnnotation::parse(sample_dictionary()).expect("valid");
        // The field is private in tests of this module, so build one directly.
        annotation.selection = ViewSelection::Indexed(99);
        assert_eq!(annotation.active_view_index(), 2);
        annotation.selection = ViewSelection::Indexed(0);
        assert_eq!(annotation.active_view_index(), 0);
    }

    #[test]
    fn invalid_activation_event_and_state_are_typed_errors() {
        let mut dict = sample_dictionary();
        dict.activation_event = "ZZ".to_owned();
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::UnknownActivationEvent("ZZ".to_owned()))
        );

        let mut dict = sample_dictionary();
        dict.activation_state = "Q".to_owned();
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::UnknownActivationState("Q".to_owned()))
        );
    }

    #[test]
    fn every_activation_code_round_trips() {
        for (code, event) in [
            ("PV", ActivationEvent::PageVisible),
            ("PI", ActivationEvent::PageInvisible),
            ("PO", ActivationEvent::DocumentOpened),
            ("PC", ActivationEvent::DocumentClosed),
        ] {
            assert_eq!(ActivationEvent::parse(code), Ok(event));
            assert_eq!(event.code(), code);
        }
        for (code, state) in [
            ("L", ActivationState::Live),
            ("F", ActivationState::Fit),
            ("U", ActivationState::Uninstantiated),
        ] {
            assert_eq!(ActivationState::parse(code), Ok(state));
            assert_eq!(state.code(), code);
        }
        assert_eq!(ActivationState::default(), ActivationState::Uninstantiated);
    }

    #[test]
    fn bounded_input_is_enforced() {
        let mut dict = sample_dictionary();
        dict.views = vec!["v".to_owned(); MAX_VIEW_COUNT + 1];
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::TooManyViews {
                count: MAX_VIEW_COUNT + 1,
                limit: MAX_VIEW_COUNT
            })
        );

        let mut dict = sample_dictionary();
        dict.views = vec!["dup".to_owned(), "dup".to_owned()];
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::DuplicateViewName("dup".to_owned()))
        );

        let mut dict = sample_dictionary();
        dict.views = vec!["".to_owned()];
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::EmptyViewName)
        );

        let mut dict = sample_dictionary();
        dict.views = vec!["x".repeat(MAX_NAME_BYTES + 1)];
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::ViewNameTooLong {
                len: MAX_NAME_BYTES + 1,
                limit: MAX_NAME_BYTES
            })
        );

        let mut dict = sample_dictionary();
        dict.stream = ThreeDStreamRef::Named(String::new());
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::EmptyStreamReference)
        );

        let mut dict = sample_dictionary();
        dict.stream = ThreeDStreamRef::Named("s".repeat(MAX_NAME_BYTES + 1));
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::StreamReferenceTooLong {
                len: MAX_NAME_BYTES + 1,
                limit: MAX_NAME_BYTES
            })
        );
    }

    #[test]
    fn empty_views_and_missing_default_are_typed_errors() {
        let mut dict = sample_dictionary();
        dict.views.clear();
        assert_eq!(ThreeDAnnotation::parse(dict), Err(ThreeDError::NoViews));

        let mut dict = sample_dictionary();
        dict.default_view = Some("missing".to_owned());
        assert_eq!(
            ThreeDAnnotation::parse(dict),
            Err(ThreeDError::DefaultViewNotFound("missing".to_owned()))
        );
    }

    #[test]
    fn object_stream_reference_is_accepted_without_a_name() {
        let mut dict = sample_dictionary();
        dict.stream = ThreeDStreamRef::Object {
            number: 42,
            generation: 0,
        };
        let annotation = ThreeDAnnotation::parse(dict).expect("object reference");
        assert_eq!(
            annotation.stream(),
            &ThreeDStreamRef::Object {
                number: 42,
                generation: 0
            }
        );
    }

    #[test]
    fn unknown_format_is_reported_honestly() {
        let mut dict = sample_dictionary();
        dict.format = "GLTF".to_owned();
        let annotation = ThreeDAnnotation::parse(dict).expect("unknown format is kept");
        assert_eq!(
            annotation.format(),
            &ThreeDFormat::Unknown("GLTF".to_owned())
        );
        let plan = annotation.resolve_plan();
        assert!(!plan.is_supported_format());
        assert!(plan.needs_backend());
        assert_eq!(
            plan.requirements(),
            &[
                BackendRequirement::ThreeDDecoder(ThreeDFormat::Unknown("GLTF".to_owned())),
                BackendRequirement::Animation,
                BackendRequirement::ViewActivation,
            ]
        );
    }

    #[test]
    fn plan_is_deterministic_and_omits_unneeded_requirements() {
        let annotation = ThreeDAnnotation::parse(sample_dictionary()).expect("valid");
        assert_eq!(annotation.resolve_plan(), annotation.resolve_plan());
        let plan = annotation.resolve_plan();
        assert!(plan.is_supported_format());
        assert_eq!(plan.active_view_index(), 1);
        assert!(plan.requirements().contains(&BackendRequirement::Animation));
        assert!(
            plan.requirements()
                .contains(&BackendRequirement::ViewActivation)
        );

        let mut quiet = sample_dictionary();
        quiet.views = vec!["only".to_owned()];
        quiet.default_view = None;
        quiet.animate = false;
        let quiet_plan = ThreeDAnnotation::parse(quiet)
            .expect("valid")
            .resolve_plan();
        assert_eq!(
            quiet_plan.requirements(),
            &[BackendRequirement::ThreeDDecoder(ThreeDFormat::U3d)]
        );
    }

    #[test]
    fn plan_descriptor_is_byte_stable_and_reflects_activation_policy() {
        let annotation = ThreeDAnnotation::parse(sample_dictionary()).expect("valid");
        let plan = annotation.resolve_plan();
        let descriptor = plan.descriptor();
        assert_eq!(descriptor, plan.descriptor());
        assert_eq!(descriptor, annotation.resolve_plan().descriptor());
        assert!(descriptor.starts_with("three-d-plan/v1|stream=named:CadModel|format=u3d|"));
        assert!(descriptor.contains("|view=1|event=PV|state=L|animate=1|"));
        assert!(descriptor.contains("decoder:u3d,animation,view-activation"));

        let mut dict = sample_dictionary();
        dict.activation_event = "PC".to_owned();
        dict.activation_state = "U".to_owned();
        dict.animate = false;
        dict.views = vec!["only".to_owned()];
        dict.default_view = None;
        let changed = ThreeDAnnotation::parse(dict)
            .expect("valid")
            .resolve_plan()
            .descriptor();
        assert_ne!(changed, descriptor);
        assert!(changed.contains("|view=0|event=PC|state=U|animate=0|"));
        assert!(changed.ends_with("decoder:u3d"));
    }

    #[test]
    fn unknown_format_descriptor_preserves_the_name() {
        let mut dict = sample_dictionary();
        dict.format = "GLTF".to_owned();
        dict.stream = ThreeDStreamRef::Object {
            number: 7,
            generation: 2,
        };
        let descriptor = ThreeDAnnotation::parse(dict)
            .expect("unknown format kept")
            .resolve_plan()
            .descriptor();
        assert!(descriptor.contains("|stream=object:7:2|"));
        assert!(descriptor.contains("|format=unknown:GLTF|"));
        assert!(descriptor.contains("decoder:unknown:GLTF"));
    }
}
