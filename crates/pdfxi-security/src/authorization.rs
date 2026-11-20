//! Explicit authorization decisions for an unlocked security session (SEC-001).
//!
//! [`Permissions`] records *what* a session may do; [`Authorization`] records
//! *how* it was unlocked and exposes named decisions such as [`Authorization::can_print`]
//! and [`Authorization::can_copy`]. Every decision is a pure function of the
//! [`AuthorizationPath`] and the permission mask, so it is deterministic,
//! auditable, and never consults a password or any document content.
//!
//! An [`AuthorizationPath::OwnerPassword`] session bypasses the `/P` mask,
//! matching PDF owner-password semantics. A
//! [`AuthorizationPath::UserPassword`] session is bound by the mask, and an
//! empty user password can only ever produce that path — never owner rights.

use std::fmt;

use crate::error::{Result, SecurityError};
use crate::password_policy::{AppliedPasswordPlan, PasswordResolution};
use crate::permissions::{PasswordRole, Permission, Permissions};

/// The path by which a security session obtained access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorizationPath {
    /// The document was not encrypted, so no credential was required.
    Unencrypted,
    /// The user (open) password was used; the `/P` mask applies.
    UserPassword,
    /// The owner (permissions) password was used; all operations are allowed.
    OwnerPassword,
}

impl AuthorizationPath {
    /// Every path in a stable order, useful for exhaustive tests.
    pub const ALL: [Self; 3] = [Self::Unencrypted, Self::UserPassword, Self::OwnerPassword];

    /// Stable lowercase identifier suitable for logging.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Unencrypted => "unencrypted",
            Self::UserPassword => "user-password",
            Self::OwnerPassword => "owner-password",
        }
    }

    /// Short human-readable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unencrypted => "unencrypted",
            Self::UserPassword => "user password",
            Self::OwnerPassword => "owner password",
        }
    }

    /// Password role that this path must carry.
    const fn expected_role(self) -> PasswordRole {
        match self {
            Self::OwnerPassword => PasswordRole::Owner,
            Self::UserPassword | Self::Unencrypted => PasswordRole::User,
        }
    }
}

impl fmt::Display for AuthorizationPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// A named operation a caller may want to perform on a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Print the document.
    Print,
    /// Copy or extract text and graphics.
    Copy,
    /// Modify document contents.
    Modify,
    /// Add or edit annotations and form fields.
    Annotate,
    /// Fill in interactive form fields.
    FillForms,
    /// Extract content for accessibility tools.
    Accessibility,
    /// Assemble, insert, or rotate pages.
    Assemble,
    /// Print at high (full) quality.
    PrintHighQuality,
}

impl Operation {
    /// Every operation in a stable order, useful for exhaustive tests.
    pub const ALL: [Self; 8] = [
        Self::Print,
        Self::Copy,
        Self::Modify,
        Self::Annotate,
        Self::FillForms,
        Self::Accessibility,
        Self::Assemble,
        Self::PrintHighQuality,
    ];

