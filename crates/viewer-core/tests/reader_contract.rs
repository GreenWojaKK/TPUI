use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use arrow_array::builder::{Int32Builder, ListBuilder, MapBuilder, StringBuilder};
use arrow_array::{ArrayRef, Int32Array, Int64Array, RecordBatch, StringArray, StructArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use viewer_core::{
    BLOCK_ROWS, ColumnId, RequestId, SessionId, ViewerEngine, ViewerError, WindowRequest,
};

fn sample_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
    ]))
}

/// `len` ids starting at `start`, and a name column with Null every third row.
fn sample_batch(schema: &SchemaRef, start: i64, len: usize) -> RecordBatch {
    let ids: Int64Array = (start..start + len as i64).collect();
    let names: StringArray = (start..start + len as i64)
        .map(|id| (id % 3 != 0).then(|| format!("row-{id}")))
        .collect();
    RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(ids) as ArrayRef, Arc::new(names)],
    )
    .unwrap()
}

/// Writes the batch split into Row Groups of at most `rows_per_group` rows.
fn write_parquet(path: &Path, schema: SchemaRef, batches: &[RecordBatch], rows_per_group: usize) {
    let props = WriterProperties::builder()
        .set_max_row_group_row_count(Some(rows_per_group))
        .build();
    let mut writer =
        ArrowWriter::try_new(File::create(path).unwrap(), schema, Some(props)).unwrap();
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.close().unwrap();
}

#[test]
fn opens_metadata_of_multi_row_group_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("multi.parquet");
    let schema = sample_schema();
    write_parquet(
        &path,
        schema.clone(),
        &[sample_batch(&schema, 0, 2500)],
        1000,
    );

    let mut engine = ViewerEngine::new();
    let info = engine.open(SessionId(7), &path).unwrap();

    assert_eq!(info.session, SessionId(7));
    assert_eq!(info.path, path);
    assert_eq!(info.file_size, fs::metadata(&path).unwrap().len());
    assert_eq!(info.total_rows, 2500);
    assert_eq!(info.row_group_rows, vec![1000, 1000, 500]);
    assert_eq!(info.schema.fields(), schema.fields());
    assert!(engine.is_open(SessionId(7)));
}

#[test]
fn opens_file_without_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[], 1000);

    let mut engine = ViewerEngine::new();
    let info = engine.open(SessionId(1), &path).unwrap();

    assert_eq!(info.total_rows, 0);
    assert!(info.row_group_rows.is_empty());
    assert_eq!(info.schema.fields(), schema.fields());
}

#[test]
fn rejects_non_parquet_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-parquet.parquet");
    fs::write(&path, b"this is not a parquet file").unwrap();

    let mut engine = ViewerEngine::new();
    let err = engine.open(SessionId(1), &path).unwrap_err();

    assert!(matches!(err, ViewerError::InvalidParquet(_)), "{err:?}");
    assert!(!engine.is_open(SessionId(1)));
}

#[test]
fn reports_missing_file_as_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.parquet");

    let mut engine = ViewerEngine::new();
    let err = engine.open(SessionId(1), &path).unwrap_err();

    assert!(
        matches!(&err, ViewerError::Io(io_err) if io_err.kind() == io::ErrorKind::NotFound),
        "{err:?}"
    );
}

#[test]
fn failed_open_keeps_other_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.parquet");
    let bad = dir.path().join("bad.parquet");
    let schema = sample_schema();
    write_parquet(&good, schema.clone(), &[sample_batch(&schema, 0, 10)], 1000);
    fs::write(&bad, b"broken").unwrap();

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &good).unwrap();
    assert!(engine.open(SessionId(2), &bad).is_err());

    assert!(engine.is_open(SessionId(1)));
    assert!(!engine.is_open(SessionId(2)));
}

