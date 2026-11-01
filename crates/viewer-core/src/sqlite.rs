//! SQLite tables and views as a data source, read-only.
//!
//! A table is shown in a fixed row order: rowid order for ordinary tables, primary key order for
//! `WITHOUT ROWID` tables, and the order SQLite returns for views and virtual tables without a rowid. Rows are
//! addressed by their position in that order, so blocks, row views and find work as for Parquet.
//!
//! Opening a table reads, in one read transaction, the row count, the storage classes present in each column,
//! and for rowid order the rowid at the start of every block. A block is then read with a range query on the
//! rowid; other orders use `LIMIT`/`OFFSET`. Each column gets one Arrow type from the storage classes found in
//! it: integers only → Int64, reals or integers and reals → Float64, text only → Utf8, blobs only → Binary, and
//! any other mix → Utf8, where numbers are written as SQLite text and blobs as `x'…'` literals. Columns without
//! values take the type of their declared affinity.
//!
//! No transaction stays open between reads, so writers are never blocked. The database file and its WAL file
//! are compared before reads, and a change ends the session with `FileChanged`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use arrow_array::builder::{BinaryBuilder, Float64Builder, Int64Builder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use arrow_select::interleave::interleave_record_batch;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params};

use crate::scan::{SCAN_BATCH_ROWS, ScanBatch};
use crate::{BLOCK_ROWS, BlockRequest, ColumnId, RowIds, ViewerError};

/// Field metadata key holding the column's declared SQLite type.
pub const DECLARED_TYPE_KEY: &str = "sqlite.declared_type";

const HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Whether the file starts with the SQLite database header.
pub fn is_sqlite_file(path: &Path) -> bool {
    let mut header = [0u8; 16];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok_and(|()| &header == HEADER)
}

/// Kind of a readable object in a SQLite database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqliteObjectKind {
    Table,
    View,
    Virtual,
}

/// A table or view of a SQLite database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqliteObject {
    pub name: String,
    pub kind: SqliteObjectKind,
}

/// Tables, views and virtual tables of the main schema, by name. Internal `sqlite_` tables and the shadow tables
/// of virtual tables are left out.
pub fn sqlite_objects(path: &Path) -> Result<Vec<SqliteObject>, ViewerError> {
    let conn = open_connection(path)?;
    let mut statement = conn.prepare("PRAGMA table_list")?;
    let mut rows = statement.query([])?;
    let mut objects = Vec::new();
    while let Some(row) = rows.next()? {
        let schema: String = row.get(0)?;
        let name: String = row.get(1)?;
        let kind: String = row.get(2)?;
        if schema != "main" || name.starts_with("sqlite_") {
            continue;
        }
        let kind = match kind.as_str() {
            "table" => SqliteObjectKind::Table,
            "view" => SqliteObjectKind::View,
            "virtual" => SqliteObjectKind::Virtual,
            _ => continue,
        };
        objects.push(SqliteObject { name, kind });
    }
    objects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(objects)
}

/// Structure of one table or view, read from the schema and pragmas. Data is not read.
#[derive(Clone, Debug)]
pub struct TableStructure {
    pub name: String,
    pub kind: SqliteObjectKind,
    pub without_rowid: bool,
    pub strict: bool,
    /// How rows are ordered on screen, e.g. `rowid` or `primary key (a, b)`.
    pub row_order: String,
    pub total_rows: u64,
    /// `CREATE` statement as stored in the schema.
    pub sql: Option<String>,
    pub columns: Vec<TableColumn>,
    pub indexes: Vec<TableIndex>,
    pub file_size: u64,
    pub page_size: u64,
    pub page_count: u64,
    pub encoding: String,
    pub journal_mode: String,
}

#[derive(Clone, Debug)]
pub struct TableColumn {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    /// Position within the primary key, starting at 1.
    pub primary_key: Option<u32>,
    /// Storage classes found in the column: `integer`, `real`, `text`, `blob`.
    pub storage: Vec<&'static str>,
    /// Arrow type the column is shown as.
    pub data_type: DataType,
}

