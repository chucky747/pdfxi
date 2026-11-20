//! Optional content group (OCG) layers and viewing state.
//!
//! This module implements the *model* behind the viewer's Layers panel. It is
//! deliberately dependency-light: it has no `egui`, renderer, or PDF backend
//! dependency. A caller parses a document's `/OCProperties` into an
//! [`OcPropertiesSummary`], and [`LayerModel::from_summary`] validates it into a
//! deterministic model. [`LayerState`] holds the mutable per-document viewing
//! state, and [`LayerRegistry`] isolates that state per open tab.
//!
//! # PDF concepts
//!
//! The summary mirrors the parts of `/OCProperties` the viewer cares about:
//!
//! - **OCGs** — the individual optional content groups, each with a stable id
//!   (the indirect object number in a real PDF), a display name, a default
//!   visibility, and a locked flag (the `/D /Locked` array in a real PDF).
//! - **Radio-button groups** — `/RBGroups`; at most one member of each group may
//!   be visible at a time. Selecting one member clears the others.
//! - **Order** — `/Order`; the display order of layers. Ids omitted from the
//!   order are appended in declaration order, so order is always deterministic.
//!
//! # Rendering boundary
//!
//! [`LayerModel::visibility_plan`] produces a [`VisibilityPlan`] the canvas can
//! consume through a simple [`VisibilityPlan::is_visible`] predicate. The plan
//! is purely a *boundary*: it says which OCG ids are currently enabled, but it
//! does not rasterize anything. **Actual optional-content rendering depends on
//! the renderer backend.** The current `pdfxi-render` backend exposes only page
//! rasters and has no optional-content API, so this crate cannot yet instruct
//! PDFium to honor per-group visibility. A backend that adds optional-content
//! support can consume the plan's visible-id set without any change to this
//! module or to the model/state machine.

use std::collections::HashMap;

use crate::tabs::TabId;

/// A stable key for one optional content group.
///
/// In a real PDF this is the indirect object number of an OCG dictionary in
/// `/OCProperties /OCGs`. Here it is an owned string so callers can supply
/// whatever identifier their parser produces; ids are compared exactly.
pub type OcgId = String;

/// Typed errors produced while parsing or manipulating optional content layers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayerError {
    /// An OCG was declared with an empty id.
    #[error("optional content group id must not be empty")]
    EmptyId,
    /// An OCG was declared with an empty display name.
    #[error("optional content group '{0}' must have a non-empty name")]
    EmptyName(OcgId),
    /// Two OCGs were declared with the same id.
    #[error("duplicate optional content group id '{0}'")]
    DuplicateId(OcgId),
    /// A radio-button group contained no members.
    #[error("radio group must contain at least one member")]
    EmptyRadioGroup,
    /// A radio-button group referenced an OCG that was never declared.
    #[error("radio group references unknown optional content group '{0}'")]
    UnknownRadioMember(OcgId),
    /// An OCG appeared in more than one radio-button group.
    #[error("optional content group '{0}' belongs to more than one radio group")]
    MultipleRadioGroups(OcgId),
    /// The display order referenced an OCG that was never declared.
    #[error("display order references unknown optional content group '{0}'")]
    UnknownOrderId(OcgId),
    /// The display order listed the same OCG more than once.
    #[error("optional content group '{0}' appears more than once in the display order")]
    DuplicateOrderId(OcgId),
    /// A runtime operation named an OCG that is not in the model.
    #[error("no optional content group with id '{0}'")]
    OcgNotFound(OcgId),
    /// The tab has no installed layer model.
    #[error("tab {0} has no optional content layers")]
    NoLayersForTab(TabId),
    /// A user action tried to change an OCG that is locked.
    #[error("optional content group '{0}' is locked and cannot be changed")]
    Locked(OcgId),
}

/// Description of one optional content group parsed from `/OCProperties`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcgInfo {
    id: OcgId,
    name: String,
    default_visible: bool,
    locked: bool,
}

impl OcgInfo {
    /// Describe one OCG.
    ///
    /// `id` is the stable key (an indirect object number in a real PDF) and
    /// `name` is the human-readable label shown in the Layers panel.
    /// `default_visible` is its initial state, and `locked` prevents user edits.
    pub fn new(
        id: impl Into<OcgId>,
        name: impl Into<String>,
        default_visible: bool,
        locked: bool,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            default_visible,
            locked,
        }
    }

    /// The stable id of this OCG.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The display name of this OCG.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this OCG starts visible.
    pub fn default_visible(&self) -> bool {
        self.default_visible
    }

    /// Whether this OCG is locked against user changes.
    pub fn locked(&self) -> bool {
        self.locked
    }
}

/// A radio-button group: at most one member may be visible at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioGroup {
    members: Vec<OcgId>,
}

impl RadioGroup {
    /// Create a group from the ids of its members, in display order.
    pub fn new(members: impl IntoIterator<Item = impl Into<OcgId>>) -> Self {
        Self {
            members: members.into_iter().map(Into::into).collect(),
        }
    }

