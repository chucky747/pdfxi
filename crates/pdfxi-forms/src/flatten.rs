//! Form flattening: bake widget appearances into page content.
//!
//! Flattening turns an interactive form into static page content. For every
//! widget annotation on a page this module selects an appearance stream, wraps
//! it in a translation to the widget's page rectangle, appends the result to the
//! page content, and removes the widget annotation. All other annotations are
//! preserved verbatim, as is the original page content.
//!
//! # Appearance-source policy
//!
//! [`AppearancePolicy`] controls where the drawn content comes from:
//!
//! - [`AppearancePolicy::PreferStored`] (the default) uses a widget's own
//!   normal `/AP /N` stream when one is supplied, and otherwise **falls back**
//!   to [`build_appearance`] using the current field value. This preserves a
//!   conforming producer's exact appearance while still handling forms whose
//!   appearance streams are missing.
//! - [`AppearancePolicy::Generate`] always calls [`build_appearance`], ignoring
//!   any stored stream. Use this when a stored appearance is known to be stale.
//! - [`AppearancePolicy::StoredOnly`] uses stored streams exclusively. A widget
//!   without one is a hard error ([`FlattenError::MissingStoredAppearance`])
//!   rather than a silent visual loss.
//!
//! For a stateful (checkbox/radio) stored appearance, the active stream is
//! chosen by the stored `/AS` name first, then by the state implied by the
//! field's current value, and finally by the sole state when only one exists.
//! If none resolve, [`FlattenError::MissingAppearanceState`] is returned.
//!
//! # Placement and fallback limits
//!
//! Appearance content is expressed in the widget's local box `(0, 0, w, h)`,
//! exactly as produced by [`crate::build_appearance`]. Flattening emits
//! `q 1 0 0 1 x y cm … Q` so the same bytes land at the widget's page position;
//! widget rotation and arbitrary `/Matrix` values are not modelled by this
//! dependency-free boundary and are left to a host adapter. When an appearance
//! stream is empty, the widget is still removed but no content is appended,
//! so a form with no paintable widgets is a true no-op.
//!
//! The model is intentionally small and fully deterministic: widgets are
//! processed in annotation order and numbers are formatted with
//! [`format_number`], so equal inputs always yield equal bytes.

use crate::appearance::{STATE_OFF, STATE_ON, WidgetAppearance, build_appearance, format_number};
use crate::field::{Field, FieldType};
use crate::form::Form;

use crate::appearance::{AppearanceError, NormalAppearance};

/// A widget's stored normal appearance, as a host adapter would supply it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAppearance {
    /// The `/N` normal appearance: one stream or named states.
    pub normal: NormalAppearance,
    /// The widget's `/AS` active state name, when present.
    pub active_state: Option<String>,
}

impl StoredAppearance {
    /// Builds a stored appearance from its `/N` and `/AS` entries.
    pub fn new(normal: NormalAppearance, active_state: Option<String>) -> Self {
        Self {
            normal,
            active_state,
        }
    }
}

/// One annotation on a flattened page.
#[derive(Debug, Clone, PartialEq)]
pub enum PageAnnotation {
    /// A form widget annotation, identified by its fully qualified field name.
    Widget {
        /// Fully qualified name of the backing field.
        field: String,
        /// Widget rectangle and visual styling.
        appearance: WidgetAppearance,
        /// Optional stored `/AP` stream used by the stored appearance policies.
        stored: Option<StoredAppearance>,
    },
    /// Any non-widget annotation, preserved byte-for-byte.
    Other {
        /// The annotation `/Subtype` name (for example `Link`).
        subtype: String,
        /// Opaque annotation data supplied by the host.
        data: Vec<u8>,
    },
}

/// A page before flattening.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlatPage {
    /// Existing page content stream bytes.
    pub content: Vec<u8>,
    /// Annotations on the page, including widget annotations.
    pub annotations: Vec<PageAnnotation>,
}

/// A page after flattening.
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedPage {
    /// Page content with widget appearances appended.
    pub content: Vec<u8>,
    /// Annotations with widget annotations removed.
    pub annotations: Vec<PageAnnotation>,
    /// Number of widget annotations that were flattened.
    pub flattened_widgets: usize,
}

impl FlattenedPage {
    /// Returns `true` when at least one widget was flattened on this page.
    pub fn changed(&self) -> bool {
        self.flattened_widgets > 0
    }
}