#[test]
fn keeps_sessions_open_until_each_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.parquet");
    let second = dir.path().join("second.parquet");
    let schema = sample_schema();
    write_parquet(
        &first,
        schema.clone(),
        &[sample_batch(&schema, 0, 10)],
        1000,
    );
    write_parquet(
        &second,
        schema.clone(),
        &[sample_batch(&schema, 100, 20)],
        1000,
    );

    let mut engine = ViewerEngine::new();
    assert_eq!(engine.open(SessionId(1), &first).unwrap().total_rows, 10);
    assert_eq!(engine.open(SessionId(2), &second).unwrap().total_rows, 20);

    let first_block = engine
        .plan_window(&window(1, 0..10, &[0]))
        .unwrap()
        .remove(0);
    let second_block = engine
        .plan_window(&window(2, 0..20, &[0]))
        .unwrap()
        .remove(0);
    let second_ids = engine.read_block(&second_block, &never_cancelled).unwrap();
    assert_eq!(
        second_ids.batch.column(0).as_ref(),
        &Int64Array::from_iter_values(100..120) as &dyn arrow_array::Array
    );

    engine.close(SessionId(1));
    assert!(matches!(
        engine.read_block(&first_block, &never_cancelled),
        Err(ViewerError::StaleSession)
    ));
    assert!(engine.read_block(&second_block, &never_cancelled).is_ok());
}

#[test]
fn opening_does_not_modify_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-only.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[sample_batch(&schema, 0, 100)], 30);
    let before = fs::read(&path).unwrap();

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    engine.close(SessionId(1));

    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!engine.is_open(SessionId(1)));
}

/// Batch with id, names mixed with Null, a string List, a Struct, and a string→integer Map column.
fn nested_batch(len: usize) -> RecordBatch {
    let ids: Int64Array = (0..len as i64).collect();
    let names: StringArray = (0..len)
        .map(|i| (i % 3 != 0).then(|| format!("이름-{i}\n둘째 줄")))
        .collect();

    let mut tags = ListBuilder::new(StringBuilder::new());
    for i in 0..len {
        if i % 5 == 0 {
            tags.append_null();
        } else {
            for j in 0..i % 4 {
                tags.values().append_value(format!("t{j}"));
            }
            tags.append(true);
        }
    }

    let xs: Int32Array = (0..len as i32).collect();
    let ys: Int32Array = (0..len as i32).map(|i| -i).collect();
    let point = StructArray::from(vec![
        (
            Arc::new(Field::new("x", DataType::Int32, false)),
            Arc::new(xs) as ArrayRef,
        ),
        (
            Arc::new(Field::new("y", DataType::Int32, false)),
            Arc::new(ys) as ArrayRef,
        ),
    ]);

    let mut attrs = MapBuilder::new(None, StringBuilder::new(), Int32Builder::new());
    for i in 0..len {
        for j in 0..i % 3 {
            attrs.keys().append_value(format!("k{j}"));
            attrs.values().append_value((i + j) as i32);
        }
        attrs.append(true).unwrap();
    }

    RecordBatch::try_from_iter(vec![
        ("id", Arc::new(ids) as ArrayRef),
        ("name", Arc::new(names) as ArrayRef),
        ("tags", Arc::new(tags.finish()) as ArrayRef),
        ("point", Arc::new(point) as ArrayRef),
        ("attrs", Arc::new(attrs.finish()) as ArrayRef),
    ])
    .unwrap()
}

fn window(session: u64, rows: std::ops::Range<u64>, columns: &[usize]) -> WindowRequest {
    WindowRequest {
        session: SessionId(session),
        request: RequestId(1),
        rows,
        columns: columns.iter().copied().map(ColumnId).collect(),
    }
}

fn never_cancelled() -> bool {
    false
}

