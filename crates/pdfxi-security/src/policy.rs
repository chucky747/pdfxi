//! Local, named protection policies (SEC-011).
//!
//! A [`ProtectionPolicy`] is a self-contained, offline description of how a
//! document may be used: a permission set, an optional expiry time, and a
//! watermark flag. [`build_policy_plan`] turns a policy into a deterministic,
//! ordered [`PolicyPlan`], and [`validate_policy`] / [`enforce_policy`] produce
//! a deterministic [`PolicyReport`] against a **caller-supplied** clock value.
//!
//! # No rights-management server
//!
//! This is a purely local model. There is no network access, no licence server,
//! no host-clock read, and no Adobe Rights Management compatibility. Time is an
//! `i64` supplied by the caller (for example a Unix timestamp), so reports are
//! reproducible and testable without wall-clock dependencies. Enforcement is a
//! decision made by the local reader; the policy itself carries no cryptography
//! and reveals no document contents.
//!
//! # Protection policies (SEC-011 completion)
//!
//! [`ProtectionPolicy`] also carries the security handler, key length, and a
//! recipient list through [`ProtectionPolicy::with_protection`].
//! [`validate_policy_spec`]
//! checks those fields with typed errors, [`build_protection_plan`] produces a
//! deterministic ordered [`ProtectionPlan`], and [`apply_protection_policy`]
//! yields a deterministic [`ProtectionApplyReport`]. Handler identifiers are
//! parsed from strings so an unknown handler is a typed error, and the caller
//! supplies the time — no host clock is read.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Result, SecurityError};
use crate::password_policy::SecurityHandler;
use crate::permissions::Permission;

/// A named, local protection policy.
///
/// The simple [`Self::new`] constructor creates a policy with no handler or
/// recipients, suitable for the permission/expiry model below. The
/// [`Self::with_protection`] / [`Self::with_protection_id`] constructors create
/// a complete protection policy with a handler, key length, and recipients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionPolicy {
    name: String,
    permissions: Permission,
    expiry: Option<i64>,
    watermark: bool,
    handler: SecurityHandler,
    key_length_bits: u16,
    recipients: Vec<String>,
}

impl ProtectionPolicy {
    /// Create a policy with no expiry, handler, or recipients.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PolicyEmptyName`] when `name` is empty.
    pub fn new(name: impl Into<String>, permissions: Permission, watermark: bool) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(SecurityError::PolicyEmptyName);
        }
        Ok(Self {
            name,
            permissions,
            expiry: None,
            watermark,
            handler: SecurityHandler::None,
            key_length_bits: 0,
            recipients: Vec::new(),
        })
    }

    /// Create a complete protection policy, validating its invariants.
    ///
    /// # Errors
    ///
    /// - [`SecurityError::PolicyEmptyName`] for an empty policy name.
    /// - [`SecurityError::PolicyNoRecipients`] / [`SecurityError::PolicyEmptyRecipient`]
    ///   / [`SecurityError::PolicyDuplicateRecipient`] for recipient problems.
    /// - [`SecurityError::PolicyInvalidKeyLength`] when the key length does not
    ///   match the handler.
    pub fn with_protection(
        name: impl Into<String>,
        handler: SecurityHandler,
        key_length_bits: u16,
        permissions: Permission,
        recipients: Vec<String>,
    ) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(SecurityError::PolicyEmptyName);
        }
        let policy = Self {
            name,
            permissions,
            expiry: None,
            watermark: false,
            handler,
            key_length_bits,
            recipients,
        };
        validate_policy_spec(&policy)?;
        Ok(policy)
    }

    /// Create a complete protection policy from a handler string identifier.
    ///
    /// # Errors
    ///
    /// [`SecurityError::PolicyUnknownHandler`] when `handler_id` is not a known
    /// security-handler identifier; otherwise the same errors as
    /// [`Self::with_protection`].
    pub fn with_protection_id(
        name: impl Into<String>,
        handler_id: &str,
        key_length_bits: u16,
        permissions: Permission,
        recipients: Vec<String>,
    ) -> Result<Self> {
        let handler = parse_handler(handler_id)
            .ok_or_else(|| SecurityError::PolicyUnknownHandler(handler_id.to_owned()))?;
        Self::with_protection(name, handler, key_length_bits, permissions, recipients)
    }

    /// Attach an expiry time (a caller-interpreted timestamp).
    pub fn with_expiry(mut self, expiry: i64) -> Self {
        self.expiry = Some(expiry);
        self
    }

    /// Set or clear the watermark requirement.
    pub fn with_watermark(mut self, watermark: bool) -> Self {
        self.watermark = watermark;
        self
    }

    /// Policy name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Permissions granted by this policy.
    pub fn permissions(&self) -> Permission {
        self.permissions
    }

    /// Expiry timestamp, if any.
    pub fn expiry(&self) -> Option<i64> {
        self.expiry
    }

    /// Whether this policy requires a watermark.
    pub fn watermark(&self) -> bool {
        self.watermark
    }

    /// Return `true` when `operation` is permitted by this policy.
    pub fn allows(&self, operation: Permission) -> bool {
        self.permissions.contains(operation)
    }

    /// Security handler this policy selects.
    pub fn handler(&self) -> SecurityHandler {
        self.handler
    }

    /// Key length in bits this policy requires.
    pub fn key_length_bits(&self) -> u16 {
        self.key_length_bits
    }

    /// Recipient labels addressed by this policy, in insertion order.
    pub fn recipient_labels(&self) -> &[String] {
        &self.recipients
    }
}

