use std::ops::Range;
use std::path::PathBuf;

use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{FieldRef, SchemaRef};

/// 한 블록이 담는 최대 행 수. 블록 시작은 Row Group 안에서 이 값의 배수에 정렬한다.
pub const BLOCK_ROWS: u64 = 512;

/// 파일 열기마다 앱이 새로 발급하는 세대 번호. 이전 파일의 응답을 구분하는 데 쓴다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// 같은 파일 안에서 화면 범위 요청이 바뀔 때마다 앱이 새로 발급하는 번호.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// 원본 스키마의 최상위 필드 위치.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnId(pub usize);

/// 열린 파일의 메타데이터 요약. 데이터를 읽기 전에 스키마와 파일 요약을 표시하는 데 쓴다.
#[derive(Clone, Debug)]
pub struct DatasetInfo {
    pub session: SessionId,
    pub path: PathBuf,
    /// 파일 크기(byte).
    pub file_size: u64,
    pub total_rows: u64,
    pub schema: SchemaRef,
    /// Row Group별 행 수. 파일에 기록된 Row Group 순서를 따른다.
    pub row_group_rows: Vec<u64>,
}

/// 화면에 필요한 행·열 범위. 행 번호는 0부터 시작하는 `[start, end)`다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub rows: Range<u64>,
    pub columns: Vec<ColumnId>,
}

/// 한 Row Group 안의 정렬된 블록 하나를 읽는 요청.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockRequest {
    pub session: SessionId,
    pub row_group: usize,
    /// Row Group 안에서의 블록 시작 행. [`BLOCK_ROWS`]의 배수다.
    pub block_start: u64,
    /// 블록의 파일 기준 행 범위.
    pub rows: Range<u64>,
    /// 원본 스키마 순서로 정렬하고 중복을 제거한 컬럼.
    pub columns: Vec<ColumnId>,
}

/// 디코딩한 블록. `batch`의 i번째 컬럼은 `columns[i]`의 원본 필드다.
#[derive(Clone, Debug)]
pub struct DecodedBlock {
    pub session: SessionId,
    /// 파일 기준 시작 행.
    pub start_row: u64,
    pub row_count: usize,
    pub columns: Vec<ColumnId>,
    pub batch: RecordBatch,
}

/// 셀 하나의 원본 값을 읽는 요청.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub row: u64,
    pub column: ColumnId,
}

/// 셀 하나만 담은 길이 1의 Arrow 배열. 블록 전체의 buffer를 유지하지 않도록 새 배열로 복사한다.
#[derive(Clone, Debug)]
pub struct ValueSlice {
    pub session: SessionId,
    pub row: u64,
    pub column: ColumnId,
    pub field: FieldRef,
    pub value: ArrayRef,
}

/// 필터 조건의 연산자.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterOp {
    Eq,
    NotEq,
    /// 문자열이 값을 포함한다. 문자열 컬럼에만 쓴다.
    Contains,
    Lt,
    LtEq,
    Gt,
    GtEq,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    /// 값 입력이 필요한 연산자인지.
    pub fn takes_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }
}

/// 컬럼 하나의 필터 조건. `value`는 사용자가 입력한 텍스트이며 컬럼 타입으로 해석한다.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FilterCondition {
    pub column: ColumnId,
    pub op: FilterOp,
    pub value: String,
}

/// 정렬 기준 컬럼과 방향.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SortKey {
    pub column: ColumnId,
    pub descending: bool,
}

/// 그리드에 나오는 행과 그 순서를 정하는 조건. 필터는 컬럼마다 하나이며 모두 만족해야 한다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ViewSpec {
    pub filters: Vec<FilterCondition>,
    pub sort: Option<SortKey>,
}

impl ViewSpec {
    /// 필터와 정렬이 없어 원본 순서 그대로인지.
    pub fn is_identity(&self) -> bool {
        self.filters.is_empty() && self.sort.is_none()
    }
}

/// 원본 행 번호의 배열. 전체 행 수가 `u32` 범위 안이면 행마다 4 byte를 쓴다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowIds {
    Narrow(Vec<u32>),
    Wide(Vec<u64>),
}

impl RowIds {
    /// `total_rows`행 파일의 행 번호를 담을 빈 배열.
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

    /// `range` 위치의 행 번호. 범위를 벗어난 부분은 버린다.
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
            Self::Narrow(rows) => rows.push(u32::try_from(row).expect("행 번호가 u32 범위 안이다")),
            Self::Wide(rows) => rows.push(row),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        (0..self.len()).map(|ix| self.get(ix).expect("범위 안의 위치"))
    }

    /// 배열이 쓰는 byte 수.
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
