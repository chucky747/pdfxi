//! Host-driven scheduling and persistence contracts for watched folders.
//!
//! This module completes the library-level watched-folder pipeline without
//! starting any thread or OS watcher. The host owns the clock and the trigger:
//! it calls [`PollSchedule::advance_tick`] once per abstract tick and polls only
//! when the returned [`ScheduleDecision`] is [`ScheduleDecision::Due`]. Every
//! value here is deterministic, so two hosts feeding the same ticks observe the
//! same decisions and the same byte-stable status reports.
//!
//! Two related contracts live here:
//!
//! - [`PollInterval`] and [`BackoffPolicy`] describe *when* polls may run.
//!   Backoff uses fixed-point integer arithmetic (a per-mille multiplier), so no
//!   floating point is involved and results are byte-identical on every host.
//! - [`MarkerSnapshot`] is the persistence contract for the in-memory
//!   [`ProcessedMarkers`] set. It serialises to canonical JSON text that the
//!   caller may store or reload by any means; this crate performs no file I/O.
//!   On reload the snapshot is validated (version, sortedness, uniqueness,
//!   traversal-free relative paths and size bounds) before it is restored.

use std::fmt;
use std::path::{Component, Path};

use crate::json::JsonValue;
use crate::watched::{ProcessedMarkers, WATCHED_MAX_FILES, WatchedFolderError};

/// Version of the marker-snapshot persistence format.
pub const MARKER_SNAPSHOT_VERSION: u64 = 1;

/// Maximum serialized byte size of a caller-supplied marker snapshot.
pub const MARKER_SNAPSHOT_MAX_BYTES: usize = 64 * 1024;

/// Maximum number of marker paths a snapshot may carry.
pub const MARKER_SNAPSHOT_MAX_PATHS: usize = WATCHED_MAX_FILES;

/// Maximum byte length of a single marker path in a snapshot.
pub const MARKER_PATH_MAX_BYTES: usize = 1024;

/// A deterministic description of how often a host may poll a watched folder.
///
/// No wall-clock or timer is used; [`PollInterval::EveryTicks`] counts abstract
/// host-supplied ticks so the schedule is reproducible in tests and on any OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollInterval {
    /// Poll on every host request; no tick gating is applied.
    EveryPoll,
    /// Poll at most once every `ticks` host-supplied ticks (`ticks` must be
    /// positive, enforced by [`PollSchedule::new`]).
    EveryTicks(u64),
}

impl PollInterval {
    /// Stable machine-readable name of this interval kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EveryPoll => "every_poll",
            Self::EveryTicks(_) => "every_ticks",
        }
    }
}

impl fmt::Display for PollInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EveryPoll => f.write_str("every_poll"),
            Self::EveryTicks(ticks) => write!(f, "every_ticks:{ticks}"),
        }
    }
}

/// Bounded exponential backoff for consecutive failed polls, in host ticks.
///
/// The delay after `n` consecutive failures is
/// `base_ticks * (multiplier_per_mille / 1000)^(n - 1)`, clamped upward to at
/// least `base_ticks` and downward to `max_ticks`, using saturating integer
/// arithmetic only. `multiplier_per_mille` is in the inclusive range
/// `1000..=10000`, so the delay never shrinks and never exceeds tenfold growth
/// per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// Delay applied after the first consecutive failure.
    pub base_ticks: u64,
    /// Growth multiplier per thousand (1000 means constant, 2000 means double).
    pub multiplier_per_mille: u32,
    /// Hard ceiling on the computed delay.
    pub max_ticks: u64,
}

impl BackoffPolicy {
    /// A conservative default: 1 tick base, doubling, capped at 64 ticks.
    pub const DEFAULT: Self = Self {
        base_ticks: 1,
        multiplier_per_mille: 2000,
        max_ticks: 64,
    };

