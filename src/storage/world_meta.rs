use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::format::{
    BLOCK_ID_MAPPING_VERSION, WORLD_META_FORMAT_VERSION, WORLD_META_MAGIC,
};
use crate::world::CHUNK_SIZE;
use crate::worldgen::WorldgenPreset;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorldMeta {
    pub(crate) format_version: u16,
    pub(crate) world_seed: u64,
    pub(crate) preset: WorldgenPreset,
    pub(crate) sea_level: i32,
    pub(crate) min_y: i32,
    pub(crate) max_y: i32,
    pub(crate) chunk_size: i32,
    pub(crate) boundary_ocean: bool,
    pub(crate) boundary_radius_chunks: i32,
    pub(crate) block_id_version: u16,
    pub(crate) save_format_version: u16,
    pub(crate) block_registry_hash: Option<String>,
}

impl WorldMeta {
    pub(crate) fn new(
        world_seed: u64,
        preset: WorldgenPreset,
        sea_level: i32,
        min_y: i32,
        max_y: i32,
        chunk_size: i32,
        boundary_ocean: bool,
        boundary_radius_chunks: i32,
    ) -> io::Result<Self> {
        let meta = Self {
            format_version: WORLD_META_FORMAT_VERSION,
            world_seed,
            preset,
            sea_level,
            min_y,
            max_y,
            chunk_size,
            boundary_ocean,
            boundary_radius_chunks,
            block_id_version: BLOCK_ID_MAPPING_VERSION,
            save_format_version: 2,
            block_registry_hash: None,
        };
        validate(&meta)?;
        Ok(meta)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorldMetaWire {
    magic: String,
    format_version: u16,
    world_seed: u64,
    preset: String,
    sea_level: i32,
    min_y: i32,
    max_y: i32,
    chunk_size: i32,
    #[serde(default)]
    boundary_ocean: bool,
    #[serde(default = "default_boundary_radius_chunks")]
    boundary_radius_chunks: i32,
    block_id_version: u16,
    #[serde(default = "default_save_format_version")]
    save_format_version: u16,
    #[serde(default)]
    block_registry_hash: Option<String>,
}

pub(crate) fn save_atomic(path: &Path, meta: &WorldMeta) -> io::Result<()> {
    validate(meta)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let wire = WorldMetaWire {
        magic: String::from_utf8_lossy(&WORLD_META_MAGIC).to_string(),
        format_version: meta.format_version,
        world_seed: meta.world_seed,
        preset: preset_to_str(meta.preset).to_string(),
        sea_level: meta.sea_level,
        min_y: meta.min_y,
        max_y: meta.max_y,
        chunk_size: meta.chunk_size,
        boundary_ocean: meta.boundary_ocean,
        boundary_radius_chunks: meta.boundary_radius_chunks,
        block_id_version: meta.block_id_version,
        save_format_version: meta.save_format_version,
        block_registry_hash: meta.block_registry_hash.clone(),
    };

    let mut bytes = serde_json::to_vec_pretty(&wire)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    bytes.push(b'\n');

    let tmp_path = tmp_path_for(path)?;
    let mut file = File::create(&tmp_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, path)?;

    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }

    Ok(())
}

pub(crate) fn load(path: &Path) -> io::Result<Option<WorldMeta>> {
    if !path.exists() {
        return Ok(None);
    }

    let bytes = fs::read(path)?;
    let wire: WorldMetaWire = serde_json::from_slice(&bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    if wire.magic.as_bytes() != WORLD_META_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid world.meta magic: expected {:?}, got {:?}",
                String::from_utf8_lossy(&WORLD_META_MAGIC),
                wire.magic
            ),
        ));
    }

    let preset = preset_from_str(&wire.preset).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported world.meta preset '{}'", wire.preset),
        )
    })?;

    let meta = WorldMeta {
        format_version: wire.format_version,
        world_seed: wire.world_seed,
        preset,
        sea_level: wire.sea_level,
        min_y: wire.min_y,
        max_y: wire.max_y,
        chunk_size: wire.chunk_size,
        boundary_ocean: wire.boundary_ocean,
        boundary_radius_chunks: wire.boundary_radius_chunks,
        block_id_version: wire.block_id_version,
        save_format_version: wire.save_format_version,
        block_registry_hash: wire.block_registry_hash,
    };
    validate(&meta)?;

    Ok(Some(meta))
}

