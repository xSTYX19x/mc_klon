use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::format::{
    BLOCK_ID_MAPPING_VERSION, WORLD_META_FORMAT_VERSION, WORLD_META_MAGIC,
};
use crate::world::CHUNK_SIZE;
use crate::world::WorldEnvironment;
use crate::worldgen::WorldgenPreset;

// `Eq` is gone with the float-carrying spawn; nothing compares metas for
// equality outside tests.
#[derive(Clone, Debug, PartialEq)]
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
    pub(crate) environment: WorldEnvironment,
    pub(crate) block_id_version: u16,
    pub(crate) save_format_version: u16,
    pub(crate) block_registry_hash: Option<String>,
    /// User-facing name. `None` falls back to the save directory's name.
    pub(crate) display_name: Option<String>,
    /// Where the player stood and looked when the world was last saved.
    /// `None` for worlds that were never saved by a build that records it, in
    /// which case startup falls back to the origin-centred spawn search.
    pub(crate) player_spawn: Option<PlayerSpawn>,
}

/// Persisted player placement. Angles are radians in the camera's own
/// convention, stored as-is so a reload restores the exact view.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlayerSpawn {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) z: f32,
    pub(crate) yaw_rad: f32,
    pub(crate) pitch_rad: f32,
}

impl PlayerSpawn {
    /// Rejects values that would place the player nowhere useful. A corrupt or
    /// hand-edited entry must degrade to the normal spawn search, not strand
    /// the player outside the world.
    pub(crate) fn is_usable(&self, min_y: i32, max_y: i32) -> bool {
        [self.x, self.y, self.z, self.yaw_rad, self.pitch_rad]
            .iter()
            .all(|value| value.is_finite())
            && self.y >= min_y as f32
            && self.y <= max_y as f32 + 1.0
    }
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
        environment: WorldEnvironment,
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
            environment,
            block_id_version: BLOCK_ID_MAPPING_VERSION,
            save_format_version: 2,
            block_registry_hash: None,
            display_name: None,
            player_spawn: None,
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
    #[serde(default = "default_world_environment")]
    environment: String,
    block_id_version: u16,
    #[serde(default = "default_save_format_version")]
    save_format_version: u16,
    #[serde(default)]
    block_registry_hash: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    player_spawn: Option<PlayerSpawn>,
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
        environment: meta.environment.meta_key().to_string(),
        block_id_version: meta.block_id_version,
        save_format_version: meta.save_format_version,
        block_registry_hash: meta.block_registry_hash.clone(),
        display_name: meta.display_name.clone(),
        player_spawn: meta.player_spawn,
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
    let environment = WorldEnvironment::from_meta_key(&wire.environment).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported world.meta environment '{}'", wire.environment),
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
        environment,
        block_id_version: wire.block_id_version,
        save_format_version: wire.save_format_version,
        block_registry_hash: wire.block_registry_hash,
        display_name: wire.display_name,
        player_spawn: wire.player_spawn,
    };
    validate(&meta)?;

    Ok(Some(meta))
}

/// Longest name the UI accepts; keeps directory names well inside filesystem limits.
pub(crate) const WORLD_NAME_MAX_LEN: usize = 48;

/// Turns a user-typed world name into a safe directory name, or `None` when
/// nothing usable is left. Only `[A-Za-z0-9._-]` survives; runs of anything else
/// collapse into a single `-`.
pub(crate) fn sanitize_world_dir_name(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut pending_sep = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.push(ch);
        } else {
            pending_sep = true;
        }
        if out.len() >= WORLD_NAME_MAX_LEN {
            break;
        }
    }
    // "." and ".." would escape or alias the worlds directory.
    let trimmed = out.trim_matches('.').trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Rewrites only the player spawn of an existing `world.meta`, so saving does
/// not have to rebuild (and risk disagreeing with) the rest of the file.
pub(crate) fn set_player_spawn(path: &Path, spawn: PlayerSpawn) -> io::Result<()> {
    let mut meta = load(path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing world meta at {}", path.display()),
        )
    })?;
    meta.player_spawn = Some(spawn);
    save_atomic(path, &meta)
}

/// Rewrites only the display name of an existing `world.meta`.
pub(crate) fn set_display_name(path: &Path, display_name: Option<&str>) -> io::Result<()> {
    let mut meta = load(path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing world meta at {}", path.display()),
        )
    })?;
    meta.display_name = display_name.map(str::to_string);
    save_atomic(path, &meta)
}

/// Moves a world directory to a sibling with `new_dir_name`. Fails if the target
/// already exists so an unrelated world can never be overwritten.
pub(crate) fn rename_world_dir(save_root: &Path, new_dir_name: &str) -> io::Result<PathBuf> {
    let parent = save_root.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("world path '{}' has no parent", save_root.display()),
        )
    })?;
    let target = parent.join(new_dir_name);
    if target == save_root {
        return Ok(target);
    }
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("'{}' already exists", target.display()),
        ));
    }
    fs::rename(save_root, &target)?;
    Ok(target)
}