    /// The ids of the members of this group.
    pub fn members(&self) -> &[OcgId] {
        &self.members
    }
}

/// A caller-supplied summary of a document's `/OCProperties` dictionary.
///
/// Build one with [`Self::new`] and the builder methods, then pass it to
/// [`LayerModel::from_summary`] for validation. No ordering guarantee comes
/// from this type; determinism is established by the validated model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OcPropertiesSummary {
    groups: Vec<OcgInfo>,
    radio_groups: Vec<RadioGroup>,
    order: Vec<OcgId>,
}

impl OcPropertiesSummary {
    /// Create an empty summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one OCG declaration.
    pub fn add_group(mut self, group: OcgInfo) -> Self {
        self.groups.push(group);
        self
    }

    /// Add a radio-button group.
    pub fn with_radio_group(mut self, group: RadioGroup) -> Self {
        self.radio_groups.push(group);
        self
    }

    /// Set the display order by OCG id.
    ///
    /// Ids not listed here are appended in declaration order. Unknown ids are
    /// rejected by [`LayerModel::from_summary`].
    pub fn with_order(mut self, order: impl IntoIterator<Item = impl Into<OcgId>>) -> Self {
        self.order = order.into_iter().map(Into::into).collect();
        self
    }

    /// The declared OCGs.
    pub fn groups(&self) -> &[OcgInfo] {
        &self.groups
    }

    /// The declared radio-button groups.
    pub fn radio_groups(&self) -> &[RadioGroup] {
        &self.radio_groups
    }

    /// The explicit display order, if any.
    pub fn order(&self) -> &[OcgId] {
        &self.order
    }
}

/// Internal definition of one layer after validation and resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LayerDef {
    id: OcgId,
    name: String,
    initial_visible: bool,
    locked: bool,
    radio_group: Option<usize>,
}

/// A read-only view of one layer in a [`LayerModel`].
#[derive(Clone, Copy, Debug)]
pub struct LayerInfo<'a> {
    def: &'a LayerDef,
}

impl<'a> LayerInfo<'a> {
    fn new(def: &'a LayerDef) -> Self {
        Self { def }
    }

    /// The stable id of this layer.
    pub fn id(&self) -> &'a str {
        &self.def.id
    }

    /// The display name of this layer.
    pub fn name(&self) -> &'a str {
        &self.def.name
    }

    /// Whether this layer starts visible before any user action.
    pub fn default_visible(&self) -> bool {
        self.def.initial_visible
    }

    /// Whether this layer is locked against user changes.
    pub fn is_locked(&self) -> bool {
        self.def.locked
    }

    /// The index of the radio-button group this layer belongs to, if any.
    pub fn radio_group(&self) -> Option<usize> {
        self.def.radio_group
    }
}

/// A validated, deterministically ordered optional content layer model.
///
/// Construct one with [`Self::from_summary`]. The model is immutable; mutable
/// viewing state lives in [`LayerState`] so the same model can back independent
/// sessions (for example, one per tab).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerModel {
    layers: Vec<LayerDef>,
    index: HashMap<OcgId, usize>,
    radio_group_count: usize,
}

impl LayerModel {
    /// Validate `summary` into a deterministic layer model.
    ///
    /// Validation rejects empty or duplicate ids, empty names, empty or
    /// dangling radio groups, OCGs in more than one radio group, and display
    /// order entries that are unknown or repeated. On success, layers are
    /// ordered exactly as the summary's `order` list, with any remaining OCGs
    /// appended in declaration order.
    pub fn from_summary(summary: OcPropertiesSummary) -> Result<Self, LayerError> {
        let mut index: HashMap<OcgId, usize> = HashMap::new();
        let mut declarations: Vec<(OcgId, bool, bool, String)> = Vec::new();

        for group in summary.groups {
            if group.id.is_empty() {
                return Err(LayerError::EmptyId);
            }
            if group.name.is_empty() {
                return Err(LayerError::EmptyName(group.id));
            }
            if index.contains_key(&group.id) {
                return Err(LayerError::DuplicateId(group.id));
            }
            index.insert(group.id.clone(), declarations.len());
            declarations.push((group.id, group.default_visible, group.locked, group.name));
        }

        let mut radio_group_of: HashMap<OcgId, usize> = HashMap::new();
        for (group_index, group) in summary.radio_groups.iter().enumerate() {
            if group.members.is_empty() {
                return Err(LayerError::EmptyRadioGroup);
            }
            for member in &group.members {
                if !index.contains_key(member) {
                    return Err(LayerError::UnknownRadioMember(member.clone()));
                }
                if radio_group_of.insert(member.clone(), group_index).is_some() {
                    return Err(LayerError::MultipleRadioGroups(member.clone()));
                }
            }
        }

        let mut ordered_indices: Vec<usize> = Vec::new();
        let mut seen_order: HashMap<OcgId, ()> = HashMap::new();
        for id in summary.order {
            let position = index
                .get(&id)
                .copied()
                .ok_or_else(|| LayerError::UnknownOrderId(id.clone()))?;
            if seen_order.insert(id.clone(), ()).is_some() {
                return Err(LayerError::DuplicateOrderId(id));
            }
            ordered_indices.push(position);
        }
        for position in 0..declarations.len() {
            if !ordered_indices.contains(&position) {
                ordered_indices.push(position);
            }
        }

        let layers = ordered_indices
            .into_iter()
            .map(|position| {
                let (id, initial_visible, locked, name) = &declarations[position];
                LayerDef {
                    id: id.clone(),
                    name: name.clone(),
                    initial_visible: *initial_visible,
                    locked: *locked,
                    radio_group: radio_group_of.get(id).copied(),
                }
            })
            .collect();

        Ok(Self {
            layers,
            index,
            radio_group_count: summary.radio_groups.len(),
        })
    }

