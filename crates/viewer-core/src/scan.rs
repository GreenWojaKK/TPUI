//! Sequential column scans. Row view and find read all rows of a file, or the rows of a row list, batch by batch.
//!
//! Scans bypass the cache and do not keep the batches they return. They read one batch at a time so
//! the caller can handle other requests between batches.

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

/// Maximum rows per scan batch.
pub const SCAN_BATCH_ROWS: usize = 8192;

/// Batch returned by a scan. Column i of `batch` is the i-th column given when the scan was created.
pub struct ScanBatch {
    /// Sequence number of the batch's first row: the position within the row list if one was given, otherwise the source row number.
    pub start: u64,
    pub batch: RecordBatch,
}

/// Sequential scan reading only the given top-level columns, in Row Group order.
pub struct ColumnScan {
    file: File,
    file_len: u64,
    modified: Option<SystemTime>,
    metadata: ArrowReaderMetadata,
    projection: ProjectionMask,
    /// File-level row range per Row Group.
    groups: Vec<Range<u64>>,
    /// Ascending list of rows to read. Reads all rows if absent.
    selection: Option<Arc<RowIds>>,
    next_group: usize,
    /// First position of the row list not yet assigned to a Row Group.
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

    /// Total rows to read.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Rows returned so far.
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    /// Reads the next batch. `None` once everything has been read.
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

    /// Creates a reader for one Row Group. No reader is created if the row list has no rows in this group.
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

/// Converts the rows of the list from position `from` that fall in `group` into group-relative skip/select runs.
/// Consumes as many rows from the list as it returns.
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
        // Consecutive rows are merged into one select.
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
