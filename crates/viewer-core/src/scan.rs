//! 컬럼 순차 읽기. 행 보기와 찾기 계산이 파일 전체 또는 행 목록의 행을 배치 단위로 읽는다.
//!
//! 캐시를 쓰지 않고, 돌려준 배치를 보유하지 않는다. 호출하는 쪽이 배치 하나마다 다른 요청을
//! 처리할 수 있도록 한 번에 배치 하나만 읽는다.

use std::fs::File;
use std::ops::Range;
use std::sync::Arc;
use std::time::SystemTime;

use arrow_array::RecordBatch;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder, RowSelection,
    RowSelector,
};

use crate::{ColumnId, RowIds, ViewerError};

/// 순차 읽기 배치 하나의 최대 행 수.
pub const SCAN_BATCH_ROWS: usize = 8192;

/// 순차 읽기가 돌려주는 배치. `batch`의 i번째 컬럼은 읽기를 만들 때 지정한 i번째 컬럼이다.
pub struct ScanBatch {
    /// 배치 첫 행의 순번. 행 목록을 지정했으면 행 목록 안의 위치, 아니면 원본 행 번호다.
    pub start: u64,
    pub batch: RecordBatch,
}

/// Row Group 순서로 지정한 최상위 컬럼만 읽는 순차 읽기.
pub struct ColumnScan {
    file: File,
    file_len: u64,
    modified: Option<SystemTime>,
    metadata: ArrowReaderMetadata,
    projection: ProjectionMask,
    /// Row Group별 파일 기준 행 범위.
    groups: Vec<Range<u64>>,
    /// 읽을 행의 오름차순 목록. 없으면 모든 행을 읽는다.
    selection: Option<Arc<RowIds>>,
    next_group: usize,
    /// 아직 Row Group에 배정하지 않은 행 목록의 첫 위치.
    selection_pos: usize,
    reader: Option<ParquetRecordBatchReader>,
    emitted: u64,
    total: u64,
}

impl ColumnScan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        file: File,
        file_len: u64,
        modified: Option<SystemTime>,
        metadata: ArrowReaderMetadata,
        groups: Vec<Range<u64>>,
        columns: &[ColumnId],
        selection: Option<Arc<RowIds>>,
    ) -> Self {
        let projection = ProjectionMask::roots(
            metadata.metadata().file_metadata().schema_descr(),
            columns.iter().map(|column| column.0),
        );
        let total = match &selection {
            Some(rows) => rows.len() as u64,
            None => groups.last().map_or(0, |group| group.end),
        };
        Self {
            file,
            file_len,
            modified,
            metadata,
            projection,
            groups,
            selection,
            next_group: 0,
            selection_pos: 0,
            reader: None,
            emitted: 0,
            total,
        }
    }

    /// 읽을 전체 행 수.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// 지금까지 돌려준 행 수.
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    /// 다음 배치를 읽는다. 모두 읽었으면 `None`이다.
    pub fn next_batch(&mut self) -> Result<Option<ScanBatch>, ViewerError> {
        loop {
            if let Some(reader) = &mut self.reader {
                match reader.next() {
                    Some(Ok(batch)) if batch.num_rows() == 0 => continue,
                    Some(Ok(batch)) => {
                        let start = self.emitted;
                        self.emitted += batch.num_rows() as u64;
                        return Ok(Some(ScanBatch { start, batch }));
                    }
                    Some(Err(err)) => return Err(ViewerError::Decode(err.into())),
                    None => self.reader = None,
                }
            }
            if self.next_group >= self.groups.len() {
                return Ok(None);
            }
            let group = self.next_group;
            self.next_group += 1;
            self.open_group(group)?;
        }
    }

    /// Row Group 하나의 reader를 만든다. 행 목록에 이 그룹의 행이 없으면 reader를 만들지 않는다.
    fn open_group(&mut self, group: usize) -> Result<(), ViewerError> {
        let range = self.groups[group].clone();
        if range.is_empty() {
            return Ok(());
        }
        let selection = match &self.selection {
            Some(rows) => {
                let (selectors, consumed) = group_selection(rows, self.selection_pos, &range);
                self.selection_pos += consumed;
                if consumed == 0 {
                    return Ok(());
                }
                Some(RowSelection::from(selectors))
            }
            None => None,
        };
        self.ensure_unchanged()?;
        let mut builder = ParquetRecordBatchReaderBuilder::new_with_metadata(
            self.file.try_clone()?,
            self.metadata.clone(),
        )
        .with_row_groups(vec![group])
        .with_projection(self.projection.clone())
        .with_batch_size(SCAN_BATCH_ROWS);
        if let Some(selection) = selection {
            builder = builder.with_row_selection(selection);
        }
        self.reader = Some(
            builder
                .build()
                .map_err(|err| ViewerError::Decode(err.into()))?,
        );
        Ok(())
    }

    fn ensure_unchanged(&self) -> Result<(), ViewerError> {
        let current = self.file.metadata()?;
        if current.len() != self.file_len || current.modified().ok() != self.modified {
            return Err(ViewerError::FileChanged);
        }
        Ok(())
    }
}

/// 행 목록의 `from` 위치부터 `group` 범위에 드는 행을 그룹 기준 skip/select로 바꾼다.
/// 돌려준 개수만큼 행 목록을 소비한다.
fn group_selection(rows: &RowIds, from: usize, group: &Range<u64>) -> (Vec<RowSelector>, usize) {
    let mut selectors = Vec::new();
    let mut cursor = group.start;
    let mut ix = from;
    while let Some(row) = rows.get(ix) {
        if row >= group.end {
            break;
        }
        if row > cursor {
            selectors.push(RowSelector::skip((row - cursor) as usize));
        }
        // 연속한 행은 select 하나로 묶는다.
        let mut run_end = row + 1;
        ix += 1;
        while rows.get(ix) == Some(run_end) && run_end < group.end {
            run_end += 1;
            ix += 1;
        }
        selectors.push(RowSelector::select((run_end - row) as usize));
        cursor = run_end;
    }
    if cursor < group.end && ix > from {
        selectors.push(RowSelector::skip((group.end - cursor) as usize));
    }
    (selectors, ix - from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(rows: &[u64]) -> RowIds {
        let mut ids = RowIds::for_total_rows(1000);
        for &row in rows {
            ids.push(row);
        }
        ids
    }

    #[test]
    fn turns_row_lists_into_group_relative_runs() {
        let rows = ids(&[2, 3, 4, 8, 15, 20, 21]);
        let (selectors, consumed) = group_selection(&rows, 0, &(0..10));
        assert_eq!(
            selectors,
            vec![
                RowSelector::skip(2),
                RowSelector::select(3),
                RowSelector::skip(3),
                RowSelector::select(1),
                RowSelector::skip(1),
            ]
        );
        assert_eq!(consumed, 4);
        let (selectors, consumed) = group_selection(&rows, 4, &(10..22));
        assert_eq!(
            selectors,
            vec![
                RowSelector::skip(5),
                RowSelector::select(1),
                RowSelector::skip(4),
                RowSelector::select(2),
            ]
        );
        assert_eq!(consumed, 3);
    }

    #[test]
    fn selects_nothing_for_groups_without_listed_rows() {
        let rows = ids(&[25]);
        let (selectors, consumed) = group_selection(&rows, 0, &(0..10));
        assert!(selectors.is_empty());
        assert_eq!(consumed, 0);
    }
}