    /// The number of layers in this model.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether this model has no layers.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Iterate over layers in deterministic display order.
    pub fn layers(&self) -> impl ExactSizeIterator<Item = LayerInfo<'_>> + '_ {
        self.layers.iter().map(LayerInfo::new)
    }

    /// Look up a layer by its id.
    pub fn get(&self, id: &str) -> Option<LayerInfo<'_>> {
        self.position(id)
            .map(|position| LayerInfo::new(&self.layers[position]))
    }

    /// The display position of a layer id, if present.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// The number of radio-button groups in this model.
    pub fn radio_group_count(&self) -> usize {
        self.radio_group_count
    }

    /// Build a visibility plan from a state snapshot.
    ///
    /// The plan lists currently visible OCG ids in deterministic display order
    /// and can be consumed by the canvas as a visibility predicate. See the
    /// [module docs](crate::layers#rendering-boundary) for the rendering caveat.
    pub fn visibility_plan(&self, state: &LayerState) -> VisibilityPlan {
        let mut visible = Vec::new();
        for (position, layer) in self.layers.iter().enumerate() {
            let is_visible = state
                .visible
                .get(position)
                .copied()
                .unwrap_or(layer.initial_visible);
            if is_visible {
                visible.push(layer.id.clone());
            }
        }
        VisibilityPlan { visible }
    }
}

/// Mutable viewing state for one document's optional content layers.
///
/// A state is created from a [`LayerModel`] and indexes into it by position, so
/// every state must be used with the model it was created from. Keeping state
/// separate from the immutable model lets a registry maintain one independent
/// copy per tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerState {
    visible: Vec<bool>,
}

impl LayerState {
    /// Create the initial state from a model's default visibility flags.
    ///
    /// A hostile `/OCProperties` summary may mark more than one member of a
    /// radio-button group as initially visible. This constructor normalizes
    /// that state so no group ever shows two members: the first declared
    /// locked-and-visible member wins, else the first declared visible member,
    /// else the first declared member. Every other member of the group is
    /// hidden.
    pub fn new(model: &LayerModel) -> Self {
        let visible: Vec<bool> = model
            .layers
            .iter()
            .map(|layer| layer.initial_visible)
            .collect();
        let mut state = Self { visible };
        state.normalize_radio_groups(model);
        state
    }

    /// Enforce radio-group exclusivity on freshly constructed state.
    ///
    /// [`LayerModel::position`] records each OCG's declaration index, which is
    /// used as the deterministic "first declared" ordering for group members.
    fn normalize_radio_groups(&mut self, model: &LayerModel) {
        for group in 0..model.radio_group_count {
            let members: Vec<usize> = model
                .layers
                .iter()
                .enumerate()
                .filter_map(|(position, layer)| {
                    (layer.radio_group == Some(group)).then_some(position)
                })
                .collect();
            if members.len() <= 1 {
                continue;
            }
            let declared = |position: usize| {
                model
                    .position(&model.layers[position].id)
                    .unwrap_or(position)
            };
            // Priority 1: first declared locked-and-visible member.
            let winner = members
                .iter()
                .copied()
                .filter(|&position| {
                    model.layers[position].locked
                        && self.visible.get(position).copied().unwrap_or(false)
                })
                .min_by_key(|&position| declared(position))
                // Priority 2: first declared visible member.
                .or_else(|| {
                    members
                        .iter()
                        .copied()
                        .filter(|&position| self.visible.get(position).copied().unwrap_or(false))
                        .min_by_key(|&position| declared(position))
                })
                // Priority 3: first declared member.
                .or_else(|| {
                    members
                        .iter()
                        .copied()
                        .min_by_key(|&position| declared(position))
                });
            if let Some(winner) = winner {
                for position in members {
                    if let Some(flag) = self.visible.get_mut(position) {
                        *flag = position == winner;
                    }
                }
            }
        }
    }

