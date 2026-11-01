use std::ops::Range;

use crate::ViewerError;

/// Cumulative row offsets of the Row Groups.
///
/// With `G` Row Groups the length is `G + 1`, and `offsets[i]..offsets[i + 1]` is the global row range
/// of Row Group i. The last value is the total row count.
#[derive(Clone, Debug)]
pub struct RowGroupIndex {
    offsets: Vec<u64>,
}

impl RowGroupIndex {
    pub fn new(row_group_rows: &[u64]) -> Result<Self, ViewerError> {
        let mut offsets = Vec::with_capacity(row_group_rows.len() + 1);
        let mut total = 0u64;
        offsets.push(total);
        for &rows in row_group_rows {
            total = total
                .checked_add(rows)
                .ok_or(ViewerError::InvalidRowCount)?;
            offsets.push(total);
        }
        Ok(Self { offsets })
    }

    pub fn total_rows(&self) -> u64 {
        self.offsets[self.offsets.len() - 1]
    }

    pub fn row_group_count(&self) -> usize {
        self.offsets.len() - 1
    }

    /// Global row range of Row Group i. Panics if `row_group` is out of range.
    pub fn row_group_range(&self, row_group: usize) -> Range<u64> {
        self.offsets[row_group]..self.offsets[row_group + 1]
    }

    /// Finds the Row Group holding a global row and the row's position within that group.
    ///
    /// Finds `i` with `offsets[i] <= row < offsets[i + 1]` by upper-bound binary search, so
    /// Row Groups with zero rows are skipped naturally. Rows at or beyond the total are `None`.
    pub fn locate(&self, row: u64) -> Option<(usize, u64)> {
        if row >= self.total_rows() {
            return None;
        }
        let row_group = self.offsets.partition_point(|&offset| offset <= row) - 1;
        Some((row_group, row - self.offsets[row_group]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_rows_across_groups() {
        let index = RowGroupIndex::new(&[3, 2, 4]).unwrap();
        assert_eq!(index.total_rows(), 9);
        assert_eq!(index.row_group_count(), 3);
        assert_eq!(index.locate(0), Some((0, 0)));
        assert_eq!(index.locate(2), Some((0, 2)));
        assert_eq!(index.locate(3), Some((1, 0)));
        assert_eq!(index.locate(5), Some((2, 0)));
        assert_eq!(index.locate(8), Some((2, 3)));
        assert_eq!(index.locate(9), None);
        assert_eq!(index.row_group_range(1), 3..5);
    }

    #[test]
    fn skips_empty_row_groups() {
        let index = RowGroupIndex::new(&[0, 2, 0, 0, 1, 0]).unwrap();
        assert_eq!(index.total_rows(), 3);
        assert_eq!(index.locate(0), Some((1, 0)));
        assert_eq!(index.locate(1), Some((1, 1)));
        assert_eq!(index.locate(2), Some((4, 0)));
        assert_eq!(index.locate(3), None);
    }

    #[test]
    fn handles_files_without_rows() {
        for rows in [&[][..], &[0, 0][..]] {
            let index = RowGroupIndex::new(rows).unwrap();
            assert_eq!(index.total_rows(), 0);
            assert_eq!(index.locate(0), None);
        }
    }

    #[test]
    fn rejects_overflowing_totals() {
        assert!(matches!(
            RowGroupIndex::new(&[u64::MAX, 1]),
            Err(ViewerError::InvalidRowCount)
        ));
    }
}