/// One deterministic step of a policy plan.
///
/// Declaration order is the canonical order used by plans and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PolicyAction {
    /// Apply the policy's permission mask.
    SetPermissions,
    /// Apply a visible watermark.
    ApplyWatermark,
    /// Evaluate the policy's expiry at the supplied time.
    CheckExpiry,
}

impl PolicyAction {
    /// Stable string identifier for persistence or comparison.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::SetPermissions => "set_permissions",
            Self::ApplyWatermark => "apply_watermark",
            Self::CheckExpiry => "check_expiry",
        }
    }
}

/// A deterministic, ordered plan derived from a [`ProtectionPolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPlan {
    actions: Vec<PolicyAction>,
}

impl PolicyPlan {
    /// The plan's actions in canonical order.
    pub fn actions(&self) -> &[PolicyAction] {
        &self.actions
    }

    /// Return `true` when the plan contains `action`.
    pub fn contains(&self, action: PolicyAction) -> bool {
        self.actions.contains(&action)
    }
}

/// Build the deterministic plan for a policy.
///
/// The plan always sets permissions, adds a watermark step only when the policy
/// requests one, and adds an expiry check only when the policy has an expiry.
pub fn build_policy_plan(policy: &ProtectionPolicy) -> PolicyPlan {
    let mut actions = vec![PolicyAction::SetPermissions];
    if policy.watermark() {
        actions.push(PolicyAction::ApplyWatermark);
    }
    if policy.expiry().is_some() {
        actions.push(PolicyAction::CheckExpiry);
    }
    actions.sort();
    PolicyPlan { actions }
}

/// Outcome of validating a policy at a caller-supplied time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyStatus {
    /// The policy is currently in force.
    Compliant,
    /// The policy has expired.
    Expired,
}

/// A deterministic report describing one policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReport {
    /// Policy name.
    pub name: String,
    /// Raw permission bits granted by the policy.
    pub permission_bits: u32,
    /// Whether the policy had an expiry configured.
    pub has_expiry: bool,
    /// Whether the policy was past its expiry at the evaluation time.
    pub expired: bool,
    /// Whether the policy requires a watermark.
    pub watermark: bool,
    /// The plan whose actions were evaluated, in canonical order.
    pub actions: Vec<PolicyAction>,
    /// Overall status.
    pub status: PolicyStatus,
}

impl PolicyReport {
    /// Return `true` when the policy is currently in force.
    pub fn is_compliant(&self) -> bool {
        self.status == PolicyStatus::Compliant
    }
}