/// Permanently removes a world directory and everything under it.
pub(crate) fn delete_world_dir(save_root: &Path) -> io::Result<()> {
    if !save_root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("'{}' is not a world directory", save_root.display()),
        ));
    }
    fs::remove_dir_all(save_root)
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

fn default_world_environment() -> String {
    WorldEnvironment::Temperate.meta_key().to_string()
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
        WorldgenPreset::ClassicFlatter => "classic_flatter",
        WorldgenPreset::Rd161348Like => "rd161348_like",
        WorldgenPreset::Hilly => "hilly",
    }
}

fn preset_from_str(value: &str) -> Option<WorldgenPreset> {
    match value {
        "flatish" => Some(WorldgenPreset::Flatish),
        "classic_flatter" => Some(WorldgenPreset::ClassicFlatter),
        "rd161348_like" => Some(WorldgenPreset::Rd161348Like),
        "hilly" => Some(WorldgenPreset::Hilly),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{WorldMeta, load, save_atomic};
    use crate::world::WorldEnvironment;
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
        let meta = WorldMeta::new(
            1234,
            WorldgenPreset::Hilly,
            64,
            0,
            255,
            16,
            false,
            128,
            WorldEnvironment::Temperate,
        )
        .unwrap();
        save_atomic(&path, &meta).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded, meta);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sanitizes_world_names() {
        use super::sanitize_world_dir_name;
        assert_eq!(sanitizes("My World"), "My-World");
        assert_eq!(sanitizes("a///b"), "a-b");
        assert_eq!(sanitizes("  spaced  "), "spaced");
        assert_eq!(sanitizes("keep_this.one-ok"), "keep_this.one-ok");
        assert_eq!(sanitize_world_dir_name(".."), None);
        assert_eq!(sanitize_world_dir_name("///"), None);
        assert_eq!(sanitize_world_dir_name(""), None);
    }

    fn sanitizes(raw: &str) -> String {
        super::sanitize_world_dir_name(raw).expect("expected a usable name")
    }

    #[test]
    fn rename_refuses_existing_target() {
        let base = std::env::temp_dir().join(format!(
            "mc_klon_rename_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let from = base.join("from");
        let to = base.join("to");
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();

        let err = super::rename_world_dir(&from, "to").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(from.is_dir());

        let moved = super::rename_world_dir(&from, "fresh").unwrap();
        assert!(moved.is_dir());
        assert!(!from.exists());
        let _ = fs::remove_dir_all(base);
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

    #[test]
    fn player_spawn_round_trips_and_stays_optional() {
        let path = temp_meta_path();
        let mut meta = WorldMeta::new(
            7,
            WorldgenPreset::Hilly,
            64,
            0,
            255,
            16,
            false,
            128,
            WorldEnvironment::Temperate,
        )
        .unwrap();
        assert!(meta.player_spawn.is_none(), "a new world has no saved spawn");
        save_atomic(&path, &meta).unwrap();
        assert!(load(&path).unwrap().unwrap().player_spawn.is_none());

        let spawn = super::PlayerSpawn {
            x: 12.5,
            y: 71.0,
            z: -304.25,
            yaw_rad: 1.25,
            pitch_rad: -0.4,
        };
        meta.player_spawn = Some(spawn);
        save_atomic(&path, &meta).unwrap();
        assert_eq!(load(&path).unwrap().unwrap().player_spawn, Some(spawn));

        // The partial rewrite must touch only the spawn.
        let moved = super::PlayerSpawn {
            x: -8.0,
            ..spawn
        };
        super::set_player_spawn(&path, moved).unwrap();
        let reloaded = load(&path).unwrap().unwrap();
        assert_eq!(reloaded.player_spawn, Some(moved));
        assert_eq!(reloaded.world_seed, 7);
        assert_eq!(reloaded.preset, WorldgenPreset::Hilly);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn unusable_spawns_are_rejected_so_startup_falls_back() {
        let ok = super::PlayerSpawn {
            x: 0.0,
            y: 64.0,
            z: 0.0,
            yaw_rad: 0.0,
            pitch_rad: 0.0,
        };
        assert!(ok.is_usable(0, 255));

        for broken in [
            super::PlayerSpawn { y: -5.0, ..ok },
            super::PlayerSpawn { y: 400.0, ..ok },
            super::PlayerSpawn {
                x: f32::NAN,
                ..ok
            },
            super::PlayerSpawn {
                yaw_rad: f32::INFINITY,
                ..ok
            },
        ] {
            assert!(!broken.is_usable(0, 255), "{broken:?} must not be used");
        }
    }

    #[test]
    fn legacy_world_meta_defaults_environment_to_temperate() {
        let path = temp_meta_path();
        fs::write(
            &path,
            r#"{
  "magic":"MKWM",
  "format_version":1,
  "world_seed":99,
  "preset":"classic_flatter",
  "sea_level":64,
  "min_y":0,
  "max_y":255,
  "chunk_size":16,
  "boundary_ocean":true,
  "boundary_radius_chunks":16,
  "block_id_version":1,
  "save_format_version":1
}"#,
        )
        .unwrap();

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.environment, WorldEnvironment::Temperate);
        let _ = fs::remove_file(path);
    }
}
