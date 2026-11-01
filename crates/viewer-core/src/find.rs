//! 찾기. 셀에 표시되는 값 전체가 입력과 같은 셀을 찾는다.
//!
//! 입력은 컬럼별 비교 값으로 해석한다. 수치·시간처럼 표시 텍스트로 바꿔 보이는 값은, 입력을
//! 컬럼 타입으로 해석한 값의 표시 텍스트가 입력과 같을 때만 비교한다. 표시 텍스트는 값마다
//! 하나로 정해지므로 값 비교가 표시 텍스트 비교와 같다.

use std::sync::Arc;

use arrow_array::ArrayRef;
use arrow_buffer::BooleanBuffer;
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::Schema;

use crate::condition::{ValueClass, equals, parse_value, value_class};
use crate::row_view::{RowView, TaskStep};
use crate::scan::ColumnScan;
use crate::{ColumnId, RowIds, SessionId, ViewerEngine, ViewerError};

/// 찾기 입력을 컬럼별 비교 값으로 해석한 결과.
#[derive(Clone, Debug)]
pub struct FindQuery {
    text: String,
    /// 비교 대상 컬럼과 그 비교 값. 원본 스키마 순서다.
    targets: Vec<(ColumnId, ArrayRef)>,
}

impl FindQuery {
    pub fn new(schema: &Schema, text: &str) -> Self {
        let options = FormatOptions::new().with_display_error(false);
        let targets = schema
            .fields()
            .iter()
            .enumerate()
            .filter_map(|(ix, field)| {
                let data_type = field.data_type();
                let value = match value_class(data_type) {
                    ValueClass::Text | ValueClass::Binary => parse_value(text, data_type).ok()?,
                    ValueClass::Scalar => {
                        let value = parse_value(text, data_type).ok()?;
                        let shown = ArrayFormatter::try_new(value.as_ref(), &options)
                            .ok()?
                            .value(0)
                            .try_to_string()
                            .ok()?;
                        if shown != text {
                            return None;
                        }
                        value
                    }
                    ValueClass::Other => return None,
                };
                Some((ColumnId(ix), value))
            })
            .collect();
        Self {
            text: text.to_string(),
            targets,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// 비교 대상 컬럼. 원본 스키마 순서다.
    pub fn columns(&self) -> Vec<ColumnId> {
        self.targets.iter().map(|(column, _)| *column).collect()
    }

    /// 배열에서 입력과 같은 셀. 비교 대상이 아닌 컬럼이면 `None`이다.
    pub fn matches(
        &self,
        column: ColumnId,
        array: &ArrayRef,
    ) -> Result<Option<BooleanBuffer>, ViewerError> {
        let Ok(ix) = self
            .targets
            .binary_search_by_key(&column, |(target, _)| *target)
        else {
            return Ok(None);
        };
        equals(array, &self.targets[ix].1).map(Some)
    }
}

/// 행 보기에서 일치 셀이 있는 원본 행의 bitmap과 일치 셀 수.
pub struct FindIndex {
    query: FindQuery,
    /// 원본 행마다 1 bit.
    bitmap: Vec<u64>,
    cells: u64,
}

/// 일치 위치를 계산하는 작업. 행 보기의 행만 비교 대상 컬럼을 순차 읽기로 읽는다.
pub struct FindTask {
    scan: Option<ColumnScan>,
    filtered: Option<Arc<RowIds>>,
    index: Option<FindIndex>,
}

impl FindTask {
    pub fn new(
        engine: &ViewerEngine,
        session: SessionId,
        query: FindQuery,
        view: &RowView,
    ) -> Result<Self, ViewerError> {
        let words = view.total_rows().div_ceil(64) as usize;
        let columns = query.columns();
        let scan = if columns.is_empty() || view.is_empty() {
            None
        } else {
            Some(engine.scan(session, &columns, view.filtered().cloned())?)
        };
        Ok(Self {
            scan,
            filtered: view.filtered().cloned(),
            index: Some(FindIndex {
                query,
                bitmap: vec![0; words],
                cells: 0,
            }),
        })
    }

    /// 배치 하나만큼 계산을 진행한다.
    pub fn step(&mut self) -> Result<TaskStep<FindIndex>, ViewerError> {
        let index = self
            .index
            .as_mut()
            .ok_or(ViewerError::InvalidRequest("이미 끝난 작업입니다"))?;
        let Some(scan) = &mut self.scan else {
            return Ok(TaskStep::Done(self.index.take().expect("결과")));
        };
        let Some(batch) = scan.next_batch()? else {
            return Ok(TaskStep::Done(self.index.take().expect("결과")));
        };
        let columns = index.query.columns();
        let mut rows = BooleanBuffer::new_unset(batch.batch.num_rows());
        for (position, column) in columns.iter().enumerate() {
            if let Some(matched) = index.query.matches(*column, batch.batch.column(position))? {
                index.cells += matched.count_set_bits() as u64;
                rows = &rows | &matched;
            }
        }
        for ix in rows.set_indices() {
            let sequence = batch.start + ix as u64;
            let row = match &self.filtered {
                Some(filtered) => filtered.get(sequence as usize).expect("행 목록 안의 위치"),
                None => sequence,
            };
            index.bitmap[(row / 64) as usize] |= 1 << (row % 64);
        }
        let progress = if scan.total() == 0 {
            1.
        } else {
            scan.emitted() as f32 / scan.total() as f32
        };
        Ok(TaskStep::Progress(progress))
    }
}

impl FindIndex {
    pub fn query(&self) -> &FindQuery {
        &self.query
    }

    /// 행 보기 전체의 일치 셀 수.
    pub fn cell_count(&self) -> u64 {
        self.cells
    }

    /// bitmap이 쓰는 byte 수.
    pub fn byte_size(&self) -> usize {
        self.bitmap.capacity() * 8
    }

    fn has_row(&self, row: u64) -> bool {
        self.bitmap
            .get((row / 64) as usize)
            .is_some_and(|word| word & (1 << (row % 64)) != 0)
    }

    /// `from` 셀 다음(또는 이전)의 일치 셀. 순서는 행 보기의 행 순서, 같은 행 안에서는 컬럼
    /// 순서이며, 끝에 닿으면 처음으로 돌아간다. `from`이 없으면 처음(또는 끝)부터 찾는다.
    pub fn next(
        &self,
        engine: &mut ViewerEngine,
        session: SessionId,
        view: &RowView,
        from: Option<(u64, ColumnId)>,
        backward: bool,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<(u64, ColumnId)>, ViewerError> {
        let len = view.len();
        if len == 0 || self.cells == 0 {
            return Ok(None);
        }
        let pick = |columns: Vec<ColumnId>| {
            if backward {
                columns.last().copied()
            } else {
                columns.first().copied()
            }
        };
        let (start, column) = match from {
            Some((position, column)) if position < len => (position, Some(column)),
            _ => {
                let start = if backward { len - 1 } else { 0 };
                if self.position_matches(view, start) {
                    let columns =
                        self.matched_columns(engine, session, view, start, is_cancelled)?;
                    if let Some(column) = pick(columns) {
                        return Ok(Some((start, column)));
                    }
                }
                (start, None)
            }
        };
        // 시작 행에서 시작 컬럼의 다음(이전) 컬럼.
        if let Some(column) = column
            && self.position_matches(view, start)
        {
            let columns = self.matched_columns(engine, session, view, start, is_cancelled)?;
            let found = if backward {
                columns.iter().rev().find(|candidate| **candidate < column)
            } else {
                columns.iter().find(|candidate| **candidate > column)
            };
            if let Some(found) = found {
                return Ok(Some((start, *found)));
            }
        }
        if let Some(position) = self.next_position(view, start, backward, is_cancelled)? {
            let columns = self.matched_columns(engine, session, view, position, is_cancelled)?;
            if let Some(column) = pick(columns) {
                return Ok(Some((position, column)));
            }
        }
        // 다른 행에 일치가 없으면 시작 행으로 돌아온다.
        if column.is_some() && self.position_matches(view, start) {
            let columns = self.matched_columns(engine, session, view, start, is_cancelled)?;
            return Ok(pick(columns).map(|column| (start, column)));
        }
        Ok(None)
    }

    fn position_matches(&self, view: &RowView, position: u64) -> bool {
        view.file_row(position).is_some_and(|row| self.has_row(row))
    }

    /// 행 보기 위치의 행에서 입력과 같은 컬럼.
    fn matched_columns(
        &self,
        engine: &mut ViewerEngine,
        session: SessionId,
        view: &RowView,
        position: u64,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<ColumnId>, ViewerError> {
        let Some(row) = view.file_row(position) else {
            return Ok(Vec::new());
        };
        let columns = self.query.columns();
        let batch = engine.read_rows(session, &[row], &columns, is_cancelled)?;
        let mut matched = Vec::new();
        for (position, column) in columns.iter().enumerate() {
            if let Some(cells) = self.query.matches(*column, batch.column(position))?
                && cells.value(0)
            {
                matched.push(*column);
            }
        }
        Ok(matched)
    }

    /// `start`를 뺀 다음(이전) 일치 행의 위치. 끝에 닿으면 처음(끝)으로 돌아간다.
    fn next_position(
        &self,
        view: &RowView,
        start: u64,
        backward: bool,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, ViewerError> {
        let len = view.len();
        if view.is_identity() {
            return Ok(if backward {
                self.previous_set(start, 0)
                    .or_else(|| self.previous_set(len, start + 1))
            } else {
                self.next_set(start + 1, len)
                    .or_else(|| self.next_set(0, start))
            });
        }
        for step in 1..len {
            if step % 65_536 == 0 && is_cancelled() {
                return Err(ViewerError::Cancelled);
            }
            let position = if backward {
                (start + len - step) % len
            } else {
                (start + step) % len
            };
            if self.position_matches(view, position) {
                return Ok(Some(position));
            }
        }
        Ok(None)
    }

    /// `[from, end)`에서 첫 일치 행.
    fn next_set(&self, from: u64, end: u64) -> Option<u64> {
        let mut row = from;
        while row < end {
            let word = self.bitmap[(row / 64) as usize] >> (row % 64);
            if word != 0 {
                let found = row + u64::from(word.trailing_zeros());
                return (found < end).then_some(found);
            }
            row = (row / 64 + 1) * 64;
        }
        None
    }

    /// `[floor, before)`에서 마지막 일치 행.
    fn previous_set(&self, before: u64, floor: u64) -> Option<u64> {
        let mut end = before;
        while end > floor {
            let row = end - 1;
            let shift = 63 - row % 64;
            let word = self.bitmap[(row / 64) as usize] << shift;
            if word != 0 {
                let found = row - u64::from(word.leading_zeros());
                return (found >= floor).then_some(found);
            }
            end = row / 64 * 64;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::{
        BinaryArray, Decimal128Array, Float64Array, Int64Array, StringArray,
        TimestampMillisecondArray,
    };
    use arrow_schema::{DataType, Field};

    use super::*;

    fn found(query: &FindQuery, column: usize, array: ArrayRef) -> Option<Vec<bool>> {
        query
            .matches(ColumnId(column), &array)
            .unwrap()
            .map(|bits| bits.iter().collect())
    }

    #[test]
    fn matches_the_whole_display_text_only() {
        let schema = Schema::new(vec![
            Field::new("s", DataType::Utf8, true),
            Field::new("f", DataType::Float64, true),
            Field::new("i", DataType::Int64, true),
        ]);
        let query = FindQuery::new(&schema, "35.5");
        assert_eq!(query.columns(), [ColumnId(0), ColumnId(1)]);
        let strings: ArrayRef =
            Arc::new(StringArray::from(vec![Some("35.5"), Some("35.50"), None]));
        assert_eq!(found(&query, 0, strings), Some(vec![true, false, false]));
        let floats: ArrayRef = Arc::new(Float64Array::from(vec![35.5, 35.55]));
        assert_eq!(found(&query, 1, floats), Some(vec![true, false]));
        // 정수 컬럼은 35.5를 표시할 수 없으므로 비교 대상이 아니다.
        let ints: ArrayRef = Arc::new(Int64Array::from(vec![35]));
        assert_eq!(found(&query, 2, ints), None);
        // 같은 값이라도 표시 텍스트가 다르면 찾지 않는다.
        assert!(FindQuery::new(&schema, "35.50").columns() == [ColumnId(0)]);
    }

    #[test]
    fn matches_decimal_timestamp_and_binary_display_text() {
        let decimal_type = DataType::Decimal128(10, 2);
        let ts_type =
            DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, Some("+09:00".into()));
        let schema = Schema::new(vec![
            Field::new("d", decimal_type, true),
            Field::new("t", ts_type, true),
            Field::new("b", DataType::Binary, true),
        ]);
        let decimals: ArrayRef = Arc::new(
            Decimal128Array::from(vec![150, 15])
                .with_precision_and_scale(10, 2)
                .unwrap(),
        );
        assert_eq!(
            found(&FindQuery::new(&schema, "1.50"), 0, decimals.clone()),
            Some(vec![true, false])
        );
        assert_eq!(found(&FindQuery::new(&schema, "1.5"), 0, decimals), None);
        let ts: ArrayRef = Arc::new(
            TimestampMillisecondArray::from(vec![1_700_000_000_123]).with_timezone("+09:00"),
        );
        let query = FindQuery::new(&schema, "2023-11-15T07:13:20.123+09:00");
        assert_eq!(found(&query, 1, ts), Some(vec![true]));
        let binary: ArrayRef = Arc::new(BinaryArray::from(vec![&[0xde, 0xad][..], &[0xde][..]]));
        assert_eq!(
            found(&FindQuery::new(&schema, "DEad"), 2, binary),
            Some(vec![true, false])
        );
    }

    #[test]
    fn walks_bitmap_both_ways() {
        let mut index = FindIndex {
            query: FindQuery::new(&Schema::empty(), "x"),
            bitmap: vec![0; 3],
            cells: 3,
        };
        for row in [3u64, 64, 130] {
            index.bitmap[(row / 64) as usize] |= 1 << (row % 64);
        }
        assert_eq!(index.next_set(0, 192), Some(3));
        assert_eq!(index.next_set(4, 192), Some(64));
        assert_eq!(index.next_set(65, 130), None);
        assert_eq!(index.previous_set(130, 0), Some(64));
        assert_eq!(index.previous_set(64, 0), Some(3));
        assert_eq!(index.previous_set(3, 0), None);
        assert_eq!(index.previous_set(192, 131), None);
        assert_eq!(index.previous_set(192, 130), Some(130));
    }
}
