//! Permission bit flags and password roles.
//!
//! PDF stores a permission mask in the `/P` entry of the encryption dictionary,
//! where a set bit means the corresponding operation is *allowed*. This module
//! exposes those bits as a lightweight, dependency-free bit-flag type and
//! combines them with a [`PasswordRole`] to model the effective rights of an
//! unlocked session.

use std::fmt;

use crate::error::{Result, SecurityError};

/// A single PDF permission bit flag.
///
/// Multiple flags can be combined with [`Permission::union`]. The numeric
/// values correspond to bits 3 through 12 of the PDF `/P` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Permission(u32);

impl Permission {
    /// Permission to print the document.
    pub const PRINT: Self = Self(1 << 2);
    /// Permission to modify the document contents.
    pub const MODIFY: Self = Self(1 << 3);
    /// Permission to copy or extract text and graphics.
    pub const COPY: Self = Self(1 << 4);
    /// Permission to add or edit annotations and form fields.
    pub const ANNOTATE: Self = Self(1 << 5);
    /// Permission to fill in interactive form fields.
    pub const FILL_FORMS: Self = Self(1 << 8);
    /// Permission to extract content for accessibility tools.
    pub const ACCESSIBILITY: Self = Self(1 << 9);
    /// Permission to assemble, insert, or rotate pages.
    pub const ASSEMBLE: Self = Self(1 << 10);
    /// Permission to print at high (full) quality.
    pub const PRINT_HIGH_QUALITY: Self = Self(1 << 11);

    /// No permission bits set (all operations denied).
    pub const NONE: Self = Self(0);

    /// Every permission bit known to this crate.
    pub const ALL: Self = Self(
        Self::PRINT.0
            | Self::MODIFY.0
            | Self::COPY.0
            | Self::ANNOTATE.0
            | Self::FILL_FORMS.0
            | Self::ACCESSIBILITY.0
            | Self::ASSEMBLE.0
            | Self::PRINT_HIGH_QUALITY.0,
    );

    /// Return the raw bit mask.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Build a permission mask from raw bits, silently discarding unknown bits.
    pub const fn from_bits_truncated(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// Build a permission mask from a raw `/P` value without validation.
    ///
    /// This is intended for callers modelling an untrusted on-disk `/P` word
    /// that must be passed to a validator. Consumers such as
    /// [`crate::password_policy::plan_encryption`] re-check the mask and return
    /// [`SecurityError::InvalidPermissionBits`] when unknown bits are present.
    pub const fn from_bits_unchecked(bits: u32) -> Self {
        Self(bits)
    }

    /// Build a permission mask from raw bits, rejecting unknown bits.
    pub fn from_bits_checked(bits: u32) -> Result<Self> {
        if bits & !Self::ALL.0 == 0 {
            Ok(Self(bits))
        } else {
            Err(SecurityError::InvalidPermissionBits(bits))
        }
    }

    /// Return `true` if all bits of `other` are present in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Combine two permission masks.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Return the bits present in both masks.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Return `true` if no permission bits are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Bits of a raw `/P` word that the PDF specification reserves as always 1.
    ///
    /// Bits 7 and 8 (values `0x40` and `0x80`) and bits 13 through 32 of the
    /// `/P` entry must be set in a well-formed encryption dictionary. They are
    /// not user-grantable permissions, so they are absent from [`Self::ALL`].
    pub const RESERVED_ALWAYS_SET: u32 = 0xFFFF_F0C0;

    /// The two low-order bits of `/P`, which are always 0.
    const ALWAYS_CLEAR: u32 = 0x0000_0003;

    /// Derive the canonical raw `/P` word for this permission mask.
    ///
    /// The result sets every reserved-always-1 bit and leaves the low two bits
    /// clear, matching the value an encrypting writer places in `/Encrypt`.
    pub const fn to_p_word(self) -> u32 {
        Self::RESERVED_ALWAYS_SET | self.0
    }

    /// Parse and validate a raw `/P` word from an untrusted document.
    ///
    /// This checks both structural validity (reserved bits set, low bits
    /// clear) and mutually dependent grants (see [`Self::validate_consistent`]).
    /// Reserved bits are stripped from the returned mask so it can be compared
    /// directly with [`Self::ALL`] and the named permission constants.
    ///
    /// # Errors
    ///
    /// - [`SecurityError::InvalidPermissionBits`] when reserved bits are clear,
    ///   low bits are set, or unknown permission bits are present.
    /// - [`SecurityError::PermissionConflict`] when grants contradict each other.
    pub fn from_p_word(raw: u32) -> Result<Self> {
        if raw & Self::RESERVED_ALWAYS_SET != Self::RESERVED_ALWAYS_SET
            || raw & Self::ALWAYS_CLEAR != 0
        {
            return Err(SecurityError::InvalidPermissionBits(raw));
        }
        let bits = Self(raw & Self::ALL.0);
        Self::validate_consistent(bits)?;
        Ok(bits)
    }

    /// Check a mask for mutually dependent grants.
    ///
    /// The PDF specification makes annotation and page assembly meaningless
    /// when modification is denied, and high-quality printing meaningless when
    /// printing is denied. This crate treats those combinations as typed
    /// conflicts rather than silently ignoring the dependent bits.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PermissionConflict`] naming the violated
    /// dependency.
    pub fn validate_consistent(bits: Self) -> Result<()> {
        if bits.contains(Self::ANNOTATE) && !bits.contains(Self::MODIFY) {
            return Err(SecurityError::PermissionConflict(
                "annotation requires modification permission",
            ));
        }
        if bits.contains(Self::ASSEMBLE) && !bits.contains(Self::MODIFY) {
            return Err(SecurityError::PermissionConflict(
                "page assembly requires modification permission",
            ));
        }
        if bits.contains(Self::PRINT_HIGH_QUALITY) && !bits.contains(Self::PRINT) {
            return Err(SecurityError::PermissionConflict(
                "high-quality printing requires printing permission",
            ));
        }
        Ok(())
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

/// The role that a password grants when it unlocks a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PasswordRole {
    /// The user (open) password: subject to the granted permission bits.
    User,
    /// The owner (permissions) password: full access to the document.
    Owner,
}

impl PasswordRole {
    /// Short human-readable label for this role.
    pub const fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Owner => "owner",
        }
    }
}

