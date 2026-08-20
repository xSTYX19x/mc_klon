use std::sync::{Arc, OnceLock};

use crate::blocks::BlockId;

use super::CHUNK_SIZE;

#[derive(Debug)]
pub(crate) struct Chunk {
    pub(crate) blocks: Arc<Vec<BlockId>>,
    pub(crate) block_data: Arc<Vec<u8>>,
    pub(crate) light: Arc<Vec<u8>>, // packed nibbles: high=sky, low=block
    pub(crate) revision: u64,
    filled_voxel_count: u16,
    solid_voxel_count: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LightLevel {
    pub(crate) sky: u8,
    pub(crate) block: u8,
}

impl LightLevel {
    pub(crate) const fn new(sky: u8, block: u8) -> Self {
        Self {
            sky: sky & 0x0F,
            block: block & 0x0F,
        }
    }

    pub(crate) const fn pack(self) -> u8 {
        ((self.sky & 0x0F) << 4) | (self.block & 0x0F)
    }

    pub(crate) const fn from_packed(packed: u8) -> Self {
        Self {
            sky: (packed >> 4) & 0x0F,
            block: packed & 0x0F,
        }
    }
}

impl Chunk {
    const BLOCK_COUNT: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;

    pub(crate) fn new_empty() -> Self {
        Self {
            blocks: Arc::new(vec![BlockId::Air; Self::BLOCK_COUNT]),
            block_data: Arc::new(vec![0; Self::BLOCK_COUNT]),
            light: Arc::new(vec![0; Self::BLOCK_COUNT]),
            revision: 0,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        }
    }