fn report_for(policy: &ProtectionPolicy, now: i64) -> PolicyReport {
    let plan = build_policy_plan(policy);
    let expired = policy.expiry().is_some_and(|expiry| now >= expiry);
    PolicyReport {
        name: policy.name().to_owned(),
        permission_bits: policy.permissions().bits(),
        has_expiry: policy.expiry().is_some(),
        expired,
        watermark: policy.watermark(),
        actions: plan.actions().to_vec(),
        status: if expired {
            PolicyStatus::Expired
        } else {
            PolicyStatus::Compliant
        },
    }
}

/// Validate a policy at time `now` without failing on expiry.
///
/// The returned report is fully determined by the policy and `now`; no clock is
/// read. An expired policy yields a [`PolicyStatus::Expired`] report.
pub fn validate_policy(policy: &ProtectionPolicy, now: i64) -> PolicyReport {
    report_for(policy, now)
}

/// Enforce a policy at time `now`.
///
/// # Errors
///
/// Returns [`SecurityError::PolicyExpired`] when `now` is at or past the
/// policy's expiry.
pub fn enforce_policy(policy: &ProtectionPolicy, now: i64) -> Result<PolicyReport> {
    let report = report_for(policy, now);
    if report.expired {
        return Err(SecurityError::PolicyExpired(policy.name().to_owned()));
    }
    Ok(report)
}

/// Ensure a policy grants `operation`, otherwise return a typed error.
///
/// # Errors
///
/// Returns [`SecurityError::PolicyPermissionDenied`] when the permission is
/// absent.
pub fn check_permission(policy: &ProtectionPolicy, operation: Permission) -> Result<()> {
    if policy.allows(operation) {
        Ok(())
    } else {
        Err(SecurityError::PolicyPermissionDenied("operation"))
    }
}

/// Parse a security handler from one of its stable string identifiers.
fn parse_handler(id: &str) -> Option<SecurityHandler> {
    SecurityHandler::ALL
        .iter()
        .copied()
        .find(|handler| handler.as_id() == id)
}

/// Key length in bits required by a handler.
fn expected_key_length(handler: SecurityHandler) -> u16 {
    match handler {
        SecurityHandler::None => 0,
        SecurityHandler::StandardR2 => 40,
        SecurityHandler::StandardR3 | SecurityHandler::StandardR4 => 128,
        SecurityHandler::StandardR6 => 256,
    }
}

/// The successful result of validating a protection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyValidationReport {
    /// Policy name.
    pub name: String,
    /// Stable handler identifier.
    pub handler_id: &'static str,
    /// Key length in bits.
    pub key_length_bits: u16,
    /// Number of recipients.
    pub recipient_count: usize,
}

/// Validate the handler/key-length/recipient invariants of a protection policy.
///
/// This is idempotent and reads no clock. It reports only labels, identifiers,
/// and counts — never key material or document contents.
///
/// # Errors
///
/// - [`SecurityError::PolicyEmptyName`] for an empty policy or recipient label.
/// - [`SecurityError::PolicyNoRecipients`], [`SecurityError::PolicyEmptyRecipient`],
///   or [`SecurityError::PolicyDuplicateRecipient`] for recipient problems.
/// - [`SecurityError::PolicyInvalidKeyLength`] when the key length does not
///   match the selected handler.
pub fn validate_policy_spec(policy: &ProtectionPolicy) -> Result<PolicyValidationReport> {
    if policy.name.is_empty() {
        return Err(SecurityError::PolicyEmptyName);
    }
    if policy.recipients.is_empty() {
        return Err(SecurityError::PolicyNoRecipients);
    }
    let mut seen = BTreeSet::new();
    for label in &policy.recipients {
        if label.is_empty() {
            return Err(SecurityError::PolicyEmptyRecipient);
        }
        if !seen.insert(label.as_str()) {
            return Err(SecurityError::PolicyDuplicateRecipient(label.clone()));
        }
    }
    if policy.key_length_bits != expected_key_length(policy.handler) {
        return Err(SecurityError::PolicyInvalidKeyLength(
            policy.key_length_bits,
        ));
    }
    Ok(PolicyValidationReport {
        name: policy.name().to_owned(),
        handler_id: policy.handler().as_id(),
        key_length_bits: policy.key_length_bits(),
        recipient_count: policy.recipients.len(),
    })
}

