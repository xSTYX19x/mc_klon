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
        // A saved manifest may spell a block with an accepted alias
        // (`core:stone`) instead of its canonical key. Decoding resolves those,
        // so encoding has to as well — otherwise re-saving such a world rewrites
        // every aliased block to the missing id. Exact canonical entries win.
        let mut aliased_ids: Vec<(NamespacedKey, u32)> = Vec::new();
        for entry in &manifest.blocks {
            let Some(key) = NamespacedKey::parse(&entry.key) else {
                continue;
            };
            let Some(block) = reg.block_for_key(&key) else {
                continue;
            };
            let canonical = block_key_for_block(block);
            if canonical != key {
                aliased_ids.push((canonical, entry.id));
            }
        }
        for (key, id) in aliased_ids {
            manifest_key_to_id.entry(key).or_insert(id);
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

#[cfg(test)]
mod tests {
    use super::{IdMapper, ManifestBlockEntry, WorldManifest, content_signature_hex};
    use crate::blocks::{BlockId, BlockRegistry, block_key_for_block};
    use crate::registry::NamespacedKey;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_manifest_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("mc_klon_manifest_{nanos}.json"))
    }

    fn key(raw: &str) -> NamespacedKey {
        NamespacedKey::parse(raw).expect("test key must parse")
    }

    #[test]
    fn every_runtime_block_round_trips_through_its_own_manifest() {
        let reg = BlockRegistry::new();
        let manifest = WorldManifest::build_from_registry(&reg);
        let mapper = IdMapper::from_manifest_and_registry(&manifest, &reg);

        for raw in 0u16..=255 {
            let Ok(block) = BlockId::from_u8(raw as u8) else {
                continue;
            };
            assert_eq!(
                mapper.decode_manifest_id(mapper.encode_runtime_block(block)),
                block,
                "block {block:?} did not survive the manifest round trip"
            );
        }
        assert_eq!(mapper.missing_blocks_count(), 0);
    }

    #[test]
    fn manifest_pins_legacy_runtime_ids_to_identical_manifest_ids() {
        // Pre-manifest saves stored raw runtime ids, so slots 0..=255 must keep
        // meaning the same block or every old column decodes to garbage.
        let reg = BlockRegistry::new();
        let manifest = WorldManifest::build_from_registry(&reg);
        for raw in 0u16..=255 {
            let Ok(block) = BlockId::from_u8(raw as u8) else {
                continue;
            };
            let entry = manifest
                .blocks
                .iter()
                .find(|entry| entry.id == raw as u32)
                .unwrap_or_else(|| panic!("manifest has no entry for id {raw}"));
            assert_eq!(entry.key, block_key_for_block(block).to_string());
        }
    }

    #[test]
    fn manifest_build_is_deterministic() {
        let reg = BlockRegistry::new();
        let a = WorldManifest::build_from_registry(&reg);
        let b = WorldManifest::build_from_registry(&reg);
        assert_eq!(a.content_signature, b.content_signature);
        let ids_a: Vec<_> = a.blocks.iter().map(|e| (e.id, e.key.as_str())).collect();
        let ids_b: Vec<_> = b.blocks.iter().map(|e| (e.id, e.key.as_str())).collect();
        assert_eq!(ids_a, ids_b);
    }

    #[test]
    fn saved_ids_are_remapped_by_key_not_by_slot() {
        // The whole point of the manifest: a save whose ids drifted away from
        // the current runtime order must still resolve to the right blocks.
        let reg = BlockRegistry::new();
        let manifest = WorldManifest {
            format_version: 1,
            game_version: "test".to_string(),
            content_signature: "deadbeefdeadbeef".to_string(),
            blocks: vec![
                ManifestBlockEntry {
                    id: 0,
                    key: "minecraft:air".to_string(),
                },
                ManifestBlockEntry {
                    id: 40,
                    key: "minecraft:stone".to_string(),
                },
                ManifestBlockEntry {
                    id: 41,
                    key: "minecraft:dirt".to_string(),
                },
                ManifestBlockEntry {
                    id: 42,
                    key: super::CORE_MISSING_BLOCK_KEY.to_string(),
                },
            ],
        };
        let mapper = IdMapper::from_manifest_and_registry(&manifest, &reg);

        assert_eq!(mapper.decode_manifest_id(40), BlockId::Stone);
        assert_eq!(mapper.decode_manifest_id(41), BlockId::Dirt);
        assert_eq!(mapper.decode_manifest_id(0), BlockId::Air);
        // Encoding writes the save's ids back, so a re-save keeps its own layout.
        assert_eq!(mapper.encode_runtime_block(BlockId::Stone), 40);
        assert_eq!(mapper.encode_runtime_block(BlockId::Dirt), 41);
        assert_eq!(mapper.missing_blocks_count(), 0);
    }

    #[test]
    fn aliased_saved_keys_encode_back_to_their_own_slot() {
        // `core:stone` is an accepted alias, so decoding it works. Encoding has
        // to land on the same id, or one save/load cycle turns the block into
        // the missing placeholder.
        let reg = BlockRegistry::new();
        let manifest = WorldManifest {
            format_version: 1,
            game_version: "test".to_string(),
            content_signature: "deadbeefdeadbeef".to_string(),
            blocks: vec![
                ManifestBlockEntry {
                    id: 0,
                    key: "minecraft:air".to_string(),
                },
                ManifestBlockEntry {
                    id: 7,
                    key: "core:stone".to_string(),
                },
                ManifestBlockEntry {
                    id: 8,
                    key: super::CORE_MISSING_BLOCK_KEY.to_string(),
                },
            ],
        };
        let mapper = IdMapper::from_manifest_and_registry(&manifest, &reg);

        assert_eq!(mapper.decode_manifest_id(7), BlockId::Stone);
        assert_eq!(mapper.encode_runtime_block(BlockId::Stone), 7);
    }

    #[test]
    fn unknown_saved_keys_degrade_to_missing_and_are_reported() {
        let reg = BlockRegistry::new();
        let manifest = WorldManifest {
            format_version: 1,
            game_version: "test".to_string(),
            content_signature: "deadbeefdeadbeef".to_string(),
            blocks: vec![
                ManifestBlockEntry {
                    id: 0,
                    key: "core:air".to_string(),
                },
                ManifestBlockEntry {
                    id: 1,
                    key: "somemod:unobtainium".to_string(),
                },
                ManifestBlockEntry {
                    id: 2,
                    key: super::CORE_MISSING_BLOCK_KEY.to_string(),
                },
            ],
        };
        let mapper = IdMapper::from_manifest_and_registry(&manifest, &reg);

        let missing_runtime = reg
            .block_for_key(&key(super::CORE_MISSING_BLOCK_KEY))
            .unwrap_or(BlockId::Air);
        assert_eq!(mapper.decode_manifest_id(1), missing_runtime);
        assert_eq!(mapper.missing_block_keys(), ["somemod:unobtainium"]);
        // Ids past the end of the table must not panic or wrap into a real block.
        assert_eq!(mapper.decode_manifest_id(9_999), missing_runtime);
        // Runtime blocks the save never knew about fall back to the same slot.
        assert_eq!(mapper.encode_runtime_block(BlockId::Stone), 2);
    }

    #[test]
    fn content_signature_tracks_the_key_set() {
        let base = [key("core:air"), key("core:dirt"), key("core:stone")];
        let same = [key("core:air"), key("core:dirt"), key("core:stone")];
        let added = [
            key("core:air"),
            key("core:dirt"),
            key("core:gravel"),
            key("core:stone"),
        ];

        assert_eq!(content_signature_hex(&base), content_signature_hex(&same));
        assert_ne!(content_signature_hex(&base), content_signature_hex(&added));
        assert_eq!(content_signature_hex(&base).len(), 16);
    }

    #[test]
    fn manifest_file_round_trips_and_rejects_drifted_headers() {
        let path = temp_manifest_path();
        let reg = BlockRegistry::new();
        let manifest = WorldManifest::build_from_registry(&reg);
        manifest.save(&path).unwrap();

        let loaded = WorldManifest::load(&path).unwrap();
        assert_eq!(loaded.content_signature, manifest.content_signature);
        assert_eq!(loaded.blocks.len(), manifest.blocks.len());
        assert_eq!(loaded.short_signature(), &manifest.content_signature[..8]);

        // A newer on-disk format must be refused rather than half-read.
        let bumped = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"format_version\": 1", "\"format_version\": 2");
        std::fs::write(&path, bumped).unwrap();
        let err = WorldManifest::load(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("format_version"),
            "expected a format_version rejection, got: {err}"
        );

        // `deny_unknown_fields` guards against silently ignoring new keys.
        std::fs::write(&path, "{\"format_version\":1,\"game_version\":\"x\",\"content_signature\":\"0\",\"blocks\":[],\"future\":1}").unwrap();
        assert!(WorldManifest::load(&path).is_err());

        let _ = std::fs::remove_file(&path);
    }
}