#[derive(Clone, Debug)]
pub struct TableIndex {
    pub name: String,
    pub unique: bool,
    /// `c` for `CREATE INDEX`, `u` for a UNIQUE constraint, `pk` for the primary key.
    pub origin: String,
    pub partial: bool,
    pub columns: Vec<String>,
    pub sql: Option<String>,
}

const INTEGER: u8 = 1;
const REAL: u8 = 2;
const TEXT: u8 = 4;
const BLOB: u8 = 8;

/// How rows are ordered and addressed.
#[derive(Clone, Debug)]
enum RowOrder {
    /// Range queries on the rowid, under the given name. `block_keys[k]` is the rowid at row `k * BLOCK_ROWS`.
    Rowid {
        name: String,
        block_keys: Arc<Vec<i64>>,
    },
    /// `LIMIT`/`OFFSET` with an optional `ORDER BY` clause.
    Offset { order_by: Option<String> },
}

/// Size and modification time of the database file and its WAL file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Signature {
    db: (u64, Option<SystemTime>),
    wal: Option<(u64, Option<SystemTime>)>,
}

impl Signature {
    fn read(path: &Path) -> Result<Self, ViewerError> {
        let stamp = |meta: fs::Metadata| (meta.len(), meta.modified().ok());
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        Ok(Self {
            db: stamp(fs::metadata(path)?),
            wal: fs::metadata(PathBuf::from(wal)).ok().map(stamp),
        })
    }
}

/// An open table or view.
pub(crate) struct SqliteTable {
    path: PathBuf,
    table: String,
    kind: SqliteObjectKind,
    without_rowid: bool,
    strict: bool,
    conn: Connection,
    schema: SchemaRef,
    /// Quoted column names, in schema order.
    columns: Vec<String>,
    /// Storage classes found per column, as `INTEGER | REAL | TEXT | BLOB` bits.
    storage: Vec<u8>,
    order: RowOrder,
    total_rows: u64,
    signature: Signature,
}

impl SqliteTable {
    pub(crate) fn open(path: &Path, table: &str) -> Result<Self, ViewerError> {
        let signature = Signature::read(path)?;
        let mut conn = open_connection(path)?;
        let object = sqlite_objects(path)?
            .into_iter()
            .find(|object| object.name == table)
            .ok_or_else(|| ViewerError::Database(format!("no table or view named {table}")))?;
        let (without_rowid, strict) = conn.query_row(
            "SELECT wr, strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            [table],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
        )?;
        let infos = column_infos(&conn, table)?;
        if infos.is_empty() {
            return Err(ViewerError::Database(format!("{table} has no columns")));
        }
        let quoted_table = quote(table);
        let columns: Vec<String> = infos.iter().map(|info| quote(&info.name)).collect();

        let tx = conn.transaction()?;
        // One pass for the row count and the storage classes of every column.
        let mut sql = String::from("SELECT count(*)");
        for column in &columns {
            let _ = write!(
                sql,
                ", sum(DISTINCT CASE typeof({column}) WHEN 'integer' THEN 1 WHEN 'real' THEN 2 \
                 WHEN 'text' THEN 4 WHEN 'blob' THEN 8 ELSE 0 END)"
            );
        }
        let _ = write!(sql, " FROM {quoted_table}");
        let (total_rows, storage) = tx.query_row(&sql, [], |row| {
            let count: i64 = row.get(0)?;
            let storage = (1..=columns.len())
                .map(|ix| Ok(row.get::<_, Option<i64>>(ix)?.unwrap_or(0) as u8))
                .collect::<rusqlite::Result<Vec<u8>>>()?;
            Ok((count as u64, storage))
        })?;

        let rowid = rowid_name(&infos).filter(|name| {
            !without_rowid
                && object.kind != SqliteObjectKind::View
                && tx
                    .prepare(&format!("SELECT {name} FROM {quoted_table} LIMIT 0"))
                    .is_ok()
        });
        let order = match rowid {
            Some(name) => {
                let mut statement = tx.prepare(&format!(
                    "SELECT {name} FROM {quoted_table} ORDER BY {name}"
                ))?;
                let mut rows = statement.query([])?;
                let mut block_keys = Vec::new();
                let mut seen = 0u64;
                while let Some(row) = rows.next()? {
                    if seen.is_multiple_of(BLOCK_ROWS) {
                        block_keys.push(row.get::<_, i64>(0)?);
                    }
                    seen += 1;
                }
                if seen != total_rows {
                    return Err(ViewerError::FileChanged);
                }
                RowOrder::Rowid {
                    name,
                    block_keys: Arc::new(block_keys),
                }
            }
            None => {
                let mut key: Vec<&ColumnInfo> =
                    infos.iter().filter(|info| info.primary_key > 0).collect();
                key.sort_by_key(|info| info.primary_key);
                let order_by = (without_rowid && !key.is_empty()).then(|| {
                    key.iter()
                        .map(|info| quote(&info.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                });
                RowOrder::Offset { order_by }
            }
        };
        drop(tx);

        let fields: Vec<Field> = infos
            .iter()
            .zip(&storage)
            .map(|(info, &classes)| {
                Field::new(
                    &info.name,
                    arrow_type(classes, &info.declared_type),
                    !info.not_null,
                )
                .with_metadata(HashMap::from([(
                    DECLARED_TYPE_KEY.to_string(),
                    info.declared_type.clone(),
                )]))
            })
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            table: table.to_string(),
            kind: object.kind,
            without_rowid,
            strict,
            conn,
            schema: Arc::new(Schema::new(fields)),
            columns,
            storage,
            order,
            total_rows,
            signature,
        })
    }

