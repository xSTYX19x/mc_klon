use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::blocks::{BlockId, BlockRegistry, CORE_MISSING_BLOCK_KEY, block_key_for_block};
use crate::registry::NamespacedKey;

const MANIFEST_FORMAT_VERSION: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestBlockEntry {
    pub(crate) id: u32,
    pub(crate) key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorldManifest {
    pub(crate) format_version: u16,
    pub(crate) game_version: String,
    pub(crate) content_signature: String,
    pub(crate) blocks: Vec<ManifestBlockEntry>,
}

impl WorldManifest {
    pub(crate) fn build_from_registry(reg: &BlockRegistry) -> WorldManifest {
        let mut keys = reg.registered_block_keys();
        keys.sort_by(|a, b| {
            a.namespace
                .cmp(&b.namespace)
                .then_with(|| a.name.cmp(&b.name))
        });
        keys.dedup();

        let content_signature = content_signature_hex(&keys);

        // Keep legacy compatibility for pre-manifest saves:
        // existing runtime block ids (u8) remain identical manifest ids.
        let mut blocks = Vec::new();
        let mut seen_keys: HashMap<String, u32> = HashMap::new();
        for raw in 0u16..=255 {
            let Ok(block) = BlockId::from_u8(raw as u8) else {
                continue;
            };
            let key = block_key_for_block(block).to_string();
            seen_keys.insert(key.clone(), raw as u32);
            blocks.push(ManifestBlockEntry {
                id: raw as u32,
                key,
            });
        }

        let mut next_id = blocks.len() as u32;
        for key in keys {
            let key_str = key.to_string();
            if seen_keys.contains_key(&key_str) {
                continue;
            }
            blocks.push(ManifestBlockEntry {
                id: next_id,
                key: key_str.clone(),
            });
            seen_keys.insert(key_str, next_id);
            next_id = next_id.wrapping_add(1);
        }

        WorldManifest {
            format_version: MANIFEST_FORMAT_VERSION,
            game_version: env!("CARGO_PKG_VERSION").to_string(),
            content_signature,
            blocks,
        }
    }

    pub(crate) fn load(path: &Path) -> io::Result<WorldManifest> {
        let bytes = fs::read(path)?;
        let manifest: WorldManifest = serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if manifest.format_version != MANIFEST_FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported world manifest format_version: expected {}, got {}",
                    MANIFEST_FORMAT_VERSION, manifest.format_version
                ),
            ));
        }
        Ok(manifest)
    }

    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut bytes = serde_json::to_vec_pretty(self)
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

    pub(crate) fn short_signature(&self) -> &str {
        let len = self.content_signature.len().min(8);
        &self.content_signature[..len]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IdMapper {
    pub(crate) from_manifest: Vec<BlockId>,
    pub(crate) to_manifest: HashMap<NamespacedKey, u32>,
    runtime_to_manifest_lut: [u32; 256],
    missing_manifest_keys: Vec<String>,
    missing_manifest_id: u32,
}

impl IdMapper {
    pub(crate) fn from_manifest_and_registry(
        manifest: &WorldManifest,
        reg: &BlockRegistry,
    ) -> IdMapper {
        let missing_key =
            NamespacedKey::parse(CORE_MISSING_BLOCK_KEY).expect("invalid hardcoded missing key");
        let missing_runtime = reg.block_for_key(&missing_key).unwrap_or(BlockId::Air);

        let mut manifest_key_to_id: HashMap<NamespacedKey, u32> =
            HashMap::with_capacity(manifest.blocks.len());
        let mut max_id = 0u32;
        let mut missing_manifest_id = None;
        for entry in &manifest.blocks {
            max_id = max_id.max(entry.id);
            if let Some(key) = NamespacedKey::parse(&entry.key) {
                if key == missing_key {
                    missing_manifest_id = Some(entry.id);
                }
                manifest_key_to_id.insert(key, entry.id);
            }
        }
        let fallback_manifest_id = missing_manifest_id
            .or_else(|| {
                NamespacedKey::parse("core:air")
                    .and_then(|air| manifest_key_to_id.get(&air).copied())
            })
            .unwrap_or(0);

        let mut from_manifest = vec![missing_runtime; (max_id as usize).saturating_add(1)];
        let mut missing_manifest_keys = Vec::new();
        for entry in &manifest.blocks {
            let Some(key) = NamespacedKey::parse(&entry.key) else {
                continue;
            };
            let runtime_lookup = reg.block_for_key(&key);
            let runtime = runtime_lookup.unwrap_or(missing_runtime);
            if runtime_lookup.is_none() && key != missing_key {
                missing_manifest_keys.push(entry.key.clone());
            }
            if let Some(slot) = from_manifest.get_mut(entry.id as usize) {
                *slot = runtime;
            }
        }
        missing_manifest_keys.sort();
        missing_manifest_keys.dedup();

        let mut to_manifest = HashMap::new();
        for runtime_key in reg.registered_block_keys() {
            let manifest_id = manifest_key_to_id
                .get(&runtime_key)
                .copied()
                .unwrap_or(fallback_manifest_id);
            to_manifest.insert(runtime_key, manifest_id);
        }

        let mut runtime_to_manifest_lut = [fallback_manifest_id; 256];
        for raw in 0u16..=255 {
            if let Ok(block) = BlockId::from_u8(raw as u8) {
                let key = block_key_for_block(block);
                runtime_to_manifest_lut[raw as usize] = to_manifest
                    .get(&key)
                    .copied()
                    .unwrap_or(fallback_manifest_id);
            }
        }

        IdMapper {
            from_manifest,
            to_manifest,
            runtime_to_manifest_lut,
            missing_manifest_keys,
            missing_manifest_id: fallback_manifest_id,
        }
    }

    pub(crate) fn decode_manifest_id(&self, manifest_id: u32) -> BlockId {
        self.from_manifest
            .get(manifest_id as usize)
            .copied()
            .or_else(|| {
                self.from_manifest
                    .get(self.missing_manifest_id as usize)
                    .copied()
            })
            .unwrap_or(BlockId::Air)
    }

    pub(crate) fn encode_runtime_block(&self, block: BlockId) -> u32 {
        self.runtime_to_manifest_lut[block.to_u8() as usize]
    }

    pub(crate) fn missing_blocks_count(&self) -> usize {
        self.missing_manifest_keys.len()
    }

    pub(crate) fn missing_block_keys(&self) -> &[String] {
        &self.missing_manifest_keys
    }

    pub(crate) fn runtime_to_manifest_lut(&self) -> [u32; 256] {
        self.runtime_to_manifest_lut
    }
}

fn content_signature_hex(keys: &[NamespacedKey]) -> String {
    let mut bytes = Vec::new();
    for key in keys {
        bytes.extend_from_slice(key.namespace.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(key.name.as_bytes());
        bytes.push(b'\n');
    }
    format!("{:016x}", fnv1a64(&bytes))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn tmp_path_for(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("manifest path '{}' has no file name", path.display()),
        )
    })?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    Ok(path.with_file_name(tmp_name))
}
