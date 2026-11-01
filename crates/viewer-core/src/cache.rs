//! Byte-budgeted LRU cache of decoded blocks. One per engine, shared by all sessions.

use std::collections::HashMap;

use arrow_array::RecordBatch;

use crate::{ColumnId, SessionId};

/// Default budget of the Arrow cache.
pub const DEFAULT_CACHE_BUDGET: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlockKey {
    pub session: SessionId,
    pub row_group: usize,
    pub block_start: u64,
    /// Columns sorted in source schema order.
    pub columns: Vec<ColumnId>,
}

struct Entry {
    batch: RecordBatch,
    bytes: usize,
    last_used: u64,
}

pub(crate) struct BlockCache {
    budget: usize,
    used: usize,
    clock: u64,
    entries: HashMap<BlockKey, Entry>,
}

/// Buffer capacity held by a batch. A slice that pins a large source buffer counts the whole source capacity.
fn retained_bytes(batch: &RecordBatch) -> usize {
    batch.get_array_memory_size()
}

impl BlockCache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            clock: 0,
            entries: HashMap::new(),
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    pub fn get(&mut self, key: &BlockKey) -> Option<RecordBatch> {
        self.clock += 1;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.clock;
        Some(entry.batch.clone())
    }

    /// Finds an entry holding the same block and `column`, returning that column's position and the batch.
    pub fn find_column(
        &mut self,
        session: SessionId,
        row_group: usize,
        block_start: u64,
        column: ColumnId,
    ) -> Option<(RecordBatch, usize)> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.iter_mut().find_map(|(key, entry)| {
            if key.session != session
                || key.row_group != row_group
                || key.block_start != block_start
            {
                return None;
            }
            let position = key
                .columns
                .iter()
                .position(|candidate| *candidate == column)?;
            entry.last_used = clock;
            Some((entry.batch.clone(), position))
        })
    }

    /// Evicts least recently used entries until the budget fits, then inserts. A single block larger
    /// than the budget is not inserted.
    pub fn insert(&mut self, key: BlockKey, batch: RecordBatch) {
        let bytes = retained_bytes(&batch);
        if bytes > self.budget {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.used -= previous.bytes;
        }
        while self.used + bytes > self.budget {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            let removed = self.entries.remove(&oldest).expect("entry just found");
            self.used -= removed.bytes;
        }
        self.clock += 1;
        self.used += bytes;
        self.entries.insert(
            key,
            Entry {
                batch,
                bytes,
                last_used: self.clock,
            },
        );
    }

    /// Removes all entries of a session. Used when a file is closed or has changed.
    pub fn remove_session(&mut self, session: SessionId) {
        let used = &mut self.used;
        self.entries.retain(|key, entry| {
            let keep = key.session != session;
            if !keep {
                *used -= entry.bytes;
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int64Array};

    use super::*;

    fn batch(len: usize) -> RecordBatch {
        let values: ArrayRef = Arc::new(Int64Array::from_iter_values(0..len as i64));
        RecordBatch::try_from_iter([("v", values)]).unwrap()
    }

    fn key(session: u64, block_start: u64) -> BlockKey {
        BlockKey {
            session: SessionId(session),
            row_group: 0,
            block_start,
            columns: vec![ColumnId(0)],
        }
    }

    #[test]
    fn evicts_least_recently_used_blocks_to_stay_within_budget() {
        let size = retained_bytes(&batch(512));
        let mut cache = BlockCache::new(size * 3);
        for start in 0..3 {
            cache.insert(key(1, start), batch(512));
        }
        assert_eq!(cache.used_bytes(), size * 3);
        // Touching entry 0 again makes entry 1 the oldest.
        assert!(cache.get(&key(1, 0)).is_some());
        cache.insert(key(2, 0), batch(512));

        assert!(cache.used_bytes() <= size * 3);
        assert!(cache.get(&key(1, 1)).is_none());
        assert!(cache.get(&key(1, 0)).is_some());
        assert!(cache.get(&key(2, 0)).is_some());
    }

    #[test]
    fn skips_blocks_larger_than_the_budget() {
        let mut cache = BlockCache::new(retained_bytes(&batch(10)));
        cache.insert(key(1, 0), batch(10_000));
        assert_eq!(cache.used_bytes(), 0);
        assert!(cache.get(&key(1, 0)).is_none());
    }

    #[test]
    fn counts_the_whole_buffer_behind_a_slice() {
        let full = batch(10_000);
        let slice = full.slice(0, 1);
        assert!(retained_bytes(&slice) >= 10_000 * 8);
    }

    #[test]
    fn removes_only_the_closed_session() {
        let mut cache = BlockCache::new(DEFAULT_CACHE_BUDGET);
        cache.insert(key(1, 0), batch(100));
        cache.insert(key(2, 0), batch(100));
        cache.remove_session(SessionId(1));
        assert_eq!(cache.used_bytes(), retained_bytes(&batch(100)));
        assert!(cache.get(&key(1, 0)).is_none());
        assert!(cache.get(&key(2, 0)).is_some());
    }

    #[test]
    fn finds_a_column_inside_a_wider_block() {
        let mut cache = BlockCache::new(DEFAULT_CACHE_BUDGET);
        let mut wide = key(1, 512);
        wide.columns = vec![ColumnId(0), ColumnId(3)];
        cache.insert(wide, batch(10));
        let (_, position) = cache
            .find_column(SessionId(1), 0, 512, ColumnId(3))
            .unwrap();
        assert_eq!(position, 1);
        assert!(
            cache
                .find_column(SessionId(1), 0, 512, ColumnId(2))
                .is_none()
        );
    }
}