    pub(crate) fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    pub(crate) fn total_rows(&self) -> u64 {
        self.total_rows
    }

    pub(crate) fn file_size(&self) -> u64 {
        self.signature.db.0
    }

    pub(crate) fn ensure_unchanged(&self) -> Result<(), ViewerError> {
        if Signature::read(&self.path)? != self.signature {
            return Err(ViewerError::FileChanged);
        }
        Ok(())
    }

    /// Reads one block: rows `block.rows` of the table order, columns `block.columns`.
    pub(crate) fn decode_block(
        &self,
        block: &BlockRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<RecordBatch, ViewerError> {
        if is_cancelled() {
            return Err(ViewerError::Cancelled);
        }
        let reader = self.reader(&block.columns);
        let count = block.rows.end - block.rows.start;
        let batch = reader.read_block(&self.conn, block.rows.start, count)?;
        if is_cancelled() {
            return Err(ViewerError::Cancelled);
        }
        Ok(batch)
    }

    /// Sequential scan with its own connection, so it can advance independently of the engine.
    pub(crate) fn scan(
        &self,
        columns: &[ColumnId],
        selection: Option<Arc<RowIds>>,
    ) -> Result<SqliteScan, ViewerError> {
        self.ensure_unchanged()?;
        let total = match &selection {
            Some(rows) => rows.len() as u64,
            None => self.total_rows,
        };
        Ok(SqliteScan {
            conn: open_connection(&self.path)?,
            reader: self.reader(columns),
            path: self.path.clone(),
            signature: self.signature,
            selection,
            next: 0,
            last_rowid: None,
            emitted: 0,
            total,
            total_rows: self.total_rows,
        })
    }

    fn reader(&self, columns: &[ColumnId]) -> BlockReader {
        BlockReader {
            table: quote(&self.table),
            columns: columns.iter().map(|c| self.columns[c.0].clone()).collect(),
            schema: Arc::new(
                self.schema
                    .project(&columns.iter().map(|c| c.0).collect::<Vec<_>>())
                    .expect("columns are within the schema"),
            ),
            order: self.order.clone(),
        }
    }

    pub(crate) fn structure(&self) -> Result<TableStructure, ViewerError> {
        let conn = &self.conn;
        let sql = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type IN ('table', 'view') AND name = ?1",
                [&self.table],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap_or(None);
        let infos = column_infos(conn, &self.table)?;
        let columns = infos
            .iter()
            .zip(&self.storage)
            .zip(self.schema.fields())
            .map(|((info, &classes), field)| TableColumn {
                name: info.name.clone(),
                declared_type: info.declared_type.clone(),
                not_null: info.not_null,
                default: info.default.clone(),
                primary_key: (info.primary_key > 0).then_some(info.primary_key as u32),
                storage: [
                    (INTEGER, "integer"),
                    (REAL, "real"),
                    (TEXT, "text"),
                    (BLOB, "blob"),
                ]
                .into_iter()
                .filter(|(bit, _)| classes & bit != 0)
                .map(|(_, name)| name)
                .collect(),
                data_type: field.data_type().clone(),
            })
            .collect();
        let mut indexes = Vec::new();
        if self.kind == SqliteObjectKind::Table {
            let mut statement = conn.prepare(
                "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1) ORDER BY seq",
            )?;
            let list = statement
                .query_map([&self.table], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (name, unique, origin, partial) in list {
                let mut statement =
                    conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
                let columns = statement
                    .query_map([&name], |row| row.get::<_, Option<String>>(0))?
                    .map(|column| column.map(|name| name.unwrap_or_else(|| "(expression)".into())))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let sql = conn
                    .query_row(
                        "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                        [&name],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .unwrap_or(None);
                indexes.push(TableIndex {
                    name,
                    unique,
                    origin,
                    partial,
                    columns,
                    sql,
                });
            }
        }
        let pragma_u64 = |name: &str| -> Result<u64, ViewerError> {
            Ok(conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))? as u64)
        };
        let pragma_text = |name: &str| -> Result<String, ViewerError> {
            Ok(conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, String>(0))?)
        };
        Ok(TableStructure {
            name: self.table.clone(),
            kind: self.kind,
            without_rowid: self.without_rowid,
            strict: self.strict,
            row_order: match &self.order {
                RowOrder::Rowid { name, .. } => name.trim_matches('"').to_string(),
                RowOrder::Offset {
                    order_by: Some(key),
                } => format!("primary key ({})", key.replace('"', "")),
                RowOrder::Offset { order_by: None } => "query order".to_string(),
            },
            total_rows: self.total_rows,
            sql,
            columns,
            indexes,
            file_size: self.signature.db.0,
            page_size: pragma_u64("page_size")?,
            page_count: pragma_u64("page_count")?,
            encoding: pragma_text("encoding")?,
            journal_mode: pragma_text("journal_mode")?,
        })
    }
}

