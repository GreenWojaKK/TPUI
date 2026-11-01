//! Contracts of SQLite sources: table listing, typing, block and value reads, scans, row views, find, table
//! structure and change detection, checked against databases written by rusqlite.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int64Type};
use arrow_array::{Array, RecordBatch};
use arrow_schema::DataType;
use arrow_select::concat::concat_batches;
use rusqlite::{Connection, params};
use viewer_core::{
    ColumnId, DECLARED_TYPE_KEY, DataSource, FilterCondition, FilterOp, FindQuery, FindTask,
    RequestId, RowIds, RowView, RowViewTask, SessionId, SortKey, SqliteObject, SqliteObjectKind,
    TaskStep, ViewSpec, ViewerEngine, ViewerError, WindowRequest, is_sqlite_file, sqlite_objects,
};

/// Rows inserted into `items`; every seventh id is deleted afterwards, leaving rowid gaps.
const INSERTED: i64 = 1500;

fn kept_ids() -> Vec<i64> {
    (1..=INSERTED).filter(|id| id % 7 != 0).collect()
}

fn name(id: i64) -> Option<String> {
    (id % 5 != 0).then(|| format!("item-{id}"))
}

fn score(id: i64) -> Option<f64> {
    (id % 11 != 0).then(|| ((id * 37) % 101) as f64 / 4.)
}

fn write_fixture(path: &Path) {
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, score REAL, mixed, data BLOB);
         CREATE INDEX items_name ON items (name);
         CREATE TABLE pairs (a TEXT NOT NULL, b INTEGER NOT NULL, v, PRIMARY KEY (a, b)) WITHOUT ROWID;
         CREATE TABLE empty (x INTEGER, y DOUBLE, z VARCHAR(4));
         CREATE VIEW scored AS SELECT id, score FROM items WHERE score > 20;",
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    for id in 1..=INSERTED {
        let mixed: rusqlite::types::Value = match id % 3 {
            0 => id.into(),
            1 => format!("t{id}").into(),
            _ => (id as f64 + 0.5).into(),
        };
        tx.execute(
            "INSERT INTO items VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name(id), score(id), mixed, vec![id as u8, 0xff]],
        )
        .unwrap();
    }
    // Inserted out of key order; the table is shown in primary key order.
    for b in (0..1100).rev() {
        tx.execute(
            "INSERT INTO pairs VALUES (?1, ?2, ?3)",
            params![format!("k{}", b % 3), b, b * 2],
        )
        .unwrap();
    }
    tx.execute("DELETE FROM items WHERE id % 7 = 0", [])
        .unwrap();
    tx.commit().unwrap();
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    write_fixture(&path);
    (dir, path)
}

fn open(engine: &mut ViewerEngine, path: &Path, table: &str) -> u64 {
    engine
        .open_source(
            SessionId(1),
            &DataSource::Sqlite {
                path: path.to_path_buf(),
                table: table.into(),
            },
        )
        .unwrap()
        .total_rows
}

/// Every row of the session through the block path, as one batch.
fn read_all(engine: &mut ViewerEngine, columns: &[usize]) -> RecordBatch {
    let total = engine.total_rows(SessionId(1)).unwrap();
    let request = WindowRequest {
        session: SessionId(1),
        request: RequestId(1),
        rows: 0..total,
        columns: columns.iter().map(|c| ColumnId(*c)).collect(),
    };
    let blocks = engine.plan_window(&request).unwrap();
    let batches: Vec<RecordBatch> = blocks
        .iter()
        .map(|block| engine.read_block(block, &|| false).unwrap().batch)
        .collect();
    concat_batches(&batches[0].schema(), &batches).unwrap()
}

fn ints(batch: &RecordBatch, column: usize) -> Vec<i64> {
    batch
        .column(column)
        .as_primitive::<Int64Type>()
        .values()
        .to_vec()
}

fn build_view(engine: &ViewerEngine, spec: &ViewSpec) -> RowView {
    let mut task = RowViewTask::new(engine, SessionId(1), spec).unwrap();
    loop {
        if let TaskStep::Done(view) = task.step(engine).unwrap() {
            return view;
        }
    }
}

#[test]
fn lists_tables_and_views_and_recognizes_the_header() {
    let (dir, path) = fixture();
    assert!(is_sqlite_file(&path));
    let other = dir.path().join("other.db");
    std::fs::write(&other, b"not a database").unwrap();
    assert!(!is_sqlite_file(&other));

    let objects = sqlite_objects(&path).unwrap();
    let object = |name: &str, kind| SqliteObject {
        name: name.into(),
        kind,
    };
    assert_eq!(
        objects,
        vec![
            object("empty", SqliteObjectKind::Table),
            object("items", SqliteObjectKind::Table),
            object("pairs", SqliteObjectKind::Table),
            object("scored", SqliteObjectKind::View),
        ]
    );
    assert!(matches!(
        sqlite_objects(&other),
        Err(ViewerError::Database(_))
    ));
}