    /// Validate that all fields are positive and internally consistent.
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::InvalidConfig`] when `base_ticks` is zero,
    /// `max_ticks` is below `base_ticks`, or the multiplier is outside
    /// `1000..=10000`.
    pub fn validate(&self) -> Result<(), WatchedFolderError> {
        if self.base_ticks == 0 {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "backoff base_ticks must be positive".to_owned(),
            });
        }
        if self.max_ticks < self.base_ticks {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "backoff max_ticks must be at least base_ticks".to_owned(),
            });
        }
        if !(1_000..=10_000).contains(&self.multiplier_per_mille) {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "backoff multiplier_per_mille must be in 1000..=10000".to_owned(),
            });
        }
        Ok(())
    }

    /// The delay in host ticks after `failures` consecutive failures.
    ///
    /// Returns zero when `failures` is zero (no backoff for a clean state).
    /// The result is always in `base_ticks..=max_ticks` for `failures > 0`.
    #[must_use]
    pub fn delay_after(&self, failures: u32) -> u64 {
        if failures == 0 {
            return 0;
        }
        let mut delay = self.base_ticks;
        for _ in 1..failures {
            let grown = delay.saturating_mul(u64::from(self.multiplier_per_mille)) / 1000;
            delay = grown.max(delay);
            if delay >= self.max_ticks {
                delay = self.max_ticks;
                break;
            }
        }
        delay.min(self.max_ticks)
    }
}

/// The decision returned by [`PollSchedule::advance_tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDecision {
    /// A poll is due now.
    Due,
    /// The host should wait `wait_ticks` more ticks before polling.
    Wait {
        /// Number of additional host ticks before a poll becomes due.
        wait_ticks: u64,
    },
}

impl ScheduleDecision {
    /// Stable machine-readable name of this decision.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Due => "due",
            Self::Wait { .. } => "wait",
        }
    }
}

/// A deterministic, host-driven poll schedule.
///
/// Holds only counters; the host supplies ticks and reports poll success or
/// failure. No timer, clock, or thread is involved.
#[derive(Debug, Clone)]
pub struct PollSchedule {
    interval: PollInterval,
    backoff: BackoffPolicy,
    consecutive_failures: u32,
    ticks_since_poll: u64,
    tick_count: u64,
    polls_started: u64,
}

