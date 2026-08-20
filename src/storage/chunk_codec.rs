//! Column payload wire format.
//!
//! Two versions exist on disk. **v2 is the only one this crate writes**; v1 is
//! read-only and must stay that way so pre-name-palette saves keep opening.
//!
//! Shared header (little endian): magic `MKCL`, `u16` format_version, `u16`
//! column flags (must be 0), `i32` × 8 world params (cx, cz, min_y, max_y,
//! sea_level, chunk_size, min_section_y, max_section_y), `u16` section count,
//! `u16` reserved (must be 0). Sections follow in strictly ascending `cy`
//! order, each prefixed by `i32 cy`, `u8` section flags and 3 zero bytes. A
//! `u32` CRC32 over everything before it closes the payload.
//!
//! The versions differ only in how a section's blocks are encoded:
//!
//! * **v1** (section flags [`SECTION_FLAGS_V1`]): a byte array of *manifest*
//!   block ids, palette-compressed like the block-data array. Decoding needs
//!   the world's `IdMapper`, so v1 blobs cannot be interpreted without the
//!   save's `manifest.json`.
//! * **v2** (section flags [`SECTION_FLAGS_V2`]): a bitpacked index array over
//!   a per-section palette of block *names*. Self-describing — unknown names
//!   degrade to air instead of silently resolving to a foreign block.
//!
//! Adding a v3 means bumping [`COLUMN_PAYLOAD_FORMAT_VERSION`] and keeping both
//! older branches in [`decode_column`]; see `docs/TODO.md` for the open
//! compaction ideas.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

use crate::blocks::{BlockId, BlockRegistry};
use crate::perf::trace as perf_trace;
use crate::storage::format::{
    COLUMN_PAYLOAD_FORMAT_VERSION, COLUMN_PAYLOAD_MAGIC, SECTION_BLOCK_COUNT,
};
use crate::world::{Chunk, ChunkCoord, World, block_data_arc, blocks_arc};

const COLUMN_RESERVED0_V1: u16 = 0;
const SECTION_FLAGS_V1: u8 = 0;
const COLUMN_PAYLOAD_FORMAT_VERSION_V1: u16 = 1;
const COLUMN_FLAGS_V2: u16 = 0;
const SECTION_FLAGS_V2: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColumnBlob {
    pub(crate) format_version: u16,
    pub(crate) cx: i32,
    pub(crate) cz: i32,
    pub(crate) min_y: i32,
    pub(crate) max_y: i32,
    pub(crate) sea_level: i32,
    pub(crate) chunk_size: i32,
    pub(crate) min_section_y: i32,
    pub(crate) max_section_y: i32,
    pub(crate) sections: Vec<SectionBlob>,
}