#[test]
fn types_columns_by_the_storage_classes_they_hold() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    let total = open(&mut engine, &path, "items");
    assert_eq!(total, kept_ids().len() as u64);
    let schema = engine.schema(SessionId(1)).unwrap();
    let types: Vec<&DataType> = schema.fields().iter().map(|f| f.data_type()).collect();
    assert_eq!(
        types,
        [
            &DataType::Int64,
            &DataType::Utf8,
            &DataType::Float64,
            &DataType::Utf8,
            &DataType::Binary
        ]
    );
    assert_eq!(schema.field(2).metadata()[DECLARED_TYPE_KEY], "REAL");

    // Without values, the declared affinity decides.
    open(&mut engine, &path, "empty");
    let schema = engine.schema(SessionId(1)).unwrap();
    let types: Vec<&DataType> = schema.fields().iter().map(|f| f.data_type()).collect();
    assert_eq!(
        types,
        [&DataType::Int64, &DataType::Float64, &DataType::Utf8]
    );
    assert_eq!(engine.total_rows(SessionId(1)).unwrap(), 0);
}

#[test]
fn reads_blocks_in_rowid_order_across_gaps() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    open(&mut engine, &path, "items");
    let batch = read_all(&mut engine, &[0, 1, 2, 3, 4]);
    let ids = kept_ids();
    assert_eq!(ints(&batch, 0), ids);
    let names = batch.column(1).as_string::<i32>();
    let scores = batch.column(2).as_primitive::<Float64Type>();
    let mixed = batch.column(3).as_string::<i32>();
    let data = batch.column(4).as_binary::<i32>();
    for (ix, &id) in ids.iter().enumerate() {
        assert_eq!(
            names.is_valid(ix).then(|| names.value(ix).to_string()),
            name(id)
        );
        assert_eq!(scores.is_valid(ix).then(|| scores.value(ix)), score(id));
        let expected = match id % 3 {
            0 => id.to_string(),
            1 => format!("t{id}"),
            _ => format!("{:?}", id as f64 + 0.5),
        };
        assert_eq!(mixed.value(ix), expected);
        assert_eq!(data.value(ix), [id as u8, 0xff]);
    }
}

#[test]
fn reads_single_values_and_rows_in_requested_order() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    open(&mut engine, &path, "items");
    let ids = kept_ids();
    let value = engine
        .read_value(
            &viewer_core::ValueRequest {
                session: SessionId(1),
                request: RequestId(2),
                row: 700,
                column: ColumnId(0),
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(value.value.as_primitive::<Int64Type>().value(0), ids[700]);

    let rows = [1280u64, 3, 600, 3];
    let batch = engine
        .read_rows(SessionId(1), &rows, &[ColumnId(0)], &|| false)
        .unwrap();
    let expected: Vec<i64> = rows.iter().map(|&row| ids[row as usize]).collect();
    assert_eq!(ints(&batch, 0), expected);
}

#[test]
fn scans_all_rows_and_listed_rows() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    open(&mut engine, &path, "items");
    let ids = kept_ids();
    let mut scan = engine.scan(SessionId(1), &[ColumnId(0)], None).unwrap();
    let mut seen = Vec::new();
    while let Some(batch) = scan.next_batch().unwrap() {
        assert_eq!(batch.start, seen.len() as u64);
        seen.extend(ints(&batch.batch, 0));
    }
    assert_eq!(seen, ids);

    let wanted = [0u64, 1, 511, 512, 1000, ids.len() as u64 - 1];
    let mut rows = RowIds::for_total_rows(ids.len() as u64);
    for row in wanted {
        rows.push(row);
    }
    let mut scan = engine
        .scan(SessionId(1), &[ColumnId(0)], Some(Arc::new(rows)))
        .unwrap();
    let mut seen = Vec::new();
    while let Some(batch) = scan.next_batch().unwrap() {
        seen.extend(ints(&batch.batch, 0));
    }
    let expected: Vec<i64> = wanted.iter().map(|&row| ids[row as usize]).collect();
    assert_eq!(seen, expected);
}

