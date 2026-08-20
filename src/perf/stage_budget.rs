//! Watchdog for stage loops whose budget only bounds the loop, not one item.
//!
//! Every budgeted stage checks `if items_done > 0 && elapsed >= budget`. That
//! guarantees forward progress — one item always runs — but puts no ceiling on
//! how long that one item may take: a 40 ms item passes a check meant to hold
//! the stage to 3 ms, and the frame is already lost by the time the loop asks.
//! Splitting such an item is a per-stage fix; this module only makes the case
//! visible, aggregated per frame instead of logged per item.

use std::fmt::Write as _;

/// Overruns recorded for one stage during a frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StageOverrun {
    pub(crate) stage: &'static str,
    pub(crate) budget_ms: f32,
    /// Items that alone took at least the stage's whole budget.
    pub(crate) items: u32,
    pub(crate) worst_ms: f32,
    pub(crate) worst_label: String,
    /// Summed `item_ms - budget_ms` over the offending items.
    pub(crate) over_ms: f32,
}

#[derive(Default)]
pub(crate) struct StageBudgetWatchdog {
    stages: Vec<StageOverrun>,
}

impl StageBudgetWatchdog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Charges one finished item against its stage budget.
    ///
    /// Returns whether this item overran. `label` is only called for an actual
    /// overrun, so the caller can format freely without paying for it on the
    /// hot path.
    pub(crate) fn record(
        &mut self,
        stage: &'static str,
        budget_ms: f32,
        item_ms: f32,
        label: impl FnOnce() -> String,
    ) -> bool {
        // A non-positive budget means the stage is disabled this frame, not that
        // everything overruns.
        if budget_ms <= 0.0 || !item_ms.is_finite() || item_ms < budget_ms {
            return false;
        }
        let over_ms = item_ms - budget_ms;
        match self.stages.iter_mut().find(|entry| entry.stage == stage) {
            Some(entry) => {
                entry.items = entry.items.saturating_add(1);
                entry.over_ms += over_ms;
                entry.budget_ms = budget_ms;
                if item_ms > entry.worst_ms {
                    entry.worst_ms = item_ms;
                    entry.worst_label = label();
                }
            }
            None => self.stages.push(StageOverrun {
                stage,
                budget_ms,
                items: 1,
                worst_ms: item_ms,
                worst_label: label(),
                over_ms,
            }),
        }
        true
    }

    /// Worst-first, so a report truncated by a reader still names the offender.
    pub(crate) fn summary(&self) -> Option<String> {
        if self.stages.is_empty() {
            return None;
        }
        let mut ordered: Vec<&StageOverrun> = self.stages.iter().collect();
        ordered.sort_by(|a, b| {
            b.worst_ms
                .partial_cmp(&a.worst_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut out = String::new();
        for (i, entry) in ordered.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(
                &mut out,
                "{}(budget={:.2}ms items={} worst={:.2}ms over=+{:.2}ms",
                entry.stage, entry.budget_ms, entry.items, entry.worst_ms, entry.over_ms
            );
            if entry.worst_label.is_empty() {
                out.push(')');
            } else {
                let _ = write!(&mut out, " worst_item[{}])", entry.worst_label);
            }
        }
        Some(out)
    }

    pub(crate) fn clear(&mut self) {
        self.stages.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_under_the_stage_budget_is_not_an_overrun() {
        let mut watchdog = StageBudgetWatchdog::new();
        assert!(!watchdog.record("lighting.remove", 3.0, 2.99, || "x".to_string()));
        assert_eq!(watchdog.summary(), None);
    }

    #[test]
    fn an_item_at_or_over_the_stage_budget_is_reported() {
        // The case this exists for: the loop's `items > 0` escape hatch let one
        // item spend the whole stage budget and more, unreported.
        let mut watchdog = StageBudgetWatchdog::new();
        assert!(watchdog.record("lighting.remove", 3.0, 19.0, || {
            "chunks=4 reason=stream_evict".to_string()
        }));
        let summary = watchdog.summary().expect("overrun must produce a summary");
        assert!(summary.contains("lighting.remove"), "{summary}");
        assert!(summary.contains("items=1"), "{summary}");
        assert!(summary.contains("worst=19.00ms"), "{summary}");
        assert!(summary.contains("over=+16.00ms"), "{summary}");
        assert!(summary.contains("chunks=4 reason=stream_evict"), "{summary}");
    }

    #[test]
    fn repeated_overruns_aggregate_instead_of_accumulating_entries() {
        let mut watchdog = StageBudgetWatchdog::new();
        for ms in [5.0, 12.0, 7.0] {
            watchdog.record("mesh.upload", 4.0, ms, || format!("ms={ms}"));
        }
        let summary = watchdog.summary().expect("overruns must produce a summary");
        assert_eq!(summary.matches("mesh.upload").count(), 1, "{summary}");
        assert!(summary.contains("items=3"), "{summary}");
        assert!(summary.contains("worst=12.00ms"), "{summary}");
        // (5-4) + (12-4) + (7-4)
        assert!(summary.contains("over=+12.00ms"), "{summary}");
        assert!(summary.contains("ms=12"), "the label must follow the worst item");
    }

    #[test]
    fn stages_are_listed_worst_first() {
        let mut watchdog = StageBudgetWatchdog::new();
        watchdog.record("small", 1.0, 2.0, String::new);
        watchdog.record("huge", 1.0, 40.0, String::new);
        watchdog.record("medium", 1.0, 9.0, String::new);
        let summary = watchdog.summary().expect("overruns must produce a summary");
        let pos = |name: &str| summary.find(name).expect("stage missing from summary");
        assert!(pos("huge") < pos("medium"), "{summary}");
        assert!(pos("medium") < pos("small"), "{summary}");
    }

    #[test]
    fn a_disabled_stage_records_nothing() {
        let mut watchdog = StageBudgetWatchdog::new();
        assert!(!watchdog.record("off", 0.0, 50.0, String::new));
        assert!(!watchdog.record("negative", -1.0, 50.0, String::new));
        assert_eq!(watchdog.summary(), None);
    }

    #[test]
    fn clear_resets_the_frame() {
        let mut watchdog = StageBudgetWatchdog::new();
        watchdog.record("stage", 1.0, 5.0, String::new);
        watchdog.clear();
        assert_eq!(watchdog.summary(), None);
    }

    #[test]
    fn the_label_is_only_built_for_an_overrun() {
        let mut watchdog = StageBudgetWatchdog::new();
        let mut built = 0usize;
        for _ in 0..100 {
            watchdog.record("stage", 10.0, 0.1, || {
                built += 1;
                String::new()
            });
        }
        assert_eq!(built, 0, "formatting must not run on well-behaved items");
    }
}