#[test]
fn reads_selected_rows_and_columns_across_row_groups() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested.parquet");
    let source = nested_batch(2500);
    write_parquet(&path, source.schema(), std::slice::from_ref(&source), 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    // Unsorted, duplicated column requests are normalized to source schema order.
    let blocks = engine
        .plan_window(&window(1, 900..1600, &[4, 0, 2, 0, 3, 1]))
        .unwrap();

    let block_rows: Vec<_> = blocks.iter().map(|block| block.rows.clone()).collect();
    assert_eq!(block_rows, vec![512..1000, 1000..1512, 1512..2000]);

    let expected_columns: Vec<_> = (0..5).map(ColumnId).collect();
    for block in &blocks {
        assert_eq!(block.columns, expected_columns);
        let decoded = engine.read_block(block, &never_cancelled).unwrap();
        let len = (block.rows.end - block.rows.start) as usize;
        let expected = source.slice(block.rows.start as usize, len);

        assert_eq!(decoded.start_row, block.rows.start);
        assert_eq!(decoded.row_count, len);
        assert_eq!(decoded.columns, expected_columns);
        for (actual, expected) in decoded.batch.columns().iter().zip(expected.columns()) {
            assert_eq!(actual.as_ref(), expected.as_ref());
        }
    }
}

#[test]
fn reads_projected_columns_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested.parquet");
    let source = nested_batch(700);
    write_parquet(&path, source.schema(), std::slice::from_ref(&source), 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    let blocks = engine.plan_window(&window(1, 600..700, &[3, 1])).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].rows, BLOCK_ROWS..700);

    let decoded = engine.read_block(&blocks[0], &never_cancelled).unwrap();
    let expected = source.project(&[1, 3]).unwrap().slice(512, 188);
    assert_eq!(decoded.columns, vec![ColumnId(1), ColumnId(3)]);
    assert_eq!(decoded.batch.num_columns(), 2);
    for (actual, expected) in decoded.batch.columns().iter().zip(expected.columns()) {
        assert_eq!(actual.as_ref(), expected.as_ref());
    }
}

#[test]
fn clamps_rows_to_file_and_skips_io_for_empty_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[sample_batch(&schema, 0, 10)], 1000);
    let empty = dir.path().join("empty.parquet");
    write_parquet(&empty, schema.clone(), &[], 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    let blocks = engine.plan_window(&window(1, 5..100, &[0])).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].rows, 0..10);
    assert!(
        engine
            .plan_window(&window(1, 10..20, &[0]))
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .plan_window(&window(1, 3..3, &[0]))
            .unwrap()
            .is_empty()
    );

    engine.open(SessionId(2), &empty).unwrap();
    assert!(
        engine
            .plan_window(&window(2, 0..50, &[0, 1]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rejects_invalid_and_stale_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[sample_batch(&schema, 0, 10)], 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(3), &path).unwrap();

    #[allow(clippy::reversed_empty_ranges)]
    let reversed = window(3, 5..2, &[0]);
    assert!(matches!(
        engine.plan_window(&reversed),
        Err(ViewerError::InvalidRequest(_))
    ));
    assert!(matches!(
        engine.plan_window(&window(3, 0..5, &[2])),
        Err(ViewerError::InvalidRequest(_))
    ));
    assert!(matches!(
        engine.plan_window(&window(2, 0..5, &[0])),
        Err(ViewerError::StaleSession)
    ));

    let block = engine
        .plan_window(&window(3, 0..5, &[0]))
        .unwrap()
        .remove(0);
    engine.close(SessionId(3));
    assert!(matches!(
        engine.read_block(&block, &never_cancelled),
        Err(ViewerError::StaleSession)
    ));
}

#[test]
fn stops_when_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[sample_batch(&schema, 0, 10)], 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    let block = engine
        .plan_window(&window(1, 0..10, &[0]))
        .unwrap()
        .remove(0);

    assert!(matches!(
        engine.read_block(&block, &|| true),
        Err(ViewerError::Cancelled)
    ));
}