    /// Stable lowercase identifier suitable for logging and typed errors.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Copy => "copy",
            Self::Modify => "modify",
            Self::Annotate => "annotate",
            Self::FillForms => "fill-forms",
            Self::Accessibility => "accessibility",
            Self::Assemble => "assemble",
            Self::PrintHighQuality => "print-high-quality",
        }
    }

    /// Human-readable label for user-facing messages.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Print => "printing",
            Self::Copy => "copying",
            Self::Modify => "modification",
            Self::Annotate => "annotation",
            Self::FillForms => "form filling",
            Self::Accessibility => "accessibility extraction",
            Self::Assemble => "page assembly",
            Self::PrintHighQuality => "high-quality printing",
        }
    }

    /// Permission bit that grants this operation to a user-role session.
    pub const fn permission(self) -> Permission {
        match self {
            Self::Print => Permission::PRINT,
            Self::Copy => Permission::COPY,
            Self::Modify => Permission::MODIFY,
            Self::Annotate => Permission::ANNOTATE,
            Self::FillForms => Permission::FILL_FORMS,
            Self::Accessibility => Permission::ACCESSIBILITY,
            Self::Assemble => Permission::ASSEMBLE,
            Self::PrintHighQuality => Permission::PRINT_HIGH_QUALITY,
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// An auditable authorization record for one unlocked session.
///
/// The record is secret-free: it holds only the [`AuthorizationPath`] and the
/// effective [`Permissions`], so it is safe to log or persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authorization {
    path: AuthorizationPath,
    permissions: Permissions,
}

impl Authorization {
    /// Build an authorization from a path and its effective permissions.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PasswordConflict`] when the path and the
    /// permission role disagree (for example an owner path carrying user
    /// rights).
    pub fn new(path: AuthorizationPath, permissions: Permissions) -> Result<Self> {
        if permissions.role() != path.expected_role() {
            return Err(SecurityError::PasswordConflict(
                "authorization path does not match the password role",
            ));
        }
        Ok(Self { path, permissions })
    }

    /// Derive an authorization from a resolved open credential.
    ///
    /// `encrypted` tells whether a security handler was active; an
    /// unencrypted session always takes the [`AuthorizationPath::Unencrypted`]
    /// path.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PasswordConflict`] if the resolution's role or
    /// owner flag is incompatible with that path.
    pub fn from_resolution(resolution: PasswordResolution, encrypted: bool) -> Result<Self> {
        let path = match (encrypted, resolution.is_owner) {
            (false, false) => AuthorizationPath::Unencrypted,
            (false, true) => {
                return Err(SecurityError::PasswordConflict(
                    "an unencrypted document has no owner credential",
                ));
            }
            (true, true) => AuthorizationPath::OwnerPassword,
            (true, false) => AuthorizationPath::UserPassword,
        };
        Self::new(path, resolution.permissions)
    }

    /// Derive an authorization from an applied password plan.
    ///
    /// This is the recommended entry point: the plan names the handler and key
    /// length, and the resolution names the role, so no caller has to decide
    /// which path applies.
    ///
    /// # Errors
    ///
    /// See [`Self::from_resolution`].
    pub fn from_applied(applied: &AppliedPasswordPlan) -> Result<Self> {
        Self::from_resolution(
            applied.resolution.clone(),
            applied.plan.method().is_encrypted(),
        )
    }

    /// Path that produced this authorization.
    pub const fn path(&self) -> AuthorizationPath {
        self.path
    }

    /// Password role backing this authorization.
    pub const fn role(&self) -> PasswordRole {
        self.permissions.role()
    }

    /// Effective permission mask of this session.
    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Whether this session was opened with owner rights.
    pub const fn is_owner(&self) -> bool {
        matches!(self.path, AuthorizationPath::OwnerPassword)
    }

    /// Decide a named operation for this session.
    ///
    /// Owner sessions allow every operation; user sessions consult the `/P`
    /// mask. The result depends only on the path and mask, so repeated calls
    /// are deterministic.
    pub const fn can(&self, operation: Operation) -> bool {
        self.permissions.allows(operation.permission())
    }

    /// Require a named operation, returning a typed error when it is denied.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PermissionDenied`] with the operation's stable
    /// identifier when the session does not grant it.
    pub fn require(&self, operation: Operation) -> Result<()> {
        if self.can(operation) {
            Ok(())
        } else {
            Err(SecurityError::PermissionDenied(operation.as_id()))
        }
    }

    /// Whether printing is allowed.
    pub const fn can_print(&self) -> bool {
        self.can(Operation::Print)
    }

    /// Whether copying or extracting content is allowed.
    pub const fn can_copy(&self) -> bool {
        self.can(Operation::Copy)
    }

    /// Whether modifying document contents is allowed.
    pub const fn can_modify(&self) -> bool {
        self.can(Operation::Modify)
    }

    /// Whether adding or editing annotations is allowed.
    pub const fn can_annotate(&self) -> bool {
        self.can(Operation::Annotate)
    }

    /// Whether filling interactive form fields is allowed.
    pub const fn can_fill_forms(&self) -> bool {
        self.can(Operation::FillForms)
    }

    /// Whether accessibly extracting content is allowed.
    pub const fn can_extract_for_accessibility(&self) -> bool {
        self.can(Operation::Accessibility)
    }

    /// Whether assembling or rotating pages is allowed.
    pub const fn can_assemble(&self) -> bool {
        self.can(Operation::Assemble)
    }

    /// Whether high-quality printing is allowed.
    pub const fn can_print_high_quality(&self) -> bool {
        self.can(Operation::PrintHighQuality)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::EncryptionMethod;
    use crate::password_policy::{PasswordCredentials, PasswordPlanInput, apply_password_plan};

    fn restricted_input() -> PasswordPlanInput {
        let mask = Permission::PRINT.union(Permission::COPY);
        PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("user", "owner"),
            mask,
        )
    }

    fn operation_for_id(id: &str) -> Operation {
        Operation::ALL
            .iter()
            .copied()
            .find(|operation| operation.as_id() == id)
            .unwrap_or(Operation::Print)
    }

    #[test]
    fn user_path_decisions_follow_the_mask() {
        let applied = apply_password_plan(&restricted_input(), b"user").expect("user opens");
        let auth = Authorization::from_applied(&applied).expect("authorization");
        assert_eq!(auth.path(), AuthorizationPath::UserPassword);
        assert_eq!(auth.role(), PasswordRole::User);
        assert!(!auth.is_owner());
        assert!(auth.can_print());
        assert!(auth.can_copy());
        assert!(!auth.can_modify());
        assert!(!auth.can_annotate());
        assert!(!auth.can_fill_forms());
        assert!(!auth.can_extract_for_accessibility());
        assert!(!auth.can_assemble());
        assert!(!auth.can_print_high_quality());
    }

    #[test]
    fn owner_path_widens_every_decision() {
        let applied = apply_password_plan(&restricted_input(), b"owner").expect("owner opens");
        let auth = Authorization::from_applied(&applied).expect("authorization");
        assert_eq!(auth.path(), AuthorizationPath::OwnerPassword);
        assert!(auth.is_owner());
        for operation in Operation::ALL {
            assert!(
                auth.can(operation),
                "{} should be allowed",
                operation.as_id()
            );
            assert_eq!(auth.require(operation), Ok(()));
        }
    }

    #[test]
    fn empty_user_password_never_reaches_the_owner_path() {
        let input = PasswordPlanInput::new(
            EncryptionMethod::Aes256,
            PasswordCredentials::from_strings("", "owner"),
            Permission::PRINT,
        );
        let applied = apply_password_plan(&input, b"").expect("empty opens as user");
        let auth = Authorization::from_applied(&applied).expect("authorization");
        assert_eq!(auth.path(), AuthorizationPath::UserPassword);
        assert!(!auth.can_modify());
        assert!(!auth.can_assemble());
        assert_eq!(
            auth.require(Operation::Modify),
            Err(SecurityError::PermissionDenied("modify"))
        );
    }

    #[test]
    fn decisions_are_deterministic_and_path_driven() {
        let applied = apply_password_plan(&restricted_input(), b"user").expect("user opens");
        let a = Authorization::from_applied(&applied).expect("authorization");
        let b = Authorization::from_applied(&applied).expect("authorization");
        assert_eq!(a, b);
        for operation in Operation::ALL {
            assert_eq!(a.can(operation), b.can(operation));
            assert_eq!(a.can(operation), a.can(operation));
        }
        let manual = Authorization::new(AuthorizationPath::UserPassword, a.permissions())
            .expect("same path and mask");
        assert_eq!(manual, a);
    }

    #[test]
    fn mismatched_path_and_role_are_typed() {
        let owner_rights = Permissions::new(Permission::ALL, PasswordRole::Owner);
        assert!(matches!(
            Authorization::new(AuthorizationPath::UserPassword, owner_rights),
            Err(SecurityError::PasswordConflict(_))
        ));
        let user_rights = Permissions::new(Permission::ALL, PasswordRole::User);
        assert!(matches!(
            Authorization::new(AuthorizationPath::OwnerPassword, user_rights),
            Err(SecurityError::PasswordConflict(_))
        ));
    }

    #[test]
    fn unencrypted_resolution_takes_the_unencrypted_path() {
        let input = PasswordPlanInput::unrestricted(
            EncryptionMethod::None,
            PasswordCredentials::from_strings("", ""),
        );
        let applied = apply_password_plan(&input, b"").expect("plain opens");
        let auth = Authorization::from_applied(&applied).expect("authorization");
        assert_eq!(auth.path(), AuthorizationPath::Unencrypted);
        for operation in Operation::ALL {
            assert!(auth.can(operation));
        }
    }

    #[test]
    fn owner_flag_on_an_unencrypted_resolution_is_rejected() {
        let resolution = PasswordResolution {
            role: PasswordRole::Owner,
            permissions: Permissions::new(Permission::ALL, PasswordRole::Owner),
            is_owner: true,
        };
        assert!(matches!(
            Authorization::from_resolution(resolution, false),
            Err(SecurityError::PasswordConflict(_))
        ));
    }

    #[test]
    fn operation_metadata_round_trips_and_is_stable() {
        for operation in Operation::ALL {
            assert_eq!(operation_for_id(operation.as_id()), operation);
            assert!(!operation.as_id().is_empty());
            assert!(!operation.label().is_empty());
            assert_eq!(operation.to_string(), operation.as_id());
        }
        for path in AuthorizationPath::ALL {
            assert!(!path.as_id().is_empty());
            assert_eq!(path.to_string(), path.as_id());
        }
    }
}
