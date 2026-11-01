use std::ops::Range;
use std::path::{Path, PathBuf};

use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{FieldRef, SchemaRef};

/// Maximum rows per block. Block starts are aligned to multiples of this within a Row Group.
pub const BLOCK_ROWS: u64 = 512;

/// Generation number the app issues for every file open. Tells responses for previous files apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// Number the app issues whenever the visible range request changes within the same file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// Position of a top-level field in the source schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnId(pub usize);

/// What a session reads: a Parquet file, or one table or view of a SQLite database.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DataSource {
    Parquet(PathBuf),
    Sqlite { path: PathBuf, table: String },
}

impl DataSource {
    /// The file the source is read from.
    pub fn path(&self) -> &Path {
        match self {
            Self::Parquet(path) | Self::Sqlite { path, .. } => path,
        }
    }

    /// Table name of a SQLite source.
    pub fn table(&self) -> Option<&str> {
        match self {
            Self::Parquet(_) => None,
            Self::Sqlite { table, .. } => Some(table),
        }
    }
}

/// Metadata summary of an open source. Used to show the schema and file summary before reading data.
#[derive(Clone, Debug)]
pub struct DatasetInfo {
    pub session: SessionId,
    pub source: DataSource,
    pub path: PathBuf,
    /// File size in bytes.
    pub file_size: u64,
    pub total_rows: u64,
    pub schema: SchemaRef,
    /// Row count per Row Group, in the Row Group order recorded in the file. A SQLite table is one group.
    pub row_group_rows: Vec<u64>,
}

/// Row and column ranges needed on screen. Rows are a zero-based `[start, end)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub rows: Range<u64>,
    pub columns: Vec<ColumnId>,
}

/// Request to read one aligned block within a Row Group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockRequest {
    pub session: SessionId,
    pub row_group: usize,
    /// Start row of the block within the Row Group. A multiple of [`BLOCK_ROWS`].
    pub block_start: u64,
    /// File-level row range of the block.
    pub rows: Range<u64>,
    /// Columns sorted in source schema order, without duplicates.
    pub columns: Vec<ColumnId>,
}

/// A decoded block. Column i of `batch` is the source field `columns[i]`.
#[derive(Clone, Debug)]
pub struct DecodedBlock {
    pub session: SessionId,
    /// File-level start row.
    pub start_row: u64,
    pub row_count: usize,
    pub columns: Vec<ColumnId>,
    pub batch: RecordBatch,
}

/// Request to read the raw value of one cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub row: u64,
    pub column: ColumnId,
}

/// Arrow array of length 1 holding one cell. Copied into a new array so it does not keep the whole block's buffers alive.
#[derive(Clone, Debug)]
pub struct ValueSlice {
    pub session: SessionId,
    pub row: u64,
    pub column: ColumnId,
    pub field: FieldRef,
    pub value: ArrayRef,
}

/// Filter condition operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterOp {
    Eq,
    NotEq,
    /// The string contains the value. String columns only.
    Contains,
    Lt,
    LtEq,
    Gt,
    GtEq,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    /// Whether the operator needs an input value.
    pub fn takes_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }
}

/// Filter condition on one column. `value` is the text the user typed, parsed as the column type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FilterCondition {
    pub column: ColumnId,
    pub op: FilterOp,
    pub value: String,
}

/// Sort column and direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SortKey {
    pub column: ColumnId,
    pub descending: bool,
}

/// Conditions deciding which rows the grid shows and in what order. One filter per column; all must hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ViewSpec {
    pub filters: Vec<FilterCondition>,
    pub sort: Option<SortKey>,
}

impl ViewSpec {
    /// Whether there is no filter or sort, so rows keep source order.
    pub fn is_identity(&self) -> bool {
        self.filters.is_empty() && self.sort.is_none()
    }
}

/// Array of source row numbers. Uses 4 bytes per row when the total row count fits in `u32`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowIds {
    Narrow(Vec<u32>),
    Wide(Vec<u64>),
}

impl RowIds {
    /// Empty array for row numbers of a file with `total_rows` rows.
    pub fn for_total_rows(total_rows: u64) -> Self {
        if total_rows <= u64::from(u32::MAX) + 1 {
            Self::Narrow(Vec::new())
        } else {
            Self::Wide(Vec::new())
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Narrow(rows) => rows.len(),
            Self::Wide(rows) => rows.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, ix: usize) -> Option<u64> {
        match self {
            Self::Narrow(rows) => rows.get(ix).map(|&row| u64::from(row)),
            Self::Wide(rows) => rows.get(ix).copied(),
        }
    }

    /// Row numbers at the positions in `range`. Parts outside the array are dropped.
    pub fn slice(&self, range: Range<usize>) -> Vec<u64> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        match self {
            Self::Narrow(rows) => rows[start..end].iter().map(|&row| u64::from(row)).collect(),
            Self::Wide(rows) => rows[start..end].to_vec(),
        }
    }

    pub fn push(&mut self, row: u64) {
        match self {
            Self::Narrow(rows) => rows.push(u32::try_from(row).expect("row number fits in u32")),
            Self::Wide(rows) => rows.push(row),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        (0..self.len()).map(|ix| self.get(ix).expect("position in range"))
    }

    /// Bytes used by the array.
    pub fn byte_size(&self) -> usize {
        match self {
            Self::Narrow(rows) => rows.capacity() * 4,
            Self::Wide(rows) => rows.capacity() * 8,
        }
    }

    pub fn shrink_to_fit(&mut self) {
        match self {
            Self::Narrow(rows) => rows.shrink_to_fit(),
            Self::Wide(rows) => rows.shrink_to_fit(),
        }
    }
}
