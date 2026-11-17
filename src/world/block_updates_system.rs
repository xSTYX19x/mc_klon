//! Neighbor updates and scheduled ticks for blocks: the `BlockSpec` hooks
//! `on_neighbor_changed` and `on_scheduled_tick` (Vanilla `neighborChanged` and
//! `LevelTicks<Block>`).
//!
//! The system sees world changes as the journal batches `State` drains
//! (`SimSystem::on_block_changes`) and notifies the changed cell and its six
//! neighbors. Changes its own hooks make never come back in a batch: it reads
//! them from the journal right after each hook and notifies around them in
//! turn, one chain level deeper. A chain stops at `MAX_CHAIN_DEPTH`; the change
//! that would go deeper stays, but nothing around it is notified.

use std::collections::VecDeque;

use glam::IVec3;

use crate::blocks::{BlockBehavior, BlockId};
use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::world::tick_scheduler::{TickKey, TickScheduler};
use crate::world::{BlockChange, World};

/// Vanilla's limit on chained neighbor updates.
const MAX_CHAIN_DEPTH: u16 = 512;
/// Changes notified per tick; the rest waits for the next tick.
const MAX_CHANGES_PER_TICK: usize = 1024;
/// Scheduled ticks run per tick; the rest stays ready for the next tick.
const MAX_SCHEDULED_TICKS_PER_TICK: usize = 256;

/// The changed cell itself (a block arriving, Vanilla `onPlace`) and its six
/// neighbors.
const NOTIFIED_OFFSETS: [IVec3; 7] = [
    IVec3::ZERO,
    IVec3::X,
    IVec3::NEG_X,
    IVec3::Y,
    IVec3::NEG_Y,
    IVec3::Z,
    IVec3::NEG_Z,
];

/// A scheduled tick. It fires only if `block` is still at `pos` by then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlockTick {
    pub(crate) pos: IVec3,
    pub(crate) block: BlockId,
}

impl TickKey for BlockTick {
    fn pos(&self) -> IVec3 {
        self.pos
    }
}

/// The scheduled-tick queue, handed to every hook.
pub(crate) struct BlockTicks {
    scheduler: TickScheduler<BlockTick>,
    current_tick: u64,
}

impl BlockTicks {
    fn new() -> Self {
        Self {
            scheduler: TickScheduler::new(),
            current_tick: 0,
        }
    }

