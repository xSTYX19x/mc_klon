//! Resource pack discovery and asset reading.
//!
//! A pack is an *overlay*: every asset is looked up in the active pack first and
//! falls back to the built-in `assets/minecraft` tree. A partial pack that only
//! replaces a handful of block textures therefore works, instead of failing to
//! load everything it does not ship.
//!
//! Packs come as a folder or a `.zip`, which is why reading goes through
//! [`PackSource`] and yields bytes rather than a filesystem path: a zip entry
//! has no path anything else could open.

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::assets::ASSET_ROOT;

/// Directory scanned for installed packs, next to `save/` and `screenshots/`.
pub(crate) const RESOURCE_PACK_DIR: &str = "resourcepacks";
/// Shown for "no pack", i.e. the bundled assets.
pub(crate) const DEFAULT_PACK_LABEL: &str = "Default";

/// One installed pack. The file/directory name is its identity in `settings.json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourcePack {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: PackKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackKind {
    Folder,
    Zip,
}

/// One place assets can be read from.
pub(crate) enum PackSource {
    Folder {
        root: PathBuf,
    },
    Zip {
        path: PathBuf,
        /// Pack-relative name -> entry index, so lookups do not scan the archive.
        index: HashMap<String, usize>,
        /// `by_index` needs `&mut`, and the source is shared and cloned.
        archive: Mutex<zip::ZipArchive<Cursor<Vec<u8>>>>,
    },
}

impl PackSource {
    fn folder(root: impl Into<PathBuf>) -> Self {
        Self::Folder { root: root.into() }
    }

    /// Reads the whole archive into memory once. Resource packs are a few MB at
    /// most, and every lookup afterwards avoids touching the disk again.
    fn zip(path: &Path) -> std::io::Result<Self> {
        let bytes = fs::read(path)?;
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

        let mut raw_names = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let Ok(entry) = archive.by_index(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            raw_names.push((entry.name().to_string(), i));
        }

        // Same two layouts the folder packs support, just inside the archive.
        let prefix = zip_asset_prefix(raw_names.iter().map(|(name, _)| name.as_str()));
        let index = raw_names
            .into_iter()
            .filter_map(|(name, i)| {
                let name = name.trim_start_matches("./").to_string();
                name.strip_prefix(&prefix).map(|rel| (rel.to_string(), i))
            })
            .collect();

        Ok(Self::Zip {
            path: path.to_path_buf(),
            index,
            archive: Mutex::new(archive),
        })
    }

    fn contains(&self, relative: &str) -> bool {
        match self {
            Self::Folder { root } => root.join(relative).exists(),
            Self::Zip { index, .. } => index.contains_key(relative),
        }
    }

    fn read(&self, relative: &str) -> Option<Vec<u8>> {
        match self {
            Self::Folder { root } => fs::read(root.join(relative)).ok(),
            Self::Zip { index, archive, .. } => {
                let entry_index = *index.get(relative)?;
                let mut archive = archive.lock().ok()?;
                let mut entry = archive.by_index(entry_index).ok()?;
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut bytes).ok()?;
                Some(bytes)
            }
        }
    }

    /// Human-readable location of `relative`, for error messages.
    fn describe(&self, relative: &str) -> String {
        match self {
            Self::Folder { root } => root.join(relative).display().to_string(),
            Self::Zip { path, .. } => format!("{}!{relative}", path.display()),
        }
    }
}

/// Picks the in-archive prefix that asset-relative paths sit behind, mirroring
/// the two folder layouts: Minecraft-style `assets/minecraft/...` or flat.
fn zip_asset_prefix<'a>(names: impl Iterator<Item = &'a str>) -> String {
    for name in names {
        if name
            .trim_start_matches("./")
            .starts_with("assets/minecraft/textures/")
        {
            return "assets/minecraft/".to_string();
        }
    }
    String::new()
}

/// Ordered asset sources. Earlier entries win; the last one is always the
/// built-in tree, so a lookup can never come up empty because of a partial pack.
#[derive(Clone)]
pub(crate) struct AssetSource {
    sources: Vec<Arc<PackSource>>,
}

impl Default for AssetSource {
    fn default() -> Self {
        Self {
            sources: vec![Arc::new(PackSource::folder(ASSET_ROOT))],
        }
    }
}

