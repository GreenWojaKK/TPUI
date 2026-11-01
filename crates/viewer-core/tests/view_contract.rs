//! Contracts of sequential scans, arbitrary row reads, row views and find, checked with a multi-Row-Group fixture.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::Int64Type;
use arrow_array::{Array, ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use viewer_core::{
    ColumnId, FilterCondition, FilterOp, FindQuery, FindTask, RowIds, RowView, RowViewTask,
    SessionId, SortKey, TaskStep, ViewSpec, ViewerEngine, ViewerError,
};

const ROWS: i64 = 2_500;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("group", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
    ]))
}

/// Writes id, id % 7 (Null on multiples of 5) and name (Null on multiples of 3) in Row Groups of 1,000 rows.
fn write_fixture(path: &Path) {
    let schema = schema();
    let ids: Int64Array = (0..ROWS).collect();
    let groups: Int64Array = (0..ROWS)
        .map(|id| (id % 5 != 0).then_some(id % 7))
        .collect();
    let names: StringArray = (0..ROWS)
        .map(|id| (id % 3 != 0).then(|| format!("row-{}", id % 11)))
        .collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(ids) as ArrayRef, Arc::new(groups), Arc::new(names)],
    )
    .unwrap();
    let props = WriterProperties::builder()
        .set_max_row_group_row_count(Some(1_000))
        .build();
    let mut writer =
        ArrowWriter::try_new(File::create(path).unwrap(), schema, Some(props)).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn open() -> (tempfile::TempDir, ViewerEngine) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view.parquet");
    write_fixture(&path);
    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    (dir, engine)
}

fn build_view(engine: &ViewerEngine, spec: &ViewSpec) -> Result<RowView, ViewerError> {
    let mut task = RowViewTask::new(engine, SessionId(1), spec)?;
    loop {
        if let TaskStep::Done(view) = task.step(engine)? {
            return Ok(view);
        }
    }
}

fn ids_of(view: &RowView) -> Vec<u64> {
    view.file_rows(0..view.len())
}

fn condition(column: usize, op: FilterOp, value: &str) -> FilterCondition {
    FilterCondition {
        column: ColumnId(column),
        op,
        value: value.into(),
    }
}

fn int64s(array: &ArrayRef) -> Vec<u64> {
    array
        .as_primitive::<Int64Type>()
        .values()
        .iter()
        .map(|id| *id as u64)
        .collect()
}

#[test]
fn scans_every_row_in_order_across_row_groups() {
    let (_dir, engine) = open();
    let mut scan = engine.scan(SessionId(1), &[ColumnId(0)], None).unwrap();
    assert_eq!(scan.total(), ROWS as u64);
    let mut seen = Vec::new();
    while let Some(batch) = scan.next_batch().unwrap() {
        assert_eq!(batch.start, seen.len() as u64);
        seen.extend(int64s(batch.batch.column(0)));
    }
    assert_eq!(seen, (0..ROWS as u64).collect::<Vec<_>>());
}

#[test]
fn scans_only_listed_rows_and_skips_row_groups_without_them() {
    let (_dir, engine) = open();
    let wanted = [3u64, 4, 999, 1000, 2499];
    let mut rows = RowIds::for_total_rows(ROWS as u64);
    for row in wanted {
        rows.push(row);
    }
    let mut scan = engine
        .scan(SessionId(1), &[ColumnId(0)], Some(Arc::new(rows)))
        .unwrap();
    let mut seen = Vec::new();
    while let Some(batch) = scan.next_batch().unwrap() {
        assert_eq!(batch.start, seen.len() as u64);
        seen.extend(int64s(batch.batch.column(0)));
    }
    assert_eq!(seen, wanted);
}

#[test]
fn reads_rows_in_the_requested_order() {
    let (_dir, mut engine) = open();
    let rows = [2499u64, 0, 1500, 514, 0];
    let batch = engine
        .read_rows(SessionId(1), &rows, &[ColumnId(2), ColumnId(0)], &|| false)
        .unwrap();
    // Columns are sorted into source schema order.
    assert_eq!(batch.schema().field(0).name(), "id");
    assert_eq!(int64s(batch.column(0)), rows);
    let names = batch.column(1).as_string::<i32>();
    assert!(names.is_null(4));
    assert_eq!(names.value(3), "row-8");
}