    /// Wakes `block` at `pos` through its `on_scheduled_tick` in `delay` ticks
    /// (at least one), if it is still there then. A pending tick for the same
    /// block and cell keeps the earlier time.
    pub(crate) fn schedule(&mut self, pos: IVec3, block: BlockId, delay: u64) {
        if !World::in_bounds(pos) {
            return;
        }
        let due_tick = self.current_tick.wrapping_add(delay.max(1));
        self.scheduler.schedule(BlockTick { pos, block }, due_tick);
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingChange {
    pos: IVec3,
    depth: u16,
}

pub(crate) struct BlockUpdatesSystem {
    ticks: BlockTicks,
    /// Changed cells still to notify around, oldest first.
    pending: VecDeque<PendingChange>,
    /// The hooks per block; tests swap in their own.
    behavior: fn(BlockId) -> BlockBehavior,
}

impl BlockUpdatesSystem {
    pub(crate) fn new() -> Self {
        Self::with_behavior(BlockId::behavior)
    }

    fn with_behavior(behavior: fn(BlockId) -> BlockBehavior) -> Self {
        Self {
            ticks: BlockTicks::new(),
            pending: VecDeque::new(),
            behavior,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.ticks.scheduler.clear();
        self.ticks.current_tick = 0;
        self.pending.clear();
    }

    fn run_scheduled_ticks(&mut self, world: &mut World) {
        self.ticks.scheduler.collect_ready(self.ticks.current_tick);
        for _ in 0..MAX_SCHEDULED_TICKS_PER_TICK {
            let Some(tick) = self.ticks.scheduler.pop_ready() else {
                break;
            };
            let state = world.get_block_state(tick.pos);
            if state.id != tick.block {
                continue;
            }
            let Some(on_tick) = (self.behavior)(state.id).on_scheduled_tick else {
                continue;
            };
            let since = world.block_change_count();
            on_tick(world, tick.pos, state, &mut self.ticks);
            // A scheduled tick starts a new chain.
            self.queue_own_changes(world, since, 0);
        }
    }

    fn notify_around(&mut self, world: &mut World, change: PendingChange) {
        for offset in NOTIFIED_OFFSETS {
            let pos = change.pos + offset;
            if !World::in_bounds(pos) {
                continue;
            }
            let state = world.get_block_state(pos);
            let Some(on_changed) = (self.behavior)(state.id).on_neighbor_changed else {
                continue;
            };
            let since = world.block_change_count();
            on_changed(world, pos, state, change.pos, &mut self.ticks);
            self.queue_own_changes(world, since, change.depth + 1);
        }
    }

    /// Queues what a hook just changed; past the depth limit the changes stay
    /// but are not notified around.
    fn queue_own_changes(&mut self, world: &World, since: usize, depth: u16) {
        if depth > MAX_CHAIN_DEPTH {
            return;
        }
        for change in world.block_changes_since(since) {
            self.pending.push_back(PendingChange {
                pos: change.pos,
                depth,
            });
        }
    }
}

impl SimSystem for BlockUpdatesSystem {
    fn name(&self) -> &'static str {
        "block_updates"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn on_columns_exited(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        active: ActiveSimColumns<'_>,
    ) {
        let is_active = |pos: IVec3| {
            let coord = World::world_to_chunk_coord(pos);
            (active.is_active)(coord.x, coord.z)
        };
        self.ticks.scheduler.retain_positions(is_active);
        self.pending.retain(|change| is_active(change.pos));
    }

    fn on_block_changes(&mut self, changes: &[BlockChange]) {
        self.pending.extend(changes.iter().map(|change| PendingChange {
            pos: change.pos,
            depth: 0,
        }));
    }

    fn fixed_tick(
        &mut self,
        world: &mut World,
        _tick_dt: f32,
        _tick_index: u64,
        _active: ActiveSimColumns<'_>,
    ) {
        self.ticks.current_tick = self.ticks.current_tick.wrapping_add(1);
        // Scheduled ticks first, so what they change is notified around in the
        // same tick, like Vanilla's synchronous neighbor updates.
        self.run_scheduled_ticks(world);
        for _ in 0..MAX_CHANGES_PER_TICK {
            let Some(change) = self.pending.pop_front() else {
                break;
            };
            self.notify_around(world, change);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockState;

    fn all_active() -> ActiveSimColumns<'static> {
        ActiveSimColumns {
            is_active: &|_, _| true,
        }
    }

    /// What `State` does around the system: hand it what changed outside, run
    /// it, drain what it changed itself (never handed back to it).
    fn hand_over_journal(system: &mut BlockUpdatesSystem, world: &mut World) {
        let mut changes = Vec::new();
        world.drain_block_changes(&mut changes);
        system.on_block_changes(&changes);
    }

    fn run_ticks(system: &mut BlockUpdatesSystem, world: &mut World, ticks: usize) {
        let mut drained = Vec::new();
        for _ in 0..ticks {
            system.fixed_tick(world, 0.05, 0, all_active());
            world.drain_block_changes(&mut drained);
            drained.clear();
        }
    }

    fn set(world: &mut World, pos: IVec3, block: BlockId) {
        world.set_block(pos, block).unwrap();
    }

    fn discard_journal(world: &mut World) {
        world.drain_block_changes(&mut Vec::new());
    }

    #[test]
    fn sand_and_gravel_fall_two_ticks_after_losing_support() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::new();
        for x in [0, 2] {
            set(&mut world, IVec3::new(x, 5, 0), BlockId::Stone);
            set(&mut world, IVec3::new(x, 8, 0), BlockId::Stone);
        }
        set(&mut world, IVec3::new(0, 9, 0), BlockId::Sand);
        set(&mut world, IVec3::new(0, 10, 0), BlockId::Sand);
        set(&mut world, IVec3::new(2, 9, 0), BlockId::Gravel);
        discard_journal(&mut world);

        set(&mut world, IVec3::new(0, 8, 0), BlockId::Air);
        set(&mut world, IVec3::new(2, 8, 0), BlockId::Air);
        hand_over_journal(&mut system, &mut world);

        run_ticks(&mut system, &mut world, 2);
        assert_eq!(world.get_block(IVec3::new(0, 9, 0)), BlockId::Sand);
        assert_eq!(world.get_block(IVec3::new(2, 9, 0)), BlockId::Gravel);

        run_ticks(&mut system, &mut world, 1);
        assert_eq!(world.get_block(IVec3::new(0, 6, 0)), BlockId::Sand);
        assert_eq!(world.get_block(IVec3::new(2, 6, 0)), BlockId::Gravel);
        assert_eq!(world.get_block(IVec3::new(2, 9, 0)), BlockId::Air);
        // The sand above follows two ticks after its support moved away.
        assert_eq!(world.get_block(IVec3::new(0, 10, 0)), BlockId::Sand);

        run_ticks(&mut system, &mut world, 2);
        assert_eq!(world.get_block(IVec3::new(0, 7, 0)), BlockId::Sand);
        for y in 8..=10 {
            assert_eq!(world.get_block(IVec3::new(0, y, 0)), BlockId::Air, "y {y}");
        }
    }

    /// As before the move: the block passes through anything non-solid and
    /// replaces the cell it lands in.
    #[test]
    fn placed_sand_falls_through_non_solid_blocks_onto_the_first_solid_one() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::new();
        set(&mut world, IVec3::new(0, 5, 0), BlockId::Stone);
        set(&mut world, IVec3::new(0, 6, 0), BlockId::Dandelion);
        set(&mut world, IVec3::new(0, 7, 0), BlockId::WaterStill);
        discard_journal(&mut world);

        set(&mut world, IVec3::new(0, 9, 0), BlockId::Sand);
        hand_over_journal(&mut system, &mut world);
        run_ticks(&mut system, &mut world, 3);

        assert_eq!(world.get_block(IVec3::new(0, 6, 0)), BlockId::Sand);
        assert_eq!(world.get_block(IVec3::new(0, 7, 0)), BlockId::WaterStill);
        assert_eq!(world.get_block(IVec3::new(0, 9, 0)), BlockId::Air);
    }

    #[test]
    fn supported_sand_stays() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::new();
        set(&mut world, IVec3::new(0, 5, 0), BlockId::Stone);
        set(&mut world, IVec3::new(0, 6, 0), BlockId::Sand);
        set(&mut world, IVec3::new(1, 6, 0), BlockId::Dirt);
        hand_over_journal(&mut system, &mut world);
        run_ticks(&mut system, &mut world, 5);
        assert_eq!(world.get_block(IVec3::new(0, 6, 0)), BlockId::Sand);
    }

    fn turn_to_cobblestone(world: &mut World, pos: IVec3, _state: BlockState, _: &mut BlockTicks) {
        world.set_block(pos, BlockId::Cobblestone).unwrap();
    }

    fn stone_and_dirt_tick_into_cobblestone(block: BlockId) -> BlockBehavior {
        match block {
            BlockId::Stone | BlockId::Dirt => BlockBehavior {
                on_scheduled_tick: Some(turn_to_cobblestone),
                ..BlockBehavior::default()
            },
            _ => BlockBehavior::default(),
        }
    }

    #[test]
    fn a_scheduled_tick_fires_exactly_after_its_delay() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::with_behavior(stone_and_dirt_tick_into_cobblestone);
        let pos = IVec3::new(3, 10, 3);
        set(&mut world, pos, BlockId::Stone);
        discard_journal(&mut world);

        system.ticks.schedule(pos, BlockId::Stone, 10);
        run_ticks(&mut system, &mut world, 9);
        assert_eq!(world.get_block(pos), BlockId::Stone);
        run_ticks(&mut system, &mut world, 1);
        assert_eq!(world.get_block(pos), BlockId::Cobblestone);
    }