/// Reads rows of the table order into Arrow batches.
struct BlockReader {
    table: String,
    columns: Vec<String>,
    schema: SchemaRef,
    order: RowOrder,
}

impl BlockReader {
    /// Rows `[start, start + count)` of the table order. `start` must be a block start for rowid order.
    fn read_block(
        &self,
        conn: &Connection,
        start: u64,
        count: u64,
    ) -> Result<RecordBatch, ViewerError> {
        let columns = self.columns.join(", ");
        let table = &self.table;
        let (batch, _) = match &self.order {
            RowOrder::Rowid { name, block_keys } => {
                debug_assert_eq!(start % BLOCK_ROWS, 0);
                let key = *block_keys
                    .get((start / BLOCK_ROWS) as usize)
                    .ok_or(ViewerError::FileChanged)?;
                let sql = format!(
                    "SELECT {name}, {columns} FROM {table} WHERE {name} >= ?1 ORDER BY {name} LIMIT ?2"
                );
                self.query(conn, &sql, params![key, count as i64], true)?
            }
            RowOrder::Offset { order_by } => {
                let order = order_by
                    .as_ref()
                    .map(|key| format!(" ORDER BY {key}"))
                    .unwrap_or_default();
                let sql = format!("SELECT {columns} FROM {table}{order} LIMIT ?1 OFFSET ?2");
                self.query(conn, &sql, params![count as i64, start as i64], false)?
            }
        };
        if batch.num_rows() as u64 != count {
            return Err(ViewerError::FileChanged);
        }
        Ok(batch)
    }

