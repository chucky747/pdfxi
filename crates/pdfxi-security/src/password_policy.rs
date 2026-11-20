//! End-to-end password and permission planning (SEC-001).
//!
//! This module closes the "open password and permissions password" row at the
//! library level. It turns a [password configuration](PasswordPlanInput) into a
//! secret-free [`EncryptionPlan`] that reports exactly which standard security
//! handler, revision, and key length would be used, and it resolves a supplied
//! open credential into [`Permissions`] without ever widening the user role.
//!
//! # Semantics
//!
//! - The **user (open) password** grants the role [`PasswordRole::User`], whose
//!   rights are the configured permission mask.
//! - The **owner (permissions) password** grants [`PasswordRole::Owner`], which
//!   bypasses the mask. It is required whenever permissions are restricted or
//!   the user password is empty; otherwise an opener could claim owner rights
//!   and silently widen permissions.
//! - An **empty user password** is allowed, but it can only ever open the
//!   document as a user. The plan never elevates an empty credential to owner,
//!   and [`resolve_open_credentials`] rejects any such result with
//!   [`SecurityError::PermissionWideningRejected`].
//! - Conflicting states (identical user/owner passwords, restricted
//!   permissions without an owner password, passwords with no encryption, a
//!   fully empty encrypted configuration) are rejected with
//!   [`SecurityError::PasswordConflict`] before a plan is produced.
//!
//! No password bytes are stored in an [`EncryptionPlan`] or printed by any
//! `Debug` implementation, so plans are safe to log or persist.

use std::fmt;

use crate::encryption::EncryptionMethod;
use crate::error::{Result, SecurityError};
use crate::permissions::{PasswordRole, Permission, Permissions};

/// The two passwords associated with a secured document.
///
/// The `Debug` implementation is redacted: it reports only whether each
/// password is empty, never its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordCredentials {
    user: Vec<u8>,
    owner: Vec<u8>,
}

impl PasswordCredentials {
    /// Build a credential pair from raw password bytes.
    pub fn new(user: impl Into<Vec<u8>>, owner: impl Into<Vec<u8>>) -> Self {
        Self {
            user: user.into(),
            owner: owner.into(),
        }
    }

    /// Build a credential pair from string passwords.
    pub fn from_strings(user: &str, owner: &str) -> Self {
        Self::new(user.as_bytes().to_vec(), owner.as_bytes().to_vec())
    }

    /// The user (open) password bytes.
    pub fn user(&self) -> &[u8] {
        &self.user
    }

    /// The owner (permissions) password bytes.
    pub fn owner(&self) -> &[u8] {
        &self.owner
    }

    /// Whether the user password is empty.
    pub fn user_is_empty(&self) -> bool {
        self.user.is_empty()
    }

    /// Whether an owner password is present.
    pub fn owner_is_present(&self) -> bool {
        !self.owner.is_empty()
    }
}

impl fmt::Debug for PasswordCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordCredentials")
            .field("user_empty", &self.user.is_empty())
            .field("owner_present", &!self.owner.is_empty())
            .finish()
    }
}

/// The standard security handler a plan selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecurityHandler {
    /// No security handler; content is plaintext.
    None,
    /// Standard handler, revision 2 (RC4-40).
    StandardR2,
    /// Standard handler, revision 3 (RC4-128).
    StandardR3,
    /// Standard handler, revision 4 (AES-128).
    StandardR4,
    /// Standard handler, revision 6 (AES-256).
    StandardR6,
}

impl SecurityHandler {
    /// Every handler in a stable order.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::StandardR2,
        Self::StandardR3,
        Self::StandardR4,
        Self::StandardR6,
    ];

    /// Security revision number (`0` for no handler).
    pub const fn revision(self) -> u8 {
        match self {
            Self::None => 0,
            Self::StandardR2 => 2,
            Self::StandardR3 => 3,
            Self::StandardR4 => 4,
            Self::StandardR6 => 6,
        }
    }

    /// Stable string identifier.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::StandardR2 => "standard-r2",
            Self::StandardR3 => "standard-r3",
            Self::StandardR4 => "standard-r4",
            Self::StandardR6 => "standard-r6",
        }
    }
}

impl fmt::Display for SecurityHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// Validated input to encryption planning.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordPlanInput {
    method: EncryptionMethod,
    credentials: PasswordCredentials,
    permissions: Permission,
}