impl AssetSource {
    /// Builds the lookup order for `pack_id`. An unknown, missing or unreadable
    /// pack falls back to the built-in assets rather than failing startup.
    pub(crate) fn for_pack(pack_id: Option<&str>) -> Self {
        let mut sources: Vec<Arc<PackSource>> = Vec::with_capacity(2);
        if let Some(id) = pack_id.filter(|id| !id.is_empty()) {
            match find_pack(id) {
                Some(pack) => match open_pack(&pack) {
                    Ok(source) => sources.push(Arc::new(source)),
                    Err(err) => crate::console_error!(
                        "[assets] could not open resource pack '{id}' ({}): {err}; using default assets",
                        pack.path.display()
                    ),
                },
                None => crate::console_error!(
                    "[assets] resource pack '{id}' not found under {RESOURCE_PACK_DIR}/; using default assets"
                ),
            }
        }
        sources.push(Arc::new(PackSource::folder(ASSET_ROOT)));
        Self { sources }
    }

    /// Raw bytes of an asset, from the first source that has it.
    pub(crate) fn read(&self, key: &str) -> Option<Vec<u8>> {
        let relative = normalize_relative(key);
        self.sources
            .iter()
            .find(|source| source.contains(&relative))
            .and_then(|source| source.read(&relative))
    }

    pub(crate) fn read_to_string(&self, key: &str) -> Option<String> {
        String::from_utf8(self.read(key)?).ok()
    }

    /// Where `key` resolves to, for error messages and logging.
    pub(crate) fn describe(&self, key: &str) -> String {
        let relative = normalize_relative(key);
        self.sources
            .iter()
            .find(|source| source.contains(&relative))
            .unwrap_or_else(|| self.builtin())
            .describe(&relative)
    }

    fn builtin(&self) -> &Arc<PackSource> {
        self.sources
            .last()
            .expect("asset source always keeps the built-in root")
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

/// Every pack under `resourcepacks/`, sorted by id so the options row keeps a
/// stable order across launches.
pub(crate) fn list_packs() -> Vec<ResourcePack> {
    let Ok(entries) = fs::read_dir(RESOURCE_PACK_DIR) else {
        return Vec::new();
    };
    let mut packs: Vec<ResourcePack> = entries
        .flatten()
        .filter_map(|entry| classify_pack(&entry.path()))
        .collect();
    packs.sort_by(|a, b| a.id.cmp(&b.id));
    packs
}

fn classify_pack(path: &Path) -> Option<ResourcePack> {
    if path.is_dir() {
        // Folder packs must actually contain assets, or every stray directory
        // under resourcepacks/ would show up in the options row.
        folder_pack_root(path)?;
        let id = path.file_name()?.to_str()?.to_string();
        return Some(ResourcePack {
            id,
            path: path.to_path_buf(),
            kind: PackKind::Folder,
        });
    }
    let is_zip = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    if !is_zip {
        return None;
    }
    let id = path.file_name()?.to_str()?.to_string();
    Some(ResourcePack {
        id,
        path: path.to_path_buf(),
        kind: PackKind::Zip,
    })
}

fn find_pack(id: &str) -> Option<ResourcePack> {
    list_packs().into_iter().find(|pack| pack.id == id)
}

fn open_pack(pack: &ResourcePack) -> std::io::Result<PackSource> {
    match pack.kind {
        PackKind::Folder => {
            let root = folder_pack_root(&pack.path).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("'{}' has no textures directory", pack.path.display()),
                )
            })?;
            Ok(PackSource::folder(root))
        }
        PackKind::Zip => PackSource::zip(&pack.path),
    }
}

