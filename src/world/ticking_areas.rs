use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use glam::IVec2;
use serde::{Deserialize, Serialize};

pub(crate) const TICKING_AREAS_FORMAT_VERSION: u16 = 1;
pub(crate) const TICKING_AREA_MAX_RADIUS_CHUNKS: i32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TickingFlags {
    bits: u32,
}

impl TickingFlags {
    pub(crate) const KEEP_LOADED: u32 = 1 << 0;
    pub(crate) const TICK_FLUIDS: u32 = 1 << 1;
    pub(crate) const TICK_LIGHTING: u32 = 1 << 2;
    pub(crate) const TICK_ENTITIES: u32 = 1 << 3;
    pub(crate) const DEFAULT_BITS: u32 =
        Self::KEEP_LOADED | Self::TICK_FLUIDS | Self::TICK_LIGHTING | Self::TICK_ENTITIES;

    pub(crate) const fn from_bits(bits: u32) -> Self {
        Self { bits }
    }

    pub(crate) const fn bits(self) -> u32 {
        self.bits
    }

    pub(crate) const fn keep_loaded(self) -> bool {
        (self.bits & Self::KEEP_LOADED) != 0
    }

    pub(crate) const fn tick_fluids(self) -> bool {
        (self.bits & Self::TICK_FLUIDS) != 0
    }

    pub(crate) const fn tick_lighting(self) -> bool {
        (self.bits & Self::TICK_LIGHTING) != 0
    }

    pub(crate) const fn tick_entities(self) -> bool {
        (self.bits & Self::TICK_ENTITIES) != 0
    }

    pub(crate) const fn any_tick(self) -> bool {
        self.tick_fluids() || self.tick_lighting() || self.tick_entities()
    }
}

impl Default for TickingFlags {
    fn default() -> Self {
        Self {
            bits: Self::DEFAULT_BITS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TickingArea {
    pub(crate) id: u32,
    pub(crate) center: IVec2,
    pub(crate) radius_chunks: i32,
    pub(crate) flags: TickingFlags,
}

impl TickingArea {
    pub(crate) fn normalized(mut self) -> Self {
        self.radius_chunks = self.radius_chunks.clamp(0, TICKING_AREA_MAX_RADIUS_CHUNKS);
        self
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TickingAreasFile {
    format_version: u16,
    areas: Vec<TickingAreaWire>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TickingAreaWire {
    id: u32,
    cx: i32,
    cz: i32,
    radius_chunks: i32,
    flags_bits: u32,
}

pub(crate) fn load(path: &Path) -> io::Result<Option<Vec<TickingArea>>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let wire: TickingAreasFile = serde_json::from_slice(&bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if wire.format_version != TICKING_AREAS_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported ticking areas format: expected {}, got {}",
                TICKING_AREAS_FORMAT_VERSION, wire.format_version
            ),
        ));
    }

    let mut out = Vec::with_capacity(wire.areas.len());
    for area in wire.areas {
        out.push(
            TickingArea {
                id: area.id,
                center: IVec2::new(area.cx, area.cz),
                radius_chunks: area.radius_chunks,
                flags: TickingFlags::from_bits(area.flags_bits),
            }
            .normalized(),
        );
    }
    Ok(Some(out))
}

pub(crate) fn save_atomic(path: &Path, areas: &[TickingArea]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let wire = TickingAreasFile {
        format_version: TICKING_AREAS_FORMAT_VERSION,
        areas: areas
            .iter()
            .map(|area| {
                let area = area.normalized();
                TickingAreaWire {
                    id: area.id,
                    cx: area.center.x,
                    cz: area.center.y,
                    radius_chunks: area.radius_chunks,
                    flags_bits: area.flags.bits(),
                }
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&wire)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    bytes.push(b'\n');

    let tmp_path = tmp_path_for(path)?;
    let mut f = File::create(&tmp_path)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn tmp_path_for(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid ticking areas path '{}': missing file name",
                path.display()
            ),
        )
    })?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    Ok(path.with_file_name(tmp_name))
}