#[test]
fn detects_file_changes_after_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[sample_batch(&schema, 0, 10)], 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    let block = engine
        .plan_window(&window(1, 0..10, &[0]))
        .unwrap()
        .remove(0);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"x")
        .unwrap();

    assert!(matches!(
        engine.read_block(&block, &never_cancelled),
        Err(ViewerError::FileChanged)
    ));
}

#[test]
fn reads_single_values_across_row_groups() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested.parquet");
    let source = nested_batch(2500);
    write_parquet(&path, source.schema(), std::slice::from_ref(&source), 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    for row in [0u64, 511, 512, 999, 1000, 1513, 2499] {
        for column in 0..source.num_columns() {
            let request = viewer_core::ValueRequest {
                session: SessionId(1),
                request: RequestId(1),
                row,
                column: ColumnId(column),
            };
            let value = engine.read_value(&request, &never_cancelled).unwrap();
            let expected = source.column(column).slice(row as usize, 1);
            assert_eq!(value.value.len(), 1);
            assert_eq!(
                value.value.as_ref(),
                expected.as_ref(),
                "row {row} column {column}"
            );
            assert_eq!(value.field.name(), source.schema().field(column).name());
        }
    }

    let out_of_range = viewer_core::ValueRequest {
        session: SessionId(1),
        request: RequestId(1),
        row: 2500,
        column: ColumnId(0),
    };
    assert!(matches!(
        engine.read_value(&out_of_range, &never_cancelled),
        Err(ViewerError::InvalidRequest(_))
    ));
}

#[test]
fn shares_one_cache_budget_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let source = nested_batch(5000);
    let paths: Vec<_> = (0..2)
        .map(|ix| {
            let path = dir.path().join(format!("file{ix}.parquet"));
            write_parquet(&path, source.schema(), std::slice::from_ref(&source), 1000);
            path
        })
        .collect();

    // Read two files alternately with a budget that holds only two or three blocks.
    let mut probe = ViewerEngine::new();
    probe.open(SessionId(99), &paths[0]).unwrap();
    let first = probe
        .plan_window(&window(99, 0..1, &[0, 1, 2, 3, 4]))
        .unwrap()
        .remove(0);
    probe.read_block(&first, &never_cancelled).unwrap();
    let block_bytes = probe.cache_bytes();
    let budget = block_bytes * 5 / 2;

    let mut engine = ViewerEngine::with_cache_budget(budget);
    engine.open(SessionId(1), &paths[0]).unwrap();
    engine.open(SessionId(2), &paths[1]).unwrap();
    for _ in 0..2 {
        for session in [1u64, 2] {
            let blocks = engine
                .plan_window(&window(session, 0..5000, &[0, 1, 2, 3, 4]))
                .unwrap();
            for block in &blocks {
                let decoded = engine.read_block(block, &never_cancelled).unwrap();
                let expected = source.slice(block.rows.start as usize, decoded.row_count);
                for (actual, expected) in decoded.batch.columns().iter().zip(expected.columns()) {
                    assert_eq!(actual.as_ref(), expected.as_ref());
                }
                assert!(engine.cache_bytes() <= budget);
            }
        }
    }
    assert!(engine.cache_bytes() > 0);

    engine.close(SessionId(1));
    engine.close(SessionId(2));
    assert_eq!(engine.cache_bytes(), 0);
}

#[test]
fn clears_cached_blocks_when_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.parquet");
    let schema = sample_schema();
    write_parquet(&path, schema.clone(), &[sample_batch(&schema, 0, 10)], 1000);

    let mut engine = ViewerEngine::new();
    engine.open(SessionId(1), &path).unwrap();
    let block = engine
        .plan_window(&window(1, 0..10, &[0]))
        .unwrap()
        .remove(0);
    engine.read_block(&block, &never_cancelled).unwrap();
    assert!(engine.cache_bytes() > 0);

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"x")
        .unwrap();
    assert!(matches!(
        engine.read_block(&block, &never_cancelled),
        Err(ViewerError::FileChanged)
    ));
    assert_eq!(engine.cache_bytes(), 0);
}
