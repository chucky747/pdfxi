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
