//! Credentials that never appear in debug output.

use std::fmt;

/// A credential string held in memory for the lifetime of a client.
///
/// The value is never printed: [`Debug`] renders `<redacted>`, there is no
/// `Display` implementation, and the bytes are exposed only to the transport
/// that builds the HTTP Basic header.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Secret(String);

impl Secret {
    /// Wraps `value`.
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Callers must not log, format or store it.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn debug_never_prints_the_value() {
        let secret = Secret::new("s3cret-value");
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret(<redacted>)");
        assert!(!rendered.contains("s3cret"));
    }
}
