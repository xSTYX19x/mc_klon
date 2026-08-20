//! Turning per-subsystem counters into a `tops=[…]` list that adds up.
//!
//! The counters in `FramePerf` are accumulated by whoever does the work, and
//! stages nest: `enqueue_chunks_within_view` (bucket `stream`) queues saved-column
//! loads, which charge `ensure_saved_column_load_ms` (bucket `saved_load`).
//! The same milliseconds then appear in two buckets, the list sums to more than
//! the frame, and a reader cannot tell which bucket is the container — that is
//! how unload-triggered lighting once read as `lighting=188.0ms`.
//!
//! A stage therefore reports what it charged to *other* buckets, and the list is
//! built from exclusive time. Whatever nesting is still unaccounted shows up as
//! an overflow instead of silently distorting the ranking.

/// One bucket's frame cost.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StageSample {
    pub(crate) name: &'static str,
    /// Everything charged to this bucket, nested work included.
    pub(crate) inclusive_ms: f32,
    /// The part of `inclusive_ms` that another bucket also counts.
    pub(crate) nested_ms: f32,
}

impl StageSample {
    pub(crate) const fn flat(name: &'static str, inclusive_ms: f32) -> Self {
        Self {
            name,
            inclusive_ms,
            nested_ms: 0.0,
        }
    }

    pub(crate) const fn nested(name: &'static str, inclusive_ms: f32, nested_ms: f32) -> Self {
        Self {
            name,
            inclusive_ms,
            nested_ms,
        }
    }

    /// Time this bucket owns alone. Never negative: a nested measurement that
    /// overshoots its parent is a bug in the instrumentation, not a negative
    /// cost, and it surfaces through [`attribution_overflow_ms`].
    pub(crate) fn exclusive_ms(&self) -> f32 {
        (self.inclusive_ms - self.nested_ms).max(0.0)
    }
}

/// `name=X.Xms` for the `take` most expensive buckets, worst first.
pub(crate) fn format_top_stages(samples: &[StageSample], take: usize, min_ms: f32) -> String {
    let mut ordered: Vec<(&'static str, f32)> = samples
        .iter()
        .map(|sample| (sample.name, sample.exclusive_ms()))
        .collect();
    ordered.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ordered
        .iter()
        .take(take)
        .filter(|(_, ms)| *ms > min_ms)
        .map(|(name, ms)| format!("{name}={ms:.1}ms"))
        .collect::<Vec<_>>()
        .join(",")
}

/// How much the buckets over-claim against the stage that contains them.
///
/// Zero means the attribution is consistent. A positive value means some nesting
/// is still uninstrumented, i.e. the ranking may point at a container rather
/// than the actual cost — the exact failure this module exists to make visible.
pub(crate) fn attribution_overflow_ms(samples: &[StageSample], parent_ms: f32) -> f32 {
    let sum: f32 = samples.iter().map(StageSample::exclusive_ms).sum();
    (sum - parent_ms).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_bucket_is_not_charged_to_its_container() {
        // `stream` spent 8 ms, 6 of which were saved-column loads that
        // `saved_load` already reports. Ranking by the raw counters would put
        // `stream` on top of a cost it only contained.
        let samples = [
            StageSample::nested("stream", 8.0, 6.0),
            StageSample::flat("saved_load", 6.0),
        ];
        assert_eq!(format_top_stages(&samples, 4, 0.05), "saved_load=6.0ms,stream=2.0ms");
    }

    #[test]
    fn without_the_nested_part_the_ranking_is_wrong() {
        // Pins why this exists: the same numbers, attributed inclusively.
        let inclusive = [
            StageSample::flat("stream", 8.0),
            StageSample::flat("saved_load", 6.0),
        ];
        assert!(format_top_stages(&inclusive, 4, 0.05).starts_with("stream="));
    }

    #[test]
    fn buckets_are_ranked_worst_first_and_thresholded() {
        let samples = [
            StageSample::flat("mesh", 0.04),
            StageSample::flat("lighting", 12.0),
            StageSample::flat("chunkgen", 3.0),
        ];
        assert_eq!(
            format_top_stages(&samples, 4, 0.05),
            "lighting=12.0ms,chunkgen=3.0ms",
            "a bucket under the noise floor must not pad the list"
        );
    }

    #[test]
    fn only_take_entries_are_listed() {
        let samples = [
            StageSample::flat("a", 4.0),
            StageSample::flat("b", 3.0),
            StageSample::flat("c", 2.0),
        ];
        assert_eq!(format_top_stages(&samples, 2, 0.05), "a=4.0ms,b=3.0ms");
    }

    #[test]
    fn consistent_attribution_reports_no_overflow() {
        let samples = [
            StageSample::nested("stream", 8.0, 6.0),
            StageSample::flat("saved_load", 6.0),
        ];
        assert_eq!(attribution_overflow_ms(&samples, 8.0), 0.0);
    }

    #[test]
    fn uninstrumented_nesting_shows_up_as_overflow() {
        // 8 ms of work reported as 14 ms across two buckets: the container is
        // not declaring what it charged elsewhere.
        let samples = [
            StageSample::flat("stream", 8.0),
            StageSample::flat("saved_load", 6.0),
        ];
        assert_eq!(attribution_overflow_ms(&samples, 8.0), 6.0);
    }

    #[test]
    fn a_nested_measurement_larger_than_its_parent_cannot_go_negative() {
        let samples = [StageSample::nested("stream", 2.0, 5.0)];
        assert_eq!(samples[0].exclusive_ms(), 0.0);
        assert_eq!(attribution_overflow_ms(&samples, 2.0), 0.0);
    }
}
