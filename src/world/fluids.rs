use glam::{IVec3, Vec3};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::blocks::BlockId;

use super::{CHUNK_SIZE, SetBlockError, World};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FluidKind {
    Water,
    Lava,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LavaDimensionMode {
    OverworldLike,
    NetherLike,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FluidState {
    pub(crate) kind: FluidKind,
    pub(crate) level: u8,
    pub(crate) falling: bool,
    pub(crate) source_origin: bool,
    pub(crate) worldgen_static: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FluidParams {
    tick_rate: u64,
    max_distance: u8,
    horizontal_level_step: u8,
    max_updates_per_tick: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FluidDebugCounts {
    pub(crate) water_scheduler: (usize, usize),
    pub(crate) lava_scheduler: (usize, usize),
    pub(crate) unloaded_writes: (u64, u64),
}

pub(crate) fn is_water(block: BlockId) -> bool {
    matches!(block, BlockId::WaterStill | BlockId::WaterFlowing)
}

pub(crate) fn is_lava(block: BlockId) -> bool {
    matches!(block, BlockId::LavaStill | BlockId::LavaFlowing)
}

pub(crate) fn fluid_kind(block: BlockId) -> Option<FluidKind> {
    if is_water(block) {
        Some(FluidKind::Water)
    } else if is_lava(block) {
        Some(FluidKind::Lava)
    } else {
        None
    }
}

pub(crate) fn make_still(kind: FluidKind) -> BlockId {
    match kind {
        FluidKind::Water => BlockId::WaterStill,
        FluidKind::Lava => BlockId::LavaStill,
    }
}

pub(crate) fn make_flowing(kind: FluidKind) -> BlockId {
    match kind {
        FluidKind::Water => BlockId::WaterFlowing,
        FluidKind::Lava => BlockId::LavaFlowing,
    }
}

pub(crate) fn fluid_data_level(data: u8) -> u8 {
    data & FLUID_LEVEL_MASK
}

pub(crate) fn fluid_data_set_level(data: u8, level: u8) -> u8 {
    (data & !FLUID_LEVEL_MASK) | (level & FLUID_LEVEL_MASK)
}

pub(crate) fn fluid_data_is_falling(data: u8) -> bool {
    (data & FLUID_FLAG_FALLING) != 0
}

pub(crate) fn fluid_data_set_falling(data: u8, on: bool) -> u8 {
    if on {
        data | FLUID_FLAG_FALLING
    } else {
        data & !FLUID_FLAG_FALLING
    }
}

pub(crate) fn fluid_data_is_source_origin(data: u8) -> bool {
    (data & FLUID_FLAG_SOURCE_ORIGIN) != 0
}

pub(crate) fn fluid_data_set_source_origin(data: u8, on: bool) -> u8 {
    if on {
        data | FLUID_FLAG_SOURCE_ORIGIN
    } else {
        data & !FLUID_FLAG_SOURCE_ORIGIN
    }
}

pub(crate) fn fluid_data_is_worldgen_static(data: u8) -> bool {
    (data & FLUID_FLAG_WORLDGEN_STATIC) != 0
}

pub(crate) fn fluid_data_set_worldgen_static(data: u8, on: bool) -> u8 {
    if on {
        data | FLUID_FLAG_WORLDGEN_STATIC
    } else {
        data & !FLUID_FLAG_WORLDGEN_STATIC
    }
}

pub(crate) fn fluid_data_make(level: u8, falling: bool, source_origin: bool) -> u8 {
    fluid_data_make_worldgen_static(level, falling, source_origin, false)
}

pub(crate) fn fluid_data_make_worldgen_static(
    level: u8,
    falling: bool,
    source_origin: bool,
    worldgen_static: bool,
) -> u8 {
    fluid_data_set_worldgen_static(
        fluid_data_set_source_origin(
            fluid_data_set_falling(fluid_data_set_level(0, level), falling),
            source_origin,
        ),
        worldgen_static,
    )
}

pub(crate) fn fluid_data_make_static(level: u8, source_origin: bool) -> u8 {
    fluid_data_make_worldgen_static(level, false, source_origin, true)
}

pub(crate) fn is_water_source(block_id: BlockId, data: u8) -> bool {
    block_id == BlockId::WaterStill && fluid_data_is_source_origin(data)
}

pub(crate) fn is_lava_source(block_id: BlockId, data: u8) -> bool {
    block_id == BlockId::LavaStill && fluid_data_is_source_origin(data)
}

pub(crate) fn is_source(block_id: BlockId, data: u8) -> bool {
    is_water_source(block_id, data) || is_lava_source(block_id, data)
}

pub(crate) fn opposite(kind: FluidKind) -> FluidKind {
    match kind {
        FluidKind::Water => FluidKind::Lava,
        FluidKind::Lava => FluidKind::Water,
    }
}

fn column_of_world_pos(pos: IVec3) -> (i32, i32) {
    (pos.x.div_euclid(CHUNK_SIZE), pos.z.div_euclid(CHUNK_SIZE))
}

const MINECRAFT_TICK_SECONDS: f32 = 1.0 / 20.0;
pub(crate) const FLUID_FIXED_STEP_SECONDS: f32 = MINECRAFT_TICK_SECONDS;

pub(crate) const WATER_SPREAD_INTERVAL_TICKS: u64 = 5;
pub(crate) const LAVA_SPREAD_INTERVAL_TICKS: u64 = 30;
pub(crate) const LAVA_SPREAD_INTERVAL_TICKS_NETHER: u64 = 10;

const FLUID_LEVEL_MASK: u8 = 0x0F;
const FLUID_FLAG_FALLING: u8 = 0x80;
const FLUID_FLAG_SOURCE_ORIGIN: u8 = 0x40;
const FLUID_FLAG_WORLDGEN_STATIC: u8 = 0x20;

const WATER_MAX_LEVEL: u8 = 7;
const LAVA_MAX_LEVEL_OVERWORLD: u8 = 6;
const LAVA_MAX_LEVEL_NETHER: u8 = 7;
const WATER_HORIZONTAL_STEP: u8 = 1;
const LAVA_HORIZONTAL_STEP_OVERWORLD: u8 = 2;
const LAVA_HORIZONTAL_STEP_NETHER: u8 = 1;

const MAX_WATER_UPDATES_PER_TICK: usize = 256;
const MAX_LAVA_UPDATES_PER_TICK: usize = 128;
pub(crate) const FLUID_MAX_TICKS_PER_FRAME: usize = 2;
const FLUID_REBUILD_SETTLE_TICKS: usize = 96;
const SCHEDULER_WHEEL_SIZE: usize = 512;

pub(crate) const FLUID_CURRENT_PUSH_SPEED: f32 = 1.39;

const HORIZONTAL_DIRS: [IVec3; 4] = [
    IVec3::new(1, 0, 0),
    IVec3::new(-1, 0, 0),
    IVec3::new(0, 0, 1),
    IVec3::new(0, 0, -1),
];

const ALL_NEIGHBOR_DIRS: [IVec3; 6] = [
    IVec3::new(1, 0, 0),
    IVec3::new(-1, 0, 0),
    IVec3::new(0, 1, 0),
    IVec3::new(0, -1, 0),
    IVec3::new(0, 0, 1),
    IVec3::new(0, 0, -1),
];

#[derive(Default)]
struct FluidScheduler {
    wheel: Vec<Vec<IVec3>>,
    ready: VecDeque<IVec3>,
    ready_set: HashSet<IVec3>,
    scheduled_due: HashMap<IVec3, u64>,
    scratch: Vec<IVec3>,
}

impl FluidScheduler {
    fn new() -> Self {
        Self {
            wheel: vec![Vec::new(); SCHEDULER_WHEEL_SIZE],
            ready: VecDeque::new(),
            ready_set: HashSet::new(),
            scheduled_due: HashMap::new(),
            scratch: Vec::with_capacity(512),
        }
    }

    fn clear(&mut self) {
        for bucket in &mut self.wheel {
            bucket.clear();
        }
        self.ready.clear();
        self.ready_set.clear();
        self.scheduled_due.clear();
        self.scratch.clear();
    }

    fn has_pending(&self) -> bool {
        !self.ready.is_empty() || !self.scheduled_due.is_empty()
    }

    fn schedule(&mut self, pos: IVec3, due_tick: u64) {
        match self.scheduled_due.get(&pos).copied() {
            Some(existing_due) if existing_due <= due_tick => return,
            _ => {
                self.scheduled_due.insert(pos, due_tick);
                let bucket = (due_tick as usize) % self.wheel.len();
                self.wheel[bucket].push(pos);
            }
        }
    }

    fn collect_ready(&mut self, current_tick: u64) {
        let bucket_index = (current_tick as usize) % self.wheel.len();
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.append(&mut self.wheel[bucket_index]);

        for pos in scratch.drain(..) {
            let Some(&due_tick) = self.scheduled_due.get(&pos) else {
                continue;
            };

            if due_tick == current_tick {
                if self.ready_set.insert(pos) {
                    self.ready.push_back(pos);
                }
            } else {
                let target_bucket = (due_tick as usize) % self.wheel.len();
                self.wheel[target_bucket].push(pos);
            }
        }

        self.scratch = scratch;
    }

    fn pop_ready(&mut self) -> Option<IVec3> {
        let pos = self.ready.pop_front()?;
        self.ready_set.remove(&pos);
        self.scheduled_due.remove(&pos);
        Some(pos)
    }

    fn retain_positions<F: Fn(IVec3) -> bool>(&mut self, keep: F) {
        for bucket in &mut self.wheel {
            bucket.retain(|pos| keep(*pos));
        }

        let mut filtered_ready = VecDeque::with_capacity(self.ready.len());
        while let Some(pos) = self.ready.pop_front() {
            if keep(pos) {
                filtered_ready.push_back(pos);
            }
        }
        self.ready = filtered_ready;
        self.ready_set.retain(|pos| keep(*pos));
        self.scheduled_due.retain(|pos, _| keep(*pos));
        self.scratch.retain(|pos| keep(*pos));
    }

    fn debug_counts(&self) -> (usize, usize) {
        (self.ready.len(), self.scheduled_due.len())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamLock {
    kind: FluidKind,
    dir: IVec3,
    origin: IVec3,
}

pub(crate) struct FluidSystem {
    water_scheduler: FluidScheduler,
    lava_scheduler: FluidScheduler,
    // Dedupe queue for block-change-triggered rescheduling. We collect first and schedule with a
    // per-step budget to avoid bursts of thousands of scheduler inserts in one frame.
    transforming: VecDeque<IVec3>,
    transforming_set: HashSet<IVec3>,
    stream_locks: HashMap<IVec3, StreamLock>,
    max_water_updates_per_tick: usize,
    max_lava_updates_per_tick: usize,
    global_tick: u64,
    lava_dimension_mode: LavaDimensionMode,
    unloaded_water_writes: u64,
    unloaded_lava_writes: u64,
}

impl FluidSystem {
    pub(crate) fn new() -> Self {
        Self {
            water_scheduler: FluidScheduler::new(),
            lava_scheduler: FluidScheduler::new(),
            transforming: VecDeque::new(),
            transforming_set: HashSet::new(),
            stream_locks: HashMap::new(),
            max_water_updates_per_tick: MAX_WATER_UPDATES_PER_TICK,
            max_lava_updates_per_tick: MAX_LAVA_UPDATES_PER_TICK,
            global_tick: 0,
            lava_dimension_mode: LavaDimensionMode::OverworldLike,
            unloaded_water_writes: 0,
            unloaded_lava_writes: 0,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_lava_dimension_mode(&mut self, mode: LavaDimensionMode) {
        self.lava_dimension_mode = mode;
    }

    fn params_for_kind(&self, kind: FluidKind) -> FluidParams {
        match kind {
            FluidKind::Water => FluidParams {
                tick_rate: WATER_SPREAD_INTERVAL_TICKS,
                max_distance: WATER_MAX_LEVEL,
                horizontal_level_step: WATER_HORIZONTAL_STEP,
                max_updates_per_tick: self.max_water_updates_per_tick,
            },
            FluidKind::Lava => match self.lava_dimension_mode {
                LavaDimensionMode::OverworldLike => FluidParams {
                    tick_rate: LAVA_SPREAD_INTERVAL_TICKS,
                    max_distance: LAVA_MAX_LEVEL_OVERWORLD,
                    horizontal_level_step: LAVA_HORIZONTAL_STEP_OVERWORLD,
                    max_updates_per_tick: self.max_lava_updates_per_tick,
                },
                LavaDimensionMode::NetherLike => FluidParams {
                    tick_rate: LAVA_SPREAD_INTERVAL_TICKS_NETHER,
                    max_distance: LAVA_MAX_LEVEL_NETHER,
                    horizontal_level_step: LAVA_HORIZONTAL_STEP_NETHER,
                    max_updates_per_tick: self.max_lava_updates_per_tick,
                },
            },
        }
    }

    pub(crate) fn tick_rate_for_kind(&self, kind: FluidKind) -> u64 {
        self.params_for_kind(kind).tick_rate
    }

    pub(crate) fn prune_outside_keep_radius_xz(
        &mut self,
        center_chunk_x: i32,
        center_chunk_z: i32,
        keep_radius_chunks: i32,
    ) {
        let keep_radius_sq = keep_radius_chunks * keep_radius_chunks;
        let keep = |pos: IVec3| {
            let chunk_x = pos.x.div_euclid(CHUNK_SIZE);
            let chunk_z = pos.z.div_euclid(CHUNK_SIZE);
            let dx = chunk_x - center_chunk_x;
            let dz = chunk_z - center_chunk_z;
            dx * dx + dz * dz <= keep_radius_sq
        };
        self.water_scheduler.retain_positions(keep);
        self.lava_scheduler.retain_positions(keep);
        let mut filtered_transforming = VecDeque::with_capacity(self.transforming.len());
        while let Some(pos) = self.transforming.pop_front() {
            if keep(pos) {
                filtered_transforming.push_back(pos);
            } else {
                self.transforming_set.remove(&pos);
            }
        }
        self.transforming = filtered_transforming;
        self.transforming_set.retain(|pos| keep(*pos));
        self.stream_locks.retain(|pos, _| keep(*pos));
    }

    pub(crate) fn retain_only_active_columns<F>(&mut self, is_active: F)
    where
        F: Fn(i32, i32) -> bool,
    {
        let is_active_ref = &is_active;
        self.water_scheduler.retain_positions(|pos| {
            let (cx, cz) = column_of_world_pos(pos);
            is_active_ref(cx, cz)
        });
        self.lava_scheduler.retain_positions(|pos| {
            let (cx, cz) = column_of_world_pos(pos);
            is_active_ref(cx, cz)
        });

        let mut filtered_transforming = VecDeque::with_capacity(self.transforming.len());
        while let Some(pos) = self.transforming.pop_front() {
            let (cx, cz) = column_of_world_pos(pos);
            if is_active_ref(cx, cz) {
                filtered_transforming.push_back(pos);
            } else {
                self.transforming_set.remove(&pos);
            }
        }
        self.transforming = filtered_transforming;
        self.transforming_set.retain(|pos| {
            let (cx, cz) = column_of_world_pos(*pos);
            is_active_ref(cx, cz)
        });
        self.stream_locks.retain(|pos, _| {
            let (cx, cz) = column_of_world_pos(*pos);
            is_active_ref(cx, cz)
        });
    }

    pub(crate) fn debug_counts(&self) -> FluidDebugCounts {
        FluidDebugCounts {
            water_scheduler: self.water_scheduler.debug_counts(),
            lava_scheduler: self.lava_scheduler.debug_counts(),
            unloaded_writes: (self.unloaded_water_writes, self.unloaded_lava_writes),
        }
    }

    fn scheduler_mut(&mut self, kind: FluidKind) -> &mut FluidScheduler {
        match kind {
            FluidKind::Water => &mut self.water_scheduler,
            FluidKind::Lava => &mut self.lava_scheduler,
        }
    }

    fn scheduler(&self, kind: FluidKind) -> &FluidScheduler {
        match kind {
            FluidKind::Water => &self.water_scheduler,
            FluidKind::Lava => &self.lava_scheduler,
        }
    }

    pub(crate) fn schedule_fluid_update(&mut self, pos: IVec3, kind: FluidKind, delay_ticks: u64) {
        if !World::in_bounds(pos) {
            return;
        }
        let due_tick = self.global_tick.wrapping_add(delay_ticks.max(1));
        self.scheduler_mut(kind).schedule(pos, due_tick);
    }

    fn schedule_ring_for_kind(
        &mut self,
        world: &World,
        center: IVec3,
        kind: FluidKind,
        delay_ticks: u64,
    ) {
        for dy in -1i32..=1 {
            for dz in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx.abs() + dy.abs() + dz.abs() > 1 {
                        continue;
                    }
                    let candidate = center + IVec3::new(dx, dy, dz);
                    if !World::in_bounds(candidate) {
                        continue;
                    }
                    if !Self::has_neighboring_kind(world, candidate, kind) {
                        continue;
                    }
                    self.schedule_fluid_update(candidate, kind, delay_ticks);
                }
            }
        }
    }

    fn has_neighboring_kind(world: &World, pos: IVec3, kind: FluidKind) -> bool {
        if fluid_kind(world.get_block(pos)) == Some(kind) {
            return true;
        }
        for dir in ALL_NEIGHBOR_DIRS {
            let n = pos + dir;
            if !World::in_bounds(n) {
                continue;
            }
            if fluid_kind(world.get_block(n)) == Some(kind) {
                return true;
            }
        }
        false
    }

    fn push_transforming(&mut self, pos: IVec3) {
        if !World::in_bounds(pos) {
            return;
        }
        if self.transforming_set.insert(pos) {
            self.transforming.push_back(pos);
        }
    }

    pub(crate) fn mark_transforming_with_neighbors(&mut self, _world: &World, pos: IVec3) {
        for dy in -1i32..=1 {
            for dz in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx.abs() + dy.abs() + dz.abs() > 2 {
                        continue;
                    }
                    let candidate = pos + IVec3::new(dx, dy, dz);
                    self.push_transforming(candidate);
                }
            }
        }
    }

    pub(crate) fn flush_transforming(&mut self, world: &World, max_items: usize) {
        for _ in 0..max_items {
            let Some(pos) = self.transforming.pop_front() else {
                break;
            };
            self.transforming_set.remove(&pos);

            for kind in [FluidKind::Water, FluidKind::Lava] {
                if Self::has_neighboring_kind(world, pos, kind) {
                    let delay = self.params_for_kind(kind).tick_rate;
                    self.schedule_fluid_update(pos, kind, delay);
                }
            }
        }
    }

    pub(crate) fn enqueue_with_neighbors(&mut self, world: &World, pos: IVec3) {
        for dy in -1i32..=1 {
            for dz in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx.abs() + dy.abs() + dz.abs() > 2 {
                        continue;
                    }
                    let candidate = pos + IVec3::new(dx, dy, dz);
                    if !World::in_bounds(candidate) {
                        continue;
                    }
                    for kind in [FluidKind::Water, FluidKind::Lava] {
                        if Self::has_neighboring_kind(world, candidate, kind) {
                            let delay = self.params_for_kind(kind).tick_rate;
                            self.schedule_fluid_update(candidate, kind, delay);
                        }
                    }
                }
            }
        }
    }

    fn has_pending(&self) -> bool {
        self.scheduler(FluidKind::Water).has_pending()
            || self.scheduler(FluidKind::Lava).has_pending()
    }

    fn get_fluid_state(world: &World, pos: IVec3) -> Option<FluidState> {
        if !World::in_bounds(pos) {
            return None;
        }

        let block = world.get_block(pos);
        let kind = fluid_kind(block)?;
        let data = world.get_block_data(pos);
        let mut level = fluid_data_level(data).min(7);
        let mut falling = fluid_data_is_falling(data);
        let source_origin = fluid_data_is_source_origin(data);
        let worldgen_static = fluid_data_is_worldgen_static(data);
        if source_origin || worldgen_static {
            level = 0;
            falling = false;
        }

        Some(FluidState {
            kind,
            level,
            falling,
            source_origin,
            worldgen_static,
        })
    }

    fn record_unloaded_write_attempt(&mut self, kind: FluidKind) {
        match kind {
            FluidKind::Water => {
                self.unloaded_water_writes = self.unloaded_water_writes.saturating_add(1);
            }
            FluidKind::Lava => {
                self.unloaded_lava_writes = self.unloaded_lava_writes.saturating_add(1);
            }
        }
    }

    fn set_block_loaded_only(
        &mut self,
        world: &mut World,
        pos: IVec3,
        block: BlockId,
        data: u8,
        kind: FluidKind,
    ) -> bool {
        match world.set_block_and_data_loaded_only(pos, block, data) {
            Ok(()) => true,
            Err(SetBlockError::ChunkNotLoaded) => {
                self.record_unloaded_write_attempt(kind);
                false
            }
            Err(SetBlockError::OutOfBounds) => false,
        }
    }

    fn set_fluid_state(
        &mut self,
        world: &mut World,
        pos: IVec3,
        state: FluidState,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        if !World::in_bounds(pos) {
            return false;
        }

        let block = if state.source_origin || state.worldgen_static {
            make_still(state.kind)
        } else {
            make_flowing(state.kind)
        };
        let mut level = state.level;
        let mut falling = state.falling;
        if state.source_origin || state.worldgen_static {
            level = 0;
            falling = false;
        }
        let data = fluid_data_make_worldgen_static(
            level,
            falling,
            state.source_origin,
            state.worldgen_static,
        );

        if world.get_block(pos) == block && world.get_block_data(pos) == data {
            return false;
        }

        if self.set_block_loaded_only(world, pos, block, data, state.kind) {
            changed_positions.insert(pos);
            return true;
        }

        false
    }

    fn clear_fluid(
        &mut self,
        world: &mut World,
        pos: IVec3,
        kind: FluidKind,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        if !World::in_bounds(pos) || fluid_kind(world.get_block(pos)).is_none() {
            return false;
        }

        if self.set_block_loaded_only(world, pos, BlockId::Air, 0, kind) {
            changed_positions.insert(pos);
            return true;
        }

        false
    }

    fn can_replace_block(block: BlockId) -> bool {
        block != BlockId::Bedrock && !block.is_solid()
    }

    fn can_flow_down_from_cell(world: &World, pos: IVec3, kind: FluidKind) -> bool {
        let below = pos + IVec3::new(0, -1, 0);
        if !World::in_bounds(below) {
            return false;
        }
        if !world.is_chunk_loaded_at(below) {
            return false;
        }

        let below_block = world.get_block(below);
        match fluid_kind(below_block) {
            Some(below_kind) if below_kind == kind => {
                // A falling cell below is part of an active drop, not stable support. Treat it as
                // "can fall" so the cell above stays falling and cannot seed sideways in mid-air.
                Self::get_fluid_state(world, below).is_some_and(|s| s.kind == kind && s.falling)
            }
            Some(_) => true,
            None => Self::can_replace_block(below_block),
        }
    }

    fn can_horizontal_spread_into(
        world: &World,
        target: IVec3,
        kind: FluidKind,
        next_level: u8,
    ) -> bool {
        if !World::in_bounds(target) {
            return false;
        }
        if !world.is_chunk_loaded_at(target) {
            return false;
        }

        let target_block = world.get_block(target);
        match fluid_kind(target_block) {
            Some(target_kind) if target_kind == kind => {
                let Some(state) = Self::get_fluid_state(world, target) else {
                    return false;
                };
                if state.falling {
                    let above = target + IVec3::new(0, 1, 0);
                    let fed_from_above =
                        World::in_bounds(above) && fluid_kind(world.get_block(above)) == Some(kind);
                    !fed_from_above
                } else {
                    state.level > next_level
                }
            }
            Some(_) => true,
            None => Self::can_replace_block(target_block),
        }
    }

    fn can_pathfind_through(world: &World, pos: IVec3, kind: FluidKind) -> bool {
        if !World::in_bounds(pos) {
            return false;
        }
        if !world.is_chunk_loaded_at(pos) {
            return false;
        }

        let block = world.get_block(pos);
        match fluid_kind(block) {
            Some(k) if k == kind => true,
            Some(_) => false,
            None => Self::can_replace_block(block),
        }
    }

    fn is_supported_horizontal_cell(world: &World, pos: IVec3, kind: FluidKind) -> bool {
        if !World::in_bounds(pos) {
            return false;
        }

        let below = pos + IVec3::new(0, -1, 0);
        if !World::in_bounds(below) {
            return false;
        }

        let below_block = world.get_block(below);
        below_block.is_solid()
            || Self::get_fluid_state(world, below).is_some_and(|s| s.kind == kind && !s.falling)
    }

    fn horizontal_source_neighbor_count(world: &World, pos: IVec3, kind: FluidKind) -> usize {
        let mut count = 0usize;
        for dir in HORIZONTAL_DIRS {
            let neighbor = pos + dir;
            if !World::in_bounds(neighbor) {
                continue;
            }
            let Some(state) = Self::get_fluid_state(world, neighbor) else {
                continue;
            };
            if state.kind == kind && state.source_origin {
                count += 1;
            }
        }
        count
    }

    fn can_convert_to_infinite_source(world: &World, pos: IVec3, kind: FluidKind) -> bool {
        if kind != FluidKind::Water {
            return false;
        }
        let below = pos + IVec3::new(0, -1, 0);
        if !World::in_bounds(below) {
            return false;
        }
        let below_block = world.get_block(below);
        let supported = fluid_kind(below_block) == Some(kind) || below_block.is_solid();
        supported && Self::horizontal_source_neighbor_count(world, pos, kind) >= 2
    }

    fn remove_stream_lock_if_kind(&mut self, pos: IVec3, kind: FluidKind) {
        if self
            .stream_locks
            .get(&pos)
            .is_some_and(|lock| lock.kind == kind)
        {
            self.stream_locks.remove(&pos);
        }
    }

    fn clear_stream_locks_for_origin(&mut self, origin: IVec3, kind: FluidKind) {
        self.stream_locks
            .retain(|_, lock| !(lock.kind == kind && lock.origin == origin));
    }

    fn is_head(world: &World, pos: IVec3, state: FluidState) -> bool {
        if state.source_origin {
            return true;
        }
        if state.falling || state.level != 0 {
            return false;
        }
        if Self::can_flow_down_from_cell(world, pos, state.kind) {
            return false;
        }

        let above = pos + IVec3::new(0, 1, 0);
        if !World::in_bounds(above) {
            return false;
        }
        Self::get_fluid_state(world, above)
            .is_some_and(|above_state| above_state.kind == state.kind && above_state.falling)
    }

    fn max_steps(params: FluidParams) -> u8 {
        if params.horizontal_level_step == 0 {
            return 0;
        }
        params.max_distance / params.horizontal_level_step
    }

    fn find_drop_straight(
        world: &World,
        origin: IVec3,
        dir: IVec3,
        kind: FluidKind,
        max_steps: u8,
    ) -> Option<u8> {
        for i in 1..=max_steps {
            let p = origin + dir * i32::from(i);
            if !Self::can_pathfind_through(world, p, kind) {
                break;
            }
            if Self::can_flow_down_from_cell(world, p, kind) {
                return Some(i);
            }
            if !Self::is_supported_horizontal_cell(world, p, kind) {
                break;
            }
        }
        None
    }

    fn compute_stream_plan(
        world: &World,
        origin: IVec3,
        kind: FluidKind,
        params: FluidParams,
    ) -> Option<(IVec3, u8)> {
        let max_steps = Self::max_steps(params);
        if max_steps == 0 {
            return None;
        }

        let mut best: Option<(IVec3, u8)> = None;
        for dir in HORIZONTAL_DIRS {
            let Some(steps) = Self::find_drop_straight(world, origin, dir, kind, max_steps) else {
                continue;
            };
            match best {
                Some((_, best_steps)) if steps >= best_steps => {}
                _ => best = Some((dir, steps)),
            }
        }

        best
    }

    fn mixing_result(
        incoming_kind: FluidKind,
        target_state: FluidState,
        incoming_is_downward: bool,
    ) -> Option<BlockId> {
        match (incoming_kind, target_state.kind) {
            (FluidKind::Water, FluidKind::Lava) => {
                if target_state.source_origin {
                    Some(BlockId::Obsidian)
                } else {
                    Some(BlockId::Cobblestone)
                }
            }
            (FluidKind::Lava, FluidKind::Water) => {
                if incoming_is_downward {
                    Some(BlockId::Stone)
                } else {
                    Some(BlockId::Cobblestone)
                }
            }
            _ => None,
        }
    }

    fn try_mix_into(
        &mut self,
        world: &mut World,
        target: IVec3,
        incoming_kind: FluidKind,
        incoming_is_downward: bool,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        if !World::in_bounds(target) {
            return false;
        }

        let Some(target_state) = Self::get_fluid_state(world, target) else {
            return false;
        };
        if target_state.kind == incoming_kind {
            return false;
        }

        let Some(replacement) =
            Self::mixing_result(incoming_kind, target_state, incoming_is_downward)
        else {
            return false;
        };

        if self.set_block_loaded_only(world, target, replacement, 0, incoming_kind) {
            self.stream_locks.remove(&target);
            changed_positions.insert(target);
            for kind in [FluidKind::Water, FluidKind::Lava] {
                let delay = self.params_for_kind(kind).tick_rate;
                self.schedule_ring_for_kind(world, target, kind, delay);
            }
            return true;
        }

        false
    }

    fn min_incoming_horizontal_level(
        world: &World,
        pos: IVec3,
        kind: FluidKind,
        horizontal_step: u8,
    ) -> Option<u8> {
        let mut best: Option<u8> = None;

        for dir in HORIZONTAL_DIRS {
            let n = pos + dir;
            if !World::in_bounds(n) {
                continue;
            }

            let Some(neighbor) = Self::get_fluid_state(world, n) else {
                continue;
            };
            if neighbor.kind != kind {
                continue;
            }

            // Do not treat a falling horizontal neighbor as an implicit level-0 source:
            // drop mouths can be marked `falling` while still carrying a higher horizontal level.
            let candidate = if neighbor.level == 0 {
                horizontal_step
            } else {
                neighbor.level.saturating_add(horizontal_step)
            };

            best = Some(best.map_or(candidate, |current| current.min(candidate)));
        }

        best
    }

    fn compute_next_state_for_pos(
        &self,
        world: &World,
        pos: IVec3,
        kind: FluidKind,
        params: FluidParams,
    ) -> Option<FluidState> {
        if !World::in_bounds(pos) {
            return None;
        }

        let Some(current_state) = Self::get_fluid_state(world, pos) else {
            return None;
        };
        if current_state.kind != kind {
            return None;
        }

        if current_state.source_origin {
            return Some(FluidState {
                kind,
                level: 0,
                falling: false,
                source_origin: true,
                worldgen_static: false,
            });
        }

        if current_state.worldgen_static {
            return Some(FluidState {
                kind,
                level: 0,
                falling: false,
                source_origin: false,
                worldgen_static: true,
            });
        }

        if !current_state.falling && Self::can_convert_to_infinite_source(world, pos, kind) {
            return Some(FluidState {
                kind,
                level: 0,
                falling: false,
                source_origin: true,
                worldgen_static: false,
            });
        }

        let above = pos + IVec3::new(0, 1, 0);
        let below = pos + IVec3::new(0, -1, 0);
        let above_state_same = Self::get_fluid_state(world, above).filter(|s| s.kind == kind);
        let below_state_same = Self::get_fluid_state(world, below).filter(|s| s.kind == kind);
        let was_falling = current_state.falling;
        let fed_from_falling_above = above_state_same.is_some_and(|s| s.falling);
        let below_is_falling_column = below_state_same.is_some_and(|s| s.falling);
        let best_horizontal_level =
            Self::min_incoming_horizontal_level(world, pos, kind, params.horizontal_level_step);
        let falling = Self::can_flow_down_from_cell(world, pos, kind);

        if falling {
            return Some(FluidState {
                kind,
                // Preserve an existing horizontal level on drop-mouth cells while marking them
                // falling. Pure vertical column cells already have level 0, so this keeps column
                // semantics while avoiding accidental pseudo-head promotion at the lip.
                level: current_state.level.min(params.max_distance),
                falling: true,
                source_origin: false,
                worldgen_static: false,
            });
        }

        if (was_falling || fed_from_falling_above) && below_is_falling_column {
            // Keep active waterfall columns marked as falling while they are stacked on top of
            // other falling cells. Otherwise intermediate cells can flip to non-falling and start
            // sideways spread, which creates "water walls" and too many landing heads.
            return Some(FluidState {
                kind,
                level: 0,
                falling: true,
                source_origin: false,
                worldgen_static: false,
            });
        }

        if was_falling && fed_from_falling_above && below_state_same.is_some() {
            // Keep the vertical column stable directly above the landing/pool interface.
            // Without this, the cell above the real landing can turn into an extra landing head
            // and start sideways spreading one level too high.
            return Some(FluidState {
                kind,
                level: 0,
                falling: true,
                source_origin: false,
                worldgen_static: false,
            });
        }

        if fed_from_falling_above {
            // Landing reset is the only non-source level-0 keep path here. The old generic
            // `above_same && level==0` rule pinned many non-source level-0 cells ("pseudo-heads")
            // in large stacks and caused runaway horizontal spread.
            return Some(FluidState {
                kind,
                level: 0,
                falling: false,
                source_origin: false,
                worldgen_static: false,
            });
        }

        if let Some(best_level) = best_horizontal_level
            && best_level <= params.max_distance
        {
            return Some(FluidState {
                kind,
                level: best_level,
                falling: false,
                source_origin: false,
                worldgen_static: false,
            });
        }

        None
    }

    fn next_horizontal_level(state: FluidState, params: FluidParams) -> Option<u8> {
        let level = if state.level == 0 || state.falling {
            params.horizontal_level_step
        } else {
            state.level.saturating_add(params.horizontal_level_step)
        };
        if level > params.max_distance {
            None
        } else {
            Some(level)
        }
    }

    fn try_flow_down_from(
        &mut self,
        world: &mut World,
        pos: IVec3,
        state: FluidState,
        params: FluidParams,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        let target = pos + IVec3::new(0, -1, 0);
        if !World::in_bounds(target) {
            return false;
        }

        let target_block = world.get_block(target);
        if fluid_kind(target_block) == Some(state.kind) {
            return false;
        }

        if fluid_kind(target_block) == Some(opposite(state.kind)) {
            return self.try_mix_into(world, target, state.kind, true, changed_positions);
        }

        if !Self::can_replace_block(target_block) {
            return false;
        }

        let changed = self.set_fluid_state(
            world,
            target,
            FluidState {
                kind: state.kind,
                level: 0,
                falling: true,
                source_origin: false,
                worldgen_static: false,
            },
            changed_positions,
        );
        if changed {
            self.schedule_fluid_update(target, state.kind, params.tick_rate);
            self.schedule_fluid_update(pos, state.kind, params.tick_rate);
        }
        changed
    }

    fn spread_horizontal_to(
        &mut self,
        world: &mut World,
        target: IVec3,
        kind: FluidKind,
        level: u8,
        params: FluidParams,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        let target_block = world.get_block(target);
        if fluid_kind(target_block) == Some(opposite(kind)) {
            return self.try_mix_into(world, target, kind, false, changed_positions);
        }

        let falling = Self::can_flow_down_from_cell(world, target, kind);

        let changed = self.set_fluid_state(
            world,
            target,
            FluidState {
                kind,
                level,
                falling,
                source_origin: false,
                worldgen_static: false,
            },
            changed_positions,
        );
        if changed {
            self.schedule_fluid_update(target, kind, params.tick_rate);
        }
        changed
    }

    fn apply_stream_plan(
        &mut self,
        world: &mut World,
        pos: IVec3,
        state: FluidState,
        dir: IVec3,
        steps: u8,
        params: FluidParams,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        if steps == 0 {
            return false;
        }

        self.clear_stream_locks_for_origin(pos, state.kind);
        let lock = StreamLock {
            kind: state.kind,
            dir,
            origin: pos,
        };
        self.stream_locks.insert(pos, lock);

        let mut changed_any = false;
        for step in 1..=steps {
            let target = pos + dir * i32::from(step);
            if !World::in_bounds(target) {
                break;
            }

            let level = (u16::from(step) * u16::from(params.horizontal_level_step))
                .min(u16::from(params.max_distance)) as u8;

            let can_keep_path = Self::can_pathfind_through(world, target, state.kind)
                || fluid_kind(world.get_block(target)) == Some(opposite(state.kind));
            if !can_keep_path {
                self.remove_stream_lock_if_kind(pos, state.kind);
                break;
            }

            if Self::can_horizontal_spread_into(world, target, state.kind, level)
                && self.spread_horizontal_to(
                    world,
                    target,
                    state.kind,
                    level,
                    params,
                    changed_positions,
                )
            {
                changed_any = true;
                self.schedule_ring_for_kind(world, target, state.kind, params.tick_rate);
            }

            if fluid_kind(world.get_block(target)) == Some(state.kind) {
                self.stream_locks.insert(target, lock);
            } else {
                self.remove_stream_lock_if_kind(target, state.kind);
                break;
            }
        }

        changed_any
    }

    fn try_flow_sideways_locked(
        &mut self,
        world: &mut World,
        pos: IVec3,
        state: FluidState,
        params: FluidParams,
        changed_positions: &mut HashSet<IVec3>,
    ) -> Option<bool> {
        let lock = self.stream_locks.get(&pos).copied()?;
        if lock.kind != state.kind {
            self.stream_locks.remove(&pos);
            return None;
        }

        if state.falling {
            return Some(false);
        }

        let target = pos + lock.dir;
        if !World::in_bounds(target) {
            self.remove_stream_lock_if_kind(pos, state.kind);
            return None;
        }

        let target_block = world.get_block(target);
        let target_kind = fluid_kind(target_block);
        if target_kind.is_none() && !Self::can_replace_block(target_block) {
            self.remove_stream_lock_if_kind(pos, state.kind);
            return None;
        }

        if target_kind == Some(state.kind) {
            self.stream_locks.insert(target, lock);
        }

        let Some(next_level) = Self::next_horizontal_level(state, params) else {
            return Some(false);
        };
        if !Self::can_horizontal_spread_into(world, target, state.kind, next_level) {
            return Some(false);
        }

        let changed = self.spread_horizontal_to(
            world,
            target,
            state.kind,
            next_level,
            params,
            changed_positions,
        );
        if changed {
            self.schedule_ring_for_kind(world, target, state.kind, params.tick_rate);
            if fluid_kind(world.get_block(target)) == Some(state.kind) {
                self.stream_locks.insert(target, lock);
            }
        }

        Some(changed)
    }

    fn try_flow_sideways_normal(
        &mut self,
        world: &mut World,
        pos: IVec3,
        state: FluidState,
        params: FluidParams,
        changed_positions: &mut HashSet<IVec3>,
    ) -> bool {
        if state.falling {
            return false;
        }

        let Some(next_level) = Self::next_horizontal_level(state, params) else {
            return false;
        };

        let mut changed_any = false;
        for dir in HORIZONTAL_DIRS {
            let target = pos + dir;
            if !Self::can_horizontal_spread_into(world, target, state.kind, next_level) {
                continue;
            }
            changed_any |= self.spread_horizontal_to(
                world,
                target,
                state.kind,
                next_level,
                params,
                changed_positions,
            );
        }
        changed_any
    }

    fn process_scheduled_pos(
        &mut self,
        world: &mut World,
        kind: FluidKind,
        pos: IVec3,
        changed_positions: &mut HashSet<IVec3>,
    ) {
        let params = self.params_for_kind(kind);
        let mut old_state = Self::get_fluid_state(world, pos);
        if old_state.is_some_and(|state| state.kind != kind) {
            self.remove_stream_lock_if_kind(pos, kind);
            return;
        }

        let mut local_changed = false;
        if old_state.is_some_and(|state| state.worldgen_static) {
            let thawed = FluidState {
                kind,
                level: 0,
                falling: false,
                source_origin: false,
                worldgen_static: false,
            };
            if self.set_fluid_state(world, pos, thawed, changed_positions) {
                local_changed = true;
            }
            old_state = Some(thawed);
        }

        let next_state = self.compute_next_state_for_pos(world, pos, kind, params);
        let current_state = match next_state {
            Some(state) => {
                if self.set_fluid_state(world, pos, state, changed_positions) {
                    local_changed = true;
                }
                Some(state)
            }
            None => {
                if old_state.is_some_and(|state| state.kind == kind)
                    && self.clear_fluid(world, pos, kind, changed_positions)
                {
                    local_changed = true;
                }
                self.remove_stream_lock_if_kind(pos, kind);
                None
            }
        };

        let Some(state) = current_state else {
            if local_changed {
                self.schedule_ring_for_kind(world, pos, kind, params.tick_rate);
            }
            return;
        };

        if state.worldgen_static {
            self.remove_stream_lock_if_kind(pos, kind);
            if local_changed {
                self.schedule_ring_for_kind(world, pos, kind, params.tick_rate);
            }
            return;
        }

        if self.try_flow_down_from(world, pos, state, params, changed_positions) {
            local_changed = true;
        } else if let Some(changed) =
            self.try_flow_sideways_locked(world, pos, state, params, changed_positions)
        {
            local_changed |= changed;
        } else if Self::is_head(world, pos, state) {
            if let Some((dir, steps)) = Self::compute_stream_plan(world, pos, state.kind, params) {
                local_changed |= self.apply_stream_plan(
                    world,
                    pos,
                    state,
                    dir,
                    steps,
                    params,
                    changed_positions,
                );
            } else {
                self.clear_stream_locks_for_origin(pos, state.kind);
                local_changed |=
                    self.try_flow_sideways_normal(world, pos, state, params, changed_positions);
            }
        } else {
            local_changed |=
                self.try_flow_sideways_normal(world, pos, state, params, changed_positions);
        }

        if local_changed {
            self.schedule_ring_for_kind(world, pos, kind, params.tick_rate);
        }
    }

    pub(crate) fn rebuild_from_world(&mut self, world: &mut World) {
        self.water_scheduler.clear();
        self.lava_scheduler.clear();
        self.transforming.clear();
        self.transforming_set.clear();
        self.stream_locks.clear();
        self.global_tick = 0;

        let mut fluid_positions = Vec::with_capacity(1024);
        for (coord, chunk) in &world.chunks {
            let chunk_x0 = coord.x * CHUNK_SIZE;
            let chunk_y0 = coord.y * CHUNK_SIZE;
            let chunk_z0 = coord.z * CHUNK_SIZE;
            for local_y in 0..CHUNK_SIZE {
                for local_z in 0..CHUNK_SIZE {
                    for local_x in 0..CHUNK_SIZE {
                        let block = chunk.get(local_x, local_y, local_z);
                        if fluid_kind(block).is_none() {
                            continue;
                        }
                        fluid_positions.push(IVec3::new(
                            chunk_x0 + local_x,
                            chunk_y0 + local_y,
                            chunk_z0 + local_z,
                        ));
                    }
                }
            }
        }

        for pos in fluid_positions {
            self.normalize_fluid_state_and_schedule(world, pos, 1);
        }

        for _ in 0..FLUID_REBUILD_SETTLE_TICKS {
            if !self.has_pending() {
                break;
            }
            let _ = self.step(world);
        }
    }

    pub(crate) fn bootstrap_column_from_world(
        &mut self,
        world: &mut World,
        col_x: i32,
        col_z: i32,
    ) {
        let mut fluid_positions = Vec::with_capacity(256);
        let mut has_loaded_chunks_in_column = false;

        for (coord, chunk) in &world.chunks {
            if coord.x != col_x || coord.z != col_z {
                continue;
            }
            has_loaded_chunks_in_column = true;
            let chunk_x0 = coord.x * CHUNK_SIZE;
            let chunk_y0 = coord.y * CHUNK_SIZE;
            let chunk_z0 = coord.z * CHUNK_SIZE;
            for local_y in 0..CHUNK_SIZE {
                for local_z in 0..CHUNK_SIZE {
                    for local_x in 0..CHUNK_SIZE {
                        if fluid_kind(chunk.get(local_x, local_y, local_z)).is_none() {
                            continue;
                        }
                        fluid_positions.push(IVec3::new(
                            chunk_x0 + local_x,
                            chunk_y0 + local_y,
                            chunk_z0 + local_z,
                        ));
                    }
                }
            }
        }

        if !has_loaded_chunks_in_column {
            return;
        }

        for pos in fluid_positions {
            self.normalize_fluid_state_and_schedule(world, pos, 1);
        }
    }

    fn normalize_fluid_state_and_schedule(
        &mut self,
        world: &mut World,
        pos: IVec3,
        delay_ticks: u64,
    ) {
        let Some(mut state) = Self::get_fluid_state(world, pos) else {
            return;
        };

        let params = self.params_for_kind(state.kind);
        state.level = state.level.min(params.max_distance);
        if state.source_origin {
            state.level = 0;
            state.falling = false;
        }
        if state.worldgen_static {
            state.level = 0;
            state.falling = false;
            state.source_origin = false;
        }

        if !self.set_block_loaded_only(
            world,
            pos,
            if state.source_origin || state.worldgen_static {
                make_still(state.kind)
            } else {
                make_flowing(state.kind)
            },
            fluid_data_make_worldgen_static(
                state.level,
                state.falling,
                state.source_origin,
                state.worldgen_static,
            ),
            state.kind,
        ) {
            return;
        }

        if state.worldgen_static {
            return;
        }

        self.schedule_fluid_update(pos, state.kind, delay_ticks);
    }

    pub(crate) fn step(&mut self, world: &mut World) -> Vec<IVec3> {
        self.global_tick = self.global_tick.wrapping_add(1);
        let mut changed_positions: HashSet<IVec3> = HashSet::new();

        let tick = self.global_tick;
        for kind in [FluidKind::Water, FluidKind::Lava] {
            let budget = self.params_for_kind(kind).max_updates_per_tick;
            self.scheduler_mut(kind).collect_ready(tick);

            for _ in 0..budget {
                let Some(pos) = self.scheduler_mut(kind).pop_ready() else {
                    break;
                };
                self.process_scheduled_pos(world, kind, pos, &mut changed_positions);
            }
        }

        changed_positions.into_iter().collect()
    }
}

fn fluid_surface_height_for_state(state: FluidState) -> f32 {
    if state.falling || state.level == 0 {
        1.0
    } else {
        ((8.0 - state.level as f32) / 8.0).clamp(1.0 / 8.0, 1.0)
    }
}

pub(crate) fn flow_vector_at_cell(world: &World, cell: IVec3, kind: FluidKind) -> Vec3 {
    let Some(state) = FluidSystem::get_fluid_state(world, cell) else {
        return Vec3::ZERO;
    };
    if state.kind != kind {
        return Vec3::ZERO;
    }

    let current_h = fluid_surface_height_for_state(state);
    let mut flow = Vec3::ZERO;

    for dir in HORIZONTAL_DIRS {
        let neighbor = cell + dir;
        let neighbor_h = FluidSystem::get_fluid_state(world, neighbor)
            .filter(|state| state.kind == kind)
            .map(fluid_surface_height_for_state)
            .unwrap_or(0.0);

        let diff = current_h - neighbor_h;
        flow += Vec3::new(dir.x as f32, 0.0, dir.z as f32) * diff;
    }

    if flow.length_squared() <= 1e-6 {
        Vec3::ZERO
    } else {
        flow.normalize()
    }
}

pub(crate) fn flow_velocity_at_point(world: &World, kind: FluidKind, p_world: Vec3) -> Vec3 {
    let cell = IVec3::new(
        p_world.x.floor() as i32,
        p_world.y.floor() as i32,
        p_world.z.floor() as i32,
    );
    let dir = flow_vector_at_cell(world, cell, kind);
    Vec3::new(dir.x, 0.0, dir.z) * FLUID_CURRENT_PUSH_SPEED
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_floor(world: &mut World, x0: i32, x1: i32, z0: i32, z1: i32, y: i32, block: BlockId) {
        for z in z0..=z1 {
            for x in x0..=x1 {
                let _ = world.set_block_and_data(IVec3::new(x, y, z), block, 0);
            }
        }
    }

    fn set_source(world: &mut World, pos: IVec3, kind: FluidKind) {
        let _ = world.set_block_and_data(pos, make_still(kind), fluid_data_make(0, false, true));
    }

    fn set_worldgen_static(world: &mut World, pos: IVec3, kind: FluidKind) {
        let _ = world.set_block_and_data(pos, make_still(kind), fluid_data_make_static(0, false));
    }

    fn fluid_state(world: &World, pos: IVec3) -> Option<FluidState> {
        FluidSystem::get_fluid_state(world, pos)
    }

    fn count_source_origin_cells(world: &World, kind: FluidKind, min: IVec3, max: IVec3) -> usize {
        let mut count = 0usize;
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    let pos = IVec3::new(x, y, z);
                    let Some(state) = fluid_state(world, pos) else {
                        continue;
                    };
                    if state.kind == kind && state.source_origin {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    fn run_ticks(system: &mut FluidSystem, world: &mut World, ticks: usize) {
        for _ in 0..ticks {
            let _ = system.step(world);
        }
    }

    #[test]
    fn settled_fluid_body_stops_scheduling_updates() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);

        let source = IVec3::new(20, 1, 20);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);

        for _ in 0..600 {
            let _ = fluids.step(&mut world);
            if !fluids.has_pending() {
                break;
            }
        }

        assert!(!fluids.has_pending(), "fluid scheduler did not settle");
        assert!(
            fluids.step(&mut world).is_empty(),
            "settled fluid unexpectedly changed without new block updates"
        );
    }

    #[test]
    fn water_spread_is_7_blocks_every_5_ticks() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);

        let source = IVec3::new(20, 1, 20);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);

        run_ticks(&mut fluids, &mut world, 4);
        assert_eq!(fluid_state(&world, source + IVec3::new(1, 0, 0)), None);

        run_ticks(&mut fluids, &mut world, 1);
        assert_eq!(
            fluid_state(&world, source + IVec3::new(1, 0, 0)).map(|s| s.level),
            Some(1)
        );

        run_ticks(&mut fluids, &mut world, 80);
        for d in 1..=7 {
            let pos = source + IVec3::new(d, 0, 0);
            assert_eq!(fluid_state(&world, pos).map(|s| s.level), Some(d as u8));
        }
        assert_eq!(fluid_state(&world, source + IVec3::new(8, 0, 0)), None);
    }

    #[test]
    fn worldgen_static_fluid_stays_dormant_without_explicit_update() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);

        let cell = IVec3::new(20, 1, 20);
        set_worldgen_static(&mut world, cell, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        run_ticks(&mut fluids, &mut world, 120);

        let center = fluid_state(&world, cell).expect("static water missing");
        assert_eq!(center.kind, FluidKind::Water);
        assert!(
            center.worldgen_static,
            "static flag was cleared unexpectedly"
        );
        assert!(
            !center.source_origin,
            "worldgen static water should not be source-origin"
        );

        for dir in HORIZONTAL_DIRS {
            assert_eq!(
                fluid_state(&world, cell + dir),
                None,
                "static water spread into {:?}",
                cell + dir
            );
        }
        assert_eq!(fluid_state(&world, cell + IVec3::new(0, -1, 0)), None);
    }

    #[test]
    fn worldgen_static_fluid_thaws_when_scheduled() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);

        let cell = IVec3::new(20, 1, 20);
        set_worldgen_static(&mut world, cell, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, cell);
        run_ticks(&mut fluids, &mut world, 40);

        let center = fluid_state(&world, cell);
        assert!(
            !center.is_some_and(|s| s.worldgen_static),
            "scheduled worldgen-static fluid should thaw into simulated fluid"
        );
    }

    #[test]
    fn fluid_updates_do_not_allocate_unloaded_neighbor_chunks() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 0, 15, 0, 0, 0, BlockId::Stone);

        let source = IVec3::new(15, 1, 0);
        set_source(&mut world, source, FluidKind::Water);

        let neighbor_pos = IVec3::new(16, 1, 0);
        let neighbor_coord = World::world_to_chunk_coord(neighbor_pos);
        assert!(
            !world.chunks.contains_key(&neighbor_coord),
            "neighbor chunk unexpectedly preloaded before test"
        );

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        let blocked_before = fluids.debug_counts().unloaded_writes.0;

        run_ticks(&mut fluids, &mut world, 20);

        let blocked_after = fluids.debug_counts().unloaded_writes.0;
        assert_eq!(
            blocked_after, blocked_before,
            "fluid should treat unloaded neighbor chunk as blocked and avoid write attempts"
        );
        assert!(
            !world.chunks.contains_key(&neighbor_coord),
            "fluid update allocated unloaded neighbor chunk"
        );
    }

    #[test]
    fn lava_spread_is_3_blocks_every_30_ticks_overworld() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);

        let source = IVec3::new(20, 1, 20);
        set_source(&mut world, source, FluidKind::Lava);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);

        run_ticks(&mut fluids, &mut world, 29);
        assert_eq!(fluid_state(&world, source + IVec3::new(1, 0, 0)), None);

        run_ticks(&mut fluids, &mut world, 1);
        assert_eq!(
            fluid_state(&world, source + IVec3::new(1, 0, 0)).map(|s| s.level),
            Some(2)
        );

        run_ticks(&mut fluids, &mut world, 120);
        assert_eq!(
            fluid_state(&world, source + IVec3::new(1, 0, 0)).map(|s| s.level),
            Some(2)
        );
        assert_eq!(
            fluid_state(&world, source + IVec3::new(2, 0, 0)).map(|s| s.level),
            Some(4)
        );
        assert_eq!(
            fluid_state(&world, source + IVec3::new(3, 0, 0)).map(|s| s.level),
            Some(6)
        );
        assert_eq!(fluid_state(&world, source + IVec3::new(4, 0, 0)), None);
    }

    #[test]
    fn waterfall_builds_column_and_spreads_at_foot() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);

        let source = IVec3::new(20, 5, 20);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 220);

        for y in 2..=4 {
            let pos = IVec3::new(20, y, 20);
            let state = fluid_state(&world, pos).expect("waterfall segment missing");
            assert_eq!(
                state.level, 0,
                "unexpected waterfall level at {:?}: {:?}",
                pos, state
            );
            assert_eq!(state.kind, FluidKind::Water);
        }

        let foot = IVec3::new(20, 1, 20);
        let foot_state = fluid_state(&world, foot).expect("waterfall foot missing");
        assert_eq!(foot_state.level, 0);
        assert!(!foot_state.falling);

        assert_eq!(
            fluid_state(&world, foot + IVec3::new(1, 0, 0)).map(|s| s.kind),
            Some(FluidKind::Water)
        );
    }

    #[test]
    fn multiple_reachable_dropoffs_pick_nearest_straight_path() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 20, 40, 20, 40, 0, BlockId::Stone);

        // Nahe Dropoff (Osten, Distanz 2 vom Source):
        let _ = world.set_block_and_data(IVec3::new(32, 0, 30), BlockId::Air, 0);
        // Weiter Dropoff (Westen, Distanz 4 vom Source):
        let _ = world.set_block_and_data(IVec3::new(26, 0, 30), BlockId::Air, 0);

        let source = IVec3::new(30, 1, 30);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 6);

        for x in 31..=32 {
            let pos = IVec3::new(x, 1, 30);
            assert!(
                fluid_state(&world, pos).is_some(),
                "expected stream cell on nearest path at {:?}",
                pos
            );
            assert_eq!(
                fluid_state(&world, IVec3::new(x, 1, 29)),
                None,
                "unexpected branch next to stream at x={x}"
            );
            assert_eq!(
                fluid_state(&world, IVec3::new(x, 1, 31)),
                None,
                "unexpected branch next to stream at x={x}"
            );
        }
        assert_eq!(
            fluid_state(&world, source + IVec3::new(1, 0, 0)).map(|s| s.kind),
            Some(FluidKind::Water)
        );
        assert!(
            fluid_state(&world, source + IVec3::new(2, 0, 0)).is_some_and(|s| s.falling),
            "edge cell should already be falling at the drop"
        );
        assert_eq!(fluid_state(&world, source + IVec3::new(-1, 0, 0)), None);
        assert_eq!(fluid_state(&world, source + IVec3::new(-2, 0, 0)), None);
        assert_eq!(fluid_state(&world, source + IVec3::new(0, 0, 1)), None);
        assert_eq!(fluid_state(&world, source + IVec3::new(0, 0, -1)), None);
    }

    #[test]
    fn no_hovering_regression_marks_horizontal_cell_over_air_as_falling_immediately() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 30, 10, 30, 0, BlockId::Stone);

        let source = IVec3::new(20, 1, 20);
        let edge_cell = source + IVec3::new(1, 0, 0);
        let _ = world.set_block_and_data(edge_cell + IVec3::new(0, -1, 0), BlockId::Air, 0);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);

        run_ticks(&mut fluids, &mut world, 5);

        let edge_state = fluid_state(&world, edge_cell).expect("expected edge stream cell");
        assert!(
            edge_state.falling,
            "horizontal cell over air must be marked falling immediately"
        );

        run_ticks(&mut fluids, &mut world, 5);
        assert_eq!(fluid_state(&world, edge_cell + IVec3::new(0, 0, 1)), None);
        assert_eq!(fluid_state(&world, edge_cell + IVec3::new(0, 0, -1)), None);
    }

    #[test]
    fn no_auto_sources_are_created_for_lava_flow() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 30, 50, 30, 50, 0, BlockId::Stone);

        let source = IVec3::new(40, 1, 40);
        set_source(&mut world, source, FluidKind::Lava);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 220);

        let source_count = count_source_origin_cells(
            &world,
            FluidKind::Lava,
            IVec3::new(30, 1, 30),
            IVec3::new(50, 3, 50),
        );
        assert_eq!(source_count, 1);
        assert!(fluid_state(&world, source).is_some_and(|s| s.source_origin));
    }

    #[test]
    fn no_lava_source_conversion_from_two_sources() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 30, 10, 30, 0, BlockId::Stone);

        let left = IVec3::new(18, 1, 20);
        let right = IVec3::new(20, 1, 20);
        let middle = IVec3::new(19, 1, 20);
        set_source(&mut world, left, FluidKind::Lava);
        set_source(&mut world, right, FluidKind::Lava);
        let _ = world.set_block_and_data(
            middle,
            BlockId::LavaFlowing,
            fluid_data_make(2, false, false),
        );

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, left);
        fluids.enqueue_with_neighbors(&world, right);
        fluids.enqueue_with_neighbors(&world, middle);
        run_ticks(&mut fluids, &mut world, 240);

        let mid_state = fluid_state(&world, middle).expect("middle lava missing");
        assert!(
            !mid_state.source_origin,
            "lava middle cell must not convert to a source"
        );
        assert_eq!(mid_state.kind, FluidKind::Lava);
    }

    #[test]
    fn infinite_source_condition_converts_middle_cell() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 30, 10, 30, 0, BlockId::Stone);

        let left = IVec3::new(18, 1, 20);
        let right = IVec3::new(20, 1, 20);
        let middle = IVec3::new(19, 1, 20);
        set_source(&mut world, left, FluidKind::Water);
        set_source(&mut world, right, FluidKind::Water);
        let _ = world.set_block_and_data(
            middle,
            BlockId::WaterFlowing,
            fluid_data_make(1, false, false),
        );

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, left);
        fluids.enqueue_with_neighbors(&world, right);
        fluids.enqueue_with_neighbors(&world, middle);
        run_ticks(&mut fluids, &mut world, 40);

        let mid_state = fluid_state(&world, middle).expect("middle fluid missing");
        assert!(mid_state.source_origin, "middle did not convert to source");
        assert_eq!(mid_state.level, 0);
        assert!(!mid_state.falling);
    }

    #[test]
    fn removing_source_clears_remaining_flow_without_residual_sources() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 20, 50, 20, 50, 0, BlockId::Stone);

        let source = IVec3::new(35, 1, 35);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 120);

        let _ = world.set_block_and_data(source, BlockId::Air, 0);
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 320);

        for z in 24..=46 {
            for x in 24..=46 {
                let pos = IVec3::new(x, 1, z);
                assert!(
                    fluid_state(&world, pos).is_none(),
                    "unexpected fluid left at {:?}",
                    pos
                );
            }
        }
        let source_count = count_source_origin_cells(
            &world,
            FluidKind::Water,
            IVec3::new(20, 1, 20),
            IVec3::new(50, 3, 50),
        );
        assert_eq!(source_count, 0);
    }

    #[test]
    fn edge_seeking_forms_single_stream_then_waterfall() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);
        set_floor(&mut world, 10, 40, 10, 40, 5, BlockId::Stone);

        let edge = IVec3::new(24, 6, 20);
        let _ = world.set_block_and_data(edge + IVec3::new(0, -1, 0), BlockId::Air, 0);

        let source = IVec3::new(20, 6, 20);
        set_source(&mut world, source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 220);

        for x in 21..=24 {
            assert!(
                fluid_state(&world, IVec3::new(x, 6, 20)).is_some(),
                "stream missing at x={x}"
            );
            assert_eq!(fluid_state(&world, IVec3::new(x, 6, 19)), None);
            assert_eq!(fluid_state(&world, IVec3::new(x, 6, 21)), None);
        }

        for y in (2..=5).rev() {
            let pos = IVec3::new(24, y, 20);
            let state = fluid_state(&world, pos).expect("waterfall cell missing");
            if y > 1 {
                assert!(state.falling, "expected falling state at {:?}", pos);
            }
        }

        assert!(
            fluid_state(&world, IVec3::new(25, 1, 20)).is_some()
                || fluid_state(&world, IVec3::new(23, 1, 20)).is_some()
                || fluid_state(&world, IVec3::new(24, 1, 21)).is_some()
                || fluid_state(&world, IVec3::new(24, 1, 19)).is_some()
        );
    }

    #[test]
    fn lava_edge_seeking_uses_overworld_step_and_keeps_single_source() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 10, 40, 10, 40, 0, BlockId::Stone);
        set_floor(&mut world, 10, 40, 10, 40, 5, BlockId::Stone);

        let source = IVec3::new(20, 6, 20);
        let edge = IVec3::new(23, 6, 20);
        let _ = world.set_block_and_data(edge + IVec3::new(0, -1, 0), BlockId::Air, 0);
        set_source(&mut world, source, FluidKind::Lava);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, source);
        run_ticks(&mut fluids, &mut world, 720);

        assert_eq!(
            fluid_state(&world, IVec3::new(21, 6, 20)).map(|s| s.level),
            Some(2)
        );
        assert_eq!(
            fluid_state(&world, IVec3::new(22, 6, 20)).map(|s| s.level),
            Some(4)
        );
        assert_eq!(
            fluid_state(&world, IVec3::new(23, 6, 20)).map(|s| s.level),
            Some(6)
        );
        assert_eq!(fluid_state(&world, IVec3::new(21, 6, 19)), None);
        assert_eq!(fluid_state(&world, IVec3::new(21, 6, 21)), None);

        let source_count = count_source_origin_cells(
            &world,
            FluidKind::Lava,
            IVec3::new(14, 1, 14),
            IVec3::new(30, 7, 26),
        );
        assert_eq!(source_count, 1);
    }

    #[test]
    fn water_lava_mixing_produces_obsidian_cobble_and_stone() {
        let mut world = World::new();
        world.chunks.clear();
        set_floor(&mut world, 40, 90, 40, 90, 0, BlockId::Stone);

        // Water -> Lava source => Obsidian
        let lava_source = IVec3::new(50, 1, 50);
        let water_source = IVec3::new(49, 1, 50);
        set_source(&mut world, lava_source, FluidKind::Lava);
        set_source(&mut world, water_source, FluidKind::Water);

        let mut fluids = FluidSystem::new();
        fluids.enqueue_with_neighbors(&world, lava_source);
        fluids.enqueue_with_neighbors(&world, water_source);
        run_ticks(&mut fluids, &mut world, 40);
        assert_eq!(world.get_block(lava_source), BlockId::Obsidian);

        // Water -> flowing lava => Cobblestone
        let flowing_lava = IVec3::new(60, 1, 60);
        let water_near = IVec3::new(59, 1, 60);
        let _ = world.set_block_and_data(
            flowing_lava,
            BlockId::LavaFlowing,
            fluid_data_make(4, false, false),
        );
        set_source(&mut world, water_near, FluidKind::Water);
        fluids.enqueue_with_neighbors(&world, flowing_lava);
        fluids.enqueue_with_neighbors(&world, water_near);
        run_ticks(&mut fluids, &mut world, 40);
        assert_eq!(world.get_block(flowing_lava), BlockId::Cobblestone);

        // Lava vertical into water => Stone
        let water_column = IVec3::new(70, 1, 70);
        let lava_above = IVec3::new(70, 3, 70);
        set_source(&mut world, water_column, FluidKind::Water);
        set_source(&mut world, lava_above, FluidKind::Lava);
        fluids.enqueue_with_neighbors(&world, water_column);
        fluids.enqueue_with_neighbors(&world, lava_above);
        run_ticks(&mut fluids, &mut world, 220);
        assert_eq!(world.get_block(water_column), BlockId::Stone);
    }
}
