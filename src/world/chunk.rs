use std::sync::{Arc, OnceLock};

use crate::blocks::BlockId;

use super::CHUNK_SIZE;

#[derive(Debug)]
pub(crate) struct Chunk {
    pub(crate) blocks: ChunkBlocks,
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
            // Starts flat: this exists to be written into immediately, so
            // compacting here would only cost a copy-on-write materialize.
            blocks: ChunkBlocks::flat(vec![BlockId::Air; Self::BLOCK_COUNT]),
            block_data: Arc::new(vec![0; Self::BLOCK_COUNT]),
            light: Arc::new(vec![0; Self::BLOCK_COUNT]),
            revision: 0,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        }
    }

    pub(crate) fn from_parts(
        blocks: ChunkBlocks,
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
        blocks: ChunkBlocks,
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
        blocks: ChunkBlocks,
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
            self.blocks.get(Self::index(x, y, z))
        } else {
            BlockId::Air
        }
    }

    pub(crate) fn set(&mut self, x: i32, y: i32, z: i32, id: BlockId) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let blocks = self.blocks.make_flat_mut();
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
        for block in self.blocks.iter() {
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

/// Bitpacked block storage for one chunk section.
///
/// Sections average 2.0 distinct block states, so a palette plus a few bits per
/// voxel replaces the 4 KiB flat array with ~679 B (see the `memory_report` and
/// `palette_probe` tests).
#[derive(Clone, Debug)]
pub(crate) struct PalettedBlocks {
    palette: Vec<BlockId>,
    bits: u8,
    words: Vec<u64>,
}

impl PalettedBlocks {
    fn encode(blocks: &[BlockId]) -> Self {
        debug_assert_eq!(blocks.len(), Chunk::BLOCK_COUNT);
        let mut palette: Vec<BlockId> = Vec::with_capacity(8);
        let mut index_of = [u16::MAX; 256];
        let mut indices = Vec::with_capacity(blocks.len());
        for &block in blocks {
            let slot = &mut index_of[block.to_u8() as usize];
            if *slot == u16::MAX {
                *slot = palette.len() as u16;
                palette.push(block);
            }
            indices.push(u32::from(*slot));
        }

        let bits = (usize::BITS - (palette.len().max(2) - 1).leading_zeros()).max(1) as u8;
        // `BLOCK_COUNT * bits` is always a multiple of 64, so the last entry
        // ends exactly on a word boundary and the carry below never runs past
        // the end.
        let mut words = vec![0u64; Chunk::BLOCK_COUNT * bits as usize / 64];
        for (i, &index) in indices.iter().enumerate() {
            let bit = i * bits as usize;
            let word = bit / 64;
            let offset = bit % 64;
            words[word] |= u64::from(index) << offset;
            // Widths that do not divide 64 (3, 5, 6, 7 bits) leave entries
            // straddling a word boundary; the high part belongs to the next
            // word. Dropping it silently corrupted every section with 5 or more
            // distinct blocks.
            if offset + bits as usize > 64 {
                words[word + 1] |= u64::from(index) >> (64 - offset);
            }
        }
        Self {
            palette,
            bits,
            words,
        }
    }

    #[inline(always)]
    fn raw_index(&self, voxel: usize) -> u64 {
        let bits = self.bits as usize;
        let bit = voxel * bits;
        let word = bit / 64;
        let offset = bit % 64;
        let mut raw = self.words[word] >> offset;
        if offset + bits > 64 {
            // `offset` is non-zero here (bits <= 8), so the shift is in range.
            raw |= self.words[word + 1] << (64 - offset);
        }
        raw & ((1u64 << bits) - 1)
    }

    #[inline(always)]
    fn get(&self, voxel: usize) -> BlockId {
        self.palette[self.raw_index(voxel) as usize]
    }

    fn decode_into(&self, dst: &mut [BlockId]) {
        for (i, slot) in dst.iter_mut().enumerate() {
            *slot = self.palette[self.raw_index(i) as usize];
        }
    }

    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        self.palette.len() + self.words.len() * 8
    }
}