#[derive(Clone, Debug)]
pub(crate) struct ColumnSaveSectionSnapshot {
    pub(crate) cy: i32,
    pub(crate) blocks: Arc<Vec<BlockId>>,
    pub(crate) data: Arc<Vec<u8>>,
    pub(crate) revision: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ColumnSaveSnapshot {
    pub(crate) cx: i32,
    pub(crate) cz: i32,
    pub(crate) min_y: i32,
    pub(crate) max_y: i32,
    pub(crate) sea_level: i32,
    pub(crate) chunk_size: i32,
    pub(crate) min_section_y: i32,
    pub(crate) max_section_y: i32,
    pub(crate) dirty_generation: u64,
    pub(crate) save_epoch: u64,
    pub(crate) sections: Vec<ColumnSaveSectionSnapshot>,
}

#[derive(Clone, Debug)]
pub(crate) struct ColumnSaveCompletion {
    pub(crate) cx: i32,
    pub(crate) cz: i32,
    pub(crate) min_section_y: i32,
    pub(crate) max_section_y: i32,
    pub(crate) dirty_generation: u64,
    pub(crate) save_epoch: u64,
    pub(crate) section_revisions: Vec<(i32, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SectionBlob {
    pub(crate) cy: i32,
    pub(crate) blocks: EncodedBlocks,
    pub(crate) data: EncodedArray,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EncodedBlocks {
    LegacyManifestIds(EncodedArray),
    NamePaletteBitpacked {
        palette: Vec<String>,
        bits_per_entry: u8,
        packed_words: Vec<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EncodedArray {
    Single(u8),
    PaletteBitpacked {
        palette: Vec<u8>,
        bits_per_entry: u8,
        packed_words: Vec<u64>,
    },
}

impl EncodedArray {
    pub(crate) fn from_values(values: &[u8]) -> io::Result<Self> {
        if values.len() != SECTION_BLOCK_COUNT {
            return invalid_data(format!(
                "encoded array source length mismatch: expected {}, got {}",
                SECTION_BLOCK_COUNT,
                values.len()
            ));
        }

        let first = values[0];
        if values.iter().all(|&v| v == first) {
            return Ok(Self::Single(first));
        }

        let mut palette = Vec::with_capacity(16);
        let mut index_of = [u16::MAX; 256];
        let mut indices = Vec::with_capacity(SECTION_BLOCK_COUNT);

        for &value in values {
            let slot = &mut index_of[value as usize];
            let idx = if *slot == u16::MAX {
                let new_idx = palette.len();
                if new_idx > u8::MAX as usize {
                    return invalid_data("palette too large");
                }
                palette.push(value);
                *slot = new_idx as u16;
                new_idx as u16
            } else {
                *slot
            };
            indices.push(idx);
        }

        let bits_per_entry = ceil_log2_u16(palette.len() as u16).max(1);
        let packed_words = pack_indices(&indices, bits_per_entry)?;
        Ok(Self::PaletteBitpacked {
            palette,
            bits_per_entry,
            packed_words,
        })
    }

    pub(crate) fn decode_values(&self) -> io::Result<Vec<u8>> {
        match self {
            Self::Single(value) => Ok(vec![*value; SECTION_BLOCK_COUNT]),
            Self::PaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            } => {
                if palette.is_empty() {
                    return invalid_data("palette bitpacked array has empty palette");
                }
                if *bits_per_entry == 0 || *bits_per_entry > 8 {
                    return invalid_data(format!(
                        "invalid bits_per_entry: expected 1..=8, got {}",
                        bits_per_entry
                    ));
                }
                if *bits_per_entry < ceil_log2_u16(palette.len() as u16).max(1) {
                    return invalid_data(format!(
                        "bits_per_entry {} too small for palette length {}",
                        bits_per_entry,
                        palette.len()
                    ));
                }
                let expected_words = expected_word_count(*bits_per_entry);
                if packed_words.len() != expected_words {
                    return invalid_data(format!(
                        "packed word count mismatch: expected {}, got {}",
                        expected_words,
                        packed_words.len()
                    ));
                }

                let mut values = Vec::with_capacity(SECTION_BLOCK_COUNT);
                let mask = (1u64 << *bits_per_entry) - 1;
                for i in 0..SECTION_BLOCK_COUNT {
                    let bit_pos = i * (*bits_per_entry as usize);
                    let word_index = bit_pos / 64;
                    let bit_offset = bit_pos % 64;
                    let mut raw = packed_words[word_index] >> bit_offset;
                    if bit_offset + (*bits_per_entry as usize) > 64 {
                        let hi_bits = bit_offset + (*bits_per_entry as usize) - 64;
                        raw |= packed_words[word_index + 1] << (64 - bit_offset);
                        debug_assert!(hi_bits > 0);
                    }
                    let palette_index = (raw & mask) as usize;
                    let Some(&value) = palette.get(palette_index) else {
                        return invalid_data(format!(
                            "palette index out of range: idx={} palette_len={}",
                            palette_index,
                            palette.len()
                        ));
                    };
                    values.push(value);
                }

                let used_bits = SECTION_BLOCK_COUNT * (*bits_per_entry as usize);
                let total_bits = packed_words.len() * 64;
                if total_bits > used_bits {
                    let trailing_bits = total_bits - used_bits;
                    let last = *packed_words.last().unwrap_or(&0);
                    let used_in_last = 64usize.saturating_sub(trailing_bits);
                    if used_in_last < 64 {
                        let trailing_mask = !0u64 << used_in_last;
                        if (last & trailing_mask) != 0 {
                            return invalid_data("non-zero trailing bits in packed indices");
                        }
                    }
                }

                Ok(values)
            }
        }
    }
}

impl EncodedBlocks {
    fn from_runtime_blocks(values: &[BlockId], block_registry: &BlockRegistry) -> io::Result<Self> {
        if values.len() != SECTION_BLOCK_COUNT {
            return invalid_data(format!(
                "encoded block source length mismatch: expected {}, got {}",
                SECTION_BLOCK_COUNT,
                values.len()
            ));
        }

        let mut palette = Vec::with_capacity(32);
        let mut index_of: HashMap<&'static str, u32> = HashMap::with_capacity(32);
        let mut indices = Vec::with_capacity(SECTION_BLOCK_COUNT);

        for &block in values {
            let name = block_registry.name_of(block);
            let idx = if let Some(idx) = index_of.get(name).copied() {
                idx
            } else {
                let idx = u32::try_from(palette.len()).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "palette length exceeds u32 range",
                    )
                })?;
                palette.push(name.to_string());
                index_of.insert(name, idx);
                idx
            };
            indices.push(idx);
        }

        if palette.is_empty() {
            return invalid_data("block palette must not be empty");
        }
        let bits_per_entry = ceil_log2_u32(palette.len() as u32).max(1);
        let packed_words = pack_indices_u32(&indices, bits_per_entry)?;
        Ok(Self::NamePaletteBitpacked {
            palette,
            bits_per_entry,
            packed_words,
        })
    }
}

pub(crate) fn encode_column(world: &World, cx: i32, cz: i32) -> io::Result<Vec<u8>> {
    let min_section_y = world.min_chunk_y();
    let max_section_y = world.max_chunk_y();
    if min_section_y > max_section_y {
        return invalid_data(format!(
            "invalid world chunk-y range: min_section_y {} > max_section_y {}",
            min_section_y, max_section_y
        ));
    }

    let block_registry = BlockRegistry::new();
    let mut section_inputs = Vec::new();
    for cy in min_section_y..=max_section_y {
        let coord = ChunkCoord {
            x: cx,
            y: cy,
            z: cz,
        };
        let Some(chunk) = world.get_chunk(coord) else {
            continue;
        };
        let mut any_non_air = false;
        for &block in chunk.blocks.iter() {
            any_non_air |= block != BlockId::Air;
        }
        section_inputs.push((
            cy,
            chunk.blocks.as_slice(),
            chunk.block_data.as_slice(),
            any_non_air,
        ));
    }

    encode_column_from_sections(
        cx,
        cz,
        world.min_y(),
        world.max_y(),
        world.sea_level(),
        world.chunk_size(),
        min_section_y,
        max_section_y,
        section_inputs,
        &block_registry,
    )
}

pub(crate) fn encode_column_snapshot(snapshot: &ColumnSaveSnapshot) -> io::Result<Vec<u8>> {
    let block_registry = BlockRegistry::new();
    let mut section_inputs = Vec::with_capacity(snapshot.sections.len());
    for section in &snapshot.sections {
        let mut any_non_air = false;
        for &block in section.blocks.iter() {
            any_non_air |= block != BlockId::Air;
        }
        section_inputs.push((
            section.cy,
            section.blocks.as_slice(),
            section.data.as_slice(),
            any_non_air,
        ));
    }
    encode_column_from_sections(
        snapshot.cx,
        snapshot.cz,
        snapshot.min_y,
        snapshot.max_y,
        snapshot.sea_level,
        snapshot.chunk_size,
        snapshot.min_section_y,
        snapshot.max_section_y,
        section_inputs,
        &block_registry,
    )
}

impl ColumnSaveSnapshot {
    pub(crate) fn completion_meta(&self) -> ColumnSaveCompletion {
        ColumnSaveCompletion {
            cx: self.cx,
            cz: self.cz,
            min_section_y: self.min_section_y,
            max_section_y: self.max_section_y,
            dirty_generation: self.dirty_generation,
            save_epoch: self.save_epoch,
            section_revisions: self
                .sections
                .iter()
                .map(|section| (section.cy, section.revision))
                .collect(),
        }
    }
}

fn encode_column_from_sections(
    cx: i32,
    cz: i32,
    min_y: i32,
    max_y: i32,
    sea_level: i32,
    chunk_size: i32,
    min_section_y: i32,
    max_section_y: i32,
    sections_input: Vec<(i32, &[BlockId], &[u8], bool)>,
    block_registry: &BlockRegistry,
) -> io::Result<Vec<u8>> {
    if min_section_y > max_section_y {
        return invalid_data(format!(
            "invalid world chunk-y range: min_section_y {} > max_section_y {}",
            min_section_y, max_section_y
        ));
    }

    let mut sections = Vec::new();
    for (cy, blocks_src, data_src, any_non_air) in sections_input {
        if blocks_src.len() != SECTION_BLOCK_COUNT {
            return invalid_data(format!(
                "section block length mismatch at cy {}: expected {}, got {}",
                cy,
                SECTION_BLOCK_COUNT,
                blocks_src.len()
            ));
        }
        if data_src.len() != SECTION_BLOCK_COUNT {
            return invalid_data(format!(
                "section data length mismatch at cy {}: expected {}, got {}",
                cy,
                SECTION_BLOCK_COUNT,
                data_src.len()
            ));
        }

        let any_non_zero_data = data_src.iter().any(|&v| v != 0);
        if !any_non_air && !any_non_zero_data {
            continue;
        }

        sections.push(SectionBlob {
            cy,
            blocks: EncodedBlocks::from_runtime_blocks(blocks_src, block_registry)?,
            data: EncodedArray::from_values(data_src)?,
        });
    }

    let column = ColumnBlob {
        format_version: COLUMN_PAYLOAD_FORMAT_VERSION,
        cx,
        cz,
        min_y,
        max_y,
        sea_level,
        chunk_size,
        min_section_y,
        max_section_y,
        sections,
    };
    encode_blob(&column)
}

pub(crate) fn encode_blob(column: &ColumnBlob) -> io::Result<Vec<u8>> {
    validate_column_blob_shape(column)?;

    let mut out = Vec::with_capacity(1024);
    out.extend_from_slice(&COLUMN_PAYLOAD_MAGIC);
    out.extend_from_slice(&COLUMN_PAYLOAD_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&COLUMN_FLAGS_V2.to_le_bytes());
    out.extend_from_slice(&column.cx.to_le_bytes());
    out.extend_from_slice(&column.cz.to_le_bytes());
    out.extend_from_slice(&column.min_y.to_le_bytes());
    out.extend_from_slice(&column.max_y.to_le_bytes());
    out.extend_from_slice(&column.sea_level.to_le_bytes());
    out.extend_from_slice(&column.chunk_size.to_le_bytes());
    out.extend_from_slice(&column.min_section_y.to_le_bytes());
    out.extend_from_slice(&column.max_section_y.to_le_bytes());
    let section_count_u16 = u16::try_from(column.sections.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "too many sections for v1 column payload: {}",
                column.sections.len()
            ),
        )
    })?;
    out.extend_from_slice(&section_count_u16.to_le_bytes());
    out.extend_from_slice(&COLUMN_RESERVED0_V1.to_le_bytes());

    for section in &column.sections {
        out.extend_from_slice(&section.cy.to_le_bytes());
        out.push(SECTION_FLAGS_V2);
        out.extend_from_slice(&[0, 0, 0]);
        write_encoded_blocks_v2(&mut out, &section.blocks)?;
        write_encoded_array(&mut out, &section.data)?;
    }

    let crc = crc32_ieee(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

pub(crate) fn decode_column(bytes: &[u8]) -> io::Result<ColumnBlob> {
    let mut rd = ReadCursor::new(bytes);
    let magic = rd.read_exact(4)?;
    if magic != COLUMN_PAYLOAD_MAGIC {
        return invalid_data(format!(
            "invalid column payload magic: expected {:?}, got {:?}",
            String::from_utf8_lossy(&COLUMN_PAYLOAD_MAGIC),
            String::from_utf8_lossy(magic)
        ));
    }

    let format_version = rd.read_u16_le()?;
    if format_version != COLUMN_PAYLOAD_FORMAT_VERSION
        && format_version != COLUMN_PAYLOAD_FORMAT_VERSION_V1
    {
        return invalid_data(format!(
            "unsupported column payload format_version: expected {} or {}, got {}",
            COLUMN_PAYLOAD_FORMAT_VERSION_V1, COLUMN_PAYLOAD_FORMAT_VERSION, format_version
        ));
    }

    let flags = rd.read_u16_le()?;
    if flags != 0 {
        return invalid_data(format!("unsupported column payload flags: {}", flags));
    }

    let cx = rd.read_i32_le()?;
    let cz = rd.read_i32_le()?;
    let min_y = rd.read_i32_le()?;
    let max_y = rd.read_i32_le()?;
    let sea_level = rd.read_i32_le()?;
    let chunk_size = rd.read_i32_le()?;
    let min_section_y = rd.read_i32_le()?;
    let max_section_y = rd.read_i32_le()?;
    let section_count = rd.read_u16_le()? as usize;
    let reserved0 = rd.read_u16_le()?;
    if reserved0 != 0 {
        return invalid_data(format!(
            "unsupported non-zero column reserved0 field: {}",
            reserved0
        ));
    }

    let mut sections = Vec::with_capacity(section_count);
    let mut last_cy: Option<i32> = None;
    for _ in 0..section_count {
        let cy = rd.read_i32_le()?;
        let section_flags = rd.read_u8()?;
        let section_reserved = rd.read_exact(3)?;
        if section_reserved != [0, 0, 0] {
            return invalid_data(format!(
                "unsupported non-zero section reserved bytes for cy {}",
                cy
            ));
        }
        if let Some(prev) = last_cy {
            if cy <= prev {
                return invalid_data(format!(
                    "section cy values must be strictly ascending: prev={}, current={}",
                    prev, cy
                ));
            }
        }

        let blocks = if format_version == COLUMN_PAYLOAD_FORMAT_VERSION_V1 {
            if section_flags != SECTION_FLAGS_V1 {
                return invalid_data(format!(
                    "unsupported non-zero v1 section flags for cy {}: {}",
                    cy, section_flags
                ));
            }
            EncodedBlocks::LegacyManifestIds(read_encoded_array(&mut rd)?)
        } else {
            if section_flags != SECTION_FLAGS_V2 {
                return invalid_data(format!(
                    "unsupported section flags for v2 payload at cy {}: {}",
                    cy, section_flags
                ));
            }
            read_encoded_blocks_v2(&mut rd)?
        };
        let data = read_encoded_array(&mut rd)?;

        // Validate payload contents now so later apply() stays simple and re-encode is deterministic.
        match &blocks {
            EncodedBlocks::LegacyManifestIds(arr) => {
                let _ = arr.decode_values()?;
            }
            EncodedBlocks::NamePaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            } => {
                validate_name_palette_blocks(palette, *bits_per_entry, packed_words)?;
            }
        }
        let _ = data.decode_values()?;

        last_cy = Some(cy);
        sections.push(SectionBlob { cy, blocks, data });
    }

    let crc_offset = rd.offset();
    let stored_crc = rd.read_u32_le()?;
    if !rd.is_at_end() {
        return invalid_data(format!(
            "trailing bytes after column payload footer: {}",
            rd.remaining_len()
        ));
    }
    let computed_crc = crc32_ieee(&bytes[..crc_offset]);
    if stored_crc != computed_crc {
        return invalid_data(format!(
            "column payload CRC32 mismatch: expected {:08x}, got {:08x}",
            stored_crc, computed_crc
        ));
    }

    let column = ColumnBlob {
        format_version,
        cx,
        cz,
        min_y,
        max_y,
        sea_level,
        chunk_size,
        min_section_y,
        max_section_y,
        sections,
    };
    validate_column_blob_shape(&column)?;
    Ok(column)
}