impl PollSchedule {
    /// Validate `interval` and `backoff` and create a clean schedule.
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::InvalidConfig`] for
    /// [`PollInterval::EveryTicks`] with zero ticks or an invalid
    /// [`BackoffPolicy`].
    pub fn new(interval: PollInterval, backoff: BackoffPolicy) -> Result<Self, WatchedFolderError> {
        if matches!(interval, PollInterval::EveryTicks(0)) {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "poll interval ticks must be positive".to_owned(),
            });
        }
        backoff.validate()?;
        Ok(Self {
            interval,
            backoff,
            consecutive_failures: 0,
            ticks_since_poll: 0,
            tick_count: 0,
            polls_started: 0,
        })
    }

    /// The configured poll interval.
    #[must_use]
    pub const fn interval(&self) -> PollInterval {
        self.interval
    }

    /// The configured backoff policy.
    #[must_use]
    pub const fn backoff(&self) -> BackoffPolicy {
        self.backoff
    }

    /// Consecutive failures reported since the last success.
    #[must_use]
    pub const fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Total host ticks observed so far.
    #[must_use]
    pub const fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Number of polls that have become due.
    #[must_use]
    pub const fn polls_started(&self) -> u64 {
        self.polls_started
    }

    /// Host ticks elapsed since the last due poll.
    #[must_use]
    pub const fn ticks_since_poll(&self) -> u64 {
        self.ticks_since_poll
    }

    /// The gap currently required between two polls.
    ///
    /// This is the larger of the configured interval and the backoff delay for
    /// the current consecutive-failure count.
    #[must_use]
    pub fn required_gap_ticks(&self) -> u64 {
        let interval_gap = match self.interval {
            PollInterval::EveryPoll => 0,
            PollInterval::EveryTicks(ticks) => ticks,
        };
        interval_gap.max(self.backoff.delay_after(self.consecutive_failures))
    }

    /// Advance one host tick and decide whether a poll is due.
    ///
    /// When the decision is [`ScheduleDecision::Due`] the since-poll counter is
    /// reset and the started-poll counter is incremented, so the next interval
    /// is measured from this point.
    pub fn advance_tick(&mut self) -> ScheduleDecision {
        self.tick_count = self.tick_count.saturating_add(1);
        self.ticks_since_poll = self.ticks_since_poll.saturating_add(1);
        let gap = self.required_gap_ticks();
        if self.ticks_since_poll >= gap {
            self.ticks_since_poll = 0;
            self.polls_started = self.polls_started.saturating_add(1);
            ScheduleDecision::Due
        } else {
            ScheduleDecision::Wait {
                wait_ticks: gap - self.ticks_since_poll,
            }
        }
    }

    /// Record a successful poll, clearing the failure counter.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Record a failed poll, growing the backoff counter by one (saturating).
    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// A stable one-line status report suitable for golden tests.
    #[must_use]
    pub fn status_summary(&self) -> String {
        format!(
            "interval={}\tfailures={}\ttick={}\tpolls={}\tgap={}",
            self.interval,
            self.consecutive_failures,
            self.tick_count,
            self.polls_started,
            self.required_gap_ticks()
        )
    }
}

/// Validate a caller-supplied relative marker path.
fn validate_marker_path(path: &str) -> Result<(), WatchedFolderError> {
    let invalid = |reason: &str| WatchedFolderError::InvalidConfig {
        reason: format!("invalid marker path '{path}': {reason}"),
    };
    if path.is_empty() {
        return Err(invalid("path is empty"));
    }
    if path.len() > MARKER_PATH_MAX_BYTES {
        return Err(invalid("path exceeds the byte bound"));
    }
    if path.contains('\0') || path.chars().any(char::is_control) {
        return Err(invalid("path contains a control character"));
    }
    if path.contains('\\') {
        return Err(invalid("path uses a backslash separator"));
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(invalid("path is absolute"));
    }
    for component in candidate.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid("path has a non-normal component"));
        }
    }
    Ok(())
}

/// A versioned, validated snapshot of processed-file markers.
///
/// The snapshot is the caller-supplied persistence data for idempotency: the
/// host serialises it with [`MarkerSnapshot::to_json_string`], stores the text
/// however it wishes, and later reloads it with
/// [`MarkerSnapshot::from_json_string`]. No filesystem access occurs here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerSnapshot {
    version: u64,
    paths: Vec<String>,
}

impl MarkerSnapshot {
    /// Capture the current paths of `markers` in sorted order.
    #[must_use]
    pub fn capture(markers: &ProcessedMarkers) -> Self {
        Self {
            version: MARKER_SNAPSHOT_VERSION,
            paths: markers.paths().map(str::to_owned).collect(),
        }
    }

    /// Snapshot format version.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Marker paths in canonical sorted order.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Serialise to canonical JSON text with sorted keys and no whitespace.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        JsonValue::object([
            ("paths".to_owned(), JsonValue::from(self.paths.clone())),
            ("version".to_owned(), JsonValue::number(self.version)),
        ])
        .to_string()
    }

    /// Parse and validate a snapshot from caller-supplied JSON text.
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::InvalidConfig`] for oversized input,
    /// malformed JSON, a wrong version, a wrong field type, too many paths,
    /// unsorted or duplicate paths, or a path that is absolute or traversal-
    /// bearing.
    pub fn from_json_string(text: &str) -> Result<Self, WatchedFolderError> {
        let invalid = |reason: String| WatchedFolderError::InvalidConfig { reason };
        if text.len() > MARKER_SNAPSHOT_MAX_BYTES {
            return Err(invalid("marker snapshot exceeds the byte bound".to_owned()));
        }
        let value = JsonValue::parse(text)
            .map_err(|error| invalid(format!("malformed marker snapshot: {}", error.reason)))?;
        let version = value
            .get("version")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| {
                invalid("marker snapshot has a missing or invalid version".to_owned())
            })?;
        if version != MARKER_SNAPSHOT_VERSION {
            return Err(invalid(format!(
                "unsupported marker snapshot version {version}; expected {MARKER_SNAPSHOT_VERSION}"
            )));
        }
        let entries = value
            .get("paths")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| {
                invalid("marker snapshot has a missing or invalid paths array".to_owned())
            })?;
        if entries.len() > MARKER_SNAPSHOT_MAX_PATHS {
            return Err(invalid(format!(
                "marker snapshot has {} paths; maximum is {MARKER_SNAPSHOT_MAX_PATHS}",
                entries.len()
            )));
        }
        let mut paths = Vec::with_capacity(entries.len());
        let mut previous: Option<&str> = None;
        for entry in entries {
            let path = entry.as_str().ok_or_else(|| {
                invalid("marker snapshot path entries must be strings".to_owned())
            })?;
            validate_marker_path(path)?;
            if previous.is_some_and(|prior| prior >= path) {
                return Err(invalid(
                    "marker snapshot paths must be strictly sorted and unique".to_owned(),
                ));
            }
            previous = Some(path);
            paths.push(path.to_owned());
        }
        Ok(Self { version, paths })
    }

    /// Restore a [`ProcessedMarkers`] set from this validated snapshot.
    #[must_use]
    pub fn restore(self) -> ProcessedMarkers {
        let mut markers = ProcessedMarkers::new();
        for path in self.paths {
            markers.mark(path);
        }
        markers
    }
}