/// How a chunk's blocks are stored.
///
/// Generated and loaded chunks land in the compact variants; anything that
/// mutates falls back to [`ChunkBlocks::Flat`], which behaves exactly as the
/// plain `Arc<Vec<BlockId>>` this replaced. A chunk therefore stays compact
/// until it is actually edited, and edited chunks keep today's write speed.
///
/// Read costs, measured in `palette_probe`: a bulk `decode_into` (what the
/// mesher's padded staging buffer needs) costs +3.6 us per mesh job, 0.03 % of
/// a mesh pass. A random single-voxel `get` costs +30 % at realistic resident
/// size — which is why `World::build_lighting_snapshot` decodes to flat once
/// rather than letting the light BFS pay it per read.
#[derive(Clone, Debug)]
pub(crate) enum ChunkBlocks {
    /// One block for the whole section — about half of all stored sections.
    /// Costs no heap at all, which beats even a shared flat buffer.
    Uniform(BlockId),
    Paletted(Arc<PalettedBlocks>),
    Flat(Arc<Vec<BlockId>>),
}

impl ChunkBlocks {
    /// Compacts a freshly built section. Use this on every path that produces a
    /// finished chunk (worldgen, save loading); use [`ChunkBlocks::flat`] only
    /// where the buffer is about to be written to.
    pub(crate) fn compact(blocks: Vec<BlockId>) -> Self {
        debug_assert_eq!(blocks.len(), Chunk::BLOCK_COUNT);
        match blocks.first() {
            Some(&first) if blocks.iter().all(|&block| block == first) => Self::Uniform(first),
            Some(_) => Self::Paletted(Arc::new(PalettedBlocks::encode(&blocks))),
            None => Self::Flat(Arc::new(blocks)),
        }
    }

    pub(crate) fn flat(blocks: Vec<BlockId>) -> Self {
        Self::Flat(Arc::new(blocks))
    }

    #[inline(always)]
    pub(crate) fn get(&self, index: usize) -> BlockId {
        match self {
            Self::Uniform(block) => *block,
            Self::Paletted(paletted) => paletted.get(index),
            Self::Flat(blocks) => blocks[index],
        }
    }

    /// Bulk unpack into a caller-owned buffer. `dst` must be exactly one
    /// section long.
    pub(crate) fn decode_into(&self, dst: &mut [BlockId]) {
        debug_assert_eq!(dst.len(), Chunk::BLOCK_COUNT);
        match self {
            Self::Uniform(block) => dst.fill(*block),
            Self::Paletted(paletted) => paletted.decode_into(dst),
            Self::Flat(blocks) => dst.clone_from_slice(blocks),
        }
    }

    /// Copies one contiguous run out, for callers staging partial faces.
    pub(crate) fn copy_range_into(&self, start: usize, dst: &mut [BlockId]) {
        match self {
            Self::Uniform(block) => dst.fill(*block),
            Self::Paletted(paletted) => {
                for (offset, slot) in dst.iter_mut().enumerate() {
                    *slot = paletted.get(start + offset);
                }
            }
            Self::Flat(blocks) => dst.clone_from_slice(&blocks[start..start + dst.len()]),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = BlockId> + '_ {
        (0..self.len()).map(|index| self.get(index))
    }

    pub(crate) fn to_vec(&self) -> Vec<BlockId> {
        let mut out = vec![BlockId::Air; Chunk::BLOCK_COUNT];
        self.decode_into(&mut out);
        out
    }

    /// Materializes into the flat variant and hands out the mutable buffer.
    /// The chunk stays flat afterwards, so repeated edits cost what they
    /// always did.
    pub(crate) fn make_flat_mut(&mut self) -> &mut Vec<BlockId> {
        if !matches!(self, Self::Flat(_)) {
            *self = Self::Flat(Arc::new(self.to_vec()));
        }
        let Self::Flat(blocks) = self else {
            unreachable!("just materialized to flat");
        };
        Arc::make_mut(blocks)
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Uniform(_) | Self::Paletted(_) => Chunk::BLOCK_COUNT,
            Self::Flat(blocks) => blocks.len(),
        }
    }

    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        match self {
            Self::Uniform(_) => 0,
            Self::Paletted(paletted) => paletted.heap_bytes(),
            Self::Flat(blocks) => blocks.len(),
        }
    }
}

impl PartialEq for ChunkBlocks {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Uniform(a), Self::Uniform(b)) => a == b,
            (Self::Flat(a), Self::Flat(b)) => a == b,
            // Representations are not canonical — the same section can arrive
            // flat from an edit and paletted from a reload — so fall back to
            // comparing contents rather than shapes.
            _ => (0..self.len()).all(|i| self.get(i) == other.get(i)),
        }
    }
}

impl Eq for ChunkBlocks {}

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
mod block_storage_tests {
    use super::{Chunk, ChunkBlocks, block_data_arc};
    use crate::blocks::BlockId;
    use crate::world::CHUNK_SIZE;
    use std::sync::Arc;