    pub(crate) fn from_parts(
        blocks: Arc<Vec<BlockId>>,
        block_data: Arc<Vec<u8>>,
        light: Arc<Vec<u8>>,
        revision: u64,
    ) -> Self {
        debug_assert_eq!(blocks.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(block_data.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(light.len(), Self::BLOCK_COUNT);
        let mut chunk = Self {
            blocks,
            block_data,
            light,
            revision,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        };
        chunk.recompute_cached_block_classification();
        chunk
    }

    pub(crate) fn from_parts_readonly(
        blocks: Arc<Vec<BlockId>>,
        block_data: Arc<Vec<u8>>,
        light: Arc<Vec<u8>>,
        revision: u64,
    ) -> Self {
        debug_assert_eq!(blocks.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(block_data.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(light.len(), Self::BLOCK_COUNT);
        Self {
            blocks,
            block_data,
            light,
            revision,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        }
    }

    pub(crate) fn from_generated_parts(
        blocks: Arc<Vec<BlockId>>,
        block_data: Arc<Vec<u8>>,
        filled_voxel_count: u16,
        solid_voxel_count: u16,
    ) -> Self {
        debug_assert_eq!(blocks.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(block_data.len(), Self::BLOCK_COUNT);
        Self {
            blocks,
            block_data,
            light: generated_zero_light().clone(),
            revision: 0,
            filled_voxel_count,
            solid_voxel_count,
        }
    }

    pub(crate) fn index(x: i32, y: i32, z: i32) -> usize {
        (x + z * CHUNK_SIZE + y * CHUNK_SIZE * CHUNK_SIZE) as usize
    }

    pub(crate) fn in_bounds(x: i32, y: i32, z: i32) -> bool {
        (0..CHUNK_SIZE).contains(&x) && (0..CHUNK_SIZE).contains(&y) && (0..CHUNK_SIZE).contains(&z)
    }

    pub(crate) fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        if Self::in_bounds(x, y, z) {
            self.blocks[Self::index(x, y, z)]
        } else {
            BlockId::Air
        }
    }

    pub(crate) fn set(&mut self, x: i32, y: i32, z: i32, id: BlockId) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let blocks = Arc::make_mut(&mut self.blocks);
            let old = blocks[i];
            if old != id {
                blocks[i] = id;
                let old_filled = old.is_voxel_filled() as i32;
                let new_filled = id.is_voxel_filled() as i32;
                let old_solid = old.is_solid() as i32;
                let new_solid = id.is_solid() as i32;
                self.filled_voxel_count = (self.filled_voxel_count as i32 + new_filled - old_filled)
                    .clamp(0, Self::BLOCK_COUNT as i32)
                    as u16;
                self.solid_voxel_count = (self.solid_voxel_count as i32 + new_solid - old_solid)
                    .clamp(0, Self::BLOCK_COUNT as i32)
                    as u16;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn get_data(&self, x: i32, y: i32, z: i32) -> u8 {
        if Self::in_bounds(x, y, z) {
            self.block_data[Self::index(x, y, z)]
        } else {
            0
        }
    }

    pub(crate) fn set_data(&mut self, x: i32, y: i32, z: i32, data: u8) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let block_data = Arc::make_mut(&mut self.block_data);
            if block_data[i] != data {
                block_data[i] = data;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn get_light_packed(&self, x: i32, y: i32, z: i32) -> u8 {
        if Self::in_bounds(x, y, z) {
            self.light[Self::index(x, y, z)]
        } else {
            0
        }
    }

    pub(crate) fn get_light(&self, x: i32, y: i32, z: i32) -> LightLevel {
        LightLevel::from_packed(self.get_light_packed(x, y, z))
    }

    pub(crate) fn set_light_packed(&mut self, x: i32, y: i32, z: i32, packed: u8) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let packed = packed & 0xFF;
            let light = Arc::make_mut(&mut self.light);
            if light[i] != packed {
                light[i] = packed;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn set_light(&mut self, x: i32, y: i32, z: i32, light: LightLevel) {
        self.set_light_packed(x, y, z, light.pack());
    }

    pub(crate) fn clear_light(&mut self) {
        let mut changed = false;
        for v in Arc::make_mut(&mut self.light).iter_mut() {
            if *v != 0 {
                *v = 0;
                changed = true;
            }
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.filled_voxel_count == 0
    }

    pub(crate) fn is_full_solid(&self) -> bool {
        self.solid_voxel_count as usize == Self::BLOCK_COUNT
    }

    pub(crate) fn recompute_cached_block_classification(&mut self) {
        let mut filled = 0u16;
        let mut solid = 0u16;
        for &block in self.blocks.iter() {
            if block.is_voxel_filled() {
                filled = filled.saturating_add(1);
            }
            if block.is_solid() {
                solid = solid.saturating_add(1);
            }
        }
        self.filled_voxel_count = filled;
        self.solid_voxel_count = solid;
    }
}

fn generated_zero_light() -> &'static Arc<Vec<u8>> {
    static ZERO_LIGHT: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    ZERO_LIGHT.get_or_init(|| Arc::new(vec![0; Chunk::BLOCK_COUNT]))
}

fn shared_zero_block_data() -> &'static Arc<Vec<u8>> {
    static ZERO_BLOCK_DATA: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    ZERO_BLOCK_DATA.get_or_init(|| Arc::new(vec![0; Chunk::BLOCK_COUNT]))
}

/// Wraps a freshly built `block_data` buffer, collapsing the all-zero case onto
/// one shared allocation.
///
/// Generated terrain leaves this array entirely zero (measured: 100 % of
/// sections, see the `memory_report` test), so keeping a private 4 KiB copy per
/// section is the single largest avoidable chunk of resident block storage.
/// Every writer goes through `Arc::make_mut`, so a section that later does get
/// block data simply copies the shared buffer once — the same allocation that
/// would otherwise have happened up front, just deferred to the sections that
/// actually need it.
pub(crate) fn block_data_arc(block_data: Vec<u8>) -> Arc<Vec<u8>> {
    debug_assert_eq!(block_data.len(), Chunk::BLOCK_COUNT);
    if block_data.len() == Chunk::BLOCK_COUNT && block_data.iter().all(|&value| value == 0) {
        shared_zero_block_data().clone()
    } else {
        Arc::new(block_data)
    }
}

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub(crate) struct ChunkCoord {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) z: i32,
}

#[cfg(test)]
mod block_data_sharing_tests {
    use super::{Chunk, block_data_arc};
    use crate::blocks::BlockId;
    use std::sync::Arc;

    #[test]
    fn all_zero_block_data_collapses_onto_one_allocation() {
        let a = block_data_arc(vec![0; Chunk::BLOCK_COUNT]);
        let b = block_data_arc(vec![0; Chunk::BLOCK_COUNT]);
        assert!(
            Arc::ptr_eq(&a, &b),
            "zero block_data must reuse the shared buffer"
        );

        let mut non_zero = vec![0u8; Chunk::BLOCK_COUNT];
        non_zero[7] = 1;
        let c = block_data_arc(non_zero);
        assert!(
            !Arc::ptr_eq(&a, &c),
            "a section with real block data must own its buffer"
        );
        assert_eq!(c[7], 1);
    }

    #[test]
    fn writing_block_data_copies_instead_of_corrupting_shared_sections() {
        let mut first = Chunk::from_parts(
            Arc::new(vec![BlockId::Stone; Chunk::BLOCK_COUNT]),
            block_data_arc(vec![0; Chunk::BLOCK_COUNT]),
            Arc::new(vec![0; Chunk::BLOCK_COUNT]),
            0,
        );
        let second = Chunk::from_parts(
            Arc::new(vec![BlockId::Stone; Chunk::BLOCK_COUNT]),
            block_data_arc(vec![0; Chunk::BLOCK_COUNT]),
            Arc::new(vec![0; Chunk::BLOCK_COUNT]),
            0,
        );
        assert!(Arc::ptr_eq(&first.block_data, &second.block_data));

        first.set_data(1, 2, 3, 9);

        assert_eq!(first.get_data(1, 2, 3), 9);
        assert_eq!(
            second.get_data(1, 2, 3),
            0,
            "make_mut must have copied; the shared zero buffer stays untouched"
        );
        assert!(!Arc::ptr_eq(&first.block_data, &second.block_data));
        // The shared buffer itself must still be all zero for every future section.
        let fresh = block_data_arc(vec![0; Chunk::BLOCK_COUNT]);
        assert!(Arc::ptr_eq(&second.block_data, &fresh));
        assert!(fresh.iter().all(|&value| value == 0));
    }
}

/// Sizes the in-memory chunk storage before the `BlockStateId` migration in
/// `docs/TODO.md` touches every hot loop.
///
/// The migration has two halves that are easy to conflate: merging
/// `blocks` + `block_data` into one state id, and palette/bitpacking at
/// runtime. They pay off very differently, and only one of them is worth the
/// churn — the numbers below say which. Run with
/// `cargo test world::chunk::memory_report -- --nocapture`.
#[cfg(test)]
mod memory_report {
    use super::Chunk;
    use crate::world::{CHUNK_SIZE, World, WorldConfig};
    use crate::worldgen::WorldgenPreset;
    use std::collections::HashSet;

    const VOXELS: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;
    /// `blocks` (1 B/voxel) + `block_data` (1 B) + `light` (1 B).
    const CURRENT_BYTES_PER_SECTION: usize = VOXELS * 3;

    #[derive(Clone, Copy, Debug, Default)]
    struct Stats {
        sections: usize,
        columns: usize,
        block_ids: usize,
        states: usize,
        uniform_sections: usize,
        zero_data_sections: usize,
        packed_blocks: usize,
        packed_states: usize,
    }

    fn bits_for(distinct: usize) -> usize {
        let mut bits = 1;
        while (1usize << bits) < distinct.max(2) {
            bits += 1;
        }
        bits
    }

    /// Palette plus bitpacked indices, the representation the TODO's second
    /// half proposes. Light stays a plain byte array either way.
    fn packed_bytes(distinct: usize, entry_bytes: usize) -> usize {
        distinct * entry_bytes + VOXELS * bits_for(distinct) / 8
    }

    fn measure(chunk: &Chunk, stats: &mut Stats) {
        let mut block_ids: HashSet<u8> = HashSet::new();
        let mut states: HashSet<u16> = HashSet::new();
        let mut any_data = false;
        for (i, &block) in chunk.blocks.iter().enumerate() {
            let data = chunk.block_data[i];
            any_data |= data != 0;
            block_ids.insert(block.to_u8());
            states.insert(u16::from(block.to_u8()) << 8 | u16::from(data));
        }

        stats.sections += 1;
        stats.block_ids += block_ids.len();
        stats.states += states.len();
        stats.uniform_sections += usize::from(states.len() == 1);
        stats.zero_data_sections += usize::from(!any_data);
        stats.packed_blocks += packed_bytes(block_ids.len(), 1);
        stats.packed_states += packed_bytes(states.len(), 2);
    }

    fn mib(bytes: usize) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    #[test]
    fn memory_report() {
        let presets = [
            ("flatish", WorldgenPreset::Flatish),
            ("classic_flatter", WorldgenPreset::ClassicFlatter),
            ("rd161348_like", WorldgenPreset::Rd161348Like),
            ("hilly", WorldgenPreset::Hilly),
        ];

        let mut all = Stats::default();
        println!(
            "\n{:<16} {:>5} {:>9} {:>8} {:>8} {:>7} {:>9} {:>9}",
            "preset", "secs", "secs/col", "ids/sec", "states", "uniform", "cur B/sec", "packed"
        );

        for (label, preset) in presets {
            let mut world = World::new();
            world.configure_world_config(WorldConfig::default(), preset);
            let mut stats = Stats::default();

            for cx in 0..4 {
                for cz in 0..4 {
                    world.ensure_column_generated(cx * CHUNK_SIZE + 512, cz * CHUNK_SIZE - 512);
                    stats.columns += 1;
                }
            }
            for chunk in world.chunks.values() {
                measure(chunk, &mut stats);
            }

            let secs = stats.sections.max(1);
            println!(
                "{:<16} {:>5} {:>9.1} {:>8.1} {:>8.1} {:>6.0}% {:>9} {:>9.0}",
                label,
                stats.sections,
                stats.sections as f64 / stats.columns.max(1) as f64,
                stats.block_ids as f64 / secs as f64,
                stats.states as f64 / secs as f64,
                stats.uniform_sections as f64 * 100.0 / secs as f64,
                CURRENT_BYTES_PER_SECTION,
                stats.packed_blocks as f64 / secs as f64,
            );

            all.sections += stats.sections;
            all.columns += stats.columns;
            all.block_ids += stats.block_ids;
            all.states += stats.states;
            all.uniform_sections += stats.uniform_sections;
            all.zero_data_sections += stats.zero_data_sections;
            all.packed_blocks += stats.packed_blocks;
            all.packed_states += stats.packed_states;
        }

        let secs = all.sections.max(1);
        let sections_per_column = all.sections as f64 / all.columns.max(1) as f64;
        println!(
            "\ntotals: {} sections over {} columns ({:.1} non-empty sections/column), {:.0}% carry only zero block_data",
            all.sections,
            all.columns,
            sections_per_column,
            all.zero_data_sections as f64 * 100.0 / secs as f64,
        );

        // Per-section averages for the three candidate layouts.
        let cur_blocks = VOXELS;
        let cur_pair = VOXELS * 2;
        let merged_state = VOXELS * 2; // one u16 BlockStateId per voxel
        let packed_blocks = all.packed_blocks / secs;
        let packed_states = all.packed_states / secs;
        println!(
            "per section: blocks+data now {} B | merged u16 BlockStateId {} B ({:+.0} B) | palette blocks {} B | palette states {} B",
            cur_pair,
            merged_state,
            merged_state as f64 - cur_pair as f64,
            packed_blocks,
            packed_states,
        );

        // Resident-memory projection. Sections are only stored when non-empty,
        // so scale by the measured non-empty rate rather than world height.
        // `block_data_arc` collapses all-zero data onto one shared buffer, so
        // only the sections that really carry data still pay for the array.
        let non_zero_rate = 1.0 - all.zero_data_sections as f64 / secs as f64;
        println!("\nresident block storage (light excluded):");
        for (label, view) in [("Normal", 8i32), ("Far", 12), ("Extreme", 16), ("Insane", 32)] {
            let columns = ((view * 2 + 1) * (view * 2 + 1)) as f64;
            let sections = (columns * sections_per_column) as usize;
            let before = sections * cur_pair;
            let shared_data = sections * cur_blocks
                + (sections as f64 * non_zero_rate) as usize * cur_blocks
                + cur_blocks;
            let merged = sections * merged_state;
            let packed = sections * packed_states;
            println!(
                "  {:<8} view {:>2} -> {:>6} sections | before {:>7.1} MiB | shared zero data {:>7.1} MiB | merged u16 {:>7.1} MiB | palette states {:>7.1} MiB",
                label,
                view,
                sections,
                mib(before),
                mib(shared_data),
                mib(merged),
                mib(packed),
            );
        }
        println!();

        // Guard the premise, not the numbers: if sections ever stop being
        // low-entropy, palette compression stops being the obvious win and the
        // migration needs re-measuring before anyone starts it.
        let states_per_section = all.states as f64 / secs as f64;
        assert!(
            states_per_section < 32.0,
            "sections now average {states_per_section:.1} distinct block states; re-measure the storage migration"
        );
        assert!(
            all.packed_states < all.sections * cur_pair,
            "palette-packing must be smaller than the two plain arrays"
        );
    }
}
