//! "Run this in N ticks": a timing wheel shared by the fluids (one per fluid
//! kind, keyed by position) and the block updates (keyed by position and
//! block, `block_updates_system::BlockTick`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use glam::IVec3;

const SCHEDULER_WHEEL_SIZE: usize = 512;

/// What a scheduler entry is about. The position is what pruning by column
/// looks at.
pub(crate) trait TickKey: Copy + Eq + Hash {
    fn pos(&self) -> IVec3;
}

impl TickKey for IVec3 {
    fn pos(&self) -> IVec3 {
        *self
    }
}

/// Entries sit in the wheel bucket of their due tick; `collect_ready` must run
/// once for every tick, in order, so no bucket is skipped. A key is pending at
/// most once: scheduling it again keeps the earlier due tick.
pub(crate) struct TickScheduler<K> {
    wheel: Vec<Vec<K>>,
    ready: VecDeque<K>,
    ready_set: HashSet<K>,
    /// Every pending key and its due tick. Visible in `world` because the
    /// fluid tests compare what is scheduled.
    pub(super) scheduled_due: HashMap<K, u64>,
    scratch: Vec<K>,
}

impl<K: TickKey> TickScheduler<K> {
    pub(crate) fn new() -> Self {
        Self {
            wheel: vec![Vec::new(); SCHEDULER_WHEEL_SIZE],
            ready: VecDeque::new(),
            ready_set: HashSet::new(),
            scheduled_due: HashMap::new(),
            scratch: Vec::with_capacity(512),
        }
    }

    pub(crate) fn clear(&mut self) {
        for bucket in &mut self.wheel {
            bucket.clear();
        }
        self.ready.clear();
        self.ready_set.clear();
        self.scheduled_due.clear();
        self.scratch.clear();
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.ready.is_empty() || !self.scheduled_due.is_empty()
    }

    pub(crate) fn schedule(&mut self, key: K, due_tick: u64) {
        match self.scheduled_due.get(&key).copied() {
            Some(existing_due) if existing_due <= due_tick => (),
            _ => {
                self.scheduled_due.insert(key, due_tick);
                let bucket = (due_tick as usize) % self.wheel.len();
                self.wheel[bucket].push(key);
            }
        }
    }

    /// Moves the entries due at `current_tick` to the ready queue.
    pub(crate) fn collect_ready(&mut self, current_tick: u64) {
        let bucket_index = (current_tick as usize) % self.wheel.len();
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.append(&mut self.wheel[bucket_index]);

        for key in scratch.drain(..) {
            let Some(&due_tick) = self.scheduled_due.get(&key) else {
                continue;
            };

            if due_tick == current_tick {
                if self.ready_set.insert(key) {
                    self.ready.push_back(key);
                }
            } else {
                let target_bucket = (due_tick as usize) % self.wheel.len();
                self.wheel[target_bucket].push(key);
            }
        }

        self.scratch = scratch;
    }

    pub(crate) fn pop_ready(&mut self) -> Option<K> {
        let key = self.ready.pop_front()?;
        self.ready_set.remove(&key);
        self.scheduled_due.remove(&key);
        Some(key)
    }

    pub(crate) fn retain_positions<F: Fn(IVec3) -> bool>(&mut self, keep: F) {
        for bucket in &mut self.wheel {
            bucket.retain(|key| keep(key.pos()));
        }

        let mut filtered_ready = VecDeque::with_capacity(self.ready.len());
        while let Some(key) = self.ready.pop_front() {
            if keep(key.pos()) {
                filtered_ready.push_back(key);
            }
        }
        self.ready = filtered_ready;
        self.ready_set.retain(|key| keep(key.pos()));
        self.scheduled_due.retain(|key, _| keep(key.pos()));
        self.scratch.retain(|key| keep(key.pos()));
    }

    /// (ready now, pending in total)
    pub(crate) fn debug_counts(&self) -> (usize, usize) {
        (self.ready.len(), self.scheduled_due.len())
    }
}
