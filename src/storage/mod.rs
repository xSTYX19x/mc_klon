use std::collections::{HashMap, VecDeque};
use std::fs;
use std::hash::Hash;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};

use crate::perf::trace as perf_trace;
use crate::storage::chunk_codec::{
    ColumnBlob, ColumnSaveCompletion, ColumnSaveSnapshot, apply_column, decode_column,
    encode_column, encode_column_snapshot,
};
use crate::storage::format::{
    REGION_CHUNK_SIDE, REGION_DIR_NAME, WORLD_MANIFEST_FILE_NAME, WORLD_META_FILE_NAME,
};
use crate::storage::region_file::RegionFile;
use crate::world::manifest::{IdMapper, WorldManifest};
use crate::world::{CHUNK_SIZE, World};

pub(crate) mod chunk_codec;
pub(crate) mod format;
pub(crate) mod region_file;
pub(crate) mod world_meta;

pub(crate) trait WorldStorage {
    fn load_column(&self, cx: i32, cz: i32) -> io::Result<Option<Vec<u8>>>;
    fn store_column(&self, cx: i32, cz: i32, bytes: &[u8]) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SaveMigrationReport {
    pub(crate) scanned_columns: usize,
    pub(crate) migrated_columns: usize,
    pub(crate) skipped_non_v1_columns: usize,
}

pub(crate) fn migrate_save_v1_to_v2(save_root: &Path) -> io::Result<SaveMigrationReport> {
    let world_meta_path = save_root.join(WORLD_META_FILE_NAME);
    let Some(meta) = world_meta::load(&world_meta_path)? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing world meta at {}", world_meta_path.display()),
        ));
    };
    let world_manifest_path = save_root.join(WORLD_MANIFEST_FILE_NAME);
    let manifest = WorldManifest::load(&world_manifest_path)?;
    let block_registry = crate::blocks::BlockRegistry::new();
    let mapper = IdMapper::from_manifest_and_registry(&manifest, &block_registry);

    let mut source_world = World::new();
    source_world.configure_world_config(
        crate::world::WorldConfig {
            seed: meta.world_seed,
            min_y: meta.min_y,
            max_y: meta.max_y,
            sea_level: meta.sea_level,
            chunk_size: meta.chunk_size,
            boundary_ocean: meta.boundary_ocean,
            boundary_radius_chunks: meta.boundary_radius_chunks,
            environment: meta.environment,
        },
        meta.preset,
    );
    source_world.set_manifest_and_mapper(manifest.clone(), mapper.clone());

    let mut verify_world = World::new();
    verify_world.configure_world_config(
        crate::world::WorldConfig {
            seed: meta.world_seed,
            min_y: meta.min_y,
            max_y: meta.max_y,
            sea_level: meta.sea_level,
            chunk_size: meta.chunk_size,
            boundary_ocean: meta.boundary_ocean,
            boundary_radius_chunks: meta.boundary_radius_chunks,
            environment: meta.environment,
        },
        meta.preset,
    );
    verify_world.set_manifest_and_mapper(manifest, mapper);

    let storage = RegionStorage::new(save_root.to_path_buf(), 64);
    let mut report = SaveMigrationReport::default();
    for (cx, cz) in discover_existing_columns(save_root)? {
        report.scanned_columns += 1;
        let Some(bytes) = storage.load_column(cx, cz)? else {
            continue;
        };
        let blob = decode_column(&bytes)?;
        if blob.format_version != 1 {
            report.skipped_non_v1_columns += 1;
            continue;
        }

        source_world.chunks.clear();
        apply_column(&mut source_world, blob)?;
        let encoded_v2 = encode_column(&source_world, cx, cz)?;
        storage.store_column(cx, cz, &encoded_v2)?;

        verify_world.chunks.clear();
        let reloaded = storage.load_column(cx, cz)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("missing column after migration at ({cx},{cz})"),
            )
        })?;
        let decoded_reloaded = decode_column(&reloaded)?;
        apply_column(&mut verify_world, decoded_reloaded)?;
        verify_column_equivalent(&source_world, &verify_world, cx, cz)?;

        report.migrated_columns += 1;
    }
    storage.flush()?;
    Ok(report)
}