impl fmt::Display for PasswordRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[cfg(test)]
mod bit_tests {
    use super::*;

    #[test]
    fn p_word_round_trips_every_named_permission() {
        let cases = [
            Permission::NONE,
            Permission::PRINT,
            Permission::MODIFY,
            Permission::COPY,
            Permission::FILL_FORMS,
            Permission::ACCESSIBILITY,
            Permission::ALL,
            Permission::PRINT.union(Permission::COPY),
            Permission::MODIFY.union(Permission::ANNOTATE),
            Permission::MODIFY.union(Permission::ASSEMBLE),
            Permission::PRINT.union(Permission::PRINT_HIGH_QUALITY),
        ];
        for mask in cases {
            let raw = mask.to_p_word();
            assert_eq!(
                raw & Permission::RESERVED_ALWAYS_SET,
                Permission::RESERVED_ALWAYS_SET
            );
            assert_eq!(raw & 0x3, 0);
            assert_eq!(Permission::from_p_word(raw), Ok(mask));
        }
        assert_eq!(Permission::ALL.to_p_word(), 0xFFFF_FFFC);
        assert_eq!(Permission::from_p_word(0xFFFF_FFFC), Ok(Permission::ALL));
    }

    #[test]
    fn from_p_word_rejects_cleared_reserved_bits() {
        let raw = Permission::ALL.to_p_word() & !(1 << 20);
        assert_eq!(
            Permission::from_p_word(raw),
            Err(SecurityError::InvalidPermissionBits(raw))
        );
        let raw = Permission::ALL.to_p_word() & !(1 << 6);
        assert_eq!(
            Permission::from_p_word(raw),
            Err(SecurityError::InvalidPermissionBits(raw))
        );
    }

    #[test]
    fn from_p_word_rejects_low_and_unknown_bits() {
        let raw = Permission::ALL.to_p_word() | 0x1;
        assert_eq!(
            Permission::from_p_word(raw),
            Err(SecurityError::InvalidPermissionBits(raw))
        );
    }

    #[test]
    fn from_p_word_rejects_conflicting_dependent_grants() {
        let annotate_only = Permission::ANNOTATE.to_p_word();
        assert_eq!(
            Permission::from_p_word(annotate_only),
            Err(SecurityError::PermissionConflict(
                "annotation requires modification permission"
            ))
        );
        let assemble_only = Permission::ASSEMBLE.to_p_word();
        assert_eq!(
            Permission::from_p_word(assemble_only),
            Err(SecurityError::PermissionConflict(
                "page assembly requires modification permission"
            ))
        );
        let high_quality_only = Permission::PRINT_HIGH_QUALITY.to_p_word();
        assert_eq!(
            Permission::from_p_word(high_quality_only),
            Err(SecurityError::PermissionConflict(
                "high-quality printing requires printing permission"
            ))
        );
    }

    #[test]
    fn validate_consistent_accepts_independent_grants() {
        let okay = [
            Permission::NONE,
            Permission::PRINT,
            Permission::MODIFY.union(Permission::ANNOTATE),
            Permission::MODIFY.union(Permission::ASSEMBLE),
            Permission::PRINT.union(Permission::PRINT_HIGH_QUALITY),
            Permission::ALL,
        ];
        for mask in okay {
            assert_eq!(Permission::validate_consistent(mask), Ok(()));
        }
    }

    #[test]
    fn permissions_decisions_follow_role_and_mask() {
        let user = Permissions::new(Permission::PRINT, PasswordRole::User);
        assert!(user.allows(Permission::PRINT));
        assert!(!user.allows(Permission::MODIFY));
        let owner = Permissions::new(Permission::NONE, PasswordRole::Owner);
        for bit in [
            Permission::PRINT,
            Permission::MODIFY,
            Permission::COPY,
            Permission::ANNOTATE,
            Permission::ASSEMBLE,
        ] {
            assert!(owner.allows(bit));
        }
    }
}

/// Effective permissions for an unlocked security session.
///
/// A [`PasswordRole::Owner`] session bypasses all permission restrictions,
/// matching the semantics of the PDF owner password. A [`PasswordRole::User`]
/// session may only perform operations whose bits are present in `bits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    bits: Permission,
    role: PasswordRole,
}

impl Permissions {
    /// Create a permission set from a mask and a password role.
    pub const fn new(bits: Permission, role: PasswordRole) -> Self {
        Self { bits, role }
    }

    /// Underlying permission bit mask.
    pub const fn bits(&self) -> Permission {
        self.bits
    }

    /// Role that produced this permission set.
    pub const fn role(&self) -> PasswordRole {
        self.role
    }

    /// Return `true` if `permission` is allowed for this session.
    ///
    /// Owner sessions always return `true`; user sessions consult the mask.
    pub const fn allows(&self, permission: Permission) -> bool {
        match self.role {
            PasswordRole::Owner => true,
            PasswordRole::User => self.bits.contains(permission),
        }
    }
}