    /// The display position of a visible locked member of `group`, if any.
    ///
    /// `exclude` skips the named position so a member never counts itself as
    /// the occupying locked member when `set_visible` consults this helper.
    fn locked_visible_member(
        &self,
        model: &LayerModel,
        group: usize,
        exclude: Option<usize>,
    ) -> Option<usize> {
        model
            .layers
            .iter()
            .enumerate()
            .find_map(|(position, layer)| {
                if layer.radio_group == Some(group)
                    && layer.locked
                    && Some(position) != exclude
                    && self.visible.get(position).copied().unwrap_or(false)
                {
                    Some(position)
                } else {
                    None
                }
            })
    }

    /// Whether the named OCG is currently visible.
    pub fn is_visible(&self, model: &LayerModel, id: &str) -> Result<bool, LayerError> {
        let position = model
            .position(id)
            .ok_or_else(|| LayerError::OcgNotFound(id.to_string()))?;
        Ok(self.visible.get(position).copied().unwrap_or(false))
    }

    /// Set the named OCG's visibility, returning the new value.
    ///
    /// Locked OCGs are rejected with [`LayerError::Locked`]. Setting a radio
    /// group member visible clears every other unlocked member of that group;
    /// setting it hidden leaves the rest of the group unchanged. A locked,
    /// visible member owns its group and cannot be cleared, so a second member
    /// can never be enabled alongside it: in that case the member stays hidden
    /// and the effective (unchanged) value `false` is returned.
    pub fn set_visible(
        &mut self,
        model: &LayerModel,
        id: &str,
        value: bool,
    ) -> Result<bool, LayerError> {
        let position = model
            .position(id)
            .ok_or_else(|| LayerError::OcgNotFound(id.to_string()))?;
        if model.layers[position].locked {
            return Err(LayerError::Locked(id.to_string()));
        }
        let group = model.layers[position].radio_group;
        let blocked = if value && let Some(group) = group {
            self.locked_visible_member(model, group, Some(position))
                .is_some()
        } else {
            false
        };
        let effective = if blocked { false } else { value };
        if effective && let Some(group) = group {
            for (other_position, layer) in model.layers.iter().enumerate() {
                if other_position != position
                    && layer.radio_group == Some(group)
                    && !layer.locked
                    && let Some(flag) = self.visible.get_mut(other_position)
                {
                    *flag = false;
                }
            }
        }
        if let Some(flag) = self.visible.get_mut(position) {
            *flag = effective;
        }
        Ok(effective)
    }

    /// Flip the named OCG's visibility, returning the new value.
    pub fn toggle(&mut self, model: &LayerModel, id: &str) -> Result<bool, LayerError> {
        let current = self.is_visible(model, id)?;
        self.set_visible(model, id, !current)
    }

    /// Show every unlocked OCG while preserving radio-group exclusivity.
    ///
    /// Locked OCGs keep their current state. A visible locked member owns its
    /// radio group, so no other member of that group is enabled; otherwise the
    /// first unlocked member in display order is shown and the group's other
    /// unlocked members are hidden. Showing all members of a radio group is not
    /// a valid PDF viewing state.
    pub fn show_all(&mut self, model: &LayerModel) {
        let mut chosen: Vec<Option<usize>> = vec![None; model.radio_group_count];
        // Pass 1: a visible locked member cannot be changed, so it wins its group.
        for (position, layer) in model.layers.iter().enumerate() {
            if layer.locked
                && self.visible.get(position).copied().unwrap_or(false)
                && let Some(group) = layer.radio_group
                && let Some(slot) = chosen.get_mut(group)
                && slot.is_none()
            {
                *slot = Some(position);
            }
        }
        // Pass 2: for groups without a visible locked member, pick the first
        // unlocked member in display order.
        for (position, layer) in model.layers.iter().enumerate() {
            if !layer.locked
                && let Some(group) = layer.radio_group
                && let Some(slot) = chosen.get_mut(group)
                && slot.is_none()
            {
                *slot = Some(position);
            }
        }
        for (position, layer) in model.layers.iter().enumerate() {
            if layer.locked {
                continue;
            }
            let visible = match layer.radio_group {
                Some(group) => chosen.get(group).and_then(|slot| *slot) == Some(position),
                None => true,
            };
            if let Some(flag) = self.visible.get_mut(position) {
                *flag = visible;
            }
        }
    }

    /// Hide every unlocked OCG; locked OCGs keep their current state.
    pub fn hide_all(&mut self, model: &LayerModel) {
        for (position, layer) in model.layers.iter().enumerate() {
            if !layer.locked
                && let Some(flag) = self.visible.get_mut(position)
            {
                *flag = false;
            }
        }
    }

    /// Count the currently visible layers.
    pub fn visible_count(&self, model: &LayerModel) -> usize {
        model
            .layers
            .iter()
            .enumerate()
            .filter(|(position, _)| self.visible.get(*position).copied().unwrap_or(false))
            .count()
    }
}

/// A backend-agnostic snapshot of which OCG ids are currently visible.
///
/// Produced by [`LayerModel::visibility_plan`]. The canvas can treat it as a
/// pure visibility predicate; applying it to actual raster output is the
/// responsibility of a renderer backend with optional-content support.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisibilityPlan {
    visible: Vec<OcgId>,
}

