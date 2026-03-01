use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

use crate::blocks::{BlockId, BlockRegistry};
use crate::storage::format::{
    COLUMN_PAYLOAD_FORMAT_VERSION, COLUMN_PAYLOAD_MAGIC, SECTION_BLOCK_COUNT,
};
use crate::world::{Chunk, ChunkCoord, World};

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
        section_inputs.push((cy, chunk.blocks.as_slice(), chunk.block_data.as_slice(), any_non_air));
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

pub(crate) fn apply_column(world: &mut World, column: ColumnBlob) -> io::Result<()> {
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

    validate_column_blob_shape(&column)?;

    for cy in column.min_section_y..=column.max_section_y {
        world.chunks.remove(&ChunkCoord {
            x: column.cx,
            y: cy,
            z: column.cz,
        });
    }

    let block_registry = BlockRegistry::new();
    for section in column.sections {
        let data_bytes = section.data.decode_values()?;
        let mut blocks = Vec::with_capacity(SECTION_BLOCK_COUNT);
        let mut any_non_air = false;
        let mut any_non_zero_data = false;
        match section.blocks {
            EncodedBlocks::LegacyManifestIds(blocks_legacy) => {
                let block_bytes = blocks_legacy.decode_values()?;
                for &raw in &block_bytes {
                    let block = world.decode_manifest_block_id(raw as u32);
                    any_non_air |= block != BlockId::Air;
                    blocks.push(block);
                }
            }
            EncodedBlocks::NamePaletteBitpacked {
                palette,
                bits_per_entry,
                packed_words,
            } => {
                let indices = unpack_indices_u32(bits_per_entry, &packed_words, palette.len())?;
                let mut warned_missing = HashSet::new();
                for palette_index in indices {
                    let name = &palette[palette_index as usize];
                    let block = block_registry.resolve(name).unwrap_or_else(|| {
                        if warned_missing.insert(name.clone()) {
                            eprintln!(
                                "save decode: unknown block '{}' in section y={} at column ({},{}); mapping to air",
                                name, section.cy, column.cx, column.cz
                            );
                        }
                        BlockId::Air
                    });
                    any_non_air |= block != BlockId::Air;
                    blocks.push(block);
                }
            }
        }
        for &raw in &data_bytes {
            any_non_zero_data |= raw != 0;
        }
        if !any_non_air && !any_non_zero_data {
            continue;
        }

        let chunk = Chunk::from_parts(
            Arc::new(blocks),
            Arc::new(data_bytes),
            Arc::new(vec![0; SECTION_BLOCK_COUNT]),
            0,
        );
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
                    return invalid_data(
                        "name-palette block encoding is not valid for v1 columns",
                    );
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
        return invalid_data(format!(
            "invalid v2 block palette length {}",
            palette.len()
        ));
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