/// A document before flattening.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlatDocument {
    /// Pages in document order.
    pub pages: Vec<FlatPage>,
    /// Whether the document catalog carries an interactive `/AcroForm`.
    pub has_acroform: bool,
}

/// A document after flattening.
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedDocument {
    /// Flattened pages in document order.
    pub pages: Vec<FlattenedPage>,
    /// Whether the interactive `/AcroForm` dictionary should be removed.
    pub acroform_removed: bool,
    /// Total number of flattened widget annotations across all pages.
    pub flattened_widgets: usize,
}

impl FlattenedDocument {
    /// Returns `true` when the operation changed any page or removed the form.
    pub fn changed(&self) -> bool {
        self.acroform_removed || self.pages.iter().any(|page| page.changed())
    }
}

/// Where a flattened widget's drawn content comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppearancePolicy {
    /// Use a stored `/AP /N` when present; generate from the field value
    /// otherwise. This is the default and the safe, loss-tolerant choice.
    #[default]
    PreferStored,
    /// Always regenerate the appearance from the current field value.
    Generate,
    /// Only use stored appearances; fail when a widget has none.
    StoredOnly,
}

/// A structured error produced while flattening a form.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FlattenError {
    /// A widget referenced a field that is not in the form.
    #[error("widget references unknown field '{name}'")]
    UnknownField {
        /// Fully qualified name of the missing field.
        name: String,
    },
    /// `StoredOnly` policy met a widget with no stored appearance.
    #[error("field '{field}' has no stored appearance and StoredOnly forbids generation")]
    MissingStoredAppearance {
        /// Fully qualified name of the field.
        field: String,
    },
    /// No appearance state could be selected for a stateful widget.
    #[error("field '{field}' has no appearance state named '{state}'")]
    MissingAppearanceState {
        /// Fully qualified name of the field.
        field: String,
        /// The state name that could not be found.
        state: String,
    },
    /// Generating a fallback appearance failed.
    #[error("cannot generate appearance for field '{field}': {source}")]
    Appearance {
        /// Fully qualified name of the field.
        field: String,
        /// The underlying appearance error.
        #[source]
        source: AppearanceError,
    },
}

/// Flattens one page: appends widget appearances to content and drops widgets.
///
/// Widgets are processed in annotation order. Non-widget annotations and the
/// original content are copied unchanged; when there are no widgets the page is
/// bit-for-bit identical to the input.
///
/// # Errors
///
/// Returns [`FlattenError`] for a widget with an unknown field, a missing
/// stored appearance under [`AppearancePolicy::StoredOnly`], an unresolvable
/// appearance state, or an invalid fallback appearance geometry.
pub fn flatten_page(
    page: &FlatPage,
    form: &Form,
    policy: AppearancePolicy,
) -> Result<FlattenedPage, FlattenError> {
    let mut added = Vec::new();
    let mut annotations = Vec::with_capacity(page.annotations.len());
    let mut flattened_widgets = 0;

    for annotation in &page.annotations {
        match annotation {
            PageAnnotation::Other { .. } => annotations.push(annotation.clone()),
            PageAnnotation::Widget {
                field,
                appearance,
                stored,
            } => {
                let model = form
                    .field(field)
                    .ok_or_else(|| FlattenError::UnknownField {
                        name: field.clone(),
                    })?;
                let content = widget_content(model, field, appearance, stored.as_ref(), policy)?;
                append_widget(&mut added, appearance, &content);
                flattened_widgets += 1;
            }
        }
    }

    let mut content = page.content.clone();
    if flattened_widgets > 0 && !added.is_empty() {
        if !content.is_empty() && !content.ends_with(b"\n") {
            content.push(b'\n');
        }
        content.extend_from_slice(&added);
    }
    Ok(FlattenedPage {
        content,
        annotations,
        flattened_widgets,
    })
}

/// Flattens every page and reports whether the `/AcroForm` can be removed.
///
/// The `/AcroForm` dictionary is removed only when the document had one **and**
/// at least one widget was actually flattened. A non-empty form whose widgets
/// are absent from the supplied pages is left completely unchanged — including
/// its form dictionary — so a partial flatten never orphans widgets that still
/// live on other pages. Flattening a blank document is likewise a genuine no-op.
///
/// # Errors
///
/// Propagates [`FlattenError`] from the first page that cannot be flattened.
pub fn flatten_document(
    document: &FlatDocument,
    form: &Form,
    policy: AppearancePolicy,
) -> Result<FlattenedDocument, FlattenError> {
    let mut pages = Vec::with_capacity(document.pages.len());
    let mut flattened_widgets = 0;
    for page in &document.pages {
        let flattened = flatten_page(page, form, policy)?;
        flattened_widgets += flattened.flattened_widgets;
        pages.push(flattened);
    }
    Ok(FlattenedDocument {
        pages,
        acroform_removed: document.has_acroform && flattened_widgets > 0,
        flattened_widgets,
    })
}

