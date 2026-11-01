//! Parquet reader that reads only the selected Row Groups, rows and top-level columns.

use std::fs::File;
use std::ops::Range;

use arrow_array::{RecordBatch, RecordBatchReader};
use arrow_select::concat::concat_batches;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ParquetRecordBatchReaderBuilder, RowSelection, RowSelector,
};

use crate::{BLOCK_ROWS, BlockRequest, ColumnId, RowGroupIndex, SessionId, ViewerError};

/// Lists the blocks overlapping a row range, in Row Group order.
///
/// Blocks are aligned to [`BLOCK_ROWS`] boundaries within a Row Group and cut at the group's end.
/// Row Groups with zero rows are skipped.
pub(crate) fn plan_blocks(
    session: SessionId,
    index: &RowGroupIndex,
    rows: Range<u64>,
    columns: &[ColumnId],
) -> Vec<BlockRequest> {
    let mut blocks = Vec::new();
    if rows.is_empty() || columns.is_empty() {
        return blocks;
    }
    let Some((first_group, _)) = index.locate(rows.start) else {
        return blocks;
    };
    for row_group in first_group..index.row_group_count() {
        let group = index.row_group_range(row_group);
        if group.start >= rows.end {
            break;
        }
        let group_len = group.end - group.start;
        let start = rows.start.max(group.start) - group.start;
        let end = rows.end.min(group.end) - group.start;
        let mut block_start = start / BLOCK_ROWS * BLOCK_ROWS;
        while block_start < end {
            let block_end = (block_start + BLOCK_ROWS).min(group_len);
            blocks.push(BlockRequest {
                session,
                row_group,
                block_start,
                rows: group.start + block_start..group.start + block_end,
                columns: columns.to_vec(),
            });
            block_start += BLOCK_ROWS;
        }
    }
    blocks
}

/// Decodes only one Row Group of the block and the set of top-level columns.
///
/// `with_row_groups` is applied before `RowSelection`, so the selection uses row positions relative
/// to the selected Row Group.
pub(crate) fn decode_block(
    file: &File,
    metadata: &ArrowReaderMetadata,
    block: &BlockRequest,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<RecordBatch, ViewerError> {
    if is_cancelled() {
        return Err(ViewerError::Cancelled);
    }

    let group_len = metadata.metadata().row_group(block.row_group).num_rows() as u64;
    let block_len = block.rows.end - block.rows.start;
    let skip_after = group_len - block.block_start - block_len;
    let selectors = [
        RowSelector::skip(block.block_start as usize),
        RowSelector::select(block_len as usize),
        RowSelector::skip(skip_after as usize),
    ];
    let selection = RowSelection::from(
        selectors
            .into_iter()
            .filter(|selector| selector.row_count > 0)
            .collect::<Vec<_>>(),
    );
    let projection = ProjectionMask::roots(
        metadata.metadata().file_metadata().schema_descr(),
        block.columns.iter().map(|column| column.0),
    );

    let reader =
        ParquetRecordBatchReaderBuilder::new_with_metadata(file.try_clone()?, metadata.clone())
            .with_row_groups(vec![block.row_group])
            .with_projection(projection)
            .with_row_selection(selection)
            .with_batch_size(BLOCK_ROWS as usize)
            .build()
            .map_err(|err| ViewerError::Decode(err.into()))?;
    let schema = reader.schema();

    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|err| ViewerError::Decode(err.into()))?);
        if is_cancelled() {
            return Err(ViewerError::Cancelled);
        }
    }
    match batches.len() {
        0 => Ok(RecordBatch::new_empty(schema)),
        1 => Ok(batches.remove(0)),
        _ => concat_batches(&schema, &batches).map_err(|err| ViewerError::Decode(err.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_of(blocks: &[BlockRequest]) -> Vec<(usize, u64, Range<u64>)> {
        blocks
            .iter()
            .map(|block| (block.row_group, block.block_start, block.rows.clone()))
            .collect()
    }

    #[test]
    fn splits_rows_into_aligned_blocks_per_row_group() {
        let index = RowGroupIndex::new(&[1000, 1000, 500]).unwrap();
        let blocks = plan_blocks(SessionId(1), &index, 900..1600, &[ColumnId(0)]);
        assert_eq!(
            rows_of(&blocks),
            vec![
                (0, 512, 512..1000),
                (1, 0, 1000..1512),
                (1, 512, 1512..2000),
            ]
        );
    }

    #[test]
    fn skips_empty_row_groups_and_truncates_last_block() {
        let index = RowGroupIndex::new(&[0, 600, 0, 10]).unwrap();
        let blocks = plan_blocks(SessionId(1), &index, 0..610, &[ColumnId(0)]);
        assert_eq!(
            rows_of(&blocks),
            vec![(1, 0, 0..512), (1, 512, 512..600), (3, 0, 600..610)]
        );
    }

    #[test]
    fn plans_nothing_for_empty_ranges() {
        let index = RowGroupIndex::new(&[10]).unwrap();
        assert!(plan_blocks(SessionId(1), &index, 5..5, &[ColumnId(0)]).is_empty());
        assert!(plan_blocks(SessionId(1), &index, 10..20, &[ColumnId(0)]).is_empty());
        assert!(plan_blocks(SessionId(1), &index, 0..10, &[]).is_empty());
    }
}
