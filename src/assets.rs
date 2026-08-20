//! Resource pack discovery and asset path resolution.
//!
//! A pack is an *overlay*: every asset is looked up in the active pack first and
//! falls back to the built-in `assets/minecraft` tree. A partial pack that only
//! replaces a handful of block textures therefore works, instead of failing to
//! load everything it does not ship.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::assets::ASSET_ROOT;

/// Directory scanned for installed packs, next to `save/` and `screenshots/`.
pub(crate) const RESOURCE_PACK_DIR: &str = "resourcepacks";
/// Shown for "no pack", i.e. the bundled assets.
pub(crate) const DEFAULT_PACK_LABEL: &str = "Default";

/// One installed pack: the directory name is its identity in `settings.json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourcePack {
    /// Directory name under `resourcepacks/`.
    pub(crate) id: String,
    /// Root the asset-relative paths are joined onto.
    pub(crate) root: PathBuf,
}

/// Ordered asset roots. Earlier entries win; the last one is always the
/// built-in tree, so resolution can never come up empty.
#[derive(Clone, Debug)]
pub(crate) struct AssetSource {
    roots: Vec<PathBuf>,
}

impl Default for AssetSource {
    fn default() -> Self {
        Self {
            roots: vec![PathBuf::from(ASSET_ROOT)],
        }
    }
}

impl AssetSource {
    /// Builds the lookup order for `pack_id`. An unknown or missing pack falls
    /// back to the built-in assets rather than failing the whole startup.
    pub(crate) fn for_pack(pack_id: Option<&str>) -> Self {
        let mut roots = Vec::with_capacity(2);
        if let Some(id) = pack_id.filter(|id| !id.is_empty()) {
            match find_pack(id) {
                Some(pack) => roots.push(pack.root),
                None => crate::console_error!(
                    "[assets] resource pack '{id}' not found under {RESOURCE_PACK_DIR}/; using default assets"
                ),
            }
        }
        roots.push(PathBuf::from(ASSET_ROOT));
        Self { roots }
    }

    /// First root that actually has `relative`, else the built-in root.
    pub(crate) fn resolve(&self, relative: &str) -> PathBuf {
        let relative = normalize_relative(relative);
        for root in &self.roots {
            let candidate = root.join(&relative);
            if candidate.exists() {
                return candidate;
            }
        }
        self.default_root().join(relative)
    }

    /// Root used for assets the active pack does not override.
    pub(crate) fn default_root(&self) -> &Path {
        self.roots
            .last()
            .expect("asset source always keeps the built-in root")
    }

    /// Root a GUI sprite loader should use for `key`, since those loaders take a
    /// root plus a key instead of a finished path.
    pub(crate) fn root_for(&self, key: &str) -> &Path {
        let relative = normalize_relative(key);
        for root in &self.roots {
            if root.join(&relative).exists() {
                return root;
            }
        }
        self.default_root()
    }
}

/// `"minecraft:textures/block/stone.png"` and `"/textures/..."` both resolve to
/// the same pack-relative path.
pub(crate) fn normalize_relative(key: &str) -> String {
    key.strip_prefix("minecraft:")
        .unwrap_or(key)
        .trim_start_matches('/')
        .to_string()
}

/// Every pack under `resourcepacks/`, sorted by id so the options row has a
/// stable order across launches.
pub(crate) fn list_packs() -> Vec<ResourcePack> {
    let Ok(entries) = fs::read_dir(RESOURCE_PACK_DIR) else {
        return Vec::new();
    };
    let mut packs: Vec<ResourcePack> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let id = path.file_name()?.to_str()?.to_string();
            pack_root(&path).map(|root| ResourcePack { id, root })
        })
        .collect();
    packs.sort_by(|a, b| a.id.cmp(&b.id));
    packs
}

fn find_pack(id: &str) -> Option<ResourcePack> {
    list_packs().into_iter().find(|pack| pack.id == id)
}

/// Accepts both layouts a pack can ship: the Minecraft-style
/// `<pack>/assets/minecraft/textures/...` and a flat `<pack>/textures/...`.
fn pack_root(pack_dir: &Path) -> Option<PathBuf> {
    let minecraft_style = pack_dir.join("assets").join("minecraft");
    if minecraft_style.join("textures").is_dir() {
        return Some(minecraft_style);
    }
    if pack_dir.join("textures").is_dir() {
        return Some(pack_dir.to_path_buf());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mc_klon_assets_{tag}_{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn normalizes_mojang_keys() {
        assert_eq!(
            normalize_relative("minecraft:textures/block/stone.png"),
            "textures/block/stone.png"
        );
        assert_eq!(
            normalize_relative("/textures/block/stone.png"),
            "textures/block/stone.png"
        );
        assert_eq!(
            normalize_relative("textures/block/stone.png"),
            "textures/block/stone.png"
        );
    }

    #[test]
    fn default_source_resolves_against_the_builtin_root() {
        let source = AssetSource::default();
        assert_eq!(source.default_root(), Path::new(ASSET_ROOT));
        assert_eq!(
            source.resolve("minecraft:textures/block/stone.png"),
            Path::new(ASSET_ROOT).join("textures/block/stone.png")
        );
    }

    /// The whole point of the overlay: a pack that only ships one texture must
    /// not shadow everything else it is missing.
    #[test]
    fn pack_overrides_only_what_it_ships() {
        let pack = temp_dir("overlay");
        fs::create_dir_all(pack.join("textures/block")).unwrap();
        fs::write(pack.join("textures/block/stone.png"), b"x").unwrap();

        let source = AssetSource {
            roots: vec![pack.clone(), PathBuf::from(ASSET_ROOT)],
        };
        assert_eq!(
            source.resolve("textures/block/stone.png"),
            pack.join("textures/block/stone.png"),
            "pack texture should win"
        );
        assert_eq!(
            source.resolve("textures/block/not_in_pack.png"),
            Path::new(ASSET_ROOT).join("textures/block/not_in_pack.png"),
            "missing texture should fall back"
        );
        assert_eq!(source.root_for("textures/block/stone.png"), pack.as_path());
        assert_eq!(
            source.root_for("textures/block/not_in_pack.png"),
            Path::new(ASSET_ROOT)
        );
        let _ = fs::remove_dir_all(pack);
    }

    #[test]
    fn unknown_pack_falls_back_instead_of_failing() {
        let source = AssetSource::for_pack(Some("does-not-exist"));
        assert_eq!(source.default_root(), Path::new(ASSET_ROOT));
        assert_eq!(
            source.resolve("textures/block/stone.png"),
            Path::new(ASSET_ROOT).join("textures/block/stone.png")
        );
    }

    #[test]
    fn detects_both_pack_layouts() {
        let flat = temp_dir("flat");
        fs::create_dir_all(flat.join("textures")).unwrap();
        assert_eq!(pack_root(&flat), Some(flat.clone()));

        let mojang = temp_dir("mojang");
        fs::create_dir_all(mojang.join("assets/minecraft/textures")).unwrap();
        assert_eq!(
            pack_root(&mojang),
            Some(mojang.join("assets").join("minecraft"))
        );

        let empty = temp_dir("empty");
        assert_eq!(
            pack_root(&empty),
            None,
            "a folder without textures is not a pack"
        );

        for dir in [flat, mojang, empty] {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
