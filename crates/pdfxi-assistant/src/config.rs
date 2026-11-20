//! Client timing configuration.

use std::time::Duration;

/// Per-request transport timeouts.
///
/// Every value is a hard ceiling: a blocked connect, a stalled response or a
/// silent event stream surfaces as [`crate::AssistantError::Timeout`] instead of
/// hanging a caller thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeouts {
    /// Establishing the TCP or TLS connection.
    pub connect: Duration,
    /// Waiting for and receiving the response headers.
    pub response: Duration,
    /// A single transport read, including event stream reads.
    pub read: Duration,
    /// Receiving a complete (non-streaming) response body.
    pub body: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            response: Duration::from_secs(30),
            read: Duration::from_secs(30),
            body: Duration::from_secs(60),
        }
    }
}

/// Polling and streaming behaviour of an [`crate::Assistant`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// Transport timeouts.
    pub timeouts: Timeouts,
    /// Delay between message polls while waiting for an answer.
    pub poll_interval: Duration,
    /// Upper bound on one prompt-to-answer turn.
    pub max_wait: Duration,
    /// How long an event stream may stay silent before it is reported idle.
    pub event_idle_timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeouts: Timeouts::default(),
            poll_interval: Duration::from_millis(250),
            max_wait: Duration::from_secs(120),
            event_idle_timeout: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClientConfig;
    use std::time::Duration;

    #[test]
    fn defaults_match_the_pinned_budget() {
        let config = ClientConfig::default();
        assert_eq!(config.poll_interval, Duration::from_millis(250));
        assert_eq!(config.max_wait, Duration::from_secs(120));
        assert!(
            (Duration::from_secs(3)..=Duration::from_secs(5)).contains(&config.event_idle_timeout)
        );
    }
}
