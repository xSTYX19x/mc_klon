pub(crate) const SAVE_ROOT_DIR: &str = "save";
pub(crate) const WORLD_META_FILE_NAME: &str = "world.meta";
pub(crate) const WORLD_MANIFEST_FILE_NAME: &str = "manifest.json";
pub(crate) const TICKING_AREAS_FILE_NAME: &str = "ticking_areas.bin";
pub(crate) const REGION_DIR_NAME: &str = "region";
pub(crate) const REGION_FILE_EXT: &str = "mca";

pub(crate) const WORLD_META_MAGIC: [u8; 4] = *b"MKWM";
pub(crate) const WORLD_META_FORMAT_VERSION: u16 = 1;
pub(crate) const BLOCK_ID_MAPPING_VERSION: u16 = 1;

pub(crate) const COLUMN_PAYLOAD_MAGIC: [u8; 4] = *b"MKCL";
pub(crate) const COLUMN_PAYLOAD_FORMAT_VERSION: u16 = 2;

pub(crate) const REGION_CHUNK_SIDE: i32 = 32;
pub(crate) const REGION_TABLE_LEN: usize = 1024;
pub(crate) const REGION_SECTOR_BYTES: usize = 4096;
pub(crate) const REGION_HEADER_BYTES: usize = REGION_SECTOR_BYTES * 2;

pub(crate) const COMPRESSION_TYPE_ZLIB: u8 = 2;
pub(crate) const SECTION_BLOCK_COUNT: usize = 16 * 16 * 16;

#[inline]
pub(crate) fn region_coords_for_chunk(cx: i32, cz: i32) -> (i32, i32) {
    (
        cx.div_euclid(REGION_CHUNK_SIDE),
        cz.div_euclid(REGION_CHUNK_SIDE),
    )
}

#[inline]
pub(crate) fn region_local_index(cx: i32, cz: i32) -> usize {
    let ix = cx & (REGION_CHUNK_SIDE - 1);
    let iz = cz & (REGION_CHUNK_SIDE - 1);
    (ix + iz * REGION_CHUNK_SIDE) as usize
}

pub(crate) fn region_file_name(rx: i32, rz: i32) -> String {
    format!("r.{rx}.{rz}.{REGION_FILE_EXT}")
}

#[cfg(test)]
mod tests {
    use super::{region_coords_for_chunk, region_file_name, region_local_index};

    #[test]
    fn region_local_index_handles_negative_coords() {
        assert_eq!(region_local_index(0, 0), 0);
        assert_eq!(region_local_index(31, 0), 31);
        assert_eq!(region_local_index(0, 31), 31 * 32);
        assert_eq!(region_local_index(31, 31), 1023);

        assert_eq!(region_local_index(-1, -1), 1023);
        assert_eq!(region_local_index(-32, -32), 0);
        assert_eq!(region_local_index(-33, -33), 1023);
        assert_eq!(region_local_index(-32, -1), 31 * 32);
        assert_eq!(region_local_index(-1, -32), 31);
    }

    #[test]
    fn region_coords_use_euclidean_division() {
        assert_eq!(region_coords_for_chunk(0, 0), (0, 0));
        assert_eq!(region_coords_for_chunk(31, 31), (0, 0));
        assert_eq!(region_coords_for_chunk(32, 32), (1, 1));

        assert_eq!(region_coords_for_chunk(-1, -1), (-1, -1));
        assert_eq!(region_coords_for_chunk(-32, -32), (-1, -1));
        assert_eq!(region_coords_for_chunk(-33, -33), (-2, -2));
    }

    #[test]
    fn region_file_name_uses_mca_extension() {
        assert_eq!(region_file_name(0, 0), "r.0.0.mca");
        assert_eq!(region_file_name(-2, 3), "r.-2.3.mca");
    }
}
