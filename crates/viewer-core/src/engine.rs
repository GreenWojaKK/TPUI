use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use parquet::arrow::arrow_reader::{ArrowReaderMetadata, ArrowReaderOptions};

use arrow_array::{RecordBatch, UInt32Array};
use arrow_schema::SchemaRef;
use arrow_select::interleave::interleave_record_batch;
use arrow_select::take::take;

use crate::cache::{BlockCache, BlockKey, DEFAULT_CACHE_BUDGET};
use crate::reader;
use crate::scan::ColumnScan;
use crate::structure::{self, ColumnChunkInfo, FileStructure};
use crate::{
    BLOCK_ROWS, BlockRequest, ColumnId, DatasetInfo, DecodedBlock, RowGroupIndex, RowIds,
    SessionId, ValueRequest, ValueSlice, ViewerError, WindowRequest,
};

/// 여러 파일을 세션별로 읽기 전용으로 열어 두는 동기 엔진. 호출 시점은 worker가 관리한다.
///
/// 디코딩한 블록은 모든 세션이 공유하는 byte 예산의 LRU 캐시에 둔다.
pub struct ViewerEngine {
    datasets: HashMap<SessionId, OpenDataset>,
    cache: BlockCache,
}

impl Default for ViewerEngine {
    fn default() -> Self {
        Self::with_cache_budget(DEFAULT_CACHE_BUDGET)
    }
}

/// 열린 파일의 세션 상태. 파일 핸들과 메타데이터는 세션 동안 재사용한다.
struct OpenDataset {
    file: File,
    /// 열 때 확인한 파일 크기와 수정 시각. 읽기 전후에 비교해 파일 변경을 감지한다.
    file_len: u64,
    modified: Option<SystemTime>,
    metadata: ArrowReaderMetadata,
    index: RowGroupIndex,
}

impl OpenDataset {
    fn ensure_unchanged(&self) -> Result<(), ViewerError> {
        let current = self.file.metadata()?;
        if current.len() != self.file_len || current.modified().ok() != self.modified {
            return Err(ViewerError::FileChanged);
        }
        Ok(())
    }
}