/// One step of a protection-application plan, in canonical order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtectionAction {
    /// Validate the policy invariants.
    ValidatePolicy,
    /// Apply the permission mask.
    SetPermissions,
    /// Wrap the content key once per recipient.
    WrapRecipientKeys,
    /// Apply a visible watermark.
    ApplyWatermark,
    /// Evaluate the policy expiry at the supplied time.
    CheckExpiry,
}

impl ProtectionAction {
    /// Stable string identifier for persistence or comparison.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::ValidatePolicy => "validate_policy",
            Self::SetPermissions => "set_permissions",
            Self::WrapRecipientKeys => "wrap_recipient_keys",
            Self::ApplyWatermark => "apply_watermark",
            Self::CheckExpiry => "check_expiry",
        }
    }
}

/// A deterministic, ordered plan derived from a protection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionPlan {
    actions: Vec<ProtectionAction>,
}

impl ProtectionPlan {
    /// Plan actions in canonical order.
    pub fn actions(&self) -> &[ProtectionAction] {
        &self.actions
    }

    /// Return `true` when the plan contains `action`.
    pub fn contains(&self, action: ProtectionAction) -> bool {
        self.actions.contains(&action)
    }
}

/// Build the deterministic protection plan for a valid policy.
///
/// The plan always validates, sets permissions, and wraps recipient keys. A
/// watermark step is added only when requested, and an expiry check only when
/// the policy has an expiry. Steps are returned in canonical order.
///
/// # Errors
///
/// Any [`validate_policy_spec`] error for an invalid policy.
pub fn build_protection_plan(policy: &ProtectionPolicy) -> Result<ProtectionPlan> {
    validate_policy_spec(policy)?;
    let mut actions = vec![
        ProtectionAction::ValidatePolicy,
        ProtectionAction::SetPermissions,
        ProtectionAction::WrapRecipientKeys,
    ];
    if policy.watermark() {
        actions.push(ProtectionAction::ApplyWatermark);
    }
    if policy.expiry().is_some() {
        actions.push(ProtectionAction::CheckExpiry);
    }
    actions.sort();
    Ok(ProtectionPlan { actions })
}

/// A deterministic report describing one protection-policy application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionApplyReport {
    /// Policy name.
    pub name: String,
    /// Stable handler identifier.
    pub handler_id: &'static str,
    /// Key length in bits.
    pub key_length_bits: u16,
    /// Raw permission bits granted by the policy.
    pub permission_bits: u32,
    /// Recipient labels in canonical (lexicographic) order.
    pub recipients: Vec<String>,
    /// The evaluated plan in canonical order.
    pub actions: Vec<ProtectionAction>,
    /// Caller-supplied time at which the plan was applied.
    pub evaluated_at: i64,
    /// Whether the policy had expired at `evaluated_at`.
    pub expired: bool,
}

/// Build and apply a protection plan at a caller-supplied time.
///
/// The report is fully determined by the policy and `now`; no clock is read.
///
/// # Errors
///
/// Any [`validate_policy_spec`] error for an invalid policy, or
/// [`SecurityError::PolicyExpired`] when `now` is at or past the expiry.
pub fn apply_protection_policy(
    policy: &ProtectionPolicy,
    now: i64,
) -> Result<ProtectionApplyReport> {
    let plan = build_protection_plan(policy)?;
    let expired = policy.expiry().is_some_and(|expiry| now >= expiry);
    if expired {
        return Err(SecurityError::PolicyExpired(policy.name().to_owned()));
    }
    let mut recipients = policy.recipient_labels().to_vec();
    recipients.sort();
    Ok(ProtectionApplyReport {
        name: policy.name().to_owned(),
        handler_id: policy.handler().as_id(),
        key_length_bits: policy.key_length_bits(),
        permission_bits: policy.permissions().bits(),
        recipients,
        actions: plan.actions().to_vec(),
        evaluated_at: now,
        expired,
    })
}