pub(crate) fn validate_column_for_world(world: &World, column: &ColumnBlob) -> io::Result<()> {
    if column.chunk_size != world.chunk_size()
        || column.min_y != world.min_y()
        || column.max_y != world.max_y()
        || column.sea_level != world.sea_level()
    {
        return invalid_data(format!(
            "column world params mismatch: expected min_y={}, max_y={}, sea_level={}, chunk_size={}, got min_y={}, max_y={}, sea_level={}, chunk_size={}",
            world.min_y(),
            world.max_y(),
            world.sea_level(),
            world.chunk_size(),
            column.min_y,
            column.max_y,
            column.sea_level,
            column.chunk_size
        ));
    }
    if column.min_section_y != world.min_chunk_y() || column.max_section_y != world.max_chunk_y() {
        return invalid_data(format!(
            "column section range mismatch: expected {}..={}, got {}..={}",
            world.min_chunk_y(),
            world.max_chunk_y(),
            column.min_section_y,
            column.max_section_y
        ));
    }

    validate_column_blob_shape(column)?;
    Ok(())
}

pub(crate) fn decode_chunk_from_section_blob(
    world: &World,
    column: &ColumnBlob,
    section: &SectionBlob,
) -> io::Result<Option<Chunk>> {
    validate_column_for_world(world, column)?;
    let block_registry = BlockRegistry::new();
    let data_bytes = section.data.decode_values()?;
    let mut any_non_air = false;
    let blocks = match &section.blocks {
        EncodedBlocks::LegacyManifestIds(blocks_legacy) => {
            let block_bytes = blocks_legacy.decode_values()?;
            let mut out = Vec::with_capacity(SECTION_BLOCK_COUNT);
            for &raw in &block_bytes {
                let block = world.decode_manifest_block_id(raw as u32);
                any_non_air |= block != BlockId::Air;
                out.push(block);
            }
            out
        }
        EncodedBlocks::NamePaletteBitpacked {
            palette,
            bits_per_entry,
            packed_words,
        } => {
            let palette_resolved = {
                let _trace_scope = perf_trace::scope("apply_saved.apply_column.palette_resolve");
                let mut resolved = Vec::with_capacity(palette.len());
                for name in palette {
                    let block = block_registry.resolve_fast(name).unwrap_or_else(|| {
                        crate::console_error!(
                            "save decode: unknown block '{}' in section y={} at column ({},{}); mapping to air",
                            name, section.cy, column.cx, column.cz
                        );
                        BlockId::Air
                    });
                    resolved.push(block);
                }
                resolved
            };
            let indices = {
                let _trace_scope = perf_trace::scope("apply_saved.apply_column.indices_unpack");
                unpack_indices_u32(*bits_per_entry, packed_words, palette.len())?
            };
            {
                let _trace_scope = perf_trace::scope("apply_saved.apply_column.blocks_fill");
                let mut out = vec![BlockId::Air; SECTION_BLOCK_COUNT];
                for (voxel_idx, palette_index) in indices.iter().enumerate() {
                    let block = palette_resolved[*palette_index as usize];
                    any_non_air |= block != BlockId::Air;
                    out[voxel_idx] = block;
                }
                out
            }
        }
    };

    let any_non_zero_data = data_bytes.iter().any(|&raw| raw != 0);
    if !any_non_air && !any_non_zero_data {
        return Ok(None);
    }

    Ok(Some(Chunk::from_parts(
        blocks_arc(blocks),
        block_data_arc(data_bytes),
        Arc::new(vec![0; SECTION_BLOCK_COUNT]),
        0,
    )))
}

pub(crate) fn apply_column(world: &mut World, column: ColumnBlob) -> io::Result<()> {
    validate_column_for_world(world, &column)?;

    for cy in column.min_section_y..=column.max_section_y {
        world.chunks.remove(&ChunkCoord {
            x: column.cx,
            y: cy,
            z: column.cz,
        });
    }

    for section in &column.sections {
        let Some(chunk) = decode_chunk_from_section_blob(world, &column, section)? else {
            continue;
        };
        world.chunks.insert(
            ChunkCoord {
                x: column.cx,
                y: section.cy,
                z: column.cz,
            },
            chunk,
        );
    }

    Ok(())
}

fn validate_column_blob_shape(column: &ColumnBlob) -> io::Result<()> {
    if column.format_version != COLUMN_PAYLOAD_FORMAT_VERSION
        && column.format_version != COLUMN_PAYLOAD_FORMAT_VERSION_V1
    {
        return invalid_data(format!(
            "unsupported column format_version {}",
            column.format_version
        ));
    }
    if column.min_y > column.max_y {
        return invalid_data(format!(
            "invalid column world height range: min_y {} > max_y {}",
            column.min_y, column.max_y
        ));
    }
    if column.chunk_size != 16 {
        return invalid_data(format!(
            "unsupported chunk_size in column payload: expected 16, got {}",
            column.chunk_size
        ));
    }
    if column.min_section_y > column.max_section_y {
        return invalid_data(format!(
            "invalid section range: min_section_y {} > max_section_y {}",
            column.min_section_y, column.max_section_y
        ));
    }
    if column.sea_level < column.min_y || column.sea_level > column.max_y {
        return invalid_data(format!(
            "invalid sea_level {} outside [{}, {}]",
            column.sea_level, column.min_y, column.max_y
        ));
    }

    let mut seen = HashSet::with_capacity(column.sections.len());
    let mut prev: Option<i32> = None;
    for section in &column.sections {
        if section.cy < column.min_section_y || section.cy > column.max_section_y {
            return invalid_data(format!(
                "section cy {} outside range {}..={}",
                section.cy, column.min_section_y, column.max_section_y
            ));
        }
        if let Some(p) = prev {
            if section.cy <= p {
                return invalid_data(format!(
                    "section order not strictly ascending: prev={}, current={}",
                    p, section.cy
                ));
            }
        }
        if !seen.insert(section.cy) {
            return invalid_data(format!("duplicate section cy {}", section.cy));
        }
        match &section.blocks {
            EncodedBlocks::LegacyManifestIds(arr) => {
                if column.format_version != COLUMN_PAYLOAD_FORMAT_VERSION_V1 {
                    return invalid_data(
                        "legacy manifest-id block encoding is only valid for v1 columns",
                    );
                }
                let _ = arr.decode_values()?;
            }
            EncodedBlocks::NamePaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            } => {
                if column.format_version == COLUMN_PAYLOAD_FORMAT_VERSION_V1 {
                    return invalid_data("name-palette block encoding is not valid for v1 columns");
                }
                validate_name_palette_blocks(palette, *bits_per_entry, packed_words)?;
                let _ = unpack_indices_u32(*bits_per_entry, packed_words, palette.len())?;
            }
        }
        let _ = section.data.decode_values()?;
        prev = Some(section.cy);
    }

    Ok(())
}

