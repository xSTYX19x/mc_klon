use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::storage::format::{
    COMPRESSION_TYPE_ZLIB, REGION_HEADER_BYTES, REGION_SECTOR_BYTES, REGION_TABLE_LEN,
    region_coords_for_chunk, region_file_name, region_local_index,
};

pub(crate) type BitVec = Vec<bool>;

pub(crate) struct RegionFile {
    pub(crate) path: PathBuf,
    file: File,
    pub(crate) locations: [u32; REGION_TABLE_LEN],
    pub(crate) timestamps: [u32; REGION_TABLE_LEN],
    pub(crate) free_sectors: BitVec, // true = free, false = occupied
    rx: i32,
    rz: i32,
}

impl RegionFile {
    pub(crate) fn open(region_dir: &Path, rx: i32, rz: i32) -> io::Result<Self> {
        fs::create_dir_all(region_dir)?;
        let path = region_dir.join(region_file_name(rx, rz));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        let mut file_len = file.metadata()?.len();
        if file_len == 0 {
            file.set_len(REGION_HEADER_BYTES as u64)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&vec![0u8; REGION_HEADER_BYTES])?;
            file_len = REGION_HEADER_BYTES as u64;
        }
        if file_len < REGION_HEADER_BYTES as u64 {
            return invalid_data(format!(
                "region file too small: {} bytes (need at least {})",
                file_len, REGION_HEADER_BYTES
            ));
        }
        if file_len % REGION_SECTOR_BYTES as u64 != 0 {
            return invalid_data(format!(
                "region file length must be 4KiB-aligned, got {}",
                file_len
            ));
        }

