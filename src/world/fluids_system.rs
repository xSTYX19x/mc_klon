use std::collections::VecDeque;

use glam::IVec3;

use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::world::World;
use crate::world::fluids::{FluidDebugCounts, FluidKind, FluidSystem};

const FLUID_BOOTSTRAP_BUDGET_PER_TICK: usize = 2;
const FLUID_TRANSFORM_FLUSH_BUDGET_PER_SIM_STEP: usize = 2048;

pub(crate) struct FluidsSystem {
    fluids: FluidSystem,
    bootstrap_queue: VecDeque<ColumnCoord>,
    bootstrap_budget_per_tick: usize,
    pending_changed_positions: Vec<IVec3>,
    debug_counts: FluidDebugCounts,
}

impl FluidsSystem {
    pub(crate) fn new(fluids: FluidSystem) -> Self {
        Self {
            fluids,
            bootstrap_queue: VecDeque::new(),
            bootstrap_budget_per_tick: FLUID_BOOTSTRAP_BUDGET_PER_TICK,
            pending_changed_positions: Vec::new(),
            debug_counts: FluidDebugCounts::default(),
        }
    }

    pub(crate) fn rebuild_from_world(&mut self, world: &mut World) {
        self.fluids.rebuild_from_world(world);
        self.bootstrap_queue.clear();
        self.pending_changed_positions.clear();
        self.debug_counts = self.fluids.debug_counts();
    }

    pub(crate) fn reset(&mut self) {
        self.fluids = FluidSystem::new();
        self.bootstrap_queue.clear();
        self.pending_changed_positions.clear();
        self.debug_counts = FluidDebugCounts::default();
    }

    pub(crate) fn mark_transforming_with_neighbors(&mut self, world: &World, pos: IVec3) {
        self.fluids.mark_transforming_with_neighbors(world, pos);
    }

    pub(crate) fn tick_rate_for_kind(&self, kind: FluidKind) -> u64 {
        self.fluids.tick_rate_for_kind(kind)
    }

    pub(crate) fn schedule_fluid_update(&mut self, pos: IVec3, kind: FluidKind, delay_ticks: u64) {
        self.fluids.schedule_fluid_update(pos, kind, delay_ticks);
    }

    pub(crate) fn prune_outside_keep_radius_xz(
        &mut self,
        center_chunk_x: i32,
        center_chunk_z: i32,
        keep_radius_chunks: i32,
    ) {
        self.fluids.prune_outside_keep_radius_xz(
            center_chunk_x,
            center_chunk_z,
            keep_radius_chunks,
        );
    }

    pub(crate) fn debug_counts(&self) -> FluidDebugCounts {
        self.debug_counts
    }

    pub(crate) fn bootstrap_queue_len(&self) -> usize {
        self.bootstrap_queue.len()
    }

    pub(crate) fn drain_changed_positions_into(&mut self, out: &mut Vec<IVec3>) {
        out.append(&mut self.pending_changed_positions);
    }

    fn queue_bootstrap_column(&mut self, col: ColumnCoord) {
        if self.bootstrap_queue.iter().any(|queued| *queued == col) {
            return;
        }
        self.bootstrap_queue.push_back(col);
    }
}

impl SimSystem for FluidsSystem {
    fn name(&self) -> &'static str {
        "fluids"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn on_columns_entered(
        &mut self,
        _world: &mut World,
        cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
        for &col in cols {
            self.queue_bootstrap_column(col);
        }
    }

    fn on_columns_exited(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        active: ActiveSimColumns<'_>,
    ) {
        self.fluids
            .retain_only_active_columns(|x, z| (active.is_active)(x, z));
    }

    fn fixed_tick(
        &mut self,
        world: &mut World,
        _tick_dt: f32,
        _tick_index: u64,
        active: ActiveSimColumns<'_>,
    ) {
        let mut budget = self.bootstrap_budget_per_tick;
        while budget > 0 {
            let Some(col) = self.bootstrap_queue.pop_front() else {
                break;
            };
            if !(active.is_active)(col.x, col.z) {
                continue;
            }
            self.fluids.bootstrap_column_from_world(world, col.x, col.z);
            budget -= 1;
        }

        self.fluids
            .flush_transforming(world, FLUID_TRANSFORM_FLUSH_BUDGET_PER_SIM_STEP);
        let changed = self.fluids.step(world);
        self.pending_changed_positions.extend(changed);
        self.debug_counts = self.fluids.debug_counts();
    }
}