fn write_encoded_array(out: &mut Vec<u8>, arr: &EncodedArray) -> io::Result<()> {
    match arr {
        EncodedArray::Single(value) => {
            out.push(0);
            out.push(*value);
            Ok(())
        }
        EncodedArray::PaletteBitpacked {
            palette,
            bits_per_entry,
            packed_words,
        } => {
            if palette.is_empty() || palette.len() > 256 {
                return invalid_data(format!("invalid palette length {}", palette.len()));
            }
            if *bits_per_entry == 0 || *bits_per_entry > 8 {
                return invalid_data(format!(
                    "invalid bits_per_entry {} (expected 1..=8)",
                    bits_per_entry
                ));
            }
            let min_bits = ceil_log2_u16(palette.len() as u16).max(1);
            if *bits_per_entry < min_bits {
                return invalid_data(format!(
                    "bits_per_entry {} too small for palette length {}",
                    bits_per_entry,
                    palette.len()
                ));
            }
            let expected_words = expected_word_count(*bits_per_entry);
            if packed_words.len() != expected_words {
                return invalid_data(format!(
                    "invalid packed_words length: expected {}, got {}",
                    expected_words,
                    packed_words.len()
                ));
            }

            out.push(1);
            out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
            out.push(*bits_per_entry);
            out.extend_from_slice(palette);
            out.extend_from_slice(&(packed_words.len() as u16).to_le_bytes());
            for &word in packed_words {
                out.extend_from_slice(&word.to_le_bytes());
            }
            Ok(())
        }
    }
}

fn write_encoded_blocks_v2(out: &mut Vec<u8>, blocks: &EncodedBlocks) -> io::Result<()> {
    let EncodedBlocks::NamePaletteBitpacked {
        palette,
        bits_per_entry,
        packed_words,
    } = blocks
    else {
        return invalid_data("attempted to write legacy block encoding as v2");
    };
    validate_name_palette_blocks(palette, *bits_per_entry, packed_words)?;

    out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
    for name in palette {
        let bytes = name.as_bytes();
        let name_len = u16::try_from(bytes.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("palette string too long: {} bytes", bytes.len()),
            )
        })?;
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out.push(*bits_per_entry);
    out.extend_from_slice(&(packed_words.len() as u16).to_le_bytes());
    for &word in packed_words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    Ok(())
}

fn read_encoded_blocks_v2(rd: &mut ReadCursor<'_>) -> io::Result<EncodedBlocks> {
    let palette_len = rd.read_u16_le()? as usize;
    if !(1..=SECTION_BLOCK_COUNT).contains(&palette_len) {
        return invalid_data(format!(
            "invalid v2 block palette length {} (expected 1..={})",
            palette_len, SECTION_BLOCK_COUNT
        ));
    }
    let mut palette = Vec::with_capacity(palette_len);
    for _ in 0..palette_len {
        let name_len = rd.read_u16_le()? as usize;
        if name_len == 0 {
            return invalid_data("empty palette block name");
        }
        let bytes = rd.read_exact(name_len)?;
        let name = std::str::from_utf8(bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        palette.push(name.to_string());
    }
    let bits_per_entry = rd.read_u8()?;
    let packed_word_count = rd.read_u16_le()? as usize;
    let expected_words = expected_word_count(bits_per_entry);
    if packed_word_count != expected_words {
        return invalid_data(format!(
            "invalid v2 packed word count: expected {}, got {}",
            expected_words, packed_word_count
        ));
    }
    let mut packed_words = Vec::with_capacity(packed_word_count);
    for _ in 0..packed_word_count {
        packed_words.push(rd.read_u64_le()?);
    }
    validate_name_palette_blocks(&palette, bits_per_entry, &packed_words)?;
    Ok(EncodedBlocks::NamePaletteBitpacked {
        palette,
        bits_per_entry,
        packed_words,
    })
}

fn validate_name_palette_blocks(
    palette: &[String],
    bits_per_entry: u8,
    packed_words: &[u64],
) -> io::Result<()> {
    if palette.is_empty() || palette.len() > SECTION_BLOCK_COUNT {
        return invalid_data(format!("invalid v2 block palette length {}", palette.len()));
    }
    if bits_per_entry == 0 || bits_per_entry > 16 {
        return invalid_data(format!(
            "invalid v2 bits_per_entry {} (expected 1..=16)",
            bits_per_entry
        ));
    }
    if bits_per_entry < ceil_log2_u32(palette.len() as u32).max(1) {
        return invalid_data(format!(
            "v2 bits_per_entry {} too small for palette length {}",
            bits_per_entry,
            palette.len()
        ));
    }
    let expected_words = expected_word_count(bits_per_entry);
    if packed_words.len() != expected_words {
        return invalid_data(format!(
            "invalid v2 packed_words length: expected {}, got {}",
            expected_words,
            packed_words.len()
        ));
    }
    if let Some(empty_idx) = palette.iter().position(|s| s.is_empty()) {
        return invalid_data(format!("empty block name at palette index {}", empty_idx));
    }
    Ok(())
}

fn read_encoded_array(rd: &mut ReadCursor<'_>) -> io::Result<EncodedArray> {
    let kind = rd.read_u8()?;
    match kind {
        0 => {
            let value = rd.read_u8()?;
            Ok(EncodedArray::Single(value))
        }
        1 => {
            let palette_len = rd.read_u16_le()? as usize;
            if !(1..=256).contains(&palette_len) {
                return invalid_data(format!("invalid palette length {}", palette_len));
            }
            let bits_per_entry = rd.read_u8()?;
            let palette = rd.read_exact(palette_len)?.to_vec();
            let word_count = rd.read_u16_le()? as usize;
            let expected_words = expected_word_count(bits_per_entry);
            if word_count != expected_words {
                return invalid_data(format!(
                    "invalid packed word count: expected {}, got {}",
                    expected_words, word_count
                ));
            }
            let mut packed_words = Vec::with_capacity(word_count);
            for _ in 0..word_count {
                packed_words.push(rd.read_u64_le()?);
            }
            Ok(EncodedArray::PaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            })
        }
        other => invalid_data(format!("unknown EncodedArray kind {}", other)),
    }
}

fn ceil_log2_u16(n: u16) -> u8 {
    debug_assert!(n > 0);
    if n <= 1 {
        0
    } else {
        (u16::BITS - (n - 1).leading_zeros()) as u8
    }
}

fn ceil_log2_u32(n: u32) -> u8 {
    debug_assert!(n > 0);
    if n <= 1 {
        0
    } else {
        (u32::BITS - (n - 1).leading_zeros()) as u8
    }
}

fn expected_word_count(bits_per_entry: u8) -> usize {
    let total_bits = SECTION_BLOCK_COUNT * (bits_per_entry as usize);
    total_bits.div_ceil(64)
}

fn pack_indices(indices: &[u16], bits_per_entry: u8) -> io::Result<Vec<u64>> {
    if indices.len() != SECTION_BLOCK_COUNT {
        return invalid_data(format!(
            "index count mismatch: expected {}, got {}",
            SECTION_BLOCK_COUNT,
            indices.len()
        ));
    }
    if bits_per_entry == 0 || bits_per_entry > 8 {
        return invalid_data(format!(
            "invalid bits_per_entry {} (expected 1..=8)",
            bits_per_entry
        ));
    }

    let mut out = vec![0u64; expected_word_count(bits_per_entry)];
    let max_index = (1u16 << bits_per_entry) - 1;
    for (i, &index) in indices.iter().enumerate() {
        if index > max_index {
            return invalid_data(format!(
                "index {} exceeds max representable value {} for {} bits",
                index, max_index, bits_per_entry
            ));
        }
        let bit_pos = i * (bits_per_entry as usize);
        let word_index = bit_pos / 64;
        let bit_offset = bit_pos % 64;
        out[word_index] |= (index as u64) << bit_offset;
        if bit_offset + (bits_per_entry as usize) > 64 {
            out[word_index + 1] |= (index as u64) >> (64 - bit_offset);
        }
    }
    Ok(out)
}