fn discover_existing_columns(save_root: &Path) -> io::Result<Vec<(i32, i32)>> {
    let region_dir = save_root.join(REGION_DIR_NAME);
    if !region_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(region_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Some((rx, rz)) = parse_region_name(&file_name) else {
            continue;
        };
        let storage = RegionStorage::new(save_root.to_path_buf(), 4);
        for lz in 0..REGION_CHUNK_SIDE {
            for lx in 0..REGION_CHUNK_SIDE {
                let cx = rx * REGION_CHUNK_SIDE + lx;
                let cz = rz * REGION_CHUNK_SIDE + lz;
                if storage.column_exists(cx, cz)? {
                    out.push((cx, cz));
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn parse_region_name(file_name: &str) -> Option<(i32, i32)> {
    if !file_name.starts_with("r.") || !file_name.ends_with(".mca") {
        return None;
    }
    let stem = file_name.strip_suffix(".mca")?;
    let mut parts = stem.split('.');
    let tag = parts.next()?;
    if tag != "r" {
        return None;
    }
    let rx = parts.next()?.parse::<i32>().ok()?;
    let rz = parts.next()?.parse::<i32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((rx, rz))
}

fn verify_column_equivalent(source: &World, restored: &World, cx: i32, cz: i32) -> io::Result<()> {
    for y in source.min_y()..=source.max_y() {
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE + lx;
                let wz = cz * CHUNK_SIZE + lz;
                let pos = glam::IVec3::new(wx, y, wz);
                if source.get_block(pos) != restored.get_block(pos) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("verification failed block mismatch at {pos:?}"),
                    ));
                }
                if source.get_block_data(pos) != restored.get_block_data(pos) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("verification failed block_data mismatch at {pos:?}"),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct LruCache<K, V> {
    cap: usize,
    entries: HashMap<K, V>,
    order: VecDeque<K>, // front = least recently used
}

impl<K: Eq + Hash + Clone, V> LruCache<K, V> {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.clone());
    }

    pub(crate) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        if self.entries.contains_key(key) {
            self.touch(key);
        }
        self.entries.get_mut(key)
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        if self.entries.contains_key(&key) {
            self.entries.insert(key.clone(), value);
            self.touch(&key);
            return None;
        }

        self.entries.insert(key.clone(), value);
        self.order.push_back(key);

        if self.entries.len() <= self.cap {
            return None;
        }

        while let Some(old_key) = self.order.pop_front() {
            if let Some(old_value) = self.entries.remove(&old_key) {
                return Some((old_key, old_value));
            }
        }

        None
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.values_mut()
    }
}

pub(crate) struct RegionStorage {
    pub(crate) root: PathBuf,
    pub(crate) open_cache: Mutex<LruCache<(i32, i32), RegionFile>>,
}

impl RegionStorage {
    pub(crate) fn new(root: PathBuf, cache_capacity: usize) -> Self {
        Self {
            root,
            open_cache: Mutex::new(LruCache::new(cache_capacity)),
        }
    }

    fn region_dir(&self) -> PathBuf {
        self.root.join(REGION_DIR_NAME)
    }

    fn with_region_file<T>(
        &self,
        cx: i32,
        cz: i32,
        f: impl FnOnce(&mut RegionFile) -> io::Result<T>,
    ) -> io::Result<T> {
        let (rx, rz) = format::region_coords_for_chunk(cx, cz);
        let key = (rx, rz);
        let mut cache = self
            .open_cache
            .lock()
            .map_err(|_| io::Error::other("region storage cache lock poisoned"))?;

        if cache.get_mut(&key).is_none() {
            let region = RegionFile::open(&self.region_dir(), rx, rz)?;
            if let Some((_evicted_key, mut evicted)) = cache.insert(key, region) {
                let _ = evicted.flush();
            }
        }

        let region = cache
            .get_mut(&key)
            .ok_or_else(|| io::Error::other("region file missing after cache insert"))?;
        f(region)
    }

    pub(crate) fn column_exists(&self, cx: i32, cz: i32) -> io::Result<bool> {
        self.with_region_file(cx, cz, |region| region.has_column(cx, cz))
    }
}

impl WorldStorage for RegionStorage {
    fn load_column(&self, cx: i32, cz: i32) -> io::Result<Option<Vec<u8>>> {
        self.with_region_file(cx, cz, |region| region.read_column(cx, cz))
    }

    fn store_column(&self, cx: i32, cz: i32, bytes: &[u8]) -> io::Result<()> {
        self.with_region_file(cx, cz, |region| region.write_column(cx, cz, bytes))
    }

    fn flush(&self) -> io::Result<()> {
        let mut cache = self
            .open_cache
            .lock()
            .map_err(|_| io::Error::other("region storage cache lock poisoned"))?;
        for region in cache.values_mut() {
            region.flush()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum SaveTask {
    StoreColumn { cx: i32, cz: i32, bytes: Vec<u8> },
    EncodeAndStoreColumn { snapshot: ColumnSaveSnapshot },
    Flush { done: Sender<io::Result<()>> },
    Nudge,
    Shutdown,
}

#[derive(Debug)]
enum CoalescedColumnTask {
    StoreBytes(Vec<u8>),
    EncodeAndStore(ColumnSaveSnapshot),
}

#[derive(Debug)]
pub(crate) struct StorageColumnSaveResult {
    pub(crate) completion: ColumnSaveCompletion,
    pub(crate) result: Result<(), String>,
}

#[derive(Debug)]
pub(crate) enum StorageColumnLoadStatus {
    Hit(ColumnBlob),
    Miss,
    Error(String),
}

#[derive(Debug)]
pub(crate) struct StorageColumnLoadResult {
    pub(crate) cx: i32,
    pub(crate) cz: i32,
    pub(crate) status: StorageColumnLoadStatus,
}

#[derive(Debug)]
enum LoadTask {
    LoadColumn { cx: i32, cz: i32 },
    Shutdown,
}

pub(crate) struct StorageLoadWorker {
    load_tx: Sender<LoadTask>,
    last_error: Arc<Mutex<Option<String>>>,
    load_result_rx: Receiver<StorageColumnLoadResult>,
    handle: Option<JoinHandle<()>>,
}

impl StorageLoadWorker {
    pub(crate) const DEFAULT_QUEUE_CAPACITY: usize = 1024;
    pub(crate) const DEFAULT_REGION_CACHE_CAPACITY: usize = 64;

    pub(crate) fn start(root: PathBuf) -> io::Result<Self> {
        Self::start_with(
            root,
            Self::DEFAULT_REGION_CACHE_CAPACITY,
            Self::DEFAULT_QUEUE_CAPACITY,
        )
    }

    pub(crate) fn start_with(
        root: PathBuf,
        cache_capacity: usize,
        queue_capacity: usize,
    ) -> io::Result<Self> {
        let storage = RegionStorage::new(root, cache_capacity);
        let (load_tx, load_rx) = crossbeam_channel::bounded(queue_capacity.max(1));
        let (load_result_tx, load_result_rx) = crossbeam_channel::unbounded();
        let last_error = Arc::new(Mutex::new(None));
        let last_error_worker = Arc::clone(&last_error);

        let handle = thread::Builder::new()
            .name("storage-reader".to_string())
            .spawn(move || {
                storage_load_worker_loop(storage, load_rx, load_result_tx, last_error_worker)
            })
            .map_err(|err| {
                io::Error::other(format!("failed to spawn storage load worker: {err}"))
            })?;

        Ok(Self {
            load_tx,
            last_error,
            load_result_rx,
            handle: Some(handle),
        })
    }

    pub(crate) fn enqueue_load_column(&self, cx: i32, cz: i32) -> io::Result<()> {
        self.check_worker_error()?;
        self.load_tx
            .try_send(LoadTask::LoadColumn { cx, cz })
            .map_err(|err| match err {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "storage load queue is full")
                }
                TrySendError::Disconnected(_) => io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "storage load worker channel disconnected",
                ),
            })
    }

    pub(crate) fn queue_len_approx(&self) -> usize {
        self.load_tx.len()
    }

    pub(crate) fn result_queue_len_approx(&self) -> usize {
        self.load_result_rx.len()
    }

    pub(crate) fn try_recv_result(&self) -> Option<StorageColumnLoadResult> {
        match self.load_result_rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    fn check_worker_error(&self) -> io::Result<()> {
        let maybe_err = self
            .last_error
            .lock()
            .map_err(|_| io::Error::other("storage load worker error lock poisoned"))?;
        if let Some(msg) = maybe_err.as_ref() {
            return Err(io::Error::other(format!(
                "storage load worker error: {msg}"
            )));
        }
        Ok(())
    }
}

impl Drop for StorageLoadWorker {
    fn drop(&mut self) {
        let _ = self.load_tx.send(LoadTask::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) struct StorageWorker {
    pub(crate) save_tx: Sender<SaveTask>,
    coalesced: Arc<Mutex<HashMap<(i32, i32), CoalescedColumnTask>>>,
    last_error: Arc<Mutex<Option<String>>>,
    save_result_rx: Receiver<StorageColumnSaveResult>,
    handle: Option<JoinHandle<()>>,
}

impl StorageWorker {
    pub(crate) const DEFAULT_QUEUE_CAPACITY: usize = 1024;
    pub(crate) const DEFAULT_REGION_CACHE_CAPACITY: usize = 64;

    pub(crate) fn start(root: PathBuf) -> io::Result<Self> {
        Self::start_with(
            root,
            Self::DEFAULT_REGION_CACHE_CAPACITY,
            Self::DEFAULT_QUEUE_CAPACITY,
        )
    }

    pub(crate) fn start_with(
        root: PathBuf,
        cache_capacity: usize,
        queue_capacity: usize,
    ) -> io::Result<Self> {
        let storage = RegionStorage::new(root, cache_capacity);
        let (save_tx, save_rx) = crossbeam_channel::bounded(queue_capacity.max(1));
        let (save_result_tx, save_result_rx) = crossbeam_channel::unbounded();
        let coalesced = Arc::new(Mutex::new(HashMap::new()));
        let coalesced_worker = Arc::clone(&coalesced);
        let last_error = Arc::new(Mutex::new(None));
        let last_error_worker = Arc::clone(&last_error);

        let handle = thread::Builder::new()
            .name("storage-writer".to_string())
            .spawn(move || {
                storage_worker_loop(
                    storage,
                    save_rx,
                    save_result_tx,
                    coalesced_worker,
                    last_error_worker,
                )
            })
            .map_err(|err| io::Error::other(format!("failed to spawn storage worker: {err}")))?;

        Ok(Self {
            save_tx,
            coalesced,
            last_error,
            save_result_rx,
            handle: Some(handle),
        })
    }

    pub(crate) fn enqueue_store_column(&self, cx: i32, cz: i32, bytes: Vec<u8>) -> io::Result<()> {
        self.check_worker_error()?;
        match self
            .save_tx
            .try_send(SaveTask::StoreColumn { cx, cz, bytes })
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(SaveTask::StoreColumn { cx, cz, bytes })) => {
                let mut map = self
                    .coalesced
                    .lock()
                    .map_err(|_| io::Error::other("storage coalesced queue lock poisoned"))?;
                map.insert((cx, cz), CoalescedColumnTask::StoreBytes(bytes));
                drop(map);
                let _ = self.save_tx.try_send(SaveTask::Nudge);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "storage worker channel disconnected",
            )),
            Err(TrySendError::Full(_other)) => Err(io::Error::other(
                "unexpected non-store task returned from try_send",
            )),
        }
    }

    pub(crate) fn enqueue_encode_and_store_column(
        &self,
        snapshot: ColumnSaveSnapshot,
    ) -> io::Result<()> {
        let cx = snapshot.cx;
        let cz = snapshot.cz;
        self.check_worker_error()?;
        match self
            .save_tx
            .try_send(SaveTask::EncodeAndStoreColumn { snapshot })
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(SaveTask::EncodeAndStoreColumn { snapshot })) => {
                let mut map = self
                    .coalesced
                    .lock()
                    .map_err(|_| io::Error::other("storage coalesced queue lock poisoned"))?;
                map.insert((cx, cz), CoalescedColumnTask::EncodeAndStore(snapshot));
                drop(map);
                let _ = self.save_tx.try_send(SaveTask::Nudge);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "storage worker channel disconnected",
            )),
            Err(TrySendError::Full(_other)) => Err(io::Error::other(
                "unexpected non-encode task returned from try_send",
            )),
        }
    }

    pub(crate) fn queue_len_approx(&self) -> usize {
        self.save_tx.len()
    }

    pub(crate) fn save_result_queue_len_approx(&self) -> usize {
        self.save_result_rx.len()
    }

    pub(crate) fn try_recv_save_result(&self) -> Option<StorageColumnSaveResult> {
        match self.save_result_rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    fn check_worker_error(&self) -> io::Result<()> {
        let maybe_err = self
            .last_error
            .lock()
            .map_err(|_| io::Error::other("storage worker error lock poisoned"))?;
        if let Some(msg) = maybe_err.as_ref() {
            return Err(io::Error::other(format!("storage worker error: {msg}")));
        }
        Ok(())
    }
}

