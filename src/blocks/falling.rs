//! Sand and gravel (`BlockSpec::falls`). A change next to a falling block, or
//! the block arriving in its cell, schedules a support check two ticks later
//! (Vanilla `FallingBlock`). If the cell below is not solid then, the block
//! drops straight onto the first solid block below and replaces whatever
//! non-solid block is there. There is no falling entity yet, so the block moves
//! at once; a column above it follows block by block through the same updates.

use glam::IVec3;

use super::{BlockBehavior, BlockState};
use crate::blocks::BlockId;
use crate::world::World;
use crate::world::block_updates_system::BlockTicks;

const FALL_CHECK_DELAY_TICKS: u64 = 2;

pub(super) const FALLING_BEHAVIOR: BlockBehavior = BlockBehavior {
    on_neighbor_changed: Some(schedule_fall_check),
    on_scheduled_tick: Some(fall),
};

fn schedule_fall_check(
    _world: &mut World,
    pos: IVec3,
    state: BlockState,
    _from: IVec3,
    ticks: &mut BlockTicks,
) {
    ticks.schedule(pos, state.id, FALL_CHECK_DELAY_TICKS);
}

fn fall(world: &mut World, pos: IVec3, state: BlockState, _ticks: &mut BlockTicks) {
    let mut landing = pos;
    while landing.y > 0 {
        let below = landing - IVec3::Y;
        if world.get_block(below).is_solid() {
            break;
        }
        landing = below;
    }
    if landing == pos {
        return;
    }
    // Landing first: if that cell's chunk is not loaded the block stays put,
    // instead of vanishing because clearing its own cell dropped an emptied
    // chunk it could no longer be restored into.
    if world.set_block_state_loaded_only(landing, state).is_err() {
        return;
    }
    let _ = world.set_block_and_data_loaded_only(pos, BlockId::Air, 0);
}