impl PasswordPlanInput {
    /// Build a plan input from its parts.
    pub fn new(
        method: EncryptionMethod,
        credentials: PasswordCredentials,
        permissions: Permission,
    ) -> Self {
        Self {
            method,
            credentials,
            permissions,
        }
    }

    /// Build a plan input granting all permissions (no restrictions).
    pub fn unrestricted(method: EncryptionMethod, credentials: PasswordCredentials) -> Self {
        Self::new(method, credentials, Permission::ALL)
    }

    /// Selected encryption method.
    pub const fn method(&self) -> EncryptionMethod {
        self.method
    }

    /// Password credentials.
    pub fn credentials(&self) -> &PasswordCredentials {
        &self.credentials
    }

    /// Requested user-role permission mask.
    pub const fn permissions(&self) -> Permission {
        self.permissions
    }
}

impl fmt::Debug for PasswordPlanInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordPlanInput")
            .field("method", &self.method)
            .field("credentials", &self.credentials)
            .field("permissions", &self.permissions)
            .finish()
    }
}

/// A secret-free description of how a document would be encrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionPlan {
    method: EncryptionMethod,
    handler: SecurityHandler,
    key_length_bits: u16,
    permissions: Permission,
    user_password_empty: bool,
    owner_password_present: bool,
}

impl EncryptionPlan {
    /// Encryption method that will be used.
    pub const fn method(&self) -> EncryptionMethod {
        self.method
    }

    /// Standard security handler that will be used.
    pub const fn handler(&self) -> SecurityHandler {
        self.handler
    }

    /// Effective standard-security revision.
    pub const fn revision(&self) -> u8 {
        self.handler.revision()
    }

    /// Key length in bits (`0` when unencrypted).
    pub const fn key_length_bits(&self) -> u16 {
        self.key_length_bits
    }

    /// Configured user-role permission mask.
    pub const fn permissions(&self) -> Permission {
        self.permissions
    }

    /// Whether the plan relies on an empty user password.
    pub const fn user_password_empty(&self) -> bool {
        self.user_password_empty
    }

    /// Whether an owner password is present.
    pub const fn owner_password_present(&self) -> bool {
        self.owner_password_present
    }

    /// Rights of a session that opened with the user password.
    ///
    /// This is always the configured mask under the [`PasswordRole::User`]
    /// role; an empty user password does not elevate it.
    pub const fn user_permissions(&self) -> Permissions {
        Permissions::new(self.permissions, PasswordRole::User)
    }

    /// Rights of a session that opened with the owner password.
    pub const fn owner_permissions(&self) -> Permissions {
        Permissions::new(Permission::ALL, PasswordRole::Owner)
    }
}

/// Map an encryption method to its standard handler, revision, and key length.
fn handler_for(method: EncryptionMethod) -> (SecurityHandler, u16) {
    match method {
        EncryptionMethod::None => (SecurityHandler::None, 0),
        EncryptionMethod::Rc4_40 => (SecurityHandler::StandardR2, 40),
        EncryptionMethod::Rc4_128 => (SecurityHandler::StandardR3, 128),
        EncryptionMethod::Aes128 => (SecurityHandler::StandardR4, 128),
        EncryptionMethod::Aes256 => (SecurityHandler::StandardR6, 256),
    }
}

/// Validate `input` and produce a secret-free encryption plan.
///
/// # Errors
///
/// - [`SecurityError::InvalidPermissionBits`] when the mask has unknown bits.
/// - [`SecurityError::PasswordConflict`] for any contradictory password state.
pub fn plan_encryption(input: &PasswordPlanInput) -> Result<EncryptionPlan> {
    let bits = input.permissions.bits();
    if bits & !Permission::ALL.bits() != 0 {
        return Err(SecurityError::InvalidPermissionBits(bits));
    }

    let (handler, key_length_bits) = handler_for(input.method);
    let user_empty = input.credentials.user_is_empty();
    let owner_present = input.credentials.owner_is_present();
    let restricted = bits != Permission::ALL.bits();

    if !input.method.is_encrypted() {
        if !user_empty || owner_present {
            return Err(SecurityError::PasswordConflict(
                "an unencrypted document cannot carry passwords",
            ));
        }
        if restricted {
            return Err(SecurityError::PasswordConflict(
                "an unencrypted document cannot restrict permissions",
            ));
        }
    } else {
        if user_empty && !owner_present {
            return Err(SecurityError::PasswordConflict(
                "an encrypted document requires at least one non-empty password",
            ));
        }
        if !owner_present && restricted {
            return Err(SecurityError::PasswordConflict(
                "restricted permissions require an owner password",
            ));
        }
        if !user_empty && input.credentials.user() == input.credentials.owner() {
            return Err(SecurityError::PasswordConflict(
                "user and owner passwords must differ",
            ));
        }
    }

    Ok(EncryptionPlan {
        method: input.method,
        handler,
        key_length_bits,
        permissions: input.permissions,
        user_password_empty: user_empty,
        owner_password_present: owner_present,
    })
}

