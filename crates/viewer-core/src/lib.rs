//! Engine that opens Parquet files read-only and decodes requested row and column ranges into Arrow batches.
//! Also provides sequential column scans and row view computation for find, filter and sort.
//!
//! Public types consist only of Rust standard and Arrow types, with no dependency on GPUI.

mod cache;
mod condition;
mod engine;
mod error;
mod find;
mod model;
mod reader;
mod row_index;
mod row_view;
mod scan;
mod structure;

pub use cache::DEFAULT_CACHE_BUDGET;
pub use condition::{
    CompiledFilter, ValueClass, check_condition, filter_ops, is_sortable, parse_value, value_class,
};
pub use engine::ViewerEngine;
pub use error::ViewerError;
pub use find::{FindIndex, FindQuery, FindTask};
pub use model::{
    BLOCK_ROWS, BlockRequest, ColumnId, DatasetInfo, DecodedBlock, FilterCondition, FilterOp,
    RequestId, RowIds, SessionId, SortKey, ValueRequest, ValueSlice, ViewSpec, WindowRequest,
};
pub use row_index::RowGroupIndex;
pub use row_view::{RowView, RowViewTask, TaskStep};
pub use scan::{ColumnScan, SCAN_BATCH_ROWS, ScanBatch};
pub use structure::{ColumnChunkInfo, FileStructure, RowGroupInfo, StatValue};