        let mut header = vec![0u8; REGION_HEADER_BYTES];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut header)?;

        let mut locations = [0u32; REGION_TABLE_LEN];
        let mut timestamps = [0u32; REGION_TABLE_LEN];
        for (i, slot) in locations.iter_mut().enumerate() {
            let base = i * 4;
            *slot = u32::from_be_bytes([
                header[base],
                header[base + 1],
                header[base + 2],
                header[base + 3],
            ]);
        }
        for (i, slot) in timestamps.iter_mut().enumerate() {
            let base = REGION_SECTOR_BYTES + i * 4;
            *slot = u32::from_be_bytes([
                header[base],
                header[base + 1],
                header[base + 2],
                header[base + 3],
            ]);
        }

        let sector_count = (file_len as usize) / REGION_SECTOR_BYTES;
        let mut free_sectors = vec![true; sector_count];
        free_sectors[0] = false;
        free_sectors[1] = false;

        for (index, &entry) in locations.iter().enumerate() {
            if entry == 0 {
                continue;
            }
            let (offset, count) = unpack_location(entry);
            if offset < 2 {
                return invalid_data(format!(
                    "invalid region location entry {}: sector offset {} overlaps header",
                    index, offset
                ));
            }
            if count == 0 {
                return invalid_data(format!(
                    "invalid region location entry {}: zero sector count",
                    index
                ));
            }
            let end = offset
                .checked_add(count)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sector overflow"))?;
            if end > sector_count {
                return invalid_data(format!(
                    "region location entry {} out of bounds: offset={} count={} total_sectors={}",
                    index, offset, count, sector_count
                ));
            }
            for slot in &mut free_sectors[offset..end] {
                if !*slot {
                    return invalid_data(format!(
                        "region location entry {} overlaps an existing allocated sector range",
                        index
                    ));
                }
                *slot = false;
            }
        }

        Ok(Self {
            path,
            file,
            locations,
            timestamps,
            free_sectors,
            rx,
            rz,
        })
    }

    pub(crate) fn open_for_chunk(region_dir: &Path, cx: i32, cz: i32) -> io::Result<Self> {
        let (rx, rz) = region_coords_for_chunk(cx, cz);
        Self::open(region_dir, rx, rz)
    }

    pub(crate) fn read_column(&mut self, cx: i32, cz: i32) -> io::Result<Option<Vec<u8>>> {
        let index = self.index_for_chunk(cx, cz)?;
        let entry = self.locations[index];
        if entry == 0 {
            return Ok(None);
        }

        let (offset, sector_count) = unpack_location(entry);
        self.validate_location_bounds(index, offset, sector_count)?;

        let byte_offset = (offset * REGION_SECTOR_BYTES) as u64;
        self.file.seek(SeekFrom::Start(byte_offset))?;

        let mut len_buf = [0u8; 4];
        self.file.read_exact(&mut len_buf)?;
        let length = u32::from_be_bytes(len_buf) as usize;
        if length < 1 {
            return invalid_data(format!(
                "invalid chunk length at index {}: {} (must include compression byte)",
                index, length
            ));
        }
        let max_len = sector_count
            .checked_mul(REGION_SECTOR_BYTES)
            .and_then(|v| v.checked_sub(4))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sector length overflow"))?;
        if length > max_len {
            return invalid_data(format!(
                "chunk length exceeds allocated sector range at index {}: length={} max={}",
                index, length, max_len
            ));
        }

        let mut compression_type = [0u8; 1];
        self.file.read_exact(&mut compression_type)?;
        if compression_type[0] != COMPRESSION_TYPE_ZLIB {
            return invalid_data(format!(
                "unsupported compression type at index {}: expected {}, got {}",
                index, COMPRESSION_TYPE_ZLIB, compression_type[0]
            ));
        }

        let payload_len = length - 1;
        let mut compressed = vec![0u8; payload_len];
        self.file.read_exact(&mut compressed)?;
        decompress_zlib(&compressed).map(Some)
    }

    pub(crate) fn has_column(&self, cx: i32, cz: i32) -> io::Result<bool> {
        let index = self.index_for_chunk(cx, cz)?;
        Ok(self.locations[index] != 0)
    }

    pub(crate) fn write_column(
        &mut self,
        cx: i32,
        cz: i32,
        uncompressed_column: &[u8],
    ) -> io::Result<()> {
        let index = self.index_for_chunk(cx, cz)?;
        let compressed = compress_zlib(uncompressed_column)?;
        let chunk_length = 1usize
            .checked_add(compressed.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "chunk length overflow"))?;
        let chunk_length_u32 = u32::try_from(chunk_length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("chunk length too large for region entry: {}", chunk_length),
            )
        })?;
        let record_bytes = 4usize
            .checked_add(chunk_length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "record length overflow"))?;
        let sector_count = record_bytes.div_ceil(REGION_SECTOR_BYTES);
        let sector_count_u8 = u8::try_from(sector_count).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "chunk record too large for region format (>{} sectors)",
                    u8::MAX
                ),
            )
        })?;
        if sector_count == 0 {
            return invalid_data("computed zero sector_count for non-empty chunk write");
        }

        let old_entry = self.locations[index];
        let old_timestamp = self.timestamps[index];
        let old_loc = if old_entry == 0 {
            None
        } else {
            let (off, cnt) = unpack_location(old_entry);
            Some((off, cnt))
        };

        let new_offset = self.allocate_sectors(sector_count)?;

        let mut record = vec![0u8; sector_count * REGION_SECTOR_BYTES];
        record[..4].copy_from_slice(&chunk_length_u32.to_be_bytes());
        record[4] = COMPRESSION_TYPE_ZLIB;
        record[5..5 + compressed.len()].copy_from_slice(&compressed);

        let write_result = (|| -> io::Result<()> {
            let byte_offset = (new_offset * REGION_SECTOR_BYTES) as u64;
            self.file.seek(SeekFrom::Start(byte_offset))?;
            self.file.write_all(&record)?;
            self.file.sync_data()?;

            self.locations[index] = pack_location(new_offset, sector_count_u8)?;
            self.timestamps[index] = unix_epoch_secs_u32();
            self.write_header()?;
            self.file.sync_data()?;
            Ok(())
        })();

        if let Err(err) = write_result {
            self.locations[index] = old_entry;
            self.timestamps[index] = old_timestamp;
            self.mark_sector_range(new_offset, sector_count, true)?;
            return Err(err);
        }

        if let Some((old_offset, old_count)) = old_loc {
            self.mark_sector_range(old_offset, old_count, true)?;
        }

        Ok(())
    }

    pub(crate) fn clear_column(&mut self, cx: i32, cz: i32) -> io::Result<()> {
        let index = self.index_for_chunk(cx, cz)?;
        let old_entry = self.locations[index];
        if old_entry == 0 {
            return Ok(());
        }

        self.locations[index] = 0;
        self.timestamps[index] = unix_epoch_secs_u32();
        self.write_header()?;
        self.file.sync_data()?;

        let (offset, count) = unpack_location(old_entry);
        self.mark_sector_range(offset, count, true)?;
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn index_for_chunk(&self, cx: i32, cz: i32) -> io::Result<usize> {
        let (rx, rz) = region_coords_for_chunk(cx, cz);
        if rx != self.rx || rz != self.rz {
            return invalid_data(format!(
                "chunk ({},{}) belongs to region ({},{}), but file is region ({},{})",
                cx, cz, rx, rz, self.rx, self.rz
            ));
        }
        Ok(region_local_index(cx, cz))
    }

    fn write_header(&mut self) -> io::Result<()> {
        let mut header = vec![0u8; REGION_HEADER_BYTES];
        for (i, entry) in self.locations.iter().enumerate() {
            let base = i * 4;
            header[base..base + 4].copy_from_slice(&entry.to_be_bytes());
        }
        for (i, entry) in self.timestamps.iter().enumerate() {
            let base = REGION_SECTOR_BYTES + i * 4;
            header[base..base + 4].copy_from_slice(&entry.to_be_bytes());
        }
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        Ok(())
    }

    fn allocate_sectors(&mut self, count: usize) -> io::Result<usize> {
        if count == 0 {
            return invalid_data("cannot allocate zero sectors");
        }
        if let Some(start) = find_free_run(&self.free_sectors, count) {
            self.mark_sector_range(start, count, false)?;
            return Ok(start);
        }

        let start = self.free_sectors.len();
        let new_total = start.checked_add(count).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "sector allocation overflow")
        })?;
        self.file
            .set_len((new_total * REGION_SECTOR_BYTES) as u64)?;
        self.free_sectors.resize(new_total, true);
        self.mark_sector_range(start, count, false)?;
        Ok(start)
    }

    fn validate_location_bounds(
        &mut self,
        index: usize,
        offset: usize,
        count: usize,
    ) -> io::Result<()> {
        if offset < 2 {
            return invalid_data(format!(
                "invalid location offset for index {}: {} overlaps header",
                index, offset
            ));
        }
        if count == 0 {
            return invalid_data(format!(
                "invalid location sector count for index {}: 0",
                index
            ));
        }
        let file_sectors = (self.file.metadata()?.len() as usize) / REGION_SECTOR_BYTES;
        if offset
            .checked_add(count)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sector range overflow"))?
            > file_sectors
        {
            return invalid_data(format!(
                "chunk location out of bounds at index {}: offset={} count={} file_sectors={}",
                index, offset, count, file_sectors
            ));
        }
        Ok(())
    }

    fn mark_sector_range(&mut self, offset: usize, count: usize, free: bool) -> io::Result<()> {
        let end = offset
            .checked_add(count)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sector range overflow"))?;
        if end > self.free_sectors.len() {
            return invalid_data(format!(
                "sector range out of bounds: {}..{} (len={})",
                offset,
                end,
                self.free_sectors.len()
            ));
        }
        for slot in &mut self.free_sectors[offset..end] {
            *slot = free;
        }
        Ok(())
    }
}