    /// Runs a query and builds a batch. With `leading_key`, the first result column is the rowid, returned
    /// separately as the last one read.
    fn query(
        &self,
        conn: &Connection,
        sql: &str,
        params: impl rusqlite::Params,
        leading_key: bool,
    ) -> Result<(RecordBatch, Option<i64>), ViewerError> {
        let mut statement = conn.prepare_cached(sql)?;
        let mut rows = statement.query(params)?;
        let mut builders: Vec<ColumnBuilder> = self
            .schema
            .fields()
            .iter()
            .map(|field| ColumnBuilder::new(field.data_type()))
            .collect();
        let offset = usize::from(leading_key);
        let mut last_key = None;
        while let Some(row) = rows.next()? {
            if leading_key {
                last_key = Some(row.get::<_, i64>(0)?);
            }
            for (ix, builder) in builders.iter_mut().enumerate() {
                builder.push(row.get_ref(ix + offset)?)?;
            }
        }
        let arrays: Vec<ArrayRef> = builders.into_iter().map(ColumnBuilder::finish).collect();
        let batch = RecordBatch::try_new(self.schema.clone(), arrays)
            .map_err(|err| ViewerError::Decode(err.into()))?;
        Ok((batch, last_key))
    }
}

/// Sequential scan of a table in its row order.
pub(crate) struct SqliteScan {
    conn: Connection,
    reader: BlockReader,
    path: PathBuf,
    signature: Signature,
    selection: Option<Arc<RowIds>>,
    /// Next position: within the row list with a selection, otherwise within the table order.
    next: u64,
    /// Rowid of the last row returned, for rowid order without a selection.
    last_rowid: Option<i64>,
    emitted: u64,
    total: u64,
    total_rows: u64,
}

impl SqliteScan {
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn emitted(&self) -> u64 {
        self.emitted
    }

    pub(crate) fn next_batch(&mut self) -> Result<Option<ScanBatch>, ViewerError> {
        if self.next >= self.total {
            return Ok(None);
        }
        if Signature::read(&self.path)? != self.signature {
            return Err(ViewerError::FileChanged);
        }
        let start = self.next;
        let batch = match &self.selection {
            None => self.next_rows()?,
            Some(rows) => {
                let rows = rows.clone();
                self.selected_rows(&rows)?
            }
        };
        self.next += batch.num_rows() as u64;
        self.emitted += batch.num_rows() as u64;
        Ok(Some(ScanBatch { start, batch }))
    }

    /// The next rows of the table order.
    fn next_rows(&mut self) -> Result<RecordBatch, ViewerError> {
        let count = (self.total - self.next).min(SCAN_BATCH_ROWS as u64);
        let columns = self.reader.columns.join(", ");
        let table = &self.reader.table;
        let (batch, last) = match &self.reader.order {
            RowOrder::Rowid { name, .. } => match self.last_rowid {
                None => self.reader.query(
                    &self.conn,
                    &format!("SELECT {name}, {columns} FROM {table} ORDER BY {name} LIMIT ?1"),
                    params![count as i64],
                    true,
                )?,
                Some(last) => self.reader.query(
                    &self.conn,
                    &format!(
                        "SELECT {name}, {columns} FROM {table} WHERE {name} > ?1 ORDER BY {name} LIMIT ?2"
                    ),
                    params![last, count as i64],
                    true,
                )?,
            },
            RowOrder::Offset { .. } => (
                self.reader.read_block(&self.conn, self.next, count)?,
                None,
            ),
        };
        if batch.num_rows() as u64 != count {
            return Err(ViewerError::FileChanged);
        }
        self.last_rowid = last;
        Ok(batch)
    }