    fn flat_section(fill: BlockId) -> Vec<BlockId> {
        vec![fill; Chunk::BLOCK_COUNT]
    }

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
    fn uniform_sections_cost_no_heap_at_all() {
        let uniform = ChunkBlocks::compact(flat_section(BlockId::Stone));
        assert!(matches!(uniform, ChunkBlocks::Uniform(BlockId::Stone)));
        assert_eq!(uniform.heap_bytes(), 0);
        assert_eq!(uniform.get(1234), BlockId::Stone);
    }

    #[test]
    fn varied_sections_palette_and_round_trip_exactly() {
        let mut blocks = flat_section(BlockId::Stone);
        blocks[0] = BlockId::Dirt;
        blocks[1] = BlockId::OakPlanks;
        blocks[Chunk::BLOCK_COUNT - 1] = BlockId::Air;

        let compact = ChunkBlocks::compact(blocks.clone());
        assert!(matches!(compact, ChunkBlocks::Paletted(_)));
        assert!(
            compact.heap_bytes() < Chunk::BLOCK_COUNT,
            "paletted storage must be smaller than the flat array"
        );
        assert_eq!(compact.to_vec(), blocks);
        for (index, &block) in blocks.iter().enumerate() {
            assert_eq!(compact.get(index), block, "mismatch at {index}");
        }

        // Partial-range staging is what the meshing neighborhood uses.
        let mut range = vec![BlockId::Air; 16];
        compact.copy_range_into(0, &mut range);
        assert_eq!(range, blocks[..16]);
    }

    #[test]
    fn every_palette_width_round_trips() {
        // Regression: entries straddling a 64-bit word boundary used to lose
        // their high bits, which silently corrupted every section whose palette
        // needed a width that does not divide 64 (5..=8 distinct blocks and up).
        // The earlier tests all happened to use <= 4 blocks, i.e. 2 bits.
        let ids: Vec<BlockId> = (0u16..=255)
            .filter_map(|raw| BlockId::from_u8(raw as u8).ok())
            .collect();
        assert!(ids.len() >= 9, "need enough block ids to exercise 4 bits");

        for distinct in 1..=ids.len() {
            let mut blocks = Vec::with_capacity(Chunk::BLOCK_COUNT);
            for i in 0..Chunk::BLOCK_COUNT {
                blocks.push(ids[i % distinct]);
            }
            let stored = ChunkBlocks::compact(blocks.clone());
            assert_eq!(
                stored.to_vec(),
                blocks,
                "bulk decode wrong for {distinct} distinct blocks"
            );
            for (index, &block) in blocks.iter().enumerate() {
                assert_eq!(
                    stored.get(index),
                    block,
                    "single read wrong at {index} for {distinct} distinct blocks"
                );
            }
            // The path the meshing neighborhood actually uses: one 16-wide run
            // per (y, z), i.e. every possible bit offset within a word.
            let mut run = vec![BlockId::Air; CHUNK_SIZE as usize];
            for start in (0..Chunk::BLOCK_COUNT).step_by(CHUNK_SIZE as usize) {
                stored.copy_range_into(start, &mut run);
                assert_eq!(
                    run.as_slice(),
                    &blocks[start..start + CHUNK_SIZE as usize],
                    "range copy wrong at {start} for {distinct} distinct blocks"
                );
            }
        }
    }

    #[test]
    fn equality_compares_contents_not_representation() {
        let blocks = flat_section(BlockId::Stone);
        assert_eq!(
            ChunkBlocks::compact(blocks.clone()),
            ChunkBlocks::flat(blocks.clone()),
            "a uniform and a flat section with the same contents are equal"
        );

        let mut other = blocks;
        other[9] = BlockId::Dirt;
        assert_ne!(
            ChunkBlocks::compact(other.clone()),
            ChunkBlocks::flat(flat_section(BlockId::Stone))
        );
    }