#[test]
fn filters_rows_with_every_condition() {
    let (_dir, engine) = open();
    let spec = ViewSpec {
        filters: vec![
            condition(1, FilterOp::Eq, "3"),
            condition(2, FilterOp::Contains, "row-1"),
        ],
        sort: None,
    };
    let view = build_view(&engine, &spec).unwrap();
    let expected: Vec<u64> = (0..ROWS)
        .filter(|id| id % 5 != 0 && id % 7 == 3)
        .filter(|id| id % 3 != 0 && format!("row-{}", id % 11).contains("row-1"))
        .map(|id| id as u64)
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(ids_of(&view), expected);
}

#[test]
fn sorts_filtered_rows_with_nulls_last_and_stable_ties() {
    let (_dir, engine) = open();
    let spec = ViewSpec {
        filters: vec![condition(0, FilterOp::Lt, "40")],
        sort: Some(SortKey {
            column: ColumnId(1),
            descending: true,
        }),
    };
    let view = build_view(&engine, &spec).unwrap();
    let mut expected: Vec<i64> = (0..40).collect();
    // Descending; Null (multiples of 5) last; equal values in source order.
    expected.sort_by_key(|id| {
        let group = (id % 5 != 0).then_some(id % 7);
        (group.is_none(), std::cmp::Reverse(group))
    });
    assert_eq!(
        ids_of(&view),
        expected.into_iter().map(|id| id as u64).collect::<Vec<_>>()
    );
}

#[test]
fn rejects_values_that_do_not_fit_the_column() {
    let (_dir, engine) = open();
    let spec = ViewSpec {
        filters: vec![condition(0, FilterOp::Eq, "abc")],
        sort: None,
    };
    assert!(matches!(
        build_view(&engine, &spec),
        Err(ViewerError::InvalidCondition(_))
    ));
}

#[test]
fn finds_cells_in_view_order_and_wraps_around() {
    let (_dir, mut engine) = open();
    let schema = engine.schema(SessionId(1)).unwrap();
    let spec = ViewSpec {
        filters: vec![condition(0, FilterOp::GtEq, "1000")],
        sort: Some(SortKey {
            column: ColumnId(0),
            descending: true,
        }),
    };
    let view = build_view(&engine, &spec).unwrap();
    // "3" matches the value 3 in the group column and id 3. id 3 is outside the filter.
    let query = FindQuery::new(&schema, "3");
    let mut task = FindTask::new(&engine, SessionId(1), query, &view).unwrap();
    let index = loop {
        if let TaskStep::Done(index) = task.step().unwrap() {
            break index;
        }
    };
    let expected: Vec<u64> = (1000..ROWS as u64)
        .rev()
        .filter(|id| id % 5 != 0 && id % 7 == 3)
        .collect();
    assert_eq!(index.cell_count(), expected.len() as u64);

    let first = index
        .next(&mut engine, SessionId(1), &view, None, false, &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(view.file_row(first.0), Some(expected[0]));
    assert_eq!(first.1, ColumnId(1));
    let second = index
        .next(
            &mut engine,
            SessionId(1),
            &view,
            Some(first),
            false,
            &|| false,
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.file_row(second.0), Some(expected[1]));
    // Going back from the start wraps to the last match.
    let last = index
        .next(&mut engine, SessionId(1), &view, Some(first), true, &|| {
            false
        })
        .unwrap()
        .unwrap();
    assert_eq!(view.file_row(last.0), expected.last().copied());
}

#[test]
fn describes_file_structure_from_metadata() {
    let (_dir, engine) = open();
    let structure = engine.file_structure(SessionId(1)).unwrap();
    assert_eq!(
        structure
            .row_groups
            .iter()
            .map(|group| group.rows)
            .collect::<Vec<_>>(),
        [1000, 1000, 500]
    );
    let id = &structure.columns[0];
    assert_eq!(id.path, "id");
    assert_eq!(id.physical_type, "INT64");
    assert_eq!(id.min.as_ref().unwrap().text, "0");
    assert_eq!(id.max.as_ref().unwrap().text, "2499");
    assert_eq!(
        id.compressed,
        structure
            .row_groups
            .iter()
            .map(|group| group.compressed)
            .sum::<u64>()
            - structure.columns[1..]
                .iter()
                .map(|column| column.compressed)
                .sum::<u64>()
    );
    let name = &structure.columns[2];
    assert_eq!(
        name.nulls,
        Some((0..ROWS).filter(|id| id % 3 == 0).count() as u64)
    );
    assert_eq!(name.logical_type.as_deref(), Some("String"));

    let second = engine.row_group_columns(SessionId(1), 1).unwrap();
    assert_eq!(second[0].min.as_ref().unwrap().text, "1000");
    assert_eq!(second[0].max.as_ref().unwrap().text, "1999");
    assert!(engine.row_group_columns(SessionId(1), 3).is_err());
}
