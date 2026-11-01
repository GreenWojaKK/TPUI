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
use crate::sqlite::{SqliteTable, TableStructure};
use crate::structure::{self, ColumnChunkInfo, FileStructure};
use crate::{
    BLOCK_ROWS, BlockRequest, ColumnId, DataSource, DatasetInfo, DecodedBlock, RowGroupIndex,
    RowIds, SessionId, ValueRequest, ValueSlice, ViewerError, WindowRequest,
};

/// Synchronous engine that keeps several sources open read-only, one per session: Parquet files and SQLite
/// tables. The worker decides when it is called.
///
/// Decoded blocks live in an LRU cache with a byte budget shared by all sessions.
pub struct ViewerEngine {
    datasets: HashMap<SessionId, OpenDataset>,
    cache: BlockCache,
}

impl Default for ViewerEngine {
    fn default() -> Self {
        Self::with_cache_budget(DEFAULT_CACHE_BUDGET)
    }
}

/// Session state of an open source. File handles, connections and metadata are reused for the whole session.
struct OpenDataset {
    schema: SchemaRef,
    index: RowGroupIndex,
    backend: Backend,
}

enum Backend {
    Parquet {
        file: File,
        /// File size and modification time seen at open. Compared before and after reads to detect changes.
        file_len: u64,
        modified: Option<SystemTime>,
        metadata: ArrowReaderMetadata,
    },
    Sqlite(Box<SqliteTable>),
}

impl OpenDataset {
    fn ensure_unchanged(&self) -> Result<(), ViewerError> {
        match &self.backend {
            Backend::Parquet {
                file,
                file_len,
                modified,
                ..
            } => {
                let current = file.metadata()?;
                if current.len() != *file_len || current.modified().ok() != *modified {
                    return Err(ViewerError::FileChanged);
                }
                Ok(())
            }
            Backend::Sqlite(table) => table.ensure_unchanged(),
        }
    }

    fn decode(
        &self,
        block: &BlockRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<RecordBatch, ViewerError> {
        match &self.backend {
            Backend::Parquet { file, metadata, .. } => {
                reader::decode_block(file, metadata, block, is_cancelled)
            }
            Backend::Sqlite(table) => table.decode_block(block, is_cancelled),
        }
    }

    fn parquet_metadata(&self) -> Result<&ArrowReaderMetadata, ViewerError> {
        match &self.backend {
            Backend::Parquet { metadata, .. } => Ok(metadata),
            Backend::Sqlite(_) => Err(ViewerError::InvalidRequest("not a Parquet file")),
        }
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

    /// Bytes of Arrow buffers held by the cache.
    pub fn cache_bytes(&self) -> usize {
        self.cache.used_bytes()
    }

    /// Opens a source as a new session. See [`Self::open`] and [`Self::open_sqlite`].
    pub fn open_source(
        &mut self,
        session: SessionId,
        source: &DataSource,
    ) -> Result<DatasetInfo, ViewerError> {
        match source {
            DataSource::Parquet(path) => self.open(session, path),
            DataSource::Sqlite { path, table } => self.open_sqlite(session, path, table),
        }
    }

    /// Opens a table or view of a SQLite database read-only as a new session. Reads the row count, the storage
    /// classes of each column and, for rowid tables, the rowid at every block start. Other sessions are left
    /// untouched.
    ///
    /// If the session number is already open, it is closed first, whether or not the open succeeds.
    pub fn open_sqlite(
        &mut self,
        session: SessionId,
        path: &Path,
        table: &str,
    ) -> Result<DatasetInfo, ViewerError> {
        self.close(session);
        let opened = SqliteTable::open(path, table)?;
        let total_rows = opened.total_rows();
        let index = RowGroupIndex::new(&[total_rows])?;
        let info = DatasetInfo {
            session,
            source: DataSource::Sqlite {
                path: path.to_path_buf(),
                table: table.to_string(),
            },
            path: path.to_path_buf(),
            file_size: opened.file_size(),
            total_rows,
            schema: opened.schema().clone(),
            row_group_rows: vec![total_rows],
        };
        self.datasets.insert(
            session,
            OpenDataset {
                schema: opened.schema().clone(),
                index,
                backend: Backend::Sqlite(Box::new(opened)),
            },
        );
        Ok(info)
    }

    /// Opens the file read-only as a new session and reads only the footer and metadata. Data
    /// pages are not read, and other sessions are left untouched.
    ///
    /// If the session number is already open, it is closed first, whether or not the open succeeds.
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
            source: DataSource::Parquet(path.to_path_buf()),
            path: path.to_path_buf(),
            file_size: file_metadata.len(),
            total_rows: index.total_rows(),
            schema: metadata.schema().clone(),
            row_group_rows,
        };
        self.datasets.insert(
            session,
            OpenDataset {
                schema: metadata.schema().clone(),
                index,
                backend: Backend::Parquet {
                    file,
                    file_len: file_metadata.len(),
                    modified: file_metadata.modified().ok(),
                    metadata,
                },
            },
        );
        Ok(info)
    }

