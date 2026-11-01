//! Parquet 파일을 읽기 전용으로 열고, 요청한 행·열 범위를 Arrow 배치로 디코딩하는 엔진.
//! 찾기·필터·정렬을 위한 컬럼 순차 읽기와 행 보기 계산도 제공한다.
//!
//! 공개 타입은 Rust 표준 타입과 Arrow 타입으로만 구성하며 GPUI에 의존하지 않는다.

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