    /// The next rows of the row list: each block holding listed rows is read once, then the rows are picked.
    fn selected_rows(&mut self, rows: &RowIds) -> Result<RecordBatch, ViewerError> {
        let end = (self.next + SCAN_BATCH_ROWS as u64).min(self.total);
        let mut blocks: Vec<RecordBatch> = Vec::new();
        let mut current: Option<u64> = None;
        let mut indices = Vec::with_capacity((end - self.next) as usize);
        for position in self.next..end {
            let row = rows
                .get(position as usize)
                .expect("position within the row list");
            let block_start = row / BLOCK_ROWS * BLOCK_ROWS;
            if current != Some(block_start) {
                let count = (block_start + BLOCK_ROWS).min(self.total_rows) - block_start;
                blocks.push(self.reader.read_block(&self.conn, block_start, count)?);
                current = Some(block_start);
            }
            indices.push((blocks.len() - 1, (row - block_start) as usize));
        }
        let refs: Vec<&RecordBatch> = blocks.iter().collect();
        interleave_record_batch(&refs, &indices).map_err(|err| ViewerError::Decode(err.into()))
    }
}

/// Column of `PRAGMA table_xinfo`, without hidden columns of virtual tables.
struct ColumnInfo {
    name: String,
    declared_type: String,
    not_null: bool,
    default: Option<String>,
    primary_key: i64,
}