impl WorldStorage for StorageWorker {
    fn load_column(&self, _cx: i32, _cz: i32) -> io::Result<Option<Vec<u8>>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "StorageWorker is write-only for load_column; use RegionStorage on main thread",
        ))
    }

    fn store_column(&self, cx: i32, cz: i32, bytes: &[u8]) -> io::Result<()> {
        self.enqueue_store_column(cx, cz, bytes.to_vec())
    }

    fn flush(&self) -> io::Result<()> {
        self.check_worker_error()?;
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        self.save_tx
            .send(SaveTask::Flush { done: done_tx })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "storage worker channel disconnected",
                )
            })?;
        done_rx.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "storage worker flush ack channel disconnected",
            )
        })?
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        let _ = self.save_tx.send(SaveTask::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn storage_load_worker_loop(
    storage: RegionStorage,
    load_rx: Receiver<LoadTask>,
    load_result_tx: Sender<StorageColumnLoadResult>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    while let Ok(task) = load_rx.recv() {
        match task {
            LoadTask::LoadColumn { cx, cz } => {
                process_load_task(&storage, &load_result_tx, cx, cz, &last_error);
            }
            LoadTask::Shutdown => break,
        }
    }
}

fn process_load_task(
    storage: &RegionStorage,
    load_result_tx: &Sender<StorageColumnLoadResult>,
    cx: i32,
    cz: i32,
    last_error: &Arc<Mutex<Option<String>>>,
) {
    let total_started = Instant::now();

    let exists_started = Instant::now();
    let exists = match storage.column_exists(cx, cz) {
        Ok(exists) => exists,
        Err(err) => {
            let msg = format!("exists ({cx},{cz}) failed: {err}");
            record_worker_error(last_error, msg.clone());
            let _ = load_result_tx.send(StorageColumnLoadResult {
                cx,
                cz,
                status: StorageColumnLoadStatus::Error(msg),
            });
            return;
        }
    };
    let exists_ms = exists_started.elapsed().as_secs_f32() * 1000.0;
    perf_trace::record_duration_ms(
        "saved_load.check_exists",
        exists_ms,
        &format!("cx={} cz={} exists={}", cx, cz, exists),
    );

    if !exists {
        let _ = load_result_tx.send(StorageColumnLoadResult {
            cx,
            cz,
            status: StorageColumnLoadStatus::Miss,
        });
        perf_trace::record_duration_ms(
            "saved_load.worker_total",
            total_started.elapsed().as_secs_f32() * 1000.0,
            &format!("cx={} cz={} miss=true", cx, cz),
        );
        return;
    }

    let read_started = Instant::now();
    let maybe_bytes = match storage.load_column(cx, cz) {
        Ok(bytes) => bytes,
        Err(err) => {
            let msg = format!("read ({cx},{cz}) failed: {err}");
            record_worker_error(last_error, msg.clone());
            let _ = load_result_tx.send(StorageColumnLoadResult {
                cx,
                cz,
                status: StorageColumnLoadStatus::Error(msg),
            });
            return;
        }
    };
    let read_ms = read_started.elapsed().as_secs_f32() * 1000.0;
    perf_trace::record_duration_ms(
        "saved_load.read_file",
        read_ms,
        &format!("cx={} cz={} hit={}", cx, cz, maybe_bytes.is_some()),
    );

    let Some(bytes) = maybe_bytes else {
        let _ = load_result_tx.send(StorageColumnLoadResult {
            cx,
            cz,
            status: StorageColumnLoadStatus::Miss,
        });
        perf_trace::record_duration_ms(
            "saved_load.worker_total",
            total_started.elapsed().as_secs_f32() * 1000.0,
            &format!("cx={} cz={} miss_after_read=true", cx, cz),
        );
        return;
    };

    let decode_started = Instant::now();
    let blob = match decode_column(&bytes) {
        Ok(blob) => blob,
        Err(err) => {
            let msg = format!("decode ({cx},{cz}) failed: {err}");
            record_worker_error(last_error, msg.clone());
            let _ = load_result_tx.send(StorageColumnLoadResult {
                cx,
                cz,
                status: StorageColumnLoadStatus::Error(msg),
            });
            return;
        }
    };
    let decode_ms = decode_started.elapsed().as_secs_f32() * 1000.0;
    perf_trace::record_duration_ms(
        "saved_load.decode",
        decode_ms,
        &format!("cx={} cz={}", cx, cz),
    );

    let _ = load_result_tx.send(StorageColumnLoadResult {
        cx,
        cz,
        status: StorageColumnLoadStatus::Hit(blob),
    });
    perf_trace::record_duration_ms(
        "saved_load.worker_total",
        total_started.elapsed().as_secs_f32() * 1000.0,
        &format!("cx={} cz={} miss=false", cx, cz),
    );
}

fn storage_worker_loop(
    storage: RegionStorage,
    save_rx: Receiver<SaveTask>,
    save_result_tx: Sender<StorageColumnSaveResult>,
    coalesced: Arc<Mutex<HashMap<(i32, i32), CoalescedColumnTask>>>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    let mut running = true;
    while running {
        match save_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(task) => {
                if !process_task(
                    task,
                    &storage,
                    &save_rx,
                    &save_result_tx,
                    &coalesced,
                    &last_error,
                ) {
                    running = false;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let _ = drain_coalesced(&storage, &save_result_tx, &coalesced, &last_error);
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = drain_coalesced(&storage, &save_result_tx, &coalesced, &last_error);
    let _ = storage.flush();
}

fn process_task(
    task: SaveTask,
    storage: &RegionStorage,
    save_rx: &Receiver<SaveTask>,
    save_result_tx: &Sender<StorageColumnSaveResult>,
    coalesced: &Arc<Mutex<HashMap<(i32, i32), CoalescedColumnTask>>>,
    last_error: &Arc<Mutex<Option<String>>>,
) -> bool {
    match task {
        SaveTask::StoreColumn { cx, cz, bytes } => {
            if let Err(err) = storage.store_column(cx, cz, &bytes) {
                record_worker_error(last_error, format!("store ({cx},{cz}) failed: {err}"));
            }
            // Opportunistically batch queued writes after a store.
            drain_pending_messages(storage, save_rx, save_result_tx, coalesced, last_error);
            let _ = drain_coalesced(storage, save_result_tx, coalesced, last_error);
            true
        }
        SaveTask::EncodeAndStoreColumn { snapshot } => {
            process_encode_and_store_task(storage, save_result_tx, snapshot, last_error);
            drain_pending_messages(storage, save_rx, save_result_tx, coalesced, last_error);
            let _ = drain_coalesced(storage, save_result_tx, coalesced, last_error);
            true
        }
        SaveTask::Flush { done } => {
            drain_pending_messages(storage, save_rx, save_result_tx, coalesced, last_error);
            let _ = drain_coalesced(storage, save_result_tx, coalesced, last_error);
            let result = match get_worker_error(last_error) {
                Some(msg) => Err(io::Error::other(msg)),
                None => storage.flush(),
            };
            let _ = done.send(result);
            true
        }
        SaveTask::Nudge => {
            let _ = drain_coalesced(storage, save_result_tx, coalesced, last_error);
            true
        }
        SaveTask::Shutdown => false,
    }
}

fn drain_pending_messages(
    storage: &RegionStorage,
    save_rx: &Receiver<SaveTask>,
    save_result_tx: &Sender<StorageColumnSaveResult>,
    coalesced: &Arc<Mutex<HashMap<(i32, i32), CoalescedColumnTask>>>,
    last_error: &Arc<Mutex<Option<String>>>,
) {
    loop {
        match save_rx.try_recv() {
            Ok(SaveTask::StoreColumn { cx, cz, bytes }) => {
                if let Err(err) = storage.store_column(cx, cz, &bytes) {
                    record_worker_error(last_error, format!("store ({cx},{cz}) failed: {err}"));
                }
            }
            Ok(SaveTask::EncodeAndStoreColumn { snapshot }) => {
                process_encode_and_store_task(storage, save_result_tx, snapshot, last_error);
            }
            Ok(SaveTask::Nudge) => {
                let _ = drain_coalesced(storage, save_result_tx, coalesced, last_error);
            }
            Ok(SaveTask::Flush { done }) => {
                let _ = drain_coalesced(storage, save_result_tx, coalesced, last_error);
                let result = match get_worker_error(last_error) {
                    Some(msg) => Err(io::Error::other(msg)),
                    None => storage.flush(),
                };
                let _ = done.send(result);
            }
            Ok(SaveTask::Shutdown) => {
                // Best effort: stop processing additional messages.
                break;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn drain_coalesced(
    storage: &RegionStorage,
    save_result_tx: &Sender<StorageColumnSaveResult>,
    coalesced: &Arc<Mutex<HashMap<(i32, i32), CoalescedColumnTask>>>,
    last_error: &Arc<Mutex<Option<String>>>,
) -> io::Result<()> {
    let pending = {
        let mut guard = coalesced
            .lock()
            .map_err(|_| io::Error::other("storage coalesced queue lock poisoned"))?;
        std::mem::take(&mut *guard)
    };
    for ((cx, cz), task) in pending {
        match task {
            CoalescedColumnTask::StoreBytes(bytes) => {
                if let Err(err) = storage.store_column(cx, cz, &bytes) {
                    record_worker_error(
                        last_error,
                        format!("coalesced store ({cx},{cz}) failed: {err}"),
                    );
                }
            }
            CoalescedColumnTask::EncodeAndStore(snapshot) => {
                process_encode_and_store_task(storage, save_result_tx, snapshot, last_error);
            }
        }
    }
    Ok(())
}

fn process_encode_and_store_task(
    storage: &RegionStorage,
    save_result_tx: &Sender<StorageColumnSaveResult>,
    snapshot: ColumnSaveSnapshot,
    last_error: &Arc<Mutex<Option<String>>>,
) {
    let completion = snapshot.completion_meta();
    let result = match encode_column_snapshot(&snapshot) {
        Ok(bytes) => match storage.store_column(snapshot.cx, snapshot.cz, &bytes) {
            Ok(()) => Ok(()),
            Err(err) => {
                let msg = format!("store ({},{}) failed: {}", snapshot.cx, snapshot.cz, err);
                record_worker_error(last_error, msg.clone());
                Err(msg)
            }
        },
        Err(err) => {
            let msg = format!("encode ({},{}) failed: {}", snapshot.cx, snapshot.cz, err);
            record_worker_error(last_error, msg.clone());
            Err(msg)
        }
    };
    let _ = save_result_tx.send(StorageColumnSaveResult { completion, result });
}

fn record_worker_error(last_error: &Arc<Mutex<Option<String>>>, msg: String) {
    crate::console_error!("[storage][error] {msg}");
    if let Ok(mut guard) = last_error.lock() {
        *guard = Some(msg);
    }
}

fn get_worker_error(last_error: &Arc<Mutex<Option<String>>>) -> Option<String> {
    last_error.lock().ok().and_then(|guard| guard.clone())
}

#[cfg(test)]
mod tests {
    use super::{LruCache, RegionStorage, WorldStorage};
    use crate::blocks::BlockId;
    use crate::storage::chunk_codec::{apply_column, decode_column, encode_column};
    use crate::world::{CHUNK_SIZE, ChunkCoord, World};
    use crate::worldgen::WorldgenPreset;
    use glam::IVec3;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::io;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn lru_cache_evicts_oldest() {
        let mut lru = LruCache::new(2);
        assert!(lru.insert(1, "a").is_none());
        assert!(lru.insert(2, "b").is_none());
        let _ = lru.get_mut(&1);
        let evicted = lru.insert(3, "c").unwrap();
        assert_eq!(evicted, (2, "b"));
        assert!(lru.get_mut(&1).is_some());
        assert!(lru.get_mut(&3).is_some());
    }

    #[test]
    fn region_storage_constructs() {
        let root = unique_tmp_dir("region_storage_constructs");
        let storage = RegionStorage::new(root.clone(), 4);
        let _ = <RegionStorage as WorldStorage>::flush(&storage);
        let _ = storage.load_column(0, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn region_storage_reopen_roundtrip_random_columns_preserves_bytes_and_decoded_columns() {
        let root = unique_tmp_dir("region_storage_roundtrip_random");
        let mut rng = TestRng::new(0x51A7_0A62_AAA1_0001);
        let mut expected_bytes: HashMap<(i32, i32), Vec<u8>> = HashMap::new();
        let mut expected_decoded = HashMap::new();

        {
            let storage = RegionStorage::new(root.clone(), 8);
            for _ in 0..14 {
                let cx = rng.range_i32(-70, 70);
                let cz = rng.range_i32(-70, 70);
                let mut world = World::new_with_seed_and_preset(123_456_789, WorldgenPreset::Hilly);
                populate_random_column(&mut world, cx, cz, &mut rng, 900);
                let bytes = encode_column(&world, cx, cz).unwrap();
                expected_decoded.insert((cx, cz), decode_column(&bytes).unwrap());
                storage.store_column(cx, cz, &bytes).unwrap();
                expected_bytes.insert((cx, cz), bytes);
            }
            storage.flush().unwrap();
        }

        {
            let storage = RegionStorage::new(root.clone(), 8);
            for (&(cx, cz), expected) in &expected_bytes {
                let loaded = storage.load_column(cx, cz).unwrap().unwrap();
                assert_eq!(
                    &loaded, expected,
                    "raw bytes mismatch for column ({cx},{cz})"
                );
                let decoded = decode_column(&loaded).unwrap();
                assert_eq!(
                    decoded,
                    expected_decoded.get(&(cx, cz)).unwrap().clone(),
                    "decoded column mismatch for ({cx},{cz})"
                );
            }
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn world_save_reload_restores_blocks_and_block_data_at_random_positions() {
        let root = unique_tmp_dir("world_restore_random");
        let storage = RegionStorage::new(root.clone(), 16);
        let mut rng = TestRng::new(0xA11C_0BAD_F00D_2026);
        let seed = 0x0123_4567_89AB_CDEF;
        let preset = WorldgenPreset::Rd161348Like;

        let target_columns = vec![(-2, -1), (-1, 0), (0, 0), (1, 1), (2, -2), (3, 2)];

        let mut world = World::new_with_seed_and_preset(seed, preset);
        for &(cx, cz) in &target_columns {
            world.ensure_column_generated(cx * CHUNK_SIZE, cz * CHUNK_SIZE);
        }

        let mut saved_revision: HashMap<ChunkCoord, u64> = HashMap::new();
        let first_saved = save_dirty_columns(&world, &storage, &mut saved_revision).unwrap();
        assert_eq!(first_saved, target_columns.len());
        assert_eq!(
            save_dirty_columns(&world, &storage, &mut saved_revision).unwrap(),
            0,
            "second save without mutations must be a no-op"
        );

        for _ in 0..1800 {
            let (cx, cz) = target_columns[rng.range_usize(0, target_columns.len() - 1)];
            let x = rng.range_i32(0, CHUNK_SIZE - 1);
            let y = rng.range_i32(world.min_y(), world.max_y());
            let z = rng.range_i32(0, CHUNK_SIZE - 1);
            let wx = cx * CHUNK_SIZE + x;
            let wz = cz * CHUNK_SIZE + z;
            let block = if rng.one_in(5) {
                if rng.one_in(2) {
                    BlockId::WaterStill
                } else {
                    BlockId::LavaFlowing
                }
            } else {
                random_non_air_block_id(&mut rng)
            };
            let data = rng.next_u8();
            world
                .set_block_and_data(IVec3::new(wx, y, wz), block, data)
                .unwrap();
        }

        let changed_columns = save_dirty_columns(&world, &storage, &mut saved_revision).unwrap();
        assert!(
            (1..=target_columns.len()).contains(&changed_columns),
            "expected some dirty columns saved, got {}",
            changed_columns
        );
        storage.flush().unwrap();

        let storage_reload = RegionStorage::new(root.clone(), 16);
        let mut restored = World::new_with_seed_and_preset(seed, preset);
        for &(cx, cz) in &target_columns {
            let bytes = storage_reload
                .load_column(cx, cz)
                .unwrap()
                .unwrap_or_else(|| panic!("missing saved column ({cx},{cz})"));
            let blob = decode_column(&bytes).unwrap();
            apply_column(&mut restored, blob).unwrap();
        }

        for _ in 0..5000 {
            let (cx, cz) = target_columns[rng.range_usize(0, target_columns.len() - 1)];
            let x = rng.range_i32(0, CHUNK_SIZE - 1);
            let y = rng.range_i32(world.min_y(), world.max_y());
            let z = rng.range_i32(0, CHUNK_SIZE - 1);
            let wx = cx * CHUNK_SIZE + x;
            let wz = cz * CHUNK_SIZE + z;
            let pos = IVec3::new(wx, y, wz);
            assert_eq!(
                restored.get_block(pos),
                world.get_block(pos),
                "block mismatch at {pos:?}"
            );
            assert_eq!(
                restored.get_block_data(pos),
                world.get_block_data(pos),
                "block_data mismatch at {pos:?}"
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    fn save_dirty_columns(
        world: &World,
        storage: &RegionStorage,
        saved_revision: &mut HashMap<ChunkCoord, u64>,
    ) -> io::Result<usize> {
        let mut columns = HashSet::new();
        for coord in world.chunks.keys() {
            columns.insert((coord.x, coord.z));
        }

        let mut saved_count = 0usize;
        for (cx, cz) in columns {
            if !column_is_dirty(world, saved_revision, cx, cz) {
                continue;
            }
            let bytes = encode_column(world, cx, cz)?;
            storage.store_column(cx, cz, &bytes)?;
            refresh_saved_revision_for_column(world, saved_revision, cx, cz);
            saved_count += 1;
        }

        Ok(saved_count)
    }

    fn column_is_dirty(
        world: &World,
        saved_revision: &HashMap<ChunkCoord, u64>,
        cx: i32,
        cz: i32,
    ) -> bool {
        for cy in world.min_chunk_y()..=world.max_chunk_y() {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            match world.get_chunk(coord) {
                Some(chunk) => {
                    if saved_revision.get(&coord).copied() != Some(chunk.revision) {
                        return true;
                    }
                }
                None => {
                    if saved_revision.contains_key(&coord) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn refresh_saved_revision_for_column(
        world: &World,
        saved_revision: &mut HashMap<ChunkCoord, u64>,
        cx: i32,
        cz: i32,
    ) {
        for cy in world.min_chunk_y()..=world.max_chunk_y() {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            if let Some(chunk) = world.get_chunk(coord) {
                saved_revision.insert(coord, chunk.revision);
            } else {
                saved_revision.remove(&coord);
            }
        }
    }

    fn populate_random_column(
        world: &mut World,
        cx: i32,
        cz: i32,
        rng: &mut TestRng,
        writes: usize,
    ) {
        for _ in 0..writes {
            let x = rng.range_i32(0, CHUNK_SIZE - 1);
            let y = rng.range_i32(world.min_y(), world.max_y());
            let z = rng.range_i32(0, CHUNK_SIZE - 1);
            let wx = cx * CHUNK_SIZE + x;
            let wz = cz * CHUNK_SIZE + z;
            let block = if rng.one_in(6) {
                BlockId::Air
            } else if rng.one_in(7) {
                if rng.one_in(2) {
                    BlockId::WaterFlowing
                } else {
                    BlockId::LavaStill
                }
            } else {
                random_non_air_block_id(rng)
            };
            let data = rng.next_u8();
            world
                .set_block_and_data(IVec3::new(wx, y, wz), block, data)
                .unwrap();
        }
    }

    fn random_non_air_block_id(rng: &mut TestRng) -> BlockId {
        loop {
            if let Ok(id) = BlockId::from_u8(rng.next_u8() % 14) {
                if id != BlockId::Air {
                    return id;
                }
            }
        }
    }

    fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("mc_klon_{tag}_{nanos}"))
    }

    struct TestRng {
        state: u64,
    }

    impl TestRng {
        fn new(seed: u64) -> Self {
            Self { state: seed.max(1) }
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.state = x;
            x
        }

        fn next_u8(&mut self) -> u8 {
            self.next_u64() as u8
        }

        fn one_in(&mut self, n: u32) -> bool {
            debug_assert!(n > 0);
            (self.next_u64() % n as u64) == 0
        }

        fn range_i32(&mut self, lo: i32, hi: i32) -> i32 {
            debug_assert!(lo <= hi);
            let span = (hi - lo + 1) as u64;
            lo + (self.next_u64() % span) as i32
        }

        fn range_usize(&mut self, lo: usize, hi: usize) -> usize {
            debug_assert!(lo <= hi);
            let span = (hi - lo + 1) as u64;
            lo + (self.next_u64() % span) as usize
        }
    }
}
