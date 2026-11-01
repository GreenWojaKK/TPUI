//! 디코딩한 블록의 byte 예산 기반 LRU 캐시. 엔진 하나에 하나만 두고 모든 세션이 공유한다.

use std::collections::HashMap;

use arrow_array::RecordBatch;

use crate::{ColumnId, SessionId};

/// 유지하는 Arrow 캐시의 기본 예산.
pub const DEFAULT_CACHE_BUDGET: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlockKey {
    pub session: SessionId,
    pub row_group: usize,
    pub block_start: u64,
    /// 원본 스키마 순서로 정렬한 컬럼.
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

/// 배치가 유지하는 buffer 용량. slice가 큰 원본 buffer를 붙잡고 있으면 원본 용량까지 센다.
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

    /// 같은 블록을 담고 `column`을 포함한 항목에서 그 컬럼의 위치와 배치를 찾는다.
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

    /// 예산을 넘지 않도록 오래 쓰지 않은 항목부터 제거한 뒤 넣는다. 블록 하나가 예산을 넘으면
    /// 넣지 않는다.
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
            let removed = self.entries.remove(&oldest).expect("방금 찾은 항목");
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

    /// 세션의 항목을 모두 제거한다. 파일을 닫거나 파일이 바뀌었을 때 쓴다.
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
        // 0번을 다시 써서 1번이 가장 오래된 항목이 된다.
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