fn column_infos(conn: &Connection, table: &str) -> Result<Vec<ColumnInfo>, ViewerError> {
    let mut statement = conn.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1) ORDER BY cid",
    )?;
    let infos = statement
        .query_map([table], |row| {
            Ok((
                ColumnInfo {
                    name: row.get(0)?,
                    declared_type: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    not_null: row.get(2)?,
                    default: row.get(3)?,
                    primary_key: row.get(4)?,
                },
                row.get::<_, i64>(5)?,
            ))
        })?
        .filter_map(|info| match info {
            Ok((_, 1)) => None,
            Ok((info, _)) => Some(Ok(info)),
            Err(err) => Some(Err(err)),
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(infos)
}

/// Name under which the rowid is reachable: `rowid`, `_rowid_` or `oid`, whichever no column shadows.
fn rowid_name(infos: &[ColumnInfo]) -> Option<String> {
    ["rowid", "_rowid_", "oid"]
        .into_iter()
        .find(|alias| {
            !infos
                .iter()
                .any(|info| info.name.eq_ignore_ascii_case(alias))
        })
        .map(str::to_string)
}

fn open_connection(path: &Path) -> Result<Connection, ViewerError> {
    if !is_sqlite_file(path) {
        // Check the header first so a missing or foreign file fails clearly instead of as an empty database.
        File::open(path)?;
        return Err(ViewerError::Database("not a SQLite database".into()));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA query_only = ON")?;
    Ok(conn)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Arrow type of a column from the storage classes found in it, or from its declared affinity without values.
fn arrow_type(classes: u8, declared: &str) -> DataType {
    match classes {
        0 => {
            let declared = declared.to_ascii_uppercase();
            if declared.contains("INT") {
                DataType::Int64
            } else if ["REAL", "FLOA", "DOUB"]
                .iter()
                .any(|t| declared.contains(t))
            {
                DataType::Float64
            } else if declared.contains("BLOB") {
                DataType::Binary
            } else {
                DataType::Utf8
            }
        }
        INTEGER => DataType::Int64,
        REAL | 3 => DataType::Float64,
        BLOB => DataType::Binary,
        _ => DataType::Utf8,
    }
}

enum ColumnBuilder {
    Int(Int64Builder),
    Float(Float64Builder),
    Text(StringBuilder),
    Binary(BinaryBuilder),
}

impl ColumnBuilder {
    fn new(data_type: &DataType) -> Self {
        match data_type {
            DataType::Int64 => Self::Int(Int64Builder::new()),
            DataType::Float64 => Self::Float(Float64Builder::new()),
            DataType::Binary => Self::Binary(BinaryBuilder::new()),
            _ => Self::Text(StringBuilder::new()),
        }
    }

    /// Appends one value. A storage class the column type cannot hold means the data changed after opening.
    fn push(&mut self, value: ValueRef) -> Result<(), ViewerError> {
        match (self, value) {
            (Self::Int(b), ValueRef::Null) => b.append_null(),
            (Self::Int(b), ValueRef::Integer(v)) => b.append_value(v),
            (Self::Float(b), ValueRef::Null) => b.append_null(),
            (Self::Float(b), ValueRef::Integer(v)) => b.append_value(v as f64),
            (Self::Float(b), ValueRef::Real(v)) => b.append_value(v),
            (Self::Binary(b), ValueRef::Null) => b.append_null(),
            (Self::Binary(b), ValueRef::Blob(v)) => b.append_value(v),
            (Self::Text(b), ValueRef::Null) => b.append_null(),
            (Self::Text(b), ValueRef::Integer(v)) => b.append_value(v.to_string()),
            (Self::Text(b), ValueRef::Real(v)) => b.append_value(real_text(v)),
            (Self::Text(b), ValueRef::Text(v)) => b.append_value(String::from_utf8_lossy(v)),
            (Self::Text(b), ValueRef::Blob(v)) => {
                let mut text = String::with_capacity(v.len() * 2 + 3);
                text.push_str("x'");
                for byte in v {
                    let _ = write!(text, "{byte:02x}");
                }
                text.push('\'');
                b.append_value(text);
            }
            _ => return Err(ViewerError::FileChanged),
        }
        Ok(())
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Int(mut b) => Arc::new(b.finish()),
            Self::Float(mut b) => Arc::new(b.finish()),
            Self::Text(mut b) => Arc::new(b.finish()),
            Self::Binary(mut b) => Arc::new(b.finish()),
        }
    }
}

/// Text of a real in a mixed column: the shortest form that reads back as the same value, with a decimal point
/// or exponent so it stays distinguishable from an integer.
fn real_text(value: f64) -> String {
    format!("{value:?}")
}

impl From<rusqlite::Error> for ViewerError {
    fn from(err: rusqlite::Error) -> Self {
        ViewerError::Database(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;

    #[test]
    fn picks_arrow_types_from_storage_classes() {
        assert_eq!(arrow_type(INTEGER, "TEXT"), DataType::Int64);
        assert_eq!(arrow_type(INTEGER | REAL, ""), DataType::Float64);
        assert_eq!(arrow_type(TEXT | INTEGER, "INTEGER"), DataType::Utf8);
        assert_eq!(arrow_type(BLOB, ""), DataType::Binary);
        assert_eq!(arrow_type(BLOB | TEXT, ""), DataType::Utf8);
        assert_eq!(arrow_type(0, "BIGINT"), DataType::Int64);
        assert_eq!(arrow_type(0, "double precision"), DataType::Float64);
        assert_eq!(arrow_type(0, "VARCHAR(10)"), DataType::Utf8);
        assert_eq!(arrow_type(0, ""), DataType::Utf8);
    }

    #[test]
    fn writes_mixed_values_as_text() {
        let mut builder = ColumnBuilder::new(&DataType::Utf8);
        builder.push(ValueRef::Integer(7)).unwrap();
        builder.push(ValueRef::Real(1.0)).unwrap();
        builder.push(ValueRef::Real(1e100)).unwrap();
        builder.push(ValueRef::Blob(&[0x0a, 0xff])).unwrap();
        builder.push(ValueRef::Null).unwrap();
        let array = builder.finish();
        let array = array
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap();
        assert_eq!(array.value(0), "7");
        assert_eq!(array.value(1), "1.0");
        assert_eq!(array.value(2), "1e100");
        assert_eq!(array.value(3), "x'0aff'");
        assert!(array.is_null(4));
        let mut ints = ColumnBuilder::new(&DataType::Int64);
        assert!(matches!(
            ints.push(ValueRef::Text(b"a")),
            Err(ViewerError::FileChanged)
        ));
    }

    #[test]
    fn quotes_identifiers() {
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
    }
}