    /// Splits the requested row and column ranges into sorted block requests within Row Groups. Performs no I/O.
    ///
    /// Rows are intersected with `[0, total_rows)`; columns are sorted into source schema order and deduplicated.
    /// An empty intersection yields an empty list.
    pub fn plan_window(&self, request: &WindowRequest) -> Result<Vec<BlockRequest>, ViewerError> {
        let dataset = self.dataset(request.session)?;
        if request.rows.start > request.rows.end {
            return Err(ViewerError::InvalidRequest(
                "row range start is greater than its end",
            ));
        }
        let columns = normalize_columns(&request.columns, dataset.schema.fields().len())?;
        let total_rows = dataset.index.total_rows();
        let rows = request.rows.start.min(total_rows)..request.rows.end.min(total_rows);
        Ok(reader::plan_blocks(
            request.session,
            &dataset.index,
            rows,
            &columns,
        ))
    }

    /// Finds one block in the cache or decodes it, inserting decoded blocks into the cache. Checks
    /// `is_cancelled` before and after batch processing.
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
                let batch = lookup(&self.datasets, block.session)?.decode(block, is_cancelled)?;
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

    /// Reads the raw value of one cell. Picks the block holding the row with the same rules as block reads,
    /// decodes only the target column, and copies just that cell into a new array of length 1.
    pub fn read_value(
        &mut self,
        request: &ValueRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ValueSlice, ViewerError> {
        let dataset = lookup(&self.datasets, request.session)?;
        let Some(field) = dataset.schema.fields().get(request.column.0).cloned() else {
            return Err(ViewerError::InvalidRequest("column is outside the schema"));
        };
        let Some((row_group, group_row)) = dataset.index.locate(request.row) else {
            return Err(ViewerError::InvalidRequest("row is outside the file"));
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
        // A block cached while reading the visible range is reused without I/O if it has the target column.
        let cached =
            self.cache
                .find_column(request.session, row_group, block_start, request.column);
        let (batch, position) = match cached {
            Some(found) => found,
            None => {
                let batch =
                    lookup(&self.datasets, request.session)?.decode(&block, is_cancelled)?;
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

    /// Reads rows from a list of source rows, in list order. Each row's block is found in the cache or decoded,
    /// and row i of the result is `rows[i]`. Columns are sorted into source schema order and deduplicated.
    pub fn read_rows(
        &mut self,
        session: SessionId,
        rows: &[u64],
        columns: &[ColumnId],
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<RecordBatch, ViewerError> {
        let dataset = self.dataset(session)?;
        let schema = dataset.schema.clone();
        let columns = normalize_columns(columns, schema.fields().len())?;
        let mut blocks: Vec<BlockRequest> = Vec::new();
        let mut block_ix: HashMap<(usize, u64), usize> = HashMap::new();
        let mut indices = Vec::with_capacity(rows.len());
        for &row in rows {
            let Some((row_group, group_row)) = dataset.index.locate(row) else {
                return Err(ViewerError::InvalidRequest("row is outside the file"));
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

    /// Creates a sequential scan reading the given columns in source order (Row Group order for Parquet, the
    /// table's row order for SQLite). `selection` is an ascending list of source rows; if present, only those
    /// rows are read. Scans do not use the cache.
    pub fn scan(
        &self,
        session: SessionId,
        columns: &[ColumnId],
        selection: Option<Arc<RowIds>>,
    ) -> Result<ColumnScan, ViewerError> {
        let dataset = self.dataset(session)?;
        let columns = normalize_columns(columns, dataset.schema.fields().len())?;
        if columns.is_empty() {
            return Err(ViewerError::InvalidRequest("no columns to read"));
        }
        dataset.ensure_unchanged()?;
        match &dataset.backend {
            Backend::Parquet {
                file,
                file_len,
                modified,
                metadata,
            } => {
                let groups = (0..dataset.index.row_group_count())
                    .map(|group| dataset.index.row_group_range(group))
                    .collect();
                Ok(ColumnScan::parquet(
                    file.try_clone()?,
                    *file_len,
                    *modified,
                    metadata.clone(),
                    groups,
                    &columns,
                    selection,
                ))
            }
            Backend::Sqlite(table) => Ok(ColumnScan::sqlite(table.scan(&columns, selection)?)),
        }
    }

    /// File structure. Uses only the storage information in the metadata and reads no data pages.
    pub fn file_structure(&self, session: SessionId) -> Result<FileStructure, ViewerError> {
        let metadata = self.dataset(session)?.parquet_metadata()?;
        Ok(structure::file_structure(
            metadata.metadata(),
            metadata.schema(),
        ))
    }

    /// Structure of a SQLite table or view: columns, indexes, `CREATE` statement and database pragmas.
    pub fn table_structure(&self, session: SessionId) -> Result<TableStructure, ViewerError> {
        match &self.dataset(session)?.backend {
            Backend::Sqlite(table) => table.structure(),
            Backend::Parquet { .. } => Err(ViewerError::InvalidRequest("not a SQLite table")),
        }
    }

    /// Per-leaf-column storage information of one Row Group.
    pub fn row_group_columns(
        &self,
        session: SessionId,
        row_group: usize,
    ) -> Result<Vec<ColumnChunkInfo>, ViewerError> {
        let dataset = self.dataset(session)?;
        let metadata = dataset.parquet_metadata()?;
        if row_group >= dataset.index.row_group_count() {
            return Err(ViewerError::InvalidRequest("Row Group is out of range"));
        }
        Ok(structure::column_infos(
            metadata.metadata(),
            metadata.schema(),
            &[row_group],
        ))
    }

    /// Arrow schema of the session's file.
    pub fn schema(&self, session: SessionId) -> Result<SchemaRef, ViewerError> {
        Ok(self.dataset(session)?.schema.clone())
    }

    /// Total rows of the session's file.
    pub fn total_rows(&self, session: SessionId) -> Result<u64, ViewerError> {
        Ok(self.dataset(session)?.index.total_rows())
    }

    /// Releases the session's file handle, metadata and index. Does nothing if the session is not open.
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

    /// If the file changed after opening, drops the session's cache entries and returns `FileChanged`.
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

/// Sorts columns into source schema order and removes duplicates. Out-of-range IDs are an error.
fn normalize_columns(
    columns: &[ColumnId],
    field_count: usize,
) -> Result<Vec<ColumnId>, ViewerError> {
    if columns.iter().any(|column| column.0 >= field_count) {
        return Err(ViewerError::InvalidRequest("column is outside the schema"));
    }
    let mut columns = columns.to_vec();
    columns.sort_unstable();
    columns.dedup();
    Ok(columns)
}