    #[test]
    fn a_scheduled_tick_is_dropped_when_its_block_left_the_cell() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::with_behavior(stone_and_dirt_tick_into_cobblestone);
        let pos = IVec3::new(3, 10, 3);
        set(&mut world, pos, BlockId::Stone);
        system.ticks.schedule(pos, BlockId::Stone, 3);
        // Dirt has a scheduled-tick hook too, but the tick belongs to stone.
        set(&mut world, pos, BlockId::Dirt);
        discard_journal(&mut world);
        run_ticks(&mut system, &mut world, 5);
        assert_eq!(world.get_block(pos), BlockId::Dirt);
    }

    /// Reacts only to arriving in its own cell, so each chain level places
    /// exactly one stone.
    fn grow_east(world: &mut World, pos: IVec3, _state: BlockState, from: IVec3, _: &mut BlockTicks) {
        let east = pos + IVec3::X;
        if from == pos && world.get_block(east) == BlockId::Air {
            world.set_block(east, BlockId::Stone).unwrap();
        }
    }

    fn stone_grows_east(block: BlockId) -> BlockBehavior {
        match block {
            BlockId::Stone => BlockBehavior {
                on_neighbor_changed: Some(grow_east),
                ..BlockBehavior::default()
            },
            _ => BlockBehavior::default(),
        }
    }

    /// Every stone a hook places wakes the next one: an endless chain that the
    /// depth limit has to end. The stone placed at depth 513 stays, but nothing
    /// is notified around it, so the chain ends there.
    #[test]
    fn a_chain_reaction_stops_at_the_depth_limit() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::with_behavior(stone_grows_east);
        let start = IVec3::new(0, 10, 0);
        set(&mut world, start, BlockId::Stone);
        hand_over_journal(&mut system, &mut world);
        run_ticks(&mut system, &mut world, 4);

        let last = MAX_CHAIN_DEPTH as i32 + 1;
        for x in 0..=last {
            assert_eq!(world.get_block(start + IVec3::X * x), BlockId::Stone, "x {x}");
        }
        assert_eq!(world.get_block(start + IVec3::X * (last + 1)), BlockId::Air);
        assert!(system.pending.is_empty());
    }

    #[test]
    fn leaving_columns_drops_their_ticks_and_changes() {
        let mut world = World::new();
        let mut system = BlockUpdatesSystem::with_behavior(stone_and_dirt_tick_into_cobblestone);
        let near = IVec3::new(1, 10, 1);
        let far = IVec3::new(200, 10, 200);
        set(&mut world, near, BlockId::Stone);
        set(&mut world, far, BlockId::Stone);
        hand_over_journal(&mut system, &mut world);
        system.ticks.schedule(near, BlockId::Stone, 2);
        system.ticks.schedule(far, BlockId::Stone, 2);

        let near_column = World::world_to_chunk_coord(near);
        let is_active = |x: i32, z: i32| x == near_column.x && z == near_column.z;
        system.on_columns_exited(&mut world, &[], ActiveSimColumns {
            is_active: &is_active,
        });
        assert!(system.pending.iter().all(|change| change.pos == near));

        run_ticks(&mut system, &mut world, 3);
        assert_eq!(world.get_block(near), BlockId::Cobblestone);
        assert_eq!(world.get_block(far), BlockId::Stone);
    }
}