fn compress_zlib(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

fn decompress_zlib(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(bytes);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn find_free_run(free_sectors: &[bool], need: usize) -> Option<usize> {
    if need == 0 {
        return None;
    }
    let mut run_start = 0usize;
    let mut run_len = 0usize;
    for (idx, &is_free) in free_sectors.iter().enumerate().skip(2) {
        if is_free {
            if run_len == 0 {
                run_start = idx;
            }
            run_len += 1;
            if run_len >= need {
                return Some(run_start);
            }
        } else {
            run_len = 0;
        }
    }
    None
}

fn pack_location(offset_sectors: usize, sector_count: u8) -> io::Result<u32> {
    if offset_sectors > 0x00FF_FFFF {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "region sector offset exceeds 24-bit location table limit: {}",
                offset_sectors
            ),
        ));
    }
    Ok(((offset_sectors as u32) << 8) | sector_count as u32)
}

fn unpack_location(entry: u32) -> (usize, usize) {
    (
        ((entry >> 8) & 0x00FF_FFFF) as usize,
        (entry & 0xFF) as usize,
    )
}

fn unix_epoch_secs_u32() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(u32::MAX as u64) as u32
}

fn invalid_data<T>(msg: impl Into<String>) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, msg.into()))
}

#[cfg(test)]
mod tests {
    use super::RegionFile;
    use crate::blocks::BlockId;
    use crate::storage::chunk_codec::{decode_column, encode_column};
    use crate::storage::format::region_coords_for_chunk;
    use crate::world::{CHUNK_SIZE, World};
    use glam::IVec3;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn region_roundtrip_and_negative_coords() {
        let tmp = unique_tmp_dir();
        let region_dir = tmp.join("region");

        let (cx, cz) = (-1, -33);
        let (rx, rz) = region_coords_for_chunk(cx, cz);
        assert_eq!((rx, rz), (-1, -2));

        let mut world = World::new();
        set(&mut world, cx, 0, cz, 0, 0, 0, BlockId::Stone, 1);
        set(&mut world, cx, 0, cz, 1, 0, 0, BlockId::Grass, 2);
        set(&mut world, cx, 3, cz, 5, 6, 7, BlockId::LavaStill, 15);
        let encoded = encode_column(&world, cx, cz).unwrap();

        {
            let mut region = RegionFile::open_for_chunk(&region_dir, cx, cz).unwrap();
            assert!(region.path.ends_with("r.-1.-2.mca"));
            region.write_column(cx, cz, &encoded).unwrap();
        }

        let read_back = {
            let mut region = RegionFile::open_for_chunk(&region_dir, cx, cz).unwrap();
            region.read_column(cx, cz).unwrap().unwrap()
        };
        assert_eq!(encoded, read_back);

        let decoded_a = decode_column(&encoded).unwrap();
        let decoded_b = decode_column(&read_back).unwrap();
        assert_eq!(decoded_a, decoded_b);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn region_overwrite_readback_works() {
        let tmp = unique_tmp_dir();
        let region_dir = tmp.join("region");
        let (cx, cz) = (10, 11);

        let mut world = World::new();
        set(&mut world, cx, 0, cz, 0, 0, 0, BlockId::Stone, 1);
        let encoded_small = encode_column(&world, cx, cz).unwrap();

        let mut region = RegionFile::open_for_chunk(&region_dir, cx, cz).unwrap();
        region.write_column(cx, cz, &encoded_small).unwrap();

        // Larger payload to force reallocation in many cases.
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let block = if (x + y + z) % 2 == 0 {
                        BlockId::Obsidian
                    } else {
                        BlockId::Sand
                    };
                    set(&mut world, cx, 5, cz, x, y, z, block, (x ^ y ^ z) as u8);
                }
            }
        }
        let encoded_large = encode_column(&world, cx, cz).unwrap();
        region.write_column(cx, cz, &encoded_large).unwrap();

        let read_back = region.read_column(cx, cz).unwrap().unwrap();
        assert_eq!(read_back, encoded_large);

        let _ = fs::remove_dir_all(&tmp);
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
        let pos = IVec3::new(
            cx * CHUNK_SIZE + x,
            cy * CHUNK_SIZE + y,
            cz * CHUNK_SIZE + z,
        );
        world.set_block_and_data(pos, block, data).unwrap();
    }

    fn unique_tmp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("mc_klon_region_test_{nanos}"))
    }
}