    #[test]
    fn editing_a_compact_section_materializes_without_touching_its_neighbours() {
        let mut first = Chunk::from_parts(
            ChunkBlocks::compact(flat_section(BlockId::Stone)),
            block_data_arc(vec![0; Chunk::BLOCK_COUNT]),
            Arc::new(vec![0; Chunk::BLOCK_COUNT]),
            0,
        );
        let second = Chunk::from_parts(
            ChunkBlocks::compact(flat_section(BlockId::Stone)),
            block_data_arc(vec![0; Chunk::BLOCK_COUNT]),
            Arc::new(vec![0; Chunk::BLOCK_COUNT]),
            0,
        );

        first.set(4, 5, 6, BlockId::Air);

        assert!(
            matches!(first.blocks, ChunkBlocks::Flat(_)),
            "an edited section falls back to the flat array"
        );
        assert_eq!(first.get(4, 5, 6), BlockId::Air);
        assert_eq!(
            second.get(4, 5, 6),
            BlockId::Stone,
            "the compact neighbour must be unaffected"
        );
        // Cached voxel counts are per chunk, so they must not follow the storage.
        assert_eq!(first.solid_voxel_count, Chunk::BLOCK_COUNT as u16 - 1);
        assert_eq!(second.solid_voxel_count, Chunk::BLOCK_COUNT as u16);
    }

    #[test]
    fn writing_block_data_copies_instead_of_corrupting_shared_sections() {
        let mut first = Chunk::from_parts(
            ChunkBlocks::compact(flat_section(BlockId::Stone)),
            block_data_arc(vec![0; Chunk::BLOCK_COUNT]),
            Arc::new(vec![0; Chunk::BLOCK_COUNT]),
            0,
        );
        let second = Chunk::from_parts(
            ChunkBlocks::compact(flat_section(BlockId::Stone)),
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
    }
}

/// Sizes the in-memory chunk storage and the cost of its compact form.
///
/// Both halves of the `docs/TODO.md` storage item are measured here: how much
/// [`ChunkBlocks`] actually saves, and what its reads cost compared to a plain
/// array. Run with
/// `cargo test --release world::chunk::storage_report -- --nocapture`; timings
/// from a debug build are meaningless.
#[cfg(test)]
mod storage_report {
    use super::ChunkBlocks;
    use crate::blocks::BlockId;
    use crate::world::{CHUNK_SIZE, World, WorldConfig};
    use crate::worldgen::WorldgenPreset;
    use std::time::Instant;

    const VOXELS: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;

    fn mib(bytes: usize) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    fn generated_sections(preset: WorldgenPreset) -> Vec<Vec<BlockId>> {
        let mut world = World::new();
        world.configure_world_config(WorldConfig::default(), preset);
        for cx in 0..4 {
            for cz in 0..4 {
                world.ensure_column_generated(cx * CHUNK_SIZE, cz * CHUNK_SIZE);
            }
        }
        world.chunks.values().map(|c| c.blocks.to_vec()).collect()
    }

    #[test]
    fn storage_report() {
        let presets = [
            ("flatish", WorldgenPreset::Flatish),
            ("classic_flatter", WorldgenPreset::ClassicFlatter),
            ("rd161348_like", WorldgenPreset::Rd161348Like),
            ("hilly", WorldgenPreset::Hilly),
        ];

        let mut all_sections = 0usize;
        let mut all_uniform = 0usize;
        let mut all_bytes = 0usize;

        println!(
            "\n{:<16} {:>6} {:>9} {:>12} {:>10}",
            "preset", "secs", "uniform", "flat B/sec", "compact"
        );
        for (label, preset) in presets {
            let sections = generated_sections(preset);
            let compact: Vec<ChunkBlocks> = sections
                .iter()
                .map(|blocks| ChunkBlocks::compact(blocks.clone()))
                .collect();
            let uniform = compact
                .iter()
                .filter(|b| matches!(b, ChunkBlocks::Uniform(_)))
                .count();
            let bytes: usize = compact.iter().map(ChunkBlocks::heap_bytes).sum();

            // Every representation must decode back to exactly what went in.
            for (blocks, stored) in sections.iter().zip(&compact) {
                assert_eq!(&stored.to_vec(), blocks, "{label}: round trip must be exact");
            }

            println!(
                "{:<16} {:>6} {:>8.0}% {:>12} {:>10.0}",
                label,
                sections.len(),
                uniform as f64 * 100.0 / sections.len().max(1) as f64,
                VOXELS,
                bytes as f64 / sections.len().max(1) as f64,
            );

            all_sections += sections.len();
            all_uniform += uniform;
            all_bytes += bytes;
        }

        let per_section = all_bytes as f64 / all_sections.max(1) as f64;
        println!(
            "\ntotals: {} sections, {:.0}% uniform, {:.0} B/section ({:.1}x smaller than flat)",
            all_sections,
            all_uniform as f64 * 100.0 / all_sections.max(1) as f64,
            per_section,
            VOXELS as f64 / per_section.max(1.0),
        );

        // Resident-memory projection. Only non-empty sections are stored, so
        // scale by the measured non-empty rate rather than world height.
        let sections_per_column = all_sections as f64 / (4 * 4 * presets.len()) as f64;
        println!("\nresident block storage (block_data and light excluded):");
        for (label, view) in [("Normal", 8i32), ("Far", 12), ("Extreme", 16), ("Insane", 32)] {
            let columns = ((view * 2 + 1) * (view * 2 + 1)) as f64;
            let sections = (columns * sections_per_column) as usize;
            println!(
                "  {:<8} view {:>2} -> {:>6} sections | flat {:>7.1} MiB | compact {:>7.1} MiB",
                label,
                view,
                sections,
                mib(sections * VOXELS),
                mib((sections as f64 * per_section) as usize),
            );
        }

        assert!(
            per_section < VOXELS as f64 / 2.0,
            "compact storage lost its advantage: {per_section:.0} B/section"
        );
    }

