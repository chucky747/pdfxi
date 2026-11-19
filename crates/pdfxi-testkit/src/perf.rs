//! Deterministic timing helpers for the `bench_open` harness.
//!
//! These helpers contain no sleeps, no randomness, and no wall-clock thresholds
//! of their own. They only reduce a set of measured [`Duration`] samples into a
//! stable summary so the benchmark output is reproducible in shape. The
//! benchmark itself lives in `examples/bench_open.rs`.

use std::time::Duration;

/// Min/median/max wall-time summary for a set of samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Smallest observed duration.
    pub min: Duration,
    /// Middle duration after sorting (lower middle for even counts).
    pub median: Duration,
    /// Largest observed duration.
    pub max: Duration,
}

/// Summarize wall-time `samples` into a [`Timing`].
///
/// Returns `None` when `samples` is empty. The median is computed from a sorted
/// copy with a deterministic tie-break: for an even number of samples the
/// lower of the two middle values is used.
pub fn summarize(samples: &[Duration]) -> Option<Timing> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort_unstable();
    let middle = (sorted.len() - 1) / 2;
    Some(Timing {
        min: sorted[0],
        median: sorted[middle],
        max: sorted[sorted.len() - 1],
    })
}

/// Format a [`Duration`] as a fixed-width, human-readable millisecond value.
///
/// The exact decimal precision is fixed so successive runs produce identical
/// formatting apart from the measured numbers themselves.
pub fn format_ms(value: Duration) -> String {
    let whole = value.as_millis();
    let fractional = value.subsec_nanos() % 1_000_000;
    format!("{whole}.{fractional:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn summarize_empty_is_none() {
        assert_eq!(summarize(&[]), None);
    }

    #[test]
    fn summarize_odd_sample_count() {
        let timing = summarize(&[d(30), d(10), d(20)]).expect("non-empty");
        assert_eq!(timing.min, d(10));
        assert_eq!(timing.median, d(20));
        assert_eq!(timing.max, d(30));
    }

    #[test]
    fn summarize_even_sample_count_uses_lower_middle() {
        let timing = summarize(&[d(40), d(10), d(30), d(20)]).expect("non-empty");
        assert_eq!(timing.median, d(20));
    }

    #[test]
    fn summarize_single_sample() {
        let timing = summarize(&[d(5)]).expect("non-empty");
        assert_eq!(
            timing,
            Timing {
                min: d(5),
                median: d(5),
                max: d(5)
            }
        );
    }

    #[test]
    fn format_ms_is_fixed_width() {
        assert_eq!(format_ms(d(12)), "12.000000");
        assert_eq!(format_ms(Duration::from_micros(1_500)), "1.500000");
    }
}
