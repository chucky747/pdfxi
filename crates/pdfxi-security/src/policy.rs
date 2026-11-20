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

use std::collections::BTreeMap;

use crate::error::{Result, SecurityError};
use crate::permissions::Permission;

/// A named, local protection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionPolicy {
    name: String,
    permissions: Permission,
    expiry: Option<i64>,
    watermark: bool,
}

impl ProtectionPolicy {
    /// Create a policy with no expiry.
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
        })
    }

    /// Attach an expiry time (a caller-interpreted timestamp).
    pub fn with_expiry(mut self, expiry: i64) -> Self {
        self.expiry = Some(expiry);
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
}