fn pack_indices_u32(indices: &[u32], bits_per_entry: u8) -> io::Result<Vec<u64>> {
    if indices.len() != SECTION_BLOCK_COUNT {
        return invalid_data(format!(
            "index count mismatch: expected {}, got {}",
            SECTION_BLOCK_COUNT,
            indices.len()
        ));
    }
    if bits_per_entry == 0 || bits_per_entry > 32 {
        return invalid_data(format!(
            "invalid bits_per_entry {} (expected 1..=32)",
            bits_per_entry
        ));
    }

    let mut out = vec![0u64; expected_word_count(bits_per_entry)];
    let max_index = if bits_per_entry == 32 {
        u32::MAX
    } else {
        (1u32 << bits_per_entry) - 1
    };
    for (i, &index) in indices.iter().enumerate() {
        if index > max_index {
            return invalid_data(format!(
                "index {} exceeds max representable value {} for {} bits",
                index, max_index, bits_per_entry
            ));
        }
        let bit_pos = i * (bits_per_entry as usize);
        let word_index = bit_pos / 64;
        let bit_offset = bit_pos % 64;
        out[word_index] |= (index as u64) << bit_offset;
        if bit_offset + (bits_per_entry as usize) > 64 {
            out[word_index + 1] |= (index as u64) >> (64 - bit_offset);
        }
    }
    Ok(out)
}

fn unpack_indices_u32(
    bits_per_entry: u8,
    packed_words: &[u64],
    palette_len: usize,
) -> io::Result<Vec<u32>> {
    if palette_len == 0 || palette_len > SECTION_BLOCK_COUNT {
        return invalid_data(format!("invalid palette_len {}", palette_len));
    }
    if bits_per_entry == 0 || bits_per_entry > 16 {
        return invalid_data(format!(
            "invalid bits_per_entry {} (expected 1..=16)",
            bits_per_entry
        ));
    }
    let min_bits = ceil_log2_u32(palette_len as u32).max(1);
    if bits_per_entry < min_bits {
        return invalid_data(format!(
            "bits_per_entry {} too small for palette_len {}",
            bits_per_entry, palette_len
        ));
    }
    let expected_words = expected_word_count(bits_per_entry);
    if packed_words.len() != expected_words {
        return invalid_data(format!(
            "invalid packed_words length: expected {}, got {}",
            expected_words,
            packed_words.len()
        ));
    }
    let mut out = Vec::with_capacity(SECTION_BLOCK_COUNT);
    let mask = (1u64 << bits_per_entry) - 1;
    for i in 0..SECTION_BLOCK_COUNT {
        let bit_pos = i * (bits_per_entry as usize);
        let word_index = bit_pos / 64;
        let bit_offset = bit_pos % 64;
        let mut raw = packed_words[word_index] >> bit_offset;
        if bit_offset + (bits_per_entry as usize) > 64 {
            raw |= packed_words[word_index + 1] << (64 - bit_offset);
        }
        let index = (raw & mask) as usize;
        if index >= palette_len {
            return invalid_data(format!(
                "v2 palette index out of range: idx={} palette_len={}",
                index, palette_len
            ));
        }
        out.push(index as u32);
    }
    Ok(out)
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg() & 0xEDB8_8320;
            crc = (crc >> 1) ^ mask;
        }
    }
    !crc
}

fn invalid_data<T>(msg: impl Into<String>) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, msg.into()))
}