/// Result of authenticating a supplied open credential against a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordResolution {
    /// Role granted to the credential.
    pub role: PasswordRole,
    /// Effective rights for the session.
    pub permissions: Permissions,
    /// Whether the credential opened the document with owner rights.
    pub is_owner: bool,
}

/// Result of applying a validated plan to a supplied open credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPasswordPlan {
    /// The secret-free plan that was applied.
    pub plan: EncryptionPlan,
    /// The authenticated resolution.
    pub resolution: PasswordResolution,
}

/// Authenticate `supplied` and return the exact rights it grants.
///
/// The plan is recomputed from `input` so a caller cannot bypass conflict
/// validation by constructing an [`EncryptionPlan`] by hand (its fields are
/// private in any case).
///
/// # Errors
///
/// - [`SecurityError::PasswordConflict`] / [`SecurityError::InvalidPermissionBits`]
///   when `input` is not a valid configuration.
/// - [`SecurityError::WrongPassword`] when `supplied` matches neither password.
/// - [`SecurityError::PermissionWideningRejected`] if an empty credential would
///   ever resolve to owner rights.
pub fn resolve_open_credentials(
    input: &PasswordPlanInput,
    supplied: &[u8],
) -> Result<PasswordResolution> {
    let plan = plan_encryption(input)?;

    if !plan.method.is_encrypted() {
        if supplied.is_empty() {
            return Ok(PasswordResolution {
                role: PasswordRole::User,
                permissions: Permissions::new(Permission::ALL, PasswordRole::User),
                is_owner: false,
            });
        }
        return Err(SecurityError::WrongPassword);
    }

    if plan.owner_password_present && supplied == input.credentials.owner() {
        if supplied.is_empty() {
            return Err(SecurityError::PermissionWideningRejected(
                "an empty password cannot grant owner rights",
            ));
        }
        return Ok(PasswordResolution {
            role: PasswordRole::Owner,
            permissions: plan.owner_permissions(),
            is_owner: true,
        });
    }

    if supplied == input.credentials.user() {
        return Ok(PasswordResolution {
            role: PasswordRole::User,
            permissions: plan.user_permissions(),
            is_owner: false,
        });
    }

    Err(SecurityError::WrongPassword)
}