impl ViewerEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cache_budget(budget: usize) -> Self {
        Self {
            datasets: HashMap::new(),
            cache: BlockCache::new(budget),
        }
    }

    /// 캐시가 유지하는 Arrow buffer의 byte 수.
    pub fn cache_bytes(&self) -> usize {
        self.cache.used_bytes()
    }

    /// 파일을 읽기 전용으로 열어 세션으로 추가하고 Footer와 메타데이터만 읽는다. 데이터
    /// 페이지는 읽지 않으며, 다른 세션은 그대로 둔다.
    ///
    /// 같은 세션 번호가 이미 열려 있으면 성공 여부와 관계없이 먼저 닫는다.
    pub fn open(&mut self, session: SessionId, path: &Path) -> Result<DatasetInfo, ViewerError> {
        self.close(session);

        let file = File::open(path)?;
        let file_metadata = file.metadata()?;
        let metadata = ArrowReaderMetadata::load(&file, ArrowReaderOptions::new())?;
        let row_group_rows = metadata
            .metadata()
            .row_groups()
            .iter()
            .map(|row_group| {
                u64::try_from(row_group.num_rows()).map_err(|_| ViewerError::InvalidRowCount)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let index = RowGroupIndex::new(&row_group_rows)?;

        let info = DatasetInfo {
            session,
            path: path.to_path_buf(),
            file_size: file_metadata.len(),
            total_rows: index.total_rows(),
            schema: metadata.schema().clone(),
            row_group_rows,
        };
        self.datasets.insert(
            session,
            OpenDataset {
                file,
                file_len: file_metadata.len(),
                modified: file_metadata.modified().ok(),
                metadata,
                index,
            },
        );
        Ok(info)
    }

    /// 요청한 행·열 범위를 Row Group 안의 정렬된 블록 요청으로 나눈다. I/O는 실행하지 않는다.
    ///
    /// 행 범위는 `[0, total_rows)`와 교차하고, 컬럼은 원본 스키마 순서로 정렬·중복 제거한다.
    /// 교집합이 비면 빈 목록을 돌려준다.
    pub fn plan_window(&self, request: &WindowRequest) -> Result<Vec<BlockRequest>, ViewerError> {
        let dataset = self.dataset(request.session)?;
        if request.rows.start > request.rows.end {
            return Err(ViewerError::InvalidRequest(
                "행 범위의 시작이 끝보다 큽니다",
            ));
        }
        let field_count = dataset.metadata.schema().fields().len();
        let columns = normalize_columns(&request.columns, field_count)?;
        let total_rows = dataset.index.total_rows();
        let rows = request.rows.start.min(total_rows)..request.rows.end.min(total_rows);
        Ok(reader::plan_blocks(
            request.session,
            &dataset.index,
            rows,
            &columns,
        ))
    }

    /// 블록 하나를 캐시에서 찾거나 디코딩한다. 디코딩한 블록은 캐시에 넣는다. 배치 처리 전후로
    /// `is_cancelled`를 확인한다.
    pub fn read_block(
        &mut self,
        block: &BlockRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<DecodedBlock, ViewerError> {
        let key = BlockKey {
            session: block.session,
            row_group: block.row_group,
            block_start: block.block_start,
            columns: block.columns.clone(),
        };
        self.ensure_unchanged(block.session)?;
        let batch = match self.cache.get(&key) {
            Some(batch) => batch,
            None => {
                let dataset = lookup(&self.datasets, block.session)?;
                let batch =
                    reader::decode_block(&dataset.file, &dataset.metadata, block, is_cancelled)?;
                self.ensure_unchanged(block.session)?;
                self.cache.insert(key, batch.clone());
                batch
            }
        };
        Ok(DecodedBlock {
            session: block.session,
            start_row: block.rows.start,
            row_count: batch.num_rows(),
            columns: block.columns.clone(),
            batch,
        })
    }

    /// 셀 하나의 원본 값을 읽는다. 블록 읽기와 같은 규칙으로 대상 행이 속한 블록을 정하고, 대상
    /// 컬럼 하나만 디코딩한 뒤 그 셀만 담은 길이 1의 새 배열로 복사한다.
    pub fn read_value(
        &mut self,
        request: &ValueRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ValueSlice, ViewerError> {
        let dataset = lookup(&self.datasets, request.session)?;
        let schema = dataset.metadata.schema();
        let Some(field) = schema.fields().get(request.column.0).cloned() else {
            return Err(ViewerError::InvalidRequest(
                "스키마 범위를 벗어난 컬럼입니다",
            ));
        };
        let Some((row_group, group_row)) = dataset.index.locate(request.row) else {
            return Err(ViewerError::InvalidRequest("파일 범위를 벗어난 행입니다"));
        };
        let group = dataset.index.row_group_range(row_group);
        let block_start = group_row / BLOCK_ROWS * BLOCK_ROWS;
        let block_end = (block_start + BLOCK_ROWS).min(group.end - group.start);
        let block = BlockRequest {
            session: request.session,
            row_group,
            block_start,
            rows: group.start + block_start..group.start + block_end,
            columns: vec![request.column],
        };

        self.ensure_unchanged(request.session)?;
        // 화면 범위를 읽으며 캐시에 넣은 블록에 대상 컬럼이 있으면 I/O 없이 쓴다.
        let cached =
            self.cache
                .find_column(request.session, row_group, block_start, request.column);
        let (batch, position) = match cached {
            Some(found) => found,
            None => {
                let dataset = lookup(&self.datasets, request.session)?;
                let batch =
                    reader::decode_block(&dataset.file, &dataset.metadata, &block, is_cancelled)?;
                self.ensure_unchanged(request.session)?;
                self.cache.insert(
                    BlockKey {
                        session: block.session,
                        row_group,
                        block_start,
                        columns: block.columns,
                    },
                    batch.clone(),
                );
                (batch, 0)
            }
        };
        let local = (group_row - block_start) as u32;
        let value = take(
            batch.column(position).as_ref(),
            &UInt32Array::from(vec![local]),
            None,
        )
        .map_err(|err| ViewerError::Decode(err.into()))?;
        Ok(ValueSlice {
            session: request.session,
            row: request.row,
            column: request.column,
            field,
            value,
        })
    }

    /// 원본 행 목록의 행을 그 순서대로 읽는다. 행마다 속한 블록을 캐시에서 찾거나 디코딩하고,
    /// 결과 배치의 i번째 행은 `rows[i]`다. 컬럼은 원본 스키마 순서로 정렬·중복 제거한다.
    pub fn read_rows(
        &mut self,
        session: SessionId,
        rows: &[u64],
        columns: &[ColumnId],
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<RecordBatch, ViewerError> {
        let dataset = self.dataset(session)?;
        let schema = dataset.metadata.schema().clone();
        let columns = normalize_columns(columns, schema.fields().len())?;
        let mut blocks: Vec<BlockRequest> = Vec::new();
        let mut block_ix: HashMap<(usize, u64), usize> = HashMap::new();
        let mut indices = Vec::with_capacity(rows.len());
        for &row in rows {
            let Some((row_group, group_row)) = dataset.index.locate(row) else {
                return Err(ViewerError::InvalidRequest("파일 범위를 벗어난 행입니다"));
            };
            let block_start = group_row / BLOCK_ROWS * BLOCK_ROWS;
            let ix = match block_ix.entry((row_group, block_start)) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => {
                    let group = dataset.index.row_group_range(row_group);
                    let block_end = (block_start + BLOCK_ROWS).min(group.end - group.start);
                    blocks.push(BlockRequest {
                        session,
                        row_group,
                        block_start,
                        rows: group.start + block_start..group.start + block_end,
                        columns: columns.clone(),
                    });
                    *entry.insert(blocks.len() - 1)
                }
            };
            indices.push((ix, (group_row - block_start) as usize));
        }
        if blocks.is_empty() {
            let projected = schema
                .project(&columns.iter().map(|column| column.0).collect::<Vec<_>>())
                .map_err(|err| ViewerError::Decode(err.into()))?;
            return Ok(RecordBatch::new_empty(Arc::new(projected)));
        }
        let mut batches = Vec::with_capacity(blocks.len());
        for block in &blocks {
            if is_cancelled() {
                return Err(ViewerError::Cancelled);
            }
            batches.push(self.read_block(block, is_cancelled)?.batch);
        }
        let refs: Vec<&RecordBatch> = batches.iter().collect();
        interleave_record_batch(&refs, &indices).map_err(|err| ViewerError::Decode(err.into()))
    }

    /// 지정한 컬럼을 Row Group 순서로 읽는 순차 읽기를 만든다. `selection`은 오름차순 원본 행
    /// 목록이며, 있으면 그 행만 읽는다. 순차 읽기는 캐시를 쓰지 않는다.
    pub fn scan(
        &self,
        session: SessionId,
        columns: &[ColumnId],
        selection: Option<Arc<RowIds>>,
    ) -> Result<ColumnScan, ViewerError> {
        let dataset = self.dataset(session)?;
        let columns = normalize_columns(columns, dataset.metadata.schema().fields().len())?;
        if columns.is_empty() {
            return Err(ViewerError::InvalidRequest("읽을 컬럼이 없습니다"));
        }
        dataset.ensure_unchanged()?;
        let groups = (0..dataset.index.row_group_count())
            .map(|group| dataset.index.row_group_range(group))
            .collect();
        Ok(ColumnScan::new(
            dataset.file.try_clone()?,
            dataset.file_len,
            dataset.modified,
            dataset.metadata.clone(),
            groups,
            &columns,
            selection,
        ))
    }

    /// 파일 구조. 메타데이터에 적힌 저장 정보만 쓰고 데이터 페이지는 읽지 않는다.
    pub fn file_structure(&self, session: SessionId) -> Result<FileStructure, ViewerError> {
        let dataset = self.dataset(session)?;
        Ok(structure::file_structure(
            dataset.metadata.metadata(),
            dataset.metadata.schema(),
        ))
    }

    /// Row Group 하나의 leaf 컬럼별 저장 정보.
    pub fn row_group_columns(
        &self,
        session: SessionId,
        row_group: usize,
    ) -> Result<Vec<ColumnChunkInfo>, ViewerError> {
        let dataset = self.dataset(session)?;
        if row_group >= dataset.index.row_group_count() {
            return Err(ViewerError::InvalidRequest("Row Group 범위를 벗어났습니다"));
        }
        Ok(structure::column_infos(
            dataset.metadata.metadata(),
            dataset.metadata.schema(),
            &[row_group],
        ))
    }

    /// 세션 파일의 Arrow 스키마.
    pub fn schema(&self, session: SessionId) -> Result<SchemaRef, ViewerError> {
        Ok(self.dataset(session)?.metadata.schema().clone())
    }

    /// 세션 파일의 전체 행 수.
    pub fn total_rows(&self, session: SessionId) -> Result<u64, ViewerError> {
        Ok(self.dataset(session)?.index.total_rows())
    }

    /// 세션의 파일 핸들, 메타데이터, 인덱스를 해제한다. 열려 있지 않은 세션이면 아무것도 하지 않는다.
    pub fn close(&mut self, session: SessionId) {
        self.datasets.remove(&session);
        self.cache.remove_session(session);
    }

    pub fn is_open(&self, session: SessionId) -> bool {
        self.datasets.contains_key(&session)
    }

    fn dataset(&self, session: SessionId) -> Result<&OpenDataset, ViewerError> {
        lookup(&self.datasets, session)
    }

    /// 파일이 열린 뒤 바뀌었으면 그 세션의 캐시 항목을 버리고 `FileChanged`를 돌려준다.
    fn ensure_unchanged(&mut self, session: SessionId) -> Result<(), ViewerError> {
        let result = lookup(&self.datasets, session)?.ensure_unchanged();
        if matches!(result, Err(ViewerError::FileChanged)) {
            self.cache.remove_session(session);
        }
        result
    }
}

fn lookup(
    datasets: &HashMap<SessionId, OpenDataset>,
    session: SessionId,
) -> Result<&OpenDataset, ViewerError> {
    datasets.get(&session).ok_or(ViewerError::StaleSession)
}

/// 컬럼을 원본 스키마 순서로 정렬하고 중복을 제거한다. 범위를 벗어난 ID는 오류다.
fn normalize_columns(
    columns: &[ColumnId],
    field_count: usize,
) -> Result<Vec<ColumnId>, ViewerError> {
    if columns.iter().any(|column| column.0 >= field_count) {
        return Err(ViewerError::InvalidRequest(
            "스키마 범위를 벗어난 컬럼입니다",
        ));
    }
    let mut columns = columns.to_vec();
    columns.sort_unstable();
    columns.dedup();
    Ok(columns)
}