impl VisibilityPlan {
    /// Whether the named OCG is visible in this plan.
    pub fn is_visible(&self, id: &str) -> bool {
        self.visible.iter().any(|candidate| candidate == id)
    }

    /// The visible OCG ids in deterministic display order.
    pub fn visible_ids(&self) -> &[OcgId] {
        &self.visible
    }

    /// The number of visible OCGs.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Whether no OCG is visible.
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }
}

/// One tab's installed layer model and viewing state.
#[derive(Clone, Debug)]
struct LayerDocument {
    model: LayerModel,
    state: LayerState,
}

/// Per-tab registry of optional content layer models and viewing states.
///
/// Each tab gets its own [`LayerState`], so toggling a layer in one document
/// never affects another. All operations return typed errors and never block:
/// they work purely on in-memory state.
#[derive(Debug, Default)]
pub struct LayerRegistry {
    documents: HashMap<TabId, LayerDocument>,
}

impl LayerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a validated layer model for `tab`, resetting its state to defaults.
    ///
    /// An invalid summary is rejected and leaves any previous installation for
    /// that tab untouched.
    pub fn install(&mut self, tab: TabId, summary: OcPropertiesSummary) -> Result<(), LayerError> {
        let model = LayerModel::from_summary(summary)?;
        let state = LayerState::new(&model);
        self.documents.insert(tab, LayerDocument { model, state });
        Ok(())
    }

    /// Remove a tab's layers, returning `true` if any were installed.
    pub fn remove(&mut self, tab: TabId) -> bool {
        self.documents.remove(&tab).is_some()
    }

    /// Whether `tab` has layers installed.
    pub fn contains(&self, tab: TabId) -> bool {
        self.documents.contains_key(&tab)
    }

    /// The layer model installed for `tab`, if any.
    pub fn model(&self, tab: TabId) -> Option<&LayerModel> {
        self.documents.get(&tab).map(|document| &document.model)
    }

    /// The viewing state installed for `tab`, if any.
    pub fn state(&self, tab: TabId) -> Option<&LayerState> {
        self.documents.get(&tab).map(|document| &document.state)
    }

    /// Mutable access to the viewing state installed for `tab`, if any.
    ///
    /// Callers must use the matching [`Self::model`] with this state. Prefer the
    /// convenience mutators on this registry when possible.
    pub fn state_mut(&mut self, tab: TabId) -> Option<&mut LayerState> {
        self.documents
            .get_mut(&tab)
            .map(|document| &mut document.state)
    }

    /// Set a layer's visibility for `tab`, returning the new value.
    pub fn set_visible(&mut self, tab: TabId, id: &str, value: bool) -> Result<bool, LayerError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(LayerError::NoLayersForTab(tab))?;
        document.state.set_visible(&document.model, id, value)
    }

    /// Toggle a layer's visibility for `tab`, returning the new value.
    pub fn toggle(&mut self, tab: TabId, id: &str) -> Result<bool, LayerError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(LayerError::NoLayersForTab(tab))?;
        document.state.toggle(&document.model, id)
    }

    /// Show all layers for `tab`, preserving radio-group exclusivity.
    pub fn show_all(&mut self, tab: TabId) -> Result<(), LayerError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(LayerError::NoLayersForTab(tab))?;
        document.state.show_all(&document.model);
        Ok(())
    }

    /// Hide all unlocked layers for `tab`.
    pub fn hide_all(&mut self, tab: TabId) -> Result<(), LayerError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(LayerError::NoLayersForTab(tab))?;
        document.state.hide_all(&document.model);
        Ok(())
    }

    /// Build a visibility plan for `tab`, or `None` if it has no layers.
    pub fn visibility_plan(&self, tab: TabId) -> Option<VisibilityPlan> {
        let document = self.documents.get(&tab)?;
        Some(document.model.visibility_plan(&document.state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64) -> TabId {
        TabId::from_u64(id)
    }

    fn summary() -> OcPropertiesSummary {
        OcPropertiesSummary::new()
            .add_group(OcgInfo::new("oc1", "Background", true, false))
            .add_group(OcgInfo::new("oc2", "Annotations", false, false))
            .add_group(OcgInfo::new("oc3", "Watermark", true, false))
    }

    fn radio_summary() -> OcPropertiesSummary {
        summary()
            .add_group(OcgInfo::new("oc4", "English", true, false))
            .add_group(OcgInfo::new("oc5", "French", false, false))
            .with_radio_group(RadioGroup::new(["oc4", "oc5"]))
    }

    #[test]
    fn default_states_are_applied_from_summary() {
        let model = LayerModel::from_summary(summary()).expect("valid summary");
        let state = LayerState::new(&model);
        assert!(state.is_visible(&model, "oc1").expect("known ocg"));
        assert!(!state.is_visible(&model, "oc2").expect("known ocg"));
        assert!(state.is_visible(&model, "oc3").expect("known ocg"));
        assert_eq!(state.visible_count(&model), 2);
    }

    #[test]
    fn toggle_flips_visibility_and_returns_the_new_value() {
        let model = LayerModel::from_summary(summary()).expect("valid summary");
        let mut state = LayerState::new(&model);
        assert!(state.toggle(&model, "oc2").expect("known ocg")); // default false -> true
        assert!(state.is_visible(&model, "oc2").expect("known ocg"));
        assert!(!state.toggle(&model, "oc2").expect("known ocg")); // back to hidden
        assert!(!state.is_visible(&model, "oc2").expect("known ocg"));
        // set_visible is explicit and idempotent.
        assert!(state.set_visible(&model, "oc2", true).expect("known ocg"));
        assert!(state.set_visible(&model, "oc2", true).expect("known ocg"));
    }

    #[test]
    fn radio_group_enforces_exclusivity() {
        let model = LayerModel::from_summary(radio_summary()).expect("valid summary");
        let mut state = LayerState::new(&model);
        assert!(state.is_visible(&model, "oc4").expect("known ocg"));
        assert!(!state.is_visible(&model, "oc5").expect("known ocg"));
        // Selecting the other member clears the first.
        assert!(state.set_visible(&model, "oc5", true).expect("known ocg"));
        assert!(!state.is_visible(&model, "oc4").expect("known ocg"));
        assert!(state.is_visible(&model, "oc5").expect("known ocg"));
        // Toggling the hidden one back on is exclusive again.
        assert!(state.toggle(&model, "oc4").expect("known ocg"));
        assert!(state.is_visible(&model, "oc4").expect("known ocg"));
        assert!(!state.is_visible(&model, "oc5").expect("known ocg"));
        // Hiding a member does not force another one on.
        assert!(!state.set_visible(&model, "oc4", false).expect("known ocg"));
        assert!(!state.is_visible(&model, "oc5").expect("known ocg"));
    }

    #[test]
    fn locked_ocg_rejects_user_changes() {
        let summary = summary().add_group(OcgInfo::new("oc9", "Locked", true, true));
        let model = LayerModel::from_summary(summary).expect("valid summary");
        let mut state = LayerState::new(&model);
        assert_eq!(
            state.toggle(&model, "oc9"),
            Err(LayerError::Locked("oc9".into()))
        );
        assert_eq!(
            state.set_visible(&model, "oc9", false),
            Err(LayerError::Locked("oc9".into()))
        );
        assert!(state.is_visible(&model, "oc9").expect("known ocg"));
        // Bulk operations skip locked layers too.
        state.hide_all(&model);
        assert!(state.is_visible(&model, "oc9").expect("known ocg"));
        state.show_all(&model);
        assert!(state.is_visible(&model, "oc9").expect("known ocg"));
    }

    #[test]
    fn layer_order_is_deterministic_and_appends_unlisted_ids() {
        let explicit = summary().with_order(["oc3", "oc1"]);
        let model = LayerModel::from_summary(explicit).expect("valid summary");
        let ids: Vec<&str> = model.layers().map(|layer| layer.id()).collect();
        // Listed ids first in the given order, then unlisted ids in declaration order.
        assert_eq!(ids, vec!["oc3", "oc1", "oc2"]);

        let full = summary().with_order(["oc2", "oc1", "oc3"]);
        let model = LayerModel::from_summary(full).expect("valid summary");
        let ids: Vec<&str> = model.layers().map(|layer| layer.id()).collect();
        assert_eq!(ids, vec!["oc2", "oc1", "oc3"]);

        // A model with no explicit order keeps declaration order.
        let model = LayerModel::from_summary(summary()).expect("valid summary");
        let ids: Vec<&str> = model.layers().map(|layer| layer.id()).collect();
        assert_eq!(ids, vec!["oc1", "oc2", "oc3"]);
        assert_eq!(model.position("oc2"), Some(1));
        assert_eq!(model.position("missing"), None);
    }

    #[test]
    fn per_tab_layer_state_is_isolated() {
        let mut registry = LayerRegistry::new();
        let a = tab(1);
        let b = tab(2);
        registry.install(a, summary()).expect("valid summary");
        registry.install(b, summary()).expect("valid summary");
        assert!(registry.toggle(a, "oc2").expect("known ocg"));
        assert!(
            registry
                .state(a)
                .expect("tab a")
                .is_visible(registry.model(a).expect("tab a model"), "oc2")
                .expect("known ocg")
        );
        let model_b = registry.model(b).expect("tab b model").clone();
        assert!(
            !registry
                .state(b)
                .expect("tab b")
                .is_visible(&model_b, "oc2")
                .expect("known ocg")
        );
        // Removing one tab leaves the other intact.
        assert!(registry.remove(a));
        assert!(!registry.contains(a));
        assert!(registry.contains(b));
    }

    #[test]
    fn show_all_and_hide_all_respect_locked_and_radio_groups() {
        let summary = radio_summary().add_group(OcgInfo::new("oc6", "Pinned", false, true));
        let model = LayerModel::from_summary(summary).expect("valid summary");
        let mut state = LayerState::new(&model);
        state.hide_all(&model);
        assert_eq!(
            state.visible_count(&model),
            0,
            "locked layer started hidden"
        );
        state.show_all(&model);
        // oc1, oc2, oc3, oc6(non-radio) visible; only oc4 (first radio member) visible.
        assert!(state.is_visible(&model, "oc1").expect("known ocg"));
        assert!(state.is_visible(&model, "oc2").expect("known ocg"));
        assert!(state.is_visible(&model, "oc4").expect("known ocg"));
        assert!(!state.is_visible(&model, "oc5").expect("known ocg"));
        assert!(
            !state
                .is_visible(&model, "oc6")
                .expect("locked stays hidden")
        );
        state.hide_all(&model);
        assert_eq!(state.visible_count(&model), 0);
    }

    #[test]
    fn unknown_ocg_returns_a_typed_error() {
        let model = LayerModel::from_summary(summary()).expect("valid summary");
        let mut state = LayerState::new(&model);
        assert_eq!(
            state.toggle(&model, "ghost"),
            Err(LayerError::OcgNotFound("ghost".into()))
        );
        assert_eq!(
            state.set_visible(&model, "ghost", true),
            Err(LayerError::OcgNotFound("ghost".into()))
        );
        assert_eq!(
            state.is_visible(&model, "ghost"),
            Err(LayerError::OcgNotFound("ghost".into()))
        );
        let mut registry = LayerRegistry::new();
        assert_eq!(
            registry.toggle(tab(7), "oc1"),
            Err(LayerError::NoLayersForTab(tab(7)))
        );
    }

    #[test]
    fn invalid_summaries_are_rejected_during_parsing() {
        assert_eq!(
            LayerModel::from_summary(
                OcPropertiesSummary::new().add_group(OcgInfo::new("", "x", true, false))
            ),
            Err(LayerError::EmptyId)
        );
        assert_eq!(
            LayerModel::from_summary(
                OcPropertiesSummary::new().add_group(OcgInfo::new("a", "", true, false))
            ),
            Err(LayerError::EmptyName("a".into()))
        );
        let duplicate = summary().add_group(OcgInfo::new("oc1", "Again", true, false));
        assert_eq!(
            LayerModel::from_summary(duplicate),
            Err(LayerError::DuplicateId("oc1".into()))
        );
        assert_eq!(
            LayerModel::from_summary(
                OcPropertiesSummary::new().with_radio_group(RadioGroup::new(Vec::<&str>::new()))
            ),
            Err(LayerError::EmptyRadioGroup)
        );
        assert_eq!(
            LayerModel::from_summary(summary().with_radio_group(RadioGroup::new(["ghost"]))),
            Err(LayerError::UnknownRadioMember("ghost".into()))
        );
        assert_eq!(
            LayerModel::from_summary(summary().with_order(["ghost"])),
            Err(LayerError::UnknownOrderId("ghost".into()))
        );
        let duplicate_order = summary().with_order(["oc1", "oc1"]);
        assert_eq!(
            LayerModel::from_summary(duplicate_order),
            Err(LayerError::DuplicateOrderId("oc1".into()))
        );
        let two_groups = summary()
            .with_radio_group(RadioGroup::new(["oc1"]))
            .with_radio_group(RadioGroup::new(["oc1"]));
        assert_eq!(
            LayerModel::from_summary(two_groups),
            Err(LayerError::MultipleRadioGroups("oc1".into()))
        );
    }

    #[test]
    fn visibility_plan_reflects_state_in_display_order() {
        let model = LayerModel::from_summary(summary().with_order(["oc3", "oc2", "oc1"]))
            .expect("valid summary");
        let state = LayerState::new(&model);
        let plan = model.visibility_plan(&state);
        // oc3 and oc1 are visible by default; oc2 is hidden.
        assert_eq!(plan.visible_ids(), &["oc3", "oc1"]);
        assert_eq!(plan.len(), 2);
        assert!(!plan.is_empty());
        assert!(plan.is_visible("oc3"));
        assert!(!plan.is_visible("oc2"));
    }

    #[test]
    fn visibility_plan_updates_after_state_changes_and_registry_access() {
        let mut registry = LayerRegistry::new();
        let a = tab(10);
        registry.install(a, summary()).expect("valid summary");
        let before = registry.visibility_plan(a).expect("installed tab");
        assert!(before.is_visible("oc1"));
        assert!(!before.is_visible("oc2"));
        registry.toggle(a, "oc2").expect("known ocg");
        let after = registry.visibility_plan(a).expect("installed tab");
        assert!(after.is_visible("oc2"));
        // The plan stays in deterministic display order, not toggle order.
        assert_eq!(after.visible_ids(), &["oc1", "oc2", "oc3"]);
        assert!(registry.visibility_plan(tab(99)).is_none());
    }

    #[test]
    fn failed_install_leaves_previous_tab_state_untouched() {
        let mut registry = LayerRegistry::new();
        let a = tab(1);
        registry.install(a, summary()).expect("valid summary");
        registry.toggle(a, "oc2").expect("known ocg");
        let bad = OcPropertiesSummary::new().add_group(OcgInfo::new("oc1", "", true, false));
        assert!(registry.install(a, bad).is_err());
        // The previous model and its toggled state are still present.
        let model = registry.model(a).expect("previous model kept");
        assert!(
            registry
                .state(a)
                .expect("previous state kept")
                .is_visible(model, "oc2")
                .expect("known ocg")
        );
    }

    #[test]
    fn construction_normalizes_multiple_visible_radio_members() {
        // Hostile summary: every member of the radio group starts visible.
        let hostile = OcPropertiesSummary::new()
            .add_group(OcgInfo::new("a", "A", true, false))
            .add_group(OcgInfo::new("b", "B", true, false))
            .add_group(OcgInfo::new("c", "C", true, false))
            .with_radio_group(RadioGroup::new(["a", "b", "c"]));
        let model = LayerModel::from_summary(hostile).expect("valid summary");
        let state = LayerState::new(&model);
        // Only the first declared visible member survives.
        assert!(state.is_visible(&model, "a").expect("known ocg"));
        assert!(!state.is_visible(&model, "b").expect("known ocg"));
        assert!(!state.is_visible(&model, "c").expect("known ocg"));
        assert_eq!(state.visible_count(&model), 1);
        assert_eq!(model.visibility_plan(&state).visible_ids(), &["a"]);

        // A locked-and-visible member outranks an earlier unlocked visible one.
        let locked = OcPropertiesSummary::new()
            .add_group(OcgInfo::new("a", "A", true, false))
            .add_group(OcgInfo::new("b", "B", true, true))
            .with_radio_group(RadioGroup::new(["a", "b"]));
        let model = LayerModel::from_summary(locked).expect("valid summary");
        let state = LayerState::new(&model);
        assert!(!state.is_visible(&model, "a").expect("known ocg"));
        assert!(state.is_visible(&model, "b").expect("known ocg"));
        assert_eq!(state.visible_count(&model), 1);

        // With no initially visible member, the first declared member wins.
        let none = OcPropertiesSummary::new()
            .add_group(OcgInfo::new("a", "A", false, false))
            .add_group(OcgInfo::new("b", "B", false, false))
            .with_radio_group(RadioGroup::new(["a", "b"]));
        let model = LayerModel::from_summary(none).expect("valid summary");
        let state = LayerState::new(&model);
        assert!(state.is_visible(&model, "a").expect("known ocg"));
        assert!(!state.is_visible(&model, "b").expect("known ocg"));
        assert_eq!(state.visible_count(&model), 1);
    }

    #[test]
    fn show_all_with_locked_visible_member_keeps_exclusivity() {
        let summary = OcPropertiesSummary::new()
            .add_group(OcgInfo::new("a", "A", true, true))
            .add_group(OcgInfo::new("b", "B", false, false))
            .add_group(OcgInfo::new("c", "C", false, false))
            .with_radio_group(RadioGroup::new(["a", "b", "c"]));
        let model = LayerModel::from_summary(summary).expect("valid summary");
        let mut state = LayerState::new(&model);
        assert!(state.is_visible(&model, "a").expect("known ocg"));
        assert!(!state.is_visible(&model, "b").expect("known ocg"));
        assert!(!state.is_visible(&model, "c").expect("known ocg"));
        state.show_all(&model);
        // The locked member still owns the group; b and c must stay hidden.
        assert!(state.is_visible(&model, "a").expect("known ocg"));
        assert!(!state.is_visible(&model, "b").expect("known ocg"));
        assert!(!state.is_visible(&model, "c").expect("known ocg"));
        assert_eq!(state.visible_count(&model), 1);
        assert_eq!(model.visibility_plan(&state).visible_ids(), &["a"]);
    }

    #[test]
    fn set_visible_on_second_member_is_blocked_by_locked_member() {
        let summary = OcPropertiesSummary::new()
            .add_group(OcgInfo::new("a", "A", true, true))
            .add_group(OcgInfo::new("b", "B", false, false))
            .with_radio_group(RadioGroup::new(["a", "b"]));
        let model = LayerModel::from_summary(summary).expect("valid summary");
        let mut state = LayerState::new(&model);
        assert!(state.is_visible(&model, "a").expect("known ocg"));
        // Switching on the second member is refused; the returned value is the
        // effective (still hidden) state.
        assert_eq!(state.set_visible(&model, "b", true), Ok(false));
        assert!(state.is_visible(&model, "a").expect("known ocg"));
        assert!(!state.is_visible(&model, "b").expect("known ocg"));
        assert_eq!(state.visible_count(&model), 1);
        // The locked member still rejects direct changes.
        assert_eq!(
            state.set_visible(&model, "a", false),
            Err(LayerError::Locked("a".into()))
        );
    }
}