struct ReadCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ReadCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn offset(&self) -> usize {
        self.offset
    }

    fn remaining_len(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn is_at_end(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn read_u8(&mut self) -> io::Result<u8> {
        let bytes = self.read_exact(1)?;
        Ok(bytes[0])
    }

    fn read_u16_le(&mut self) -> io::Result<u16> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32_le(&mut self) -> io::Result<u32> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i32_le(&mut self) -> io::Result<i32> {
        let bytes = self.read_exact(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64_le(&mut self) -> io::Result<u64> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_exact(&mut self, len: usize) -> io::Result<&'a [u8]> {
        let end = self.offset.checked_add(len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "column payload cursor overflow")
        })?;
        if end > self.bytes.len() {
            return invalid_data(format!(
                "unexpected EOF while reading column payload: need {} bytes, have {}",
                len,
                self.remaining_len()
            ));
        }
        let out = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{SECTION_BLOCK_COUNT, apply_column, decode_column, encode_blob, encode_column};
    use crate::blocks::BlockId;
    use crate::world::{CHUNK_SIZE, ChunkCoord, World};
    use glam::IVec3;
    use std::io;

    #[test]
    fn random_column_sections_roundtrip_preserves_all_4096_entries() {
        let mut world = World::new();
        let mut rng = TestRng::new(0xC0DE_600D_A11C_E5E5);
        let cx = -12;
        let cz = 34;
        for _ in 0..(SECTION_BLOCK_COUNT * 2) {
            let x = rng.range_i32(0, CHUNK_SIZE - 1);
            let z = rng.range_i32(0, CHUNK_SIZE - 1);
            let y = rng.range_i32(world.min_y(), world.max_y());
            let wx = cx * CHUNK_SIZE + x;
            let wz = cz * CHUNK_SIZE + z;
            world
                .set_block_and_data(
                    IVec3::new(wx, y, wz),
                    random_block_id(&mut rng),
                    rng.next_u8(),
                )
                .unwrap();
        }

        let encoded = encode_column(&world, cx, cz).unwrap();
        let decoded = decode_column(&encoded).unwrap();
        assert_eq!(decoded.cx, cx);
        assert_eq!(decoded.cz, cz);
        assert_eq!(decoded.format_version, super::COLUMN_PAYLOAD_FORMAT_VERSION);
        let reencoded = encode_blob(&decoded).unwrap();
        let redecode = decode_column(&reencoded).unwrap();
        assert_eq!(redecode, decoded);
    }

    #[test]
    fn decode_column_reports_crc_mismatch_after_payload_corruption() {
        let mut world = World::new();
        let (cx, cz) = (4, -2);
        let mut rng = TestRng::new(0xDEAD_BEEF_1234_5678);

        for _ in 0..512 {
            let x = rng.range_i32(0, CHUNK_SIZE - 1);
            let y = rng.range_i32(world.min_y(), world.max_y());
            let z = rng.range_i32(0, CHUNK_SIZE - 1);
            let wx = cx * CHUNK_SIZE + x;
            let wz = cz * CHUNK_SIZE + z;
            world
                .set_block_and_data(
                    IVec3::new(wx, y, wz),
                    random_block_id(&mut rng),
                    rng.next_u8(),
                )
                .unwrap();
        }

        let mut encoded = encode_column(&world, cx, cz).unwrap();
        let corrupt_idx = encoded.len() - 5; // last payload byte before CRC32 trailer
        encoded[corrupt_idx] ^= 0x01;

        let err = decode_column(&encoded).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("CRC32 mismatch"),
            "expected CRC32 mismatch, got: {err}"
        );
    }

    #[test]
    fn encode_decode_encode_column_is_bit_exact() {
        let mut world = World::new();
        let (cx, cz) = (-3, 7);

        // Section y=0
        set(world_mut(&mut world), cx, 0, cz, 0, 0, 0, BlockId::Stone, 5);
        set(world_mut(&mut world), cx, 0, cz, 1, 0, 0, BlockId::Grass, 9);
        set(
            world_mut(&mut world),
            cx,
            0,
            cz,
            2,
            0,
            0,
            BlockId::WaterStill,
            13,
        );
        // Section y=3
        set(
            world_mut(&mut world),
            cx,
            3,
            cz,
            4,
            5,
            6,
            BlockId::Obsidian,
            42,
        );
        set(
            world_mut(&mut world),
            cx,
            3,
            cz,
            15,
            15,
            15,
            BlockId::Sand,
            7,
        );

        let bytes1 = encode_column(&world, cx, cz).unwrap();
        let blob = decode_column(&bytes1).unwrap();
        let bytes2 = encode_blob(&blob).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn apply_column_restores_exact_blocks_and_data() {
        let mut src = World::new();
        let (cx, cz) = (2, -1);

        set(world_mut(&mut src), cx, 0, cz, 0, 0, 0, BlockId::Stone, 17);
        set(world_mut(&mut src), cx, 0, cz, 5, 2, 3, BlockId::Dirt, 99);
        set(
            world_mut(&mut src),
            cx,
            1,
            cz,
            2,
            7,
            8,
            BlockId::OakPlanks,
            3,
        );
        set(
            world_mut(&mut src),
            cx,
            15,
            cz,
            9,
            1,
            10,
            BlockId::LavaStill,
            12,
        );

        let encoded = encode_column(&src, cx, cz).unwrap();
        let blob = decode_column(&encoded).unwrap();

        let mut dst = World::new();
        // Add stale data that must be removed because the column blob is authoritative.
        set(
            world_mut(&mut dst),
            cx,
            4,
            cz,
            1,
            1,
            1,
            BlockId::Cobblestone,
            55,
        );
        apply_column(&mut dst, blob).unwrap();

        for cy in dst.min_chunk_y()..=dst.max_chunk_y() {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            if let Some(chunk) = dst.get_chunk(coord) {
                assert_eq!(chunk.revision, 0);
                assert_eq!(chunk.light.len(), 16 * 16 * 16);
            }
        }

        for y in src.min_y()..=src.max_y() {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let wx = cx * CHUNK_SIZE + x;
                    let wz = cz * CHUNK_SIZE + z;
                    let pos = IVec3::new(wx, y, wz);
                    assert_eq!(
                        dst.get_block(pos),
                        src.get_block(pos),
                        "block mismatch at {pos:?}"
                    );
                    assert_eq!(
                        dst.get_block_data(pos),
                        src.get_block_data(pos),
                        "data mismatch at {pos:?}"
                    );
                }
            }
        }
    }

    /// Writes a v1 column payload. Production only ever emits v2, so without a
    /// writer here the v1 branch of `decode_column` has no coverage at all and
    /// could rot unnoticed until someone opens a pre-name-palette save.
    fn encode_blob_v1(column: &super::ColumnBlob) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(&super::COLUMN_PAYLOAD_MAGIC);
        out.extend_from_slice(&super::COLUMN_PAYLOAD_FORMAT_VERSION_V1.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&column.cx.to_le_bytes());
        out.extend_from_slice(&column.cz.to_le_bytes());
        out.extend_from_slice(&column.min_y.to_le_bytes());
        out.extend_from_slice(&column.max_y.to_le_bytes());
        out.extend_from_slice(&column.sea_level.to_le_bytes());
        out.extend_from_slice(&column.chunk_size.to_le_bytes());
        out.extend_from_slice(&column.min_section_y.to_le_bytes());
        out.extend_from_slice(&column.max_section_y.to_le_bytes());
        out.extend_from_slice(&(column.sections.len() as u16).to_le_bytes());
        out.extend_from_slice(&super::COLUMN_RESERVED0_V1.to_le_bytes());

        for section in &column.sections {
            let super::EncodedBlocks::LegacyManifestIds(blocks) = &section.blocks else {
                panic!("v1 sections carry manifest ids, not a name palette");
            };
            out.extend_from_slice(&section.cy.to_le_bytes());
            out.push(super::SECTION_FLAGS_V1);
            out.extend_from_slice(&[0, 0, 0]);
            super::write_encoded_array(&mut out, blocks)?;
            super::write_encoded_array(&mut out, &section.data)?;
        }

        let crc = super::crc32_ieee(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(out)
    }

    /// One-section v1 column sized to `world`, so `validate_column_for_world`
    /// accepts it on apply.
    fn v1_column(
        world: &World,
        cx: i32,
        cz: i32,
        cy: i32,
        manifest_ids: &[u8],
        data: &[u8],
    ) -> super::ColumnBlob {
        super::ColumnBlob {
            format_version: super::COLUMN_PAYLOAD_FORMAT_VERSION_V1,
            cx,
            cz,
            min_y: world.min_y(),
            max_y: world.max_y(),
            sea_level: world.sea_level(),
            chunk_size: world.chunk_size(),
            min_section_y: world.min_chunk_y(),
            max_section_y: world.max_chunk_y(),
            sections: vec![super::SectionBlob {
                cy,
                blocks: super::EncodedBlocks::LegacyManifestIds(
                    super::EncodedArray::from_values(manifest_ids).unwrap(),
                ),
                data: super::EncodedArray::from_values(data).unwrap(),
            }],
        }
    }

    #[test]
    fn v1_columns_still_decode_and_map_through_the_manifest() {
        let world = World::new();
        let (cx, cz, cy) = (3, -7, 2);

        // v1 stored one manifest id per voxel. `build_from_registry` pins ids
        // 0..=255 to the matching runtime ids, so a fresh world round-trips the
        // legacy bytes unchanged — that identity is the compat guarantee.
        let mut manifest_ids = vec![BlockId::Air.to_u8(); SECTION_BLOCK_COUNT];
        let mut data = vec![0u8; SECTION_BLOCK_COUNT];
        let placed = [
            (0, 0, 0, BlockId::Stone, 7u8),
            (5, 2, 3, BlockId::Dirt, 42),
            (15, 15, 15, BlockId::OakPlanks, 0),
        ];
        for &(x, y, z, block, value) in &placed {
            let idx = crate::world::Chunk::index(x, y, z);
            manifest_ids[idx] = block.to_u8();
            data[idx] = value;
        }

        let bytes = encode_blob_v1(&v1_column(&world, cx, cz, cy, &manifest_ids, &data)).unwrap();
        let decoded = decode_column(&bytes).unwrap();
        assert_eq!(
            decoded.format_version,
            super::COLUMN_PAYLOAD_FORMAT_VERSION_V1
        );
        assert!(matches!(
            decoded.sections[0].blocks,
            super::EncodedBlocks::LegacyManifestIds(_)
        ));

        let mut dst = World::new();
        apply_column(&mut dst, decoded).unwrap();
        for &(x, y, z, block, value) in &placed {
            let pos = IVec3::new(
                cx * CHUNK_SIZE + x,
                cy * CHUNK_SIZE + y,
                cz * CHUNK_SIZE + z,
            );
            assert_eq!(dst.get_block(pos), block, "block mismatch at {pos:?}");
            assert_eq!(dst.get_block_data(pos), value, "data mismatch at {pos:?}");
        }
    }

    #[test]
    fn v1_columns_reject_v2_section_flags() {
        let world = World::new();
        let blocks = vec![BlockId::Stone.to_u8(); SECTION_BLOCK_COUNT];
        let data = vec![0u8; SECTION_BLOCK_COUNT];
        let mut bytes = encode_blob_v1(&v1_column(&world, 0, 0, 0, &blocks, &data)).unwrap();

        // Section flags sit right after the 44-byte header plus `i32 cy`.
        let flags_offset = 44 + 4;
        assert_eq!(bytes[flags_offset], super::SECTION_FLAGS_V1);
        bytes[flags_offset] = super::SECTION_FLAGS_V2;
        refresh_crc(&mut bytes);

        let err = decode_column(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("v1 section flags"),
            "expected a v1 section-flag rejection, got: {err}"
        );
    }

    #[test]
    fn v2_columns_reject_v1_section_flags() {
        let mut world = World::new();
        set(world_mut(&mut world), 0, 0, 0, 1, 1, 1, BlockId::Stone, 0);
        let mut bytes = encode_column(&world, 0, 0).unwrap();

        let flags_offset = 44 + 4;
        assert_eq!(bytes[flags_offset], super::SECTION_FLAGS_V2);
        bytes[flags_offset] = super::SECTION_FLAGS_V1;
        refresh_crc(&mut bytes);

        let err = decode_column(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("section flags for v2 payload"),
            "expected a v2 section-flag rejection, got: {err}"
        );
    }

    #[test]
    fn decode_column_rejects_header_signature_drift() {
        let mut world = World::new();
        set(world_mut(&mut world), 0, 0, 0, 2, 3, 4, BlockId::Dirt, 1);
        let good = encode_column(&world, 0, 0).unwrap();

        // Every guarded header field, with the CRC refreshed so the payload
        // fails on the field itself rather than on the checksum.
        let cases: [(usize, &[u8], &str); 4] = [
            (0, b"MKXX", "magic"),
            (4, &3u16.to_le_bytes(), "format_version"),
            (6, &1u16.to_le_bytes(), "flags"),
            (42, &1u16.to_le_bytes(), "reserved0"),
        ];
        for (offset, patch, expected) in cases {
            let mut bytes = good.clone();
            bytes[offset..offset + patch.len()].copy_from_slice(patch);
            refresh_crc(&mut bytes);
            let err = decode_column(&bytes).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert!(
                err.to_string().contains(expected),
                "offset {offset}: expected an error naming '{expected}', got: {err}"
            );
        }
    }

    #[test]
    fn decode_column_rejects_truncated_and_padded_payloads() {
        let mut world = World::new();
        set(world_mut(&mut world), 0, 0, 0, 6, 6, 6, BlockId::Cobblestone, 9);
        let good = encode_column(&world, 0, 0).unwrap();

        let truncated = &good[..good.len() - 8];
        assert_eq!(
            decode_column(truncated).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let mut padded = good.clone();
        padded.push(0);
        let err = decode_column(&padded).unwrap_err();
        assert!(
            err.to_string().contains("trailing bytes"),
            "expected a trailing-byte rejection, got: {err}"
        );
    }

    #[test]
    fn unknown_block_names_decode_to_air_instead_of_failing() {
        let world = World::new();
        // A section from a future/modded build: one known name, one this
        // registry has never heard of. Opening the save must not hard-fail.
        let mut indices = vec![0u32; SECTION_BLOCK_COUNT];
        let stone_idx = crate::world::Chunk::index(1, 1, 1);
        let ghost_idx = crate::world::Chunk::index(2, 1, 1);
        indices[stone_idx] = 1;
        indices[ghost_idx] = 2;

        let column = super::ColumnBlob {
            format_version: super::COLUMN_PAYLOAD_FORMAT_VERSION,
            cx: 0,
            cz: 0,
            min_y: world.min_y(),
            max_y: world.max_y(),
            sea_level: world.sea_level(),
            chunk_size: world.chunk_size(),
            min_section_y: world.min_chunk_y(),
            max_section_y: world.max_chunk_y(),
            sections: vec![super::SectionBlob {
                cy: 0,
                blocks: super::EncodedBlocks::NamePaletteBitpacked {
                    palette: vec![
                        "core:air".to_string(),
                        "core:stone".to_string(),
                        "somemod:unobtainium".to_string(),
                    ],
                    bits_per_entry: 2,
                    packed_words: super::pack_indices_u32(&indices, 2).unwrap(),
                },
                data: super::EncodedArray::from_values(&vec![0u8; SECTION_BLOCK_COUNT]).unwrap(),
            }],
        };

        let bytes = encode_blob(&column).unwrap();
        let decoded = decode_column(&bytes).unwrap();
        let mut dst = World::new();
        apply_column(&mut dst, decoded).unwrap();

        assert_eq!(dst.get_block(IVec3::new(1, 1, 1)), BlockId::Stone);
        assert_eq!(
            dst.get_block(IVec3::new(2, 1, 1)),
            BlockId::Air,
            "an unknown palette name must degrade to air, not to a neighbouring id"
        );
    }

    /// Rewrites the CRC32 trailer so a patched payload fails on the field under
    /// test instead of on the checksum.
    fn refresh_crc(bytes: &mut [u8]) {
        let len = bytes.len();
        let crc = super::crc32_ieee(&bytes[..len - 4]);
        bytes[len - 4..].copy_from_slice(&crc.to_le_bytes());
    }

    fn world_mut(world: &mut World) -> &mut World {
        world
    }

    fn set(
        world: &mut World,
        cx: i32,
        cy: i32,
        cz: i32,
        x: i32,
        y: i32,
        z: i32,
        block: BlockId,
        data: u8,
    ) {
        let wx = cx * CHUNK_SIZE + x;
        let wy = cy * CHUNK_SIZE + y;
        let wz = cz * CHUNK_SIZE + z;
        world
            .set_block_and_data(IVec3::new(wx, wy, wz), block, data)
            .unwrap();
    }

    fn random_block_id(rng: &mut TestRng) -> BlockId {
        loop {
            if let Ok(id) = BlockId::from_u8(rng.next_u8() % 14) {
                return id;
            }
        }
    }

    struct TestRng {
        state: u64,
    }

    impl TestRng {
        fn new(seed: u64) -> Self {
            Self { state: seed.max(1) }
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.state = x;
            x
        }

        fn next_u8(&mut self) -> u8 {
            self.next_u64() as u8
        }

        fn range_i32(&mut self, lo: i32, hi: i32) -> i32 {
            debug_assert!(lo <= hi);
            let span = (hi - lo + 1) as u64;
            lo + (self.next_u64() % span) as i32
        }

        fn range_usize(&mut self, lo: usize, hi: usize) -> usize {
            debug_assert!(lo <= hi);
            let span = (hi - lo + 1) as u64;
            lo + (self.next_u64() % span) as usize
        }
    }
}