fn validate(meta: &WorldMeta) -> io::Result<()> {
    if meta.format_version != WORLD_META_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported world.meta format_version: expected {}, got {}",
                WORLD_META_FORMAT_VERSION, meta.format_version
            ),
        ));
    }

    if meta.block_id_version != BLOCK_ID_MAPPING_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported block_id_version in world.meta: expected {}, got {}",
                BLOCK_ID_MAPPING_VERSION, meta.block_id_version
            ),
        ));
    }

    if meta.save_format_version != 1 && meta.save_format_version != 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported save_format_version in world.meta: expected 1 or 2, got {}",
                meta.save_format_version
            ),
        ));
    }

    if meta.chunk_size != CHUNK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported chunk_size in world.meta: expected {}, got {}",
                CHUNK_SIZE, meta.chunk_size
            ),
        ));
    }

    if meta.min_y > meta.max_y {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid world.meta height range: min_y {} > max_y {}",
                meta.min_y, meta.max_y
            ),
        ));
    }

    if meta.min_y.rem_euclid(meta.chunk_size) != 0 || meta.max_y.rem_euclid(meta.chunk_size) != 15 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "world.meta height bounds must align to 16-block sections (got min_y={}, max_y={})",
                meta.min_y, meta.max_y
            ),
        ));
    }

    if meta.sea_level < meta.min_y || meta.sea_level > meta.max_y {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "world.meta sea_level {} outside [{}, {}]",
                meta.sea_level, meta.min_y, meta.max_y
            ),
        ));
    }

    if meta.boundary_radius_chunks < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "world.meta boundary_radius_chunks must be >= 0, got {}",
                meta.boundary_radius_chunks
            ),
        ));
    }

    Ok(())
}

fn default_save_format_version() -> u16 {
    1
}

fn default_boundary_radius_chunks() -> i32 {
    128
}

fn tmp_path_for(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("world meta path '{}' has no file name", path.display()),
        )
    })?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    Ok(path.with_file_name(tmp_name))
}

fn preset_to_str(preset: WorldgenPreset) -> &'static str {
    match preset {
        WorldgenPreset::Flatish => "flatish",
        WorldgenPreset::Rd161348Like => "rd161348_like",
        WorldgenPreset::Hilly => "hilly",
    }
}

fn preset_from_str(value: &str) -> Option<WorldgenPreset> {
    match value {
        "flatish" => Some(WorldgenPreset::Flatish),
        "rd161348_like" => Some(WorldgenPreset::Rd161348Like),
        "hilly" => Some(WorldgenPreset::Hilly),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{WorldMeta, load, save_atomic};
    use crate::worldgen::WorldgenPreset;
    use std::fs;
    use std::io;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_meta_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("mc_klon_world_meta_{nanos}.json"))
    }

    #[test]
    fn roundtrips_world_meta() {
        let path = temp_meta_path();
        let meta = WorldMeta::new(1234, WorldgenPreset::Hilly, 64, 0, 255, 16, false, 128).unwrap();
        save_atomic(&path, &meta).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded, meta);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_unknown_fields() {
        let path = temp_meta_path();
        fs::write(
            &path,
            r#"{
  "magic":"MKWM",
  "format_version":1,
  "world_seed":1,
  "preset":"flatish",
  "sea_level":64,
  "min_y":0,
  "max_y":255,
  "chunk_size":16,
  "boundary_ocean":false,
  "boundary_radius_chunks":128,
  "block_id_version":1,
  "extra":true
}"#,
        )
        .unwrap();

        let err = load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(path);
    }
}