/// Accepts both layouts a folder pack can ship: the Minecraft-style
/// `<pack>/assets/minecraft/textures/...` and a flat `<pack>/textures/...`.
fn folder_pack_root(pack_dir: &Path) -> Option<PathBuf> {
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
    use std::fs::File;
    use std::io::Write;
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

    /// Builds a zip with the given `name -> contents` entries.
    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }

    fn source_of(sources: Vec<PackSource>) -> AssetSource {
        AssetSource {
            sources: sources.into_iter().map(Arc::new).collect(),
        }
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

    /// The whole point of the overlay: a pack that only ships one texture must
    /// not shadow everything else it is missing.
    #[test]
    fn folder_pack_overrides_only_what_it_ships() {
        let pack = temp_dir("overlay");
        let builtin = temp_dir("builtin");
        fs::create_dir_all(pack.join("textures/block")).unwrap();
        fs::create_dir_all(builtin.join("textures/block")).unwrap();
        fs::write(pack.join("textures/block/stone.png"), b"packed").unwrap();
        fs::write(builtin.join("textures/block/stone.png"), b"builtin").unwrap();
        fs::write(builtin.join("textures/block/dirt.png"), b"builtin-dirt").unwrap();

        let source = source_of(vec![
            PackSource::folder(&pack),
            PackSource::folder(&builtin),
        ]);
        assert_eq!(
            source.read("textures/block/stone.png").as_deref(),
            Some(&b"packed"[..])
        );
        assert_eq!(
            source.read("textures/block/dirt.png").as_deref(),
            Some(&b"builtin-dirt"[..]),
            "missing asset should fall back"
        );
        assert_eq!(source.read("textures/block/missing.png"), None);

        for dir in [pack, builtin] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn reads_a_flat_zip_pack() {
        let dir = temp_dir("zip_flat");
        let zip_path = dir.join("pack.zip");
        write_zip(
            &zip_path,
            &[
                ("pack.mcmeta", b"{}"),
                ("textures/block/stone.png", b"zipped-stone"),
            ],
        );

        let source = PackSource::zip(&zip_path).unwrap();
        assert!(source.contains("textures/block/stone.png"));
        assert_eq!(
            source.read("textures/block/stone.png").as_deref(),
            Some(&b"zipped-stone"[..])
        );
        assert!(!source.contains("textures/block/dirt.png"));
        let _ = fs::remove_dir_all(dir);
    }

    /// Vanilla packs nest everything under `assets/minecraft/`; that prefix has
    /// to be stripped so keys look the same as in a flat pack.
    #[test]
    fn reads_a_minecraft_style_zip_pack() {
        let dir = temp_dir("zip_mojang");
        let zip_path = dir.join("pack.zip");
        write_zip(
            &zip_path,
            &[
                ("pack.mcmeta", b"{}"),
                ("assets/minecraft/textures/block/stone.png", b"nested"),
            ],
        );

        let source = PackSource::zip(&zip_path).unwrap();
        assert!(source.contains("textures/block/stone.png"));
        assert_eq!(
            source.read("textures/block/stone.png").as_deref(),
            Some(&b"nested"[..])
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn zip_pack_falls_back_for_missing_assets() {
        let dir = temp_dir("zip_overlay");
        let builtin = temp_dir("zip_builtin");
        let zip_path = dir.join("pack.zip");
        write_zip(&zip_path, &[("textures/block/stone.png", b"zipped")]);
        fs::create_dir_all(builtin.join("textures/block")).unwrap();
        fs::write(builtin.join("textures/block/dirt.png"), b"builtin-dirt").unwrap();

        let source = source_of(vec![
            PackSource::zip(&zip_path).unwrap(),
            PackSource::folder(&builtin),
        ]);
        assert_eq!(
            source.read("minecraft:textures/block/stone.png").as_deref(),
            Some(&b"zipped"[..])
        );
        assert_eq!(
            source.read("textures/block/dirt.png").as_deref(),
            Some(&b"builtin-dirt"[..])
        );
        assert!(
            source
                .describe("textures/block/stone.png")
                .contains(".zip!")
        );

        for d in [dir, builtin] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn unknown_pack_falls_back_instead_of_failing() {
        let source = AssetSource::for_pack(Some("does-not-exist"));
        assert_eq!(
            source.describe("textures/block/stone.png"),
            Path::new(ASSET_ROOT)
                .join("textures/block/stone.png")
                .display()
                .to_string()
        );
    }

    #[test]
    fn detects_folder_pack_layouts() {
        let flat = temp_dir("flat");
        fs::create_dir_all(flat.join("textures")).unwrap();
        assert_eq!(folder_pack_root(&flat), Some(flat.clone()));

        let mojang = temp_dir("mojang");
        fs::create_dir_all(mojang.join("assets/minecraft/textures")).unwrap();
        assert_eq!(
            folder_pack_root(&mojang),
            Some(mojang.join("assets").join("minecraft"))
        );

        let empty = temp_dir("empty");
        assert_eq!(
            folder_pack_root(&empty),
            None,
            "a folder without textures is not a pack"
        );
        assert_eq!(classify_pack(&empty), None);

        for dir in [flat, mojang, empty] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn classifies_zip_files_case_insensitively() {
        let dir = temp_dir("classify");
        let upper = dir.join("Pack.ZIP");
        write_zip(&upper, &[("textures/block/stone.png", b"x")]);
        let pack = classify_pack(&upper).expect("zip should be recognized");
        assert_eq!(pack.kind, PackKind::Zip);
        assert_eq!(pack.id, "Pack.ZIP");

        assert_eq!(classify_pack(&dir.join("notes.txt")), None);
        let _ = fs::remove_dir_all(dir);
    }
}