/// Chooses and returns the content bytes for one widget.
fn widget_content(
    field: &Field,
    name: &str,
    appearance: &WidgetAppearance,
    stored: Option<&StoredAppearance>,
    policy: AppearancePolicy,
) -> Result<Vec<u8>, FlattenError> {
    match policy {
        AppearancePolicy::Generate => generated_content(field, name, appearance),
        AppearancePolicy::StoredOnly => stored_content(field, name, stored)?.ok_or_else(|| {
            FlattenError::MissingStoredAppearance {
                field: name.to_owned(),
            }
        }),
        AppearancePolicy::PreferStored => match stored_content(field, name, stored)? {
            Some(content) => Ok(content),
            None => generated_content(field, name, appearance),
        },
    }
}

/// Generates a fresh appearance stream and extracts its active content.
fn generated_content(
    field: &Field,
    name: &str,
    appearance: &WidgetAppearance,
) -> Result<Vec<u8>, FlattenError> {
    let dictionary = build_appearance(field, appearance, field.value()).map_err(|source| {
        FlattenError::Appearance {
            field: name.to_owned(),
            source,
        }
    })?;
    content_from_normal(
        &dictionary.normal,
        dictionary.active_state.as_deref(),
        field,
        name,
    )
}

/// Extracts the active content bytes from a stored appearance, if any.
fn stored_content(
    field: &Field,
    name: &str,
    stored: Option<&StoredAppearance>,
) -> Result<Option<Vec<u8>>, FlattenError> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    let content = content_from_normal(&stored.normal, stored.active_state.as_deref(), field, name)?;
    Ok(Some(content))
}

/// Selects a stream from a normal appearance, resolving stateful widgets.
fn content_from_normal(
    normal: &NormalAppearance,
    active_state: Option<&str>,
    field: &Field,
    name: &str,
) -> Result<Vec<u8>, FlattenError> {
    match normal {
        NormalAppearance::Stream(stream) => Ok(stream.content.clone()),
        NormalAppearance::States(states) => {
            let mut candidates: Vec<&str> = Vec::new();
            if let Some(state) = active_state {
                candidates.push(state);
            }
            let derived = derived_state(field);
            if let Some(state) = derived.as_deref() {
                candidates.push(state);
            }
            for candidate in candidates {
                if let Some(stream) = states.get(candidate) {
                    return Ok(stream.content.clone());
                }
            }
            if states.len() == 1
                && let Some(stream) = states.values().next()
            {
                return Ok(stream.content.clone());
            }
            let state = active_state
                .map(str::to_owned)
                .or(derived)
                .unwrap_or_else(|| "Off".to_owned());
            Err(FlattenError::MissingAppearanceState {
                field: name.to_owned(),
                state,
            })
        }
    }
}

/// Implies a button state name from a field's current value.
fn derived_state(field: &Field) -> Option<String> {
    match field.field_type() {
        FieldType::CheckBox => Some(
            if field.value().as_bool().unwrap_or(false) {
                STATE_ON
            } else {
                STATE_OFF
            }
            .to_owned(),
        ),
        FieldType::Radio => Some(
            field
                .value()
                .as_choices()
                .and_then(|selected| selected.first())
                .cloned()
                .unwrap_or_else(|| STATE_OFF.to_owned()),
        ),
        _ => None,
    }
}

/// Appends one appearance stream, translated into page coordinates.
fn append_widget(out: &mut Vec<u8>, appearance: &WidgetAppearance, content: &[u8]) {
    if content.is_empty() {
        return;
    }
    out.extend_from_slice(b"q\n1 0 0 1 ");
    out.extend_from_slice(format_number(appearance.rect.x).as_bytes());
    out.push(b' ');
    out.extend_from_slice(format_number(appearance.rect.y).as_bytes());
    out.extend_from_slice(b" cm\n");
    out.extend_from_slice(content);
    if !content.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(b"Q\n");
}

#[cfg(test)]
mod tests;
