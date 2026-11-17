use std::any::Any;

use crate::world::{BlockChange, World};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ColumnCoord {
    pub x: i32,
    pub z: i32,
}

pub struct ActiveSimColumns<'a> {
    pub is_active: &'a dyn Fn(i32, i32) -> bool,
}

pub trait SimSystem {
    fn name(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn on_columns_entered(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
    }
    fn on_columns_exited(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
    }

    /// A drained batch of journaled world changes, oldest first: player
    /// edits, environment ticks and the other systems — never the system's
    /// own changes, and never fluid flow, which keeps its own coalesced path.
    fn on_block_changes(&mut self, _changes: &[BlockChange]) {}

    fn fixed_tick(
        &mut self,
        world: &mut World,
        tick_dt: f32,
        tick_index: u64,
        active: ActiveSimColumns<'_>,
    );
}