    #[test]
    fn read_cost_report() {
        let sections = generated_sections(WorldgenPreset::Hilly);
        let flat: Vec<ChunkBlocks> = sections
            .iter()
            .map(|blocks| ChunkBlocks::flat(blocks.clone()))
            .collect();
        let compact: Vec<ChunkBlocks> = sections
            .iter()
            .map(|blocks| ChunkBlocks::compact(blocks.clone()))
            .collect();

        // Debug builds run this ~100x slower and their timings say nothing
        // anyway; keep the round trip covered without stalling the suite.
        let rounds = if cfg!(debug_assertions) { 2 } else { 200 };
        let mut scratch = vec![BlockId::Air; VOXELS];

        // Single timing runs swing by 50 % here, so take the minimum of a few
        // attempts: the fastest run is the one least polluted by other load.
        let best_of = |mut attempt: Box<dyn FnMut() -> f64 + '_>| {
            (0..5).map(|_| attempt()).fold(f64::INFINITY, f64::min)
        };
        let bulk_ns = |storage: &[ChunkBlocks], scratch: &mut Vec<BlockId>| {
            let start = Instant::now();
            for _ in 0..rounds {
                for stored in storage {
                    stored.decode_into(scratch);
                    std::hint::black_box(&scratch);
                }
            }
            start.elapsed().as_nanos() as f64 / (rounds * storage.len()) as f64
        };
        let flat_bulk = best_of(Box::new(|| bulk_ns(&flat, &mut scratch)));
        let compact_bulk = best_of(Box::new(|| bulk_ns(&compact, &mut scratch)));

        // Scattered single reads at a realistic resident size — the pattern the
        // light BFS uses, and the only one where the compact form costs.
        let resident = 6857;
        let big_flat: Vec<ChunkBlocks> = (0..resident)
            .map(|i| ChunkBlocks::flat(sections[i % sections.len()].clone()))
            .collect();
        let big_compact: Vec<ChunkBlocks> = (0..resident)
            .map(|i| ChunkBlocks::compact(sections[i % sections.len()].clone()))
            .collect();
        let probes: Vec<(usize, usize)> = {
            let mut rng = 0x2545_F491_4F6C_DD1Du64;
            (0..16384)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    ((rng as usize) % resident, ((rng >> 20) as usize) % VOXELS)
                })
                .collect()
        };
        let big_rounds = rounds.min(20);
        let random_ns = |storage: &[ChunkBlocks]| {
            let start = Instant::now();
            for _ in 0..big_rounds {
                let mut acc = 0u32;
                for &(section, voxel) in &probes {
                    acc = acc.wrapping_add(storage[section].get(voxel).to_u8() as u32);
                }
                std::hint::black_box(acc);
            }
            start.elapsed().as_nanos() as f64 / (big_rounds * probes.len()) as f64
        };
        let flat_random = best_of(Box::new(|| random_ns(&big_flat)));
        let compact_random = best_of(Box::new(|| random_ns(&big_compact)));

        println!(
            "\nbulk decode into the meshing staging buffer: flat {:.2} us | compact {:.2} us ({:+.2} us per mesh job, {:.3} % of a 12.9 ms mesh pass)",
            flat_bulk / 1000.0,
            compact_bulk / 1000.0,
            (compact_bulk - flat_bulk) / 1000.0,
            (compact_bulk - flat_bulk) / 1000.0 / 12_900.0 * 100.0,
        );
        println!(
            "random single-voxel read over {} sections ({:.1} MiB flat): flat {:.2} ns | compact {:.2} ns ({:+.0} %)\n",
            resident,
            mib(resident * VOXELS),
            flat_random,
            compact_random,
            (compact_random - flat_random) / flat_random * 100.0,
        );
    }
}