/// Validate a configuration, plan it, and authenticate `supplied`.
///
/// # Errors
///
/// See [`plan_encryption`] and [`resolve_open_credentials`].
pub fn apply_password_plan(
    input: &PasswordPlanInput,
    supplied: &[u8],
) -> Result<AppliedPasswordPlan> {
    let plan = plan_encryption(input)?;
    let resolution = resolve_open_credentials(input, supplied)?;
    Ok(AppliedPasswordPlan { plan, resolution })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restricted() -> Permission {
        Permission::PRINT.union(Permission::COPY)
    }

    #[test]
    fn handler_and_key_length_are_reported_for_every_method() {
        let cases = [
            (EncryptionMethod::None, SecurityHandler::None, 0u16, 0u8),
            (EncryptionMethod::Rc4_40, SecurityHandler::StandardR2, 40, 2),
            (
                EncryptionMethod::Rc4_128,
                SecurityHandler::StandardR3,
                128,
                3,
            ),
            (
                EncryptionMethod::Aes128,
                SecurityHandler::StandardR4,
                128,
                4,
            ),
            (
                EncryptionMethod::Aes256,
                SecurityHandler::StandardR6,
                256,
                6,
            ),
        ];
        for (method, handler, bits, revision) in cases {
            let creds = if method.is_encrypted() {
                PasswordCredentials::from_strings("user", "owner")
            } else {
                PasswordCredentials::from_strings("", "")
            };
            let plan = plan_encryption(&PasswordPlanInput::unrestricted(method, creds))
                .expect("valid configuration");
            assert_eq!(plan.method(), method);
            assert_eq!(plan.handler(), handler);
            assert_eq!(plan.handler().as_id(), handler.as_id());
            assert_eq!(plan.key_length_bits(), bits);
            assert_eq!(plan.revision(), revision);
        }
        assert_eq!(EncryptionMethod::Aes256.accepts_key_length(256), Ok(()));
    }

    #[test]
    fn user_and_owner_roles_have_distinct_rights() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("user", "owner"),
            restricted(),
        );
        let plan = plan_encryption(&input).expect("plan");
        assert_eq!(plan.user_permissions().role(), PasswordRole::User);
        assert!(!plan.user_permissions().allows(Permission::MODIFY));
        assert!(plan.user_permissions().allows(Permission::PRINT));
        assert_eq!(plan.owner_permissions().role(), PasswordRole::Owner);
        assert!(plan.owner_permissions().allows(Permission::MODIFY));
    }

    #[test]
    fn empty_user_password_does_not_widen_permissions() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("", "owner"),
            restricted(),
        );
        let plan = plan_encryption(&input).expect("permissions-password plan");
        assert!(plan.user_password_empty());
        assert!(plan.owner_password_present());
        let empty_open = resolve_open_credentials(&input, b"").expect("empty opens as user");
        assert_eq!(empty_open.role, PasswordRole::User);
        assert!(!empty_open.is_owner);
        assert_eq!(empty_open.permissions, plan.user_permissions());
        assert!(!empty_open.permissions.allows(Permission::MODIFY));
        assert!(!empty_open.permissions.allows(Permission::ASSEMBLE));
    }

    #[test]
    fn empty_user_password_without_owner_password_is_rejected() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes128,
            PasswordCredentials::from_strings("", ""),
            restricted(),
        );
        assert_eq!(
            plan_encryption(&input),
            Err(SecurityError::PasswordConflict(
                "an encrypted document requires at least one non-empty password"
            ))
        );
    }

    #[test]
    fn restricted_permissions_without_owner_password_are_rejected() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes128,
            PasswordCredentials::from_strings("user", ""),
            restricted(),
        );
        assert!(matches!(
            plan_encryption(&input),
            Err(SecurityError::PasswordConflict(_))
        ));
    }

    #[test]
    fn identical_user_and_owner_passwords_are_rejected() {
        let input = PasswordPlanInput::unrestricted(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("same", "same"),
        );
        assert!(matches!(
            plan_encryption(&input),
            Err(SecurityError::PasswordConflict(_))
        ));
    }

    #[test]
    fn passwords_without_encryption_are_rejected() {
        let input = PasswordPlanInput::unrestricted(
            EncryptionMethod::None,
            PasswordCredentials::from_strings("", "owner"),
        );
        assert!(matches!(
            plan_encryption(&input),
            Err(SecurityError::PasswordConflict(_))
        ));
        let restricted_plain = PasswordPlanInput::new(
            EncryptionMethod::None,
            PasswordCredentials::from_strings("", ""),
            restricted(),
        );
        assert!(matches!(
            plan_encryption(&restricted_plain),
            Err(SecurityError::PasswordConflict(_))
        ));
    }

    #[test]
    fn invalid_permission_bits_are_typed() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("user", "owner"),
            Permission::from_bits_unchecked(1 << 31),
        );
        assert!(matches!(
            plan_encryption(&input),
            Err(SecurityError::InvalidPermissionBits(_))
        ));
    }

    #[test]
    fn wrong_and_missing_passwords_are_typed() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("user", "owner"),
            restricted(),
        );
        assert_eq!(
            resolve_open_credentials(&input, b"nope"),
            Err(SecurityError::WrongPassword)
        );
        assert_eq!(
            resolve_open_credentials(&input, b""),
            Err(SecurityError::WrongPassword)
        );
    }

    #[test]
    fn owner_password_bypasses_the_mask_and_apply_reports_handler() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("user", "owner"),
            restricted(),
        );
        let applied = apply_password_plan(&input, b"owner").expect("owner opens");
        assert_eq!(applied.plan.handler(), SecurityHandler::StandardR6);
        assert_eq!(applied.plan.key_length_bits(), 256);
        assert!(applied.resolution.is_owner);
        assert!(applied.resolution.permissions.allows(Permission::ASSEMBLE));
    }

    #[test]
    fn plan_and_credentials_never_print_secrets() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("TOP_SECRET_USER", "TOP_SECRET_OWNER"),
            restricted(),
        );
        let plan = plan_encryption(&input).expect("plan");
        let debug = format!("{:?}\n{:?}\n{:?}", input, input.credentials(), plan);
        assert!(!debug.contains("TOP_SECRET"));
        assert_eq!(
            plan,
            plan_encryption(&input).expect("deterministic plan"),
            "planning is deterministic"
        );
    }
}