/// Measures what the v2 name palette actually costs on disk.
///
/// The open question in `docs/TODO.md` is whether a more compact column layout
/// is worth designing. Raw payload sizes cannot answer that: region files store
/// zlib-compressed payloads, and a per-section palette of repeated block names
/// is exactly the input deflate is best at. Run the report with
/// `cargo test storage::chunk_codec::v2_size_report -- --nocapture`.
#[cfg(test)]
mod v2_size_report {
    use super::{ColumnBlob, EncodedArray, EncodedBlocks, decode_column, encode_column};
    use crate::blocks::BlockRegistry;
    use crate::world::{CHUNK_SIZE, World, WorldConfig};
    use crate::worldgen::WorldgenPreset;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    /// Byte split of one encoded v2 payload, mirroring the writers in this
    /// module. Kept as counts rather than a parser so it stays in step with
    /// `encode_blob` by construction.
    #[derive(Clone, Copy, Debug, Default)]
    struct Breakdown {
        total: usize,
        header: usize,
        section_headers: usize,
        palette_names: usize,
        block_indices: usize,
        block_data: usize,
        crc: usize,
        sections: usize,
        palette_entries: usize,
    }

    impl Breakdown {
        fn add(&mut self, other: &Breakdown) {
            self.total += other.total;
            self.header += other.header;
            self.section_headers += other.section_headers;
            self.palette_names += other.palette_names;
            self.block_indices += other.block_indices;
            self.block_data += other.block_data;
            self.crc += other.crc;
            self.sections += other.sections;
            self.palette_entries += other.palette_entries;
        }
    }

    const COLUMN_HEADER_BYTES: usize = 44;
    const SECTION_HEADER_BYTES: usize = 8;
    const CRC_BYTES: usize = 4;

    fn encoded_array_bytes(arr: &EncodedArray) -> usize {
        match arr {
            EncodedArray::Single(_) => 2,
            EncodedArray::PaletteBitpacked {
                palette,
                packed_words,
                ..
            } => 1 + 2 + 1 + palette.len() + 2 + packed_words.len() * 8,
        }
    }

    fn breakdown(column: &ColumnBlob) -> Breakdown {
        let mut out = Breakdown {
            total: 0,
            header: COLUMN_HEADER_BYTES,
            crc: CRC_BYTES,
            sections: column.sections.len(),
            ..Breakdown::default()
        };

        for section in &column.sections {
            out.section_headers += SECTION_HEADER_BYTES;
            match &section.blocks {
                EncodedBlocks::NamePaletteBitpacked {
                    palette,
                    packed_words,
                    ..
                } => {
                    out.palette_entries += palette.len();
                    out.palette_names +=
                        2 + palette.iter().map(|name| 2 + name.len()).sum::<usize>();
                    out.block_indices += 1 + 2 + packed_words.len() * 8;
                }
                EncodedBlocks::LegacyManifestIds(arr) => {
                    out.block_indices += encoded_array_bytes(arr);
                }
            }
            out.block_data += encoded_array_bytes(&section.data);
        }

        out.total = out.header
            + out.section_headers
            + out.palette_names
            + out.block_indices
            + out.block_data
            + out.crc;
        out
    }

