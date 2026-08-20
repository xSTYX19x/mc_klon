use crate::assets::AssetSource;
use crate::texture::decode_png_rgba;
use anyhow::{Context, Result};
use std::collections::HashMap;
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
/// - löst Pfade über die aktive Resource-Pack-Kette auf
pub struct TextureRegistry {
    source: AssetSource,
    textures: HashMap<AssetId, TextureImage>,
}

impl TextureRegistry {
    pub fn new(source: AssetSource) -> Self {
        Self {
            source,
            textures: HashMap::new(),
        }
    }

    /// Holt eine Textur aus dem Cache oder lädt sie von Disk (PNG -> RGBA8).
    /// Fehlermeldungen enthalten sowohl Key als auch finalen Dateipfad.
    pub fn get_or_load(&mut self, key: impl Into<AssetId>) -> Result<TextureHandle> {
        let key: AssetId = key.into();
        if self.textures.contains_key(&key) {
            return Ok(key);
        }

        // Namescheme:
        // - "minecraft:textures/block/stone.png" -> "textures/block/stone.png"
        // - "textures/block/stone.png"           -> unverändert
        // Der aktive Pack gewinnt, sonst greifen die Standard-Assets.
        let location = self.source.describe(key.as_str());
        let bytes = self.source.read(key.as_str()).with_context(|| {
            format!(
                "failed to load texture key '{}' from '{}'",
                key.as_str(),
                location
            )
        })?;
        let (width, height, rgba) = decode_png_rgba(&bytes, &location)
            .with_context(|| format!("failed to decode texture key '{}'", key.as_str()))?;
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
