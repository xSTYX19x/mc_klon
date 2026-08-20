//! One shared CPU budget for all incremental lighting stages of a frame.
//!
//! The three lighting pumps (`block_edit`, `chunk_added`, `chunk_removed`) each
//! used to own an independent per-frame budget and ran back to back inside
//! `rebuild_world_lighting_if_dirty`. Their worst cases therefore *added up*:
//! 1.5 + 6.0 + 3.0 = 10.5 ms of a 16.6 ms frame could go to lighting alone,
//! and none of the three could see that the others had already spent the frame.
//!
//! Here they draw from one budget instead, in priority order. That does two
//! things at once: the stacked worst case becomes a single number, and a stage
//! that has nothing to do leaves its share to the ones behind it — during pure
//! streaming the chunk-add pump can use nearly the whole budget, which is more
//! than its old private 6 ms.

use std::time::Instant;

/// The share one stage may use, in ms.
///
/// * `remaining_ms` — what is left of the frame's lighting budget.
/// * `stage_cap_ms` — the stage's own maximum; a stage never grows past it, so
///   one stage cannot monopolize a frame just because it ran first.
/// * `reserve_ms`  — kept back for the stages *behind* this one, so a busy
///   early stage cannot starve a later one.
/// * `floor_ms`    — a stage always gets at least this much, even on an already
///   exhausted budget. Without it a stage behind two busy ones would never take
///   a single step and its backlog would grow without bound. Steps are
///   individually bounded, so the floor costs at most one step.
pub(crate) fn stage_budget_ms(
    remaining_ms: f32,
    stage_cap_ms: f32,
    reserve_ms: f32,
    floor_ms: f32,
) -> f32 {
    let cap = stage_cap_ms.max(0.0);
    let floor = floor_ms.clamp(0.0, cap);
    let share = (remaining_ms - reserve_ms.max(0.0)).min(cap);
    share.max(floor)
}

/// Tracks how much of the frame's lighting budget is already spent.
///
/// Time is measured from construction rather than accumulated per stage, so
/// anything happening between the pumps is charged too — the budget describes
/// the frame, not the sum of the stages.
#[derive(Debug)]
pub(crate) struct LightingFrameBudget {
    started: Instant,
    total_ms: f32,
}

impl LightingFrameBudget {
    pub(crate) fn new(total_ms: f32) -> Self {
        Self {
            started: Instant::now(),
            total_ms: total_ms.max(0.0),
        }
    }

    pub(crate) fn used_ms(&self) -> f32 {
        self.started.elapsed().as_secs_f32() * 1000.0
    }

    pub(crate) fn remaining_ms(&self) -> f32 {
        (self.total_ms - self.used_ms()).max(0.0)
    }

    pub(crate) fn total_ms(&self) -> f32 {
        self.total_ms
    }

    /// Budget for the next stage. See [`stage_budget_ms`].
    pub(crate) fn stage_budget_ms(&self, stage_cap_ms: f32, reserve_ms: f32, floor_ms: f32) -> f32 {
        stage_budget_ms(self.remaining_ms(), stage_cap_ms, reserve_ms, floor_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_never_exceeds_its_own_cap() {
        // Even with the whole frame free, one stage may not eat all of it.
        assert_eq!(stage_budget_ms(100.0, 6.0, 0.0, 0.25), 6.0);
    }

    #[test]
    fn an_idle_predecessor_hands_its_share_on() {
        // The point of sharing: with nothing spent yet, the chunk-add stage gets
        // its full cap instead of what a private budget split would have left.
        let full = stage_budget_ms(7.0, 6.0, 1.0, 0.25);
        let after_busy_predecessor = stage_budget_ms(1.5, 6.0, 1.0, 0.25);
        assert_eq!(full, 6.0);
        assert!(
            after_busy_predecessor < full,
            "a stage must see what the frame already spent: {after_busy_predecessor}"
        );
        assert_eq!(after_busy_predecessor, 0.5);
    }

    #[test]
    fn the_reserve_is_kept_for_later_stages() {
        // 3 ms left, 1 ms reserved for the stage behind this one.
        assert_eq!(stage_budget_ms(3.0, 6.0, 1.0, 0.25), 2.0);
    }

    #[test]
    fn an_exhausted_budget_still_leaves_the_floor() {
        // Otherwise the last stage in the order never steps and its backlog
        // grows without bound.
        assert_eq!(stage_budget_ms(0.0, 3.0, 0.0, 0.25), 0.25);
        assert_eq!(stage_budget_ms(-5.0, 3.0, 2.0, 0.25), 0.25);
    }

    #[test]
    fn the_floor_cannot_exceed_the_stage_cap() {
        assert_eq!(stage_budget_ms(0.0, 0.1, 0.0, 0.25), 0.1);
        assert_eq!(stage_budget_ms(0.0, 0.0, 0.0, 0.25), 0.0);
    }

    #[test]
    fn the_stacked_worst_case_is_one_budget_plus_the_floors() {
        // The regression this replaces: 1.5 + 6.0 + 3.0 private budgets.
        let total = 7.0;
        let mut remaining = total;
        let mut granted = 0.0;
        for (cap, reserve, floor) in [(1.5, 2.0, 0.25), (6.0, 1.0, 0.25), (3.0, 0.0, 0.25)] {
            let share = stage_budget_ms(remaining, cap, reserve, floor);
            granted += share;
            remaining = (remaining - share).max(0.0);
        }
        assert!(
            granted <= total + 3.0 * 0.25,
            "stages were granted {granted}ms against a {total}ms budget"
        );
        assert!(granted < 10.5, "must beat the old stacked worst case");
    }

    #[test]
    fn every_stage_gets_something_in_the_worst_order() {
        // Two busy stages first, the third must still make progress.
        let mut remaining: f32 = 7.0;
        let mut shares = Vec::new();
        for (cap, reserve, floor) in [(1.5, 2.0, 0.25), (6.0, 1.0, 0.25), (3.0, 0.0, 0.25)] {
            let share = stage_budget_ms(remaining, cap, reserve, floor);
            shares.push(share);
            // Pretend each stage spends everything it was granted.
            remaining = (remaining - share).max(0.0);
        }
        assert!(
            shares.iter().all(|share| *share > 0.0),
            "no stage may be starved outright: {shares:?}"
        );
    }
}