    /// The realistic v3 candidate: the same payload with every palette *name*
    /// replaced by a `u16` block id. Only the size matters here, so this writes
    /// the delta rather than a decodable format. Header and CRC are copied from
    /// the real payload so the palette encoding is the *only* difference the
    /// compressor sees — otherwise zeroed filler would fake a saving.
    fn compact_id_palette_bytes(
        encoded: &[u8],
        column: &ColumnBlob,
        reg: &BlockRegistry,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(encoded.len());
        out.extend_from_slice(&encoded[..COLUMN_HEADER_BYTES]);
        for section in &column.sections {
            out.extend_from_slice(&section.cy.to_le_bytes());
            out.push(super::SECTION_FLAGS_V2);
            out.extend_from_slice(&[0, 0, 0]);
            if let EncodedBlocks::NamePaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            } = &section.blocks
            {
                out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
                for name in palette {
                    let id = reg.resolve_fast(name).map(|b| b.to_u8()).unwrap_or(0);
                    out.extend_from_slice(&u16::from(id).to_le_bytes());
                }
                out.push(*bits_per_entry);
                out.extend_from_slice(&(packed_words.len() as u16).to_le_bytes());
                for &word in packed_words {
                    out.extend_from_slice(&word.to_le_bytes());
                }
            }
            match &section.data {
                EncodedArray::Single(value) => {
                    out.push(0);
                    out.push(*value);
                }
                EncodedArray::PaletteBitpacked {
                    palette,
                    bits_per_entry,
                    packed_words,
                } => {
                    out.push(1);
                    out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
                    out.push(*bits_per_entry);
                    out.extend_from_slice(palette);
                    out.extend_from_slice(&(packed_words.len() as u16).to_le_bytes());
                    for &word in packed_words {
                        out.extend_from_slice(&word.to_le_bytes());
                    }
                }
            }
        }
        out.extend_from_slice(&encoded[encoded.len() - CRC_BYTES..]);
        out
    }

    /// The conservative v3 candidate: keep block *names* — and with them the
    /// self-describing decode v2 exists for — but hoist one palette to the
    /// column so the same names stop repeating in every section. Sections then
    /// index the column palette, which costs a bit or two more per voxel.
    fn column_palette_bytes(encoded: &[u8], column: &ColumnBlob) -> Vec<u8> {
        let mut names: Vec<&str> = Vec::new();
        for section in &column.sections {
            if let EncodedBlocks::NamePaletteBitpacked { palette, .. } = &section.blocks {
                for name in palette {
                    if !names.contains(&name.as_str()) {
                        names.push(name.as_str());
                    }
                }
            }
        }
        let column_bits = super::ceil_log2_u32(names.len() as u32).max(1);

        let mut out = Vec::with_capacity(encoded.len());
        out.extend_from_slice(&encoded[..COLUMN_HEADER_BYTES]);
        out.extend_from_slice(&(names.len() as u16).to_le_bytes());
        for name in &names {
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }

        for section in &column.sections {
            out.extend_from_slice(&section.cy.to_le_bytes());
            out.push(super::SECTION_FLAGS_V2);
            out.extend_from_slice(&[0, 0, 0]);
            if let EncodedBlocks::NamePaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            } = &section.blocks
            {
                let local = super::unpack_indices_u32(*bits_per_entry, packed_words, palette.len())
                    .expect("section indices must unpack");
                let remapped: Vec<u32> = local
                    .iter()
                    .map(|&idx| {
                        names
                            .iter()
                            .position(|name| *name == palette[idx as usize].as_str())
                            .expect("name must be in the column palette") as u32
                    })
                    .collect();
                let words =
                    super::pack_indices_u32(&remapped, column_bits).expect("repack must succeed");
                out.push(column_bits);
                out.extend_from_slice(&(words.len() as u16).to_le_bytes());
                for &word in &words {
                    out.extend_from_slice(&word.to_le_bytes());
                }
            }
            match &section.data {
                EncodedArray::Single(value) => {
                    out.push(0);
                    out.push(*value);
                }
                EncodedArray::PaletteBitpacked {
                    palette,
                    bits_per_entry,
                    packed_words,
                } => {
                    out.push(1);
                    out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
                    out.push(*bits_per_entry);
                    out.extend_from_slice(palette);
                    out.extend_from_slice(&(packed_words.len() as u16).to_le_bytes());
                    for &word in packed_words {
                        out.extend_from_slice(&word.to_le_bytes());
                    }
                }
            }
        }
        out.extend_from_slice(&encoded[encoded.len() - CRC_BYTES..]);
        out
    }

    /// Same settings as `region_file.rs`, so the numbers are on-disk sizes.
    fn zlib_len(bytes: &[u8]) -> usize {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("zlib write");
        encoder.finish().expect("zlib finish").len()
    }

    /// Signed change against the v2 baseline: positive saves bytes, negative
    /// costs them. `pct` would clamp a regression to zero and hide it.
    fn delta_pct(baseline: usize, candidate: usize) -> f64 {
        if baseline == 0 {
            0.0
        } else {
            (baseline as f64 - candidate as f64) * 100.0 / baseline as f64
        }
    }

    fn pct(part: usize, whole: usize) -> f64 {
        if whole == 0 {
            0.0
        } else {
            part as f64 * 100.0 / whole as f64
        }
    }

    #[test]
    fn v2_name_palette_overhead_report() {
        let presets = [
            ("flatish", WorldgenPreset::Flatish),
            ("classic_flatter", WorldgenPreset::ClassicFlatter),
            ("rd161348_like", WorldgenPreset::Rd161348Like),
            ("hilly", WorldgenPreset::Hilly),
        ];
        let reg = BlockRegistry::new();

        let mut all = Breakdown::default();
        let mut all_raw_z = 0usize;
        let mut all_compact_z = 0usize;
        let mut all_colpal_z = 0usize;
        let mut all_columns = 0usize;

        println!(
            "\n{:<16} {:>4} {:>9} {:>9} {:>7} {:>9} {:>9} {:>7}",
            "preset", "cols", "raw", "names", "names%", "zlib", "zlib_id", "zlib_col"
        );

        for (label, preset) in presets {
            let mut world = World::new();
            world.configure_world_config(WorldConfig::default(), preset);

            let mut agg = Breakdown::default();
            let mut raw_z = 0usize;
            let mut compact_z = 0usize;
            let mut colpal_z = 0usize;
            let mut columns = 0usize;

            // A 4x4 patch of columns, offset per preset so they do not all sample
            // the same terrain features.
            for cx in 0..4 {
                for cz in 0..4 {
                    let (wx, wz) = ((cx * CHUNK_SIZE) + 512, (cz * CHUNK_SIZE) - 512);
                    world.ensure_column_generated(wx, wz);
                    let (col_x, col_z) = (wx.div_euclid(CHUNK_SIZE), wz.div_euclid(CHUNK_SIZE));
                    let bytes = encode_column(&world, col_x, col_z).expect("encode column");
                    let blob = decode_column(&bytes).expect("decode column");
                    assert_eq!(
                        breakdown(&blob).total,
                        bytes.len(),
                        "{label}: breakdown must account for every encoded byte"
                    );

                    agg.add(&breakdown(&blob));
                    raw_z += zlib_len(&bytes);
                    compact_z += zlib_len(&compact_id_palette_bytes(&bytes, &blob, &reg));
                    colpal_z += zlib_len(&column_palette_bytes(&bytes, &blob));
                    columns += 1;
                }
            }

            println!(
                "{:<16} {:>4} {:>9} {:>9} {:>6.1}% {:>9} {:>9} {:>9}",
                label,
                columns,
                agg.total,
                agg.palette_names,
                pct(agg.palette_names, agg.total),
                raw_z,
                compact_z,
                colpal_z,
            );

            all.add(&agg);
            all_raw_z += raw_z;
            all_compact_z += compact_z;
            all_colpal_z += colpal_z;
            all_columns += columns;
        }

        let sections = all.sections.max(1);
        println!(
            "\ntotals: {} columns, {} sections, {:.1} palette entries/section",
            all_columns,
            all.sections,
            all.palette_entries as f64 / sections as f64
        );
        println!(
            "raw split: header {:.1}% | section headers {:.1}% | palette names {:.1}% | block indices {:.1}% | block data {:.1}%",
            pct(all.header, all.total),
            pct(all.section_headers, all.total),
            pct(all.palette_names, all.total),
            pct(all.block_indices, all.total),
            pct(all.block_data, all.total),
        );
        println!(
            "on disk: v2 {} B zlib ({:.1}% of raw) | u16-id palette {} B ({:+.1}%) | column palette {} B ({:+.1}%)\n",
            all_raw_z,
            pct(all_raw_z, all.total),
            all_compact_z,
            delta_pct(all_raw_z, all_compact_z),
            all_colpal_z,
            delta_pct(all_raw_z, all_colpal_z),
        );

        // Worldgen columns use ~2 names per section. Player builds are the
        // expensive case for a per-section name palette, so bracket it: same
        // terrain, but every section seeded with many distinct block types.
        let mut built = World::new();
        let mut built_agg = Breakdown::default();
        let mut built_raw_z = 0usize;
        let mut built_compact_z = 0usize;
        let mut built_colpal_z = 0usize;
        let palette_blocks: Vec<crate::blocks::BlockId> = (0u16..=255)
            .filter_map(|raw| crate::blocks::BlockId::from_u8(raw as u8).ok())
            .collect();

        for cx in 0..4 {
            for cz in 0..4 {
                let (wx, wz) = (cx * CHUNK_SIZE, cz * CHUNK_SIZE);
                built.ensure_column_generated(wx, wz);
                for (i, &block) in palette_blocks.iter().enumerate() {
                    let x = wx + (i % CHUNK_SIZE as usize) as i32;
                    let z = wz + ((i / CHUNK_SIZE as usize) % CHUNK_SIZE as usize) as i32;
                    for y in (built.min_y() + 1..built.max_y()).step_by(7) {
                        let _ = built.set_block(glam::IVec3::new(x, y, z), block);
                    }
                }
                let (col_x, col_z) = (wx.div_euclid(CHUNK_SIZE), wz.div_euclid(CHUNK_SIZE));
                let bytes = encode_column(&built, col_x, col_z).expect("encode built column");
                let blob = decode_column(&bytes).expect("decode built column");
                built_agg.add(&breakdown(&blob));
                built_raw_z += zlib_len(&bytes);
                built_compact_z += zlib_len(&compact_id_palette_bytes(&bytes, &blob, &reg));
                built_colpal_z += zlib_len(&column_palette_bytes(&bytes, &blob));
            }
        }

        println!(
            "built worst case: {} sections, {:.1} palette entries/section, names {:.1}% of raw | zlib v2 {} B | u16-id {} B ({:+.1}%) | column palette {} B ({:+.1}%)\n",
            built_agg.sections,
            built_agg.palette_entries as f64 / built_agg.sections.max(1) as f64,
            pct(built_agg.palette_names, built_agg.total),
            built_raw_z,
            built_compact_z,
            delta_pct(built_raw_z, built_compact_z),
            built_colpal_z,
            delta_pct(built_raw_z, built_colpal_z),
        );

        // Budget guard, not a target: the palette is per section and its cost
        // grows with block variety, so a large jump means the encoder started
        // emitting near-unique palettes and the compaction item needs revisiting.
        let names_share = pct(all.palette_names, all.total);
        assert!(
            names_share < 10.0,
            "v2 name-palette overhead grew to {names_share:.1}% of the raw payload"
        );
        assert!(
            all_raw_z <= all.total,
            "zlib must not inflate column payloads"
        );
    }
}
