use crate::texture::load_png_rgba;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Mojang-ähnlicher Asset-Key:
/// z.B. "minecraft:textures/block/stone.png" oder "textures/block/stone.png"
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct AssetId(Arc<str>);

impl AssetId {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into().into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AssetId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for AssetId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

pub type TextureHandle = AssetId;

/// Dekodierte RGBA8-PNG im RAM.
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Registry für Textur-Assets:
/// - mappt Mojang-Key -> dekodierte PNG-Daten
/// - löst Pfade relativ zu `assets_root` auf
pub struct TextureRegistry {
    assets_root: PathBuf,
    textures: HashMap<AssetId, TextureImage>,
}

impl TextureRegistry {
    pub fn new(assets_root: impl Into<PathBuf>) -> Self {
        Self {
            assets_root: assets_root.into(),
            textures: HashMap::new(),
        }
    }

    fn resolve_path(&self, key: &AssetId) -> PathBuf {
        // Namescheme:
        // - "minecraft:textures/block/stone.png" -> "textures/block/stone.png"
        // - "textures/block/stone.png"           -> unverändert
        let relative = key
            .as_str()
            .strip_prefix("minecraft:")
            .unwrap_or(key.as_str())
            .trim_start_matches('/');
        self.assets_root.join(relative)
    }

    /// Holt eine Textur aus dem Cache oder lädt sie von Disk (PNG -> RGBA8).
    /// Fehlermeldungen enthalten sowohl Key als auch finalen Dateipfad.
    pub fn get_or_load(&mut self, key: impl Into<AssetId>) -> Result<TextureHandle> {
        let key: AssetId = key.into();
        if self.textures.contains_key(&key) {
            return Ok(key);
        }

        let path = self.resolve_path(&key);
        let (width, height, rgba) = load_png_rgba(&path).with_context(|| {
            // Use static string formatting to avoid allocation in hot path
            format!(
                "failed to load texture key '{}' from '{}'",
                key.as_str(),
                path.display()
            )
        })?;
        // Clone is now just cloning the Arc, much cheaper than cloning String
        self.textures.insert(
            key.clone(),
            TextureImage {
                width,
                height,
                rgba,
            },
        );
        Ok(key)
    }

    pub fn get(&self, handle: &TextureHandle) -> Option<&TextureImage> {
        self.textures.get(handle)
    }
}