/// Serialise processed markers into canonical, caller-storable JSON text.
#[must_use]
pub fn serialize_markers(markers: &ProcessedMarkers) -> String {
    MarkerSnapshot::capture(markers).to_json_string()
}

/// Restore processed markers from validated, caller-supplied JSON text.
///
/// # Errors
///
/// Returns [`WatchedFolderError::InvalidConfig`] for any malformed, unsafe, or
/// oversized snapshot; see [`MarkerSnapshot::from_json_string`].
pub fn deserialize_markers(text: &str) -> Result<ProcessedMarkers, WatchedFolderError> {
    Ok(MarkerSnapshot::from_json_string(text)?.restore())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_is_deterministic_across_identical_tick_streams() {
        let make = || {
            PollSchedule::new(PollInterval::EveryTicks(2), BackoffPolicy::DEFAULT)
                .expect("test-only schedule")
        };
        let mut a = make();
        let mut b = make();
        for _ in 0..9 {
            let da = a.advance_tick();
            let db = b.advance_tick();
            assert_eq!(da, db);
            // Keep both clean so the stream is identical.
            if da == ScheduleDecision::Due {
                a.record_success();
                b.record_success();
            }
        }
        assert_eq!(a.tick_count(), 9);
        assert_eq!(a.polls_started(), 4);
        assert_eq!(a.status_summary(), b.status_summary());
        assert_eq!(
            a.status_summary(),
            "interval=every_ticks:2\tfailures=0\ttick=9\tpolls=4\tgap=2"
        );
    }

    #[test]
    fn every_poll_is_always_due_until_a_failure_gates_it() {
        let mut schedule =
            PollSchedule::new(PollInterval::EveryPoll, BackoffPolicy::DEFAULT).expect("schedule");
        assert_eq!(schedule.advance_tick(), ScheduleDecision::Due);
        assert_eq!(schedule.advance_tick(), ScheduleDecision::Due);
        schedule.record_failure();
        // base_ticks = 1, so one more tick makes it due again.
        assert_eq!(schedule.advance_tick(), ScheduleDecision::Due);
        schedule.record_failure();
        // two failures -> 2 tick backoff, measured from the last due poll
        // (whose counter was reset), so one tick still waits and the next is due.
        assert_eq!(
            schedule.advance_tick(),
            ScheduleDecision::Wait { wait_ticks: 1 }
        );
        assert_eq!(schedule.advance_tick(), ScheduleDecision::Due);
    }

    #[test]
    fn backoff_is_bounded_monotonic_and_zero_at_clean_state() {
        let policy = BackoffPolicy::DEFAULT;
        assert_eq!(policy.delay_after(0), 0);
        assert_eq!(policy.delay_after(1), 1);
        assert_eq!(policy.delay_after(2), 2);
        assert_eq!(policy.delay_after(3), 4);
        assert_eq!(policy.delay_after(7), 64);
        assert_eq!(policy.delay_after(u32::MAX), policy.max_ticks);
        let mut previous = policy.delay_after(1);
        for failures in 2..32 {
            let current = policy.delay_after(failures);
            assert!(current >= previous);
            assert!(current <= policy.max_ticks);
            assert!(current >= policy.base_ticks);
            previous = current;
        }
    }

    #[test]
    fn failure_then_success_resets_the_backoff_gate() {
        let mut schedule = PollSchedule::new(PollInterval::EveryTicks(3), BackoffPolicy::DEFAULT)
            .expect("schedule");
        for tick in 0..3 {
            let decision = schedule.advance_tick();
            if tick == 2 {
                assert_eq!(decision, ScheduleDecision::Due);
            } else {
                assert!(matches!(decision, ScheduleDecision::Wait { .. }));
            }
        }
        schedule.record_failure();
        schedule.record_failure();
        assert_eq!(schedule.required_gap_ticks(), 3); // max(interval 3, backoff 2)
        schedule.record_success();
        assert_eq!(schedule.consecutive_failures(), 0);
        assert_eq!(schedule.required_gap_ticks(), 3);
    }

    #[test]
    fn invalid_schedule_config_is_a_typed_error() {
        assert!(matches!(
            PollSchedule::new(PollInterval::EveryTicks(0), BackoffPolicy::DEFAULT),
            Err(WatchedFolderError::InvalidConfig { .. })
        ));
        let base_zero = BackoffPolicy {
            base_ticks: 0,
            ..BackoffPolicy::DEFAULT
        };
        assert!(base_zero.validate().is_err());
        let max_too_small = BackoffPolicy {
            base_ticks: 4,
            max_ticks: 2,
            ..BackoffPolicy::DEFAULT
        };
        assert!(max_too_small.validate().is_err());
        let bad_multiplier = BackoffPolicy {
            multiplier_per_mille: 999,
            ..BackoffPolicy::DEFAULT
        };
        assert!(matches!(
            PollSchedule::new(PollInterval::EveryPoll, bad_multiplier),
            Err(WatchedFolderError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn markers_round_trip_through_the_persistence_contract() {
        let mut original = ProcessedMarkers::new();
        original.mark("nested/b.md");
        original.mark("a.txt");
        original.mark("a.txt"); // idempotent
        let text = serialize_markers(&original);
        assert!(text.starts_with('{') && text.ends_with('}'));
        assert_eq!(text, serialize_markers(&original));
        let snapshot = MarkerSnapshot::from_json_string(&text).expect("snapshot");
        assert_eq!(snapshot.version(), MARKER_SNAPSHOT_VERSION);
        assert_eq!(snapshot.paths(), ["a.txt", "nested/b.md"]);
        let restored = deserialize_markers(&text).expect("restore");
        assert_eq!(restored, original);
        assert_eq!(serialize_markers(&restored), text);
    }

    #[test]
    fn empty_markers_round_trip_to_an_empty_snapshot() {
        let empty = ProcessedMarkers::new();
        let text = serialize_markers(&empty);
        let restored = deserialize_markers(&text).expect("restore empty");
        assert!(restored.is_empty());
        assert_eq!(deserialize_markers(&text).expect("again"), restored);
    }

    #[test]
    fn invalid_marker_snapshots_are_rejected() {
        let wrong_version = r#"{"paths":[],"version":99}"#;
        assert!(deserialize_markers(wrong_version).is_err());
        let unsorted = r#"{"paths":["b.txt","a.txt"],"version":1}"#;
        assert!(deserialize_markers(unsorted).is_err());
        let duplicate = r#"{"paths":["a.txt","a.txt"],"version":1}"#;
        assert!(deserialize_markers(duplicate).is_err());
        let traversal = r#"{"paths":["../escape.txt"],"version":1}"#;
        assert!(deserialize_markers(traversal).is_err());
        let absolute = r#"{"paths":["C:/absolute.txt"],"version":1}"#;
        assert!(deserialize_markers(absolute).is_err());
        let backslash = r#"{"paths":["dir\\file.txt"],"version":1}"#;
        assert!(deserialize_markers(backslash).is_err());
        let wrong_type = r#"{"paths":"a.txt","version":1}"#;
        assert!(deserialize_markers(wrong_type).is_err());
        let huge = "x".repeat(MARKER_SNAPSHOT_MAX_BYTES + 1);
        assert!(deserialize_markers(&huge).is_err());
    }

    #[test]
    fn marker_paths_preserve_subdirectories_and_deterministic_order() {
        let mut markers = ProcessedMarkers::new();
        for path in ["z.txt", "a/nested/deep.md", "b.html", "a/other.txt"] {
            markers.mark(path);
        }
        let text = serialize_markers(&markers);
        let snapshot = MarkerSnapshot::from_json_string(&text).expect("snapshot");
        assert_eq!(
            snapshot.paths(),
            ["a/nested/deep.md", "a/other.txt", "b.html", "z.txt"]
        );
        assert_eq!(deserialize_markers(&text).expect("restore"), markers);
    }
}