/// A collection of named local policies with deterministic lookups.
#[derive(Debug, Clone, Default)]
pub struct PolicySet {
    policies: BTreeMap<String, ProtectionPolicy>,
}

impl PolicySet {
    /// Create an empty policy set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a policy, rejecting empty names and duplicate names.
    ///
    /// # Errors
    ///
    /// [`SecurityError::PolicyEmptyName`] or
    /// [`SecurityError::PolicyDuplicateName`].
    pub fn add(&mut self, policy: ProtectionPolicy) -> Result<()> {
        if policy.name().is_empty() {
            return Err(SecurityError::PolicyEmptyName);
        }
        if self.policies.contains_key(policy.name()) {
            return Err(SecurityError::PolicyDuplicateName(policy.name().to_owned()));
        }
        self.policies.insert(policy.name().to_owned(), policy);
        Ok(())
    }

    /// Look up a policy by name.
    pub fn get(&self, name: &str) -> Option<&ProtectionPolicy> {
        self.policies.get(name)
    }

    /// All policy names in deterministic (lexicographic) order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.policies.keys().map(String::as_str)
    }

    /// Number of policies.
    pub fn len(&self) -> usize {
        self.policies.len()
    }

    /// Return `true` when no policies are stored.
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Enforce the named policy at time `now`.
    ///
    /// # Errors
    ///
    /// [`SecurityError::PolicyNotFound`] when no policy has that name, or
    /// [`SecurityError::PolicyExpired`] when it has expired.
    pub fn apply(&self, name: &str, now: i64) -> Result<PolicyReport> {
        let policy = self
            .get(name)
            .ok_or_else(|| SecurityError::PolicyNotFound(name.to_owned()))?;
        enforce_policy(policy, now)
    }

    /// Build and apply the protection plan of the named policy at time `now`.
    ///
    /// # Errors
    ///
    /// [`SecurityError::PolicyNotFound`] when no policy has that name, plus any
    /// error from [`apply_protection_policy`].
    pub fn apply_protection(&self, name: &str, now: i64) -> Result<ProtectionApplyReport> {
        let policy = self
            .get(name)
            .ok_or_else(|| SecurityError::PolicyNotFound(name.to_owned()))?;
        apply_protection_policy(policy, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ProtectionPolicy {
        ProtectionPolicy::new("internal", Permission::PRINT.union(Permission::COPY), true)
            .expect("valid policy")
            .with_expiry(1_000)
    }

    #[test]
    fn policy_plan_is_deterministic_and_in_canonical_order() {
        let plan = build_policy_plan(&policy());
        assert_eq!(
            plan.actions(),
            &[
                PolicyAction::SetPermissions,
                PolicyAction::ApplyWatermark,
                PolicyAction::CheckExpiry,
            ]
        );
        assert!(plan.actions().windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(plan.actions(), build_policy_plan(&policy()).actions());
    }

    #[test]
    fn plan_reflects_watermark_and_expiry_flags() {
        let plain = ProtectionPolicy::new("plain", Permission::PRINT, false).expect("policy");
        let plan = build_policy_plan(&plain);
        assert_eq!(plan.actions(), &[PolicyAction::SetPermissions]);
        assert!(!plan.contains(PolicyAction::ApplyWatermark));
        assert!(!plan.contains(PolicyAction::CheckExpiry));
        assert_eq!(PolicyAction::SetPermissions.as_id(), "set_permissions");
    }

    #[test]
    fn validate_policy_reports_compliant_and_expired_without_failing() {
        let policy = policy();
        let before = validate_policy(&policy, 999);
        assert!(before.is_compliant());
        assert!(!before.expired);
        assert!(before.has_expiry);
        assert!(before.watermark);
        let at_expiry = validate_policy(&policy, 1_000);
        assert_eq!(at_expiry.status, PolicyStatus::Expired);
        assert!(at_expiry.expired);
    }

    #[test]
    fn enforce_expired_policy_returns_typed_error() {
        let policy = policy();
        assert!(enforce_policy(&policy, 999).is_ok());
        assert_eq!(
            enforce_policy(&policy, 1_001),
            Err(SecurityError::PolicyExpired("internal".to_owned()))
        );
    }

    #[test]
    fn policy_report_is_deterministic() {
        let a = validate_policy(&policy(), 42);
        let b = validate_policy(&policy(), 42);
        assert_eq!(a, b);
        assert_eq!(a.name, "internal");
        assert_eq!(
            a.permission_bits,
            Permission::PRINT.union(Permission::COPY).bits()
        );
    }

    #[test]
    fn permission_check_is_a_typed_error() {
        let policy = policy();
        assert!(check_permission(&policy, Permission::COPY).is_ok());
        assert_eq!(
            check_permission(&policy, Permission::MODIFY),
            Err(SecurityError::PolicyPermissionDenied("operation"))
        );
    }

    #[test]
    fn policy_set_lookup_apply_and_name_validation() {
        let mut set = PolicySet::new();
        assert!(set.is_empty());
        set.add(policy()).expect("add");
        assert_eq!(set.len(), 1);
        assert_eq!(set.names().collect::<Vec<_>>(), vec!["internal"]);
        assert!(set.apply("internal", 999).is_ok());
        assert_eq!(
            set.apply("missing", 999),
            Err(SecurityError::PolicyNotFound("missing".to_owned()))
        );
        assert_eq!(
            set.add(policy()),
            Err(SecurityError::PolicyDuplicateName("internal".to_owned()))
        );
        assert_eq!(
            ProtectionPolicy::new("", Permission::PRINT, false),
            Err(SecurityError::PolicyEmptyName)
        );
    }

    // ---- SEC-011 completion: protection policies -------------------------

    fn protection_policy(handler_id: &str, bits: u16) -> ProtectionPolicy {
        ProtectionPolicy::with_protection_id(
            "corporate",
            handler_id,
            bits,
            Permission::PRINT.union(Permission::COPY),
            vec!["alice".into(), "bob".into()],
        )
        .expect("valid protection policy")
    }

    #[test]
    fn protection_policy_validates_handler_key_length_and_recipients() {
        let policy = protection_policy("standard-r4", 128);
        let report = validate_policy_spec(&policy).expect("valid");
        assert_eq!(report.name, "corporate");
        assert_eq!(report.handler_id, "standard-r4");
        assert_eq!(report.key_length_bits, 128);
        assert_eq!(report.recipient_count, 2);

        assert_eq!(
            ProtectionPolicy::with_protection_id(
                "p",
                "nonexistent-handler",
                128,
                Permission::PRINT,
                vec!["alice".into()]
            ),
            Err(SecurityError::PolicyUnknownHandler(
                "nonexistent-handler".to_owned()
            ))
        );
        assert_eq!(
            ProtectionPolicy::with_protection_id(
                "p",
                "standard-r6",
                128,
                Permission::PRINT,
                vec!["alice".into()]
            ),
            Err(SecurityError::PolicyInvalidKeyLength(128))
        );
        assert_eq!(
            ProtectionPolicy::with_protection_id(
                "p",
                "standard-r4",
                128,
                Permission::PRINT,
                Vec::new()
            ),
            Err(SecurityError::PolicyNoRecipients)
        );
        assert_eq!(
            ProtectionPolicy::with_protection_id(
                "p",
                "standard-r4",
                128,
                Permission::PRINT,
                vec!["alice".into(), "alice".into()]
            ),
            Err(SecurityError::PolicyDuplicateRecipient("alice".to_owned()))
        );
        assert_eq!(
            ProtectionPolicy::with_protection_id(
                "p",
                "standard-r4",
                128,
                Permission::PRINT,
                vec![String::new()]
            ),
            Err(SecurityError::PolicyEmptyRecipient)
        );
    }

    #[test]
    fn protection_policy_exposes_handler_key_length_and_recipients() {
        let policy = protection_policy("standard-r6", 256);
        assert_eq!(policy.handler(), SecurityHandler::StandardR6);
        assert_eq!(policy.key_length_bits(), 256);
        assert_eq!(
            policy.recipient_labels(),
            &["alice".to_owned(), "bob".to_owned()]
        );
        assert!(policy.allows(Permission::PRINT));
        assert!(!policy.allows(Permission::MODIFY));
    }

    #[test]
    fn protection_plan_is_deterministic_and_canonical() {
        let policy = protection_policy("standard-r4", 128).with_expiry(1_000);
        let plan = build_protection_plan(&policy).expect("plan");
        assert_eq!(
            plan.actions(),
            &[
                ProtectionAction::ValidatePolicy,
                ProtectionAction::SetPermissions,
                ProtectionAction::WrapRecipientKeys,
                ProtectionAction::CheckExpiry,
            ]
        );
        assert!(!plan.contains(ProtectionAction::ApplyWatermark));
        // A watermarked policy adds the watermark step in canonical order.
        let watermarked = protection_policy("standard-r4", 128)
            .with_watermark(true)
            .with_expiry(1_000);
        let wm_plan = build_protection_plan(&watermarked).expect("wm plan");
        assert_eq!(
            wm_plan.actions(),
            &[
                ProtectionAction::ValidatePolicy,
                ProtectionAction::SetPermissions,
                ProtectionAction::WrapRecipientKeys,
                ProtectionAction::ApplyWatermark,
                ProtectionAction::CheckExpiry,
            ]
        );
        // Recipient keys are always wrapped; the plan is stable across calls.
        assert!(plan.contains(ProtectionAction::WrapRecipientKeys));
        assert_eq!(
            ProtectionAction::WrapRecipientKeys.as_id(),
            "wrap_recipient_keys"
        );
        assert_eq!(plan, build_protection_plan(&policy).expect("plan again"));

        let plain = protection_policy("standard-r4", 128);
        let plain_plan = build_protection_plan(&plain).expect("plan");
        assert_eq!(
            plain_plan.actions(),
            &[
                ProtectionAction::ValidatePolicy,
                ProtectionAction::SetPermissions,
                ProtectionAction::WrapRecipientKeys,
            ]
        );
    }

    #[test]
    fn protection_apply_report_is_deterministic() {
        let policy = protection_policy("standard-r6", 256).with_expiry(2_000);
        let a = apply_protection_policy(&policy, 1_000).expect("apply");
        let b = apply_protection_policy(&policy, 1_000).expect("apply");
        assert_eq!(a, b);
        assert_eq!(a.name, "corporate");
        assert_eq!(a.handler_id, "standard-r6");
        assert_eq!(a.key_length_bits, 256);
        assert_eq!(a.recipients, vec!["alice", "bob"]);
        assert_eq!(a.evaluated_at, 1_000);
        assert!(!a.expired);
        assert_eq!(
            a.actions,
            build_protection_plan(&policy)
                .expect("plan")
                .actions()
                .to_vec()
        );
    }

    #[test]
    fn protection_apply_rejects_expired_policy_with_typed_error() {
        let policy = protection_policy("standard-r4", 128).with_expiry(500);
        assert!(apply_protection_policy(&policy, 499).is_ok());
        assert_eq!(
            apply_protection_policy(&policy, 500),
            Err(SecurityError::PolicyExpired("corporate".to_owned()))
        );
    }

    #[test]
    fn policy_set_applies_protection_plan() {
        let mut set = PolicySet::new();
        set.add(protection_policy("standard-r4", 128).with_expiry(1_000))
            .expect("add");
        let report = set.apply_protection("corporate", 999).expect("apply");
        assert_eq!(report.handler_id, "standard-r4");
        assert!(report.actions.contains(&ProtectionAction::CheckExpiry));
        assert_eq!(
            set.apply_protection("missing", 999),
            Err(SecurityError::PolicyNotFound("missing".to_owned()))
        );
    }
}