#[test]
fn filters_sorts_and_finds_with_the_shared_rules() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    open(&mut engine, &path, "items");
    let ids = kept_ids();
    // Names containing "12", sorted by score descending; Null scores last, ties in rowid order.
    let spec = ViewSpec {
        filters: vec![FilterCondition {
            column: ColumnId(1),
            op: FilterOp::Contains,
            value: "12".into(),
        }],
        sort: Some(SortKey {
            column: ColumnId(2),
            descending: true,
        }),
    };
    let view = build_view(&engine, &spec);
    let mut expected: Vec<(usize, i64)> = ids
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, id)| name(*id).is_some_and(|n| n.contains("12")))
        .collect();
    expected.sort_by(|(ra, a), (rb, b)| match (score(*a), score(*b)) {
        (Some(x), Some(y)) => y.total_cmp(&x).then(ra.cmp(rb)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => ra.cmp(rb),
    });
    let rows: Vec<u64> = expected.iter().map(|(row, _)| *row as u64).collect();
    assert_eq!(view.file_rows(0..view.len()), rows);

    // Find the whole value "item-121" within the view.
    let schema = engine.schema(SessionId(1)).unwrap();
    let mut task = FindTask::new(
        &engine,
        SessionId(1),
        FindQuery::new(&schema, "item-121"),
        &view,
    )
    .unwrap();
    let index = loop {
        if let TaskStep::Done(index) = task.step().unwrap() {
            break index;
        }
    };
    assert_eq!(index.cell_count(), 1);
    let found = index
        .next(&mut engine, SessionId(1), &view, None, false, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(found.1, ColumnId(1));
    let row = view.file_row(found.0).unwrap() as usize;
    assert_eq!(ids[row], 121);
}

#[test]
fn reads_without_rowid_tables_in_primary_key_order_and_views() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    assert_eq!(open(&mut engine, &path, "pairs"), 1100);
    let batch = read_all(&mut engine, &[0, 1]);
    let keys: Vec<(String, i64)> = (0..batch.num_rows())
        .map(|ix| {
            (
                batch.column(0).as_string::<i32>().value(ix).to_string(),
                batch.column(1).as_primitive::<Int64Type>().value(ix),
            )
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    let mut scan = engine.scan(SessionId(1), &[ColumnId(1)], None).unwrap();
    let mut seen = Vec::new();
    while let Some(batch) = scan.next_batch().unwrap() {
        seen.extend(ints(&batch.batch, 0));
    }
    assert_eq!(seen, keys.iter().map(|(_, b)| *b).collect::<Vec<_>>());

    let expected = kept_ids()
        .into_iter()
        .filter(|id| score(*id).is_some_and(|s| s > 20.))
        .count();
    assert_eq!(open(&mut engine, &path, "scored"), expected as u64);
    assert_eq!(read_all(&mut engine, &[0]).num_rows(), expected);
}

#[test]
fn describes_table_structure() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    open(&mut engine, &path, "items");
    let structure = engine.table_structure(SessionId(1)).unwrap();
    assert_eq!(structure.row_order, "rowid");
    assert_eq!(structure.total_rows, kept_ids().len() as u64);
    assert!(
        structure
            .sql
            .as_deref()
            .unwrap()
            .starts_with("CREATE TABLE items")
    );
    assert_eq!(structure.columns[0].primary_key, Some(1));
    assert_eq!(structure.columns[3].storage, ["integer", "real", "text"]);
    assert_eq!(structure.columns[3].declared_type, "");
    let index = structure
        .indexes
        .iter()
        .find(|index| index.name == "items_name")
        .unwrap();
    assert_eq!(index.columns, ["name"]);
    assert!(!index.unique);
    assert!(engine.file_structure(SessionId(1)).is_err());

    open(&mut engine, &path, "pairs");
    let structure = engine.table_structure(SessionId(1)).unwrap();
    assert!(structure.without_rowid);
    assert_eq!(structure.row_order, "primary key (a, b)");
}

#[test]
fn reports_missing_tables_and_changes_after_opening() {
    let (_dir, path) = fixture();
    let mut engine = ViewerEngine::new();
    let missing = engine.open_source(
        SessionId(1),
        &DataSource::Sqlite {
            path: path.clone(),
            table: "nope".into(),
        },
    );
    assert!(matches!(missing, Err(ViewerError::Database(_))));

    open(&mut engine, &path, "items");
    let before = std::fs::read(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("INSERT INTO items (name) VALUES ('new')", [])
            .unwrap();
    }
    let request = WindowRequest {
        session: SessionId(1),
        request: RequestId(1),
        rows: 0..10,
        columns: vec![ColumnId(0)],
    };
    let block = engine.plan_window(&request).unwrap().remove(0);
    assert!(matches!(
        engine.read_block(&block, &|| false),
        Err(ViewerError::FileChanged)
    ));
    // Reading never writes: the change above came from the other connection only.
    assert_ne!(before, std::fs::read(&path).unwrap());
}

#[test]
fn opening_and_reading_leave_the_file_unchanged() {
    let (_dir, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    let mut engine = ViewerEngine::new();
    open(&mut engine, &path, "items");
    read_all(&mut engine, &[0, 1, 2, 3, 4]);
    engine.table_structure(SessionId(1)).unwrap();
    engine.close(SessionId(1));
    assert_eq!(before, std::fs::read(&path).unwrap());
}
