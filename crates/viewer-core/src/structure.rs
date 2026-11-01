//! File structure. Builds column and Row Group sizes, compression, encodings and statistics from the
//! storage information in the Parquet metadata alone. Data pages are not read.

use std::collections::BTreeSet;

use arrow_array::{Array, ArrayRef, BooleanArray};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_ord::ord::make_comparator;
use arrow_schema::{Schema, SortOptions};
use parquet::arrow::arrow_reader::statistics::StatisticsConverter;
use parquet::basic::LogicalType;
use parquet::file::metadata::ParquetMetaData;

/// Structure of the whole file.
#[derive(Clone, Debug)]
pub struct FileStructure {
    /// Parquet format version.
    pub format_version: i32,
    /// Program that wrote the file.
    pub created_by: Option<String>,
    /// Keys of the file's key-value metadata and the byte length of each value.
    pub key_values: Vec<(String, usize)>,
    pub row_groups: Vec<RowGroupInfo>,
    /// Per-leaf-column information over all Row Groups.
    pub columns: Vec<ColumnChunkInfo>,
}

/// Row count and size of one Row Group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGroupInfo {
    pub rows: u64,
    /// Compressed size in bytes.
    pub compressed: u64,
    /// Uncompressed size in bytes.
    pub uncompressed: u64,
}

/// Storage information of one leaf column. Over several Row Groups, sizes and null counts are sums, and min/max
/// are the overall minimum and maximum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnChunkInfo {
    /// Column path joined with dots, e.g. `address.city`.
    pub path: String,
    pub physical_type: String,
    pub logical_type: Option<String>,
    pub codecs: Vec<String>,
    pub encodings: Vec<String>,
    pub compressed: u64,
    pub uncompressed: u64,
    /// Sum of null counts when every Row Group records one.
    pub nulls: Option<u64>,
    pub min: Option<StatValue>,
    pub max: Option<StatValue>,
    /// Every Row Group has a page index.
    pub page_index: bool,
    /// Every Row Group has a Bloom filter.
    pub bloom_filter: bool,
}

/// Display text of a statistics value. If `exact` is false, it is an approximation truncated in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatValue {
    pub text: String,
    pub exact: bool,
}

/// Builds the structure of the whole file.
pub(crate) fn file_structure(metadata: &ParquetMetaData, schema: &Schema) -> FileStructure {
    let file = metadata.file_metadata();
    let row_groups = metadata
        .row_groups()
        .iter()
        .map(|group| RowGroupInfo {
            rows: group.num_rows().max(0) as u64,
            compressed: group
                .columns()
                .iter()
                .map(|column| column.compressed_size().max(0) as u64)
                .sum(),
            uncompressed: group
                .columns()
                .iter()
                .map(|column| column.uncompressed_size().max(0) as u64)
                .sum(),
        })
        .collect();
    let all: Vec<usize> = (0..metadata.num_row_groups()).collect();
    FileStructure {
        format_version: file.version(),
        created_by: file.created_by().map(str::to_string),
        key_values: file
            .key_value_metadata()
            .map(|pairs| {
                pairs
                    .iter()
                    .map(|pair| (pair.key.clone(), pair.value.as_ref().map_or(0, String::len)))
                    .collect()
            })
            .unwrap_or_default(),
        row_groups,
        columns: column_infos(metadata, schema, &all),
    }
}

/// Per-leaf-column information over the given Row Groups.
pub(crate) fn column_infos(
    metadata: &ParquetMetaData,
    schema: &Schema,
    row_groups: &[usize],
) -> Vec<ColumnChunkInfo> {
    let parquet_schema = metadata.file_metadata().schema_descr();
    (0..parquet_schema.num_columns())
        .map(|leaf| {
            let descr = parquet_schema.column(leaf);
            let chunks: Vec<_> = row_groups
                .iter()
                .map(|&group| metadata.row_group(group).column(leaf))
                .collect();
            let codecs: BTreeSet<String> = chunks
                .iter()
                .map(|chunk| chunk.compression().to_string())
                .collect();
            let encodings: BTreeSet<String> = chunks
                .iter()
                .flat_map(|chunk| chunk.encodings().map(|encoding| format!("{encoding:?}")))
                .collect();
            let nulls = chunks
                .iter()
                .map(|chunk| chunk.statistics().and_then(|stats| stats.null_count_opt()))
                .sum::<Option<u64>>();
            let (min, max) = min_max(metadata, schema, descr.path().parts(), row_groups);
            ColumnChunkInfo {
                path: descr.path().string(),
                physical_type: format!("{:?}", descr.physical_type()),
                logical_type: descr.logical_type_ref().map(logical_type_label),
                codecs: codecs.into_iter().collect(),
                encodings: encodings.into_iter().collect(),
                compressed: chunks
                    .iter()
                    .map(|chunk| chunk.compressed_size().max(0) as u64)
                    .sum(),
                uncompressed: chunks
                    .iter()
                    .map(|chunk| chunk.uncompressed_size().max(0) as u64)
                    .sum(),
                nulls,
                min,
                max,
                page_index: !chunks.is_empty()
                    && chunks
                        .iter()
                        .all(|chunk| chunk.column_index_offset().is_some()),
                bloom_filter: !chunks.is_empty()
                    && chunks
                        .iter()
                        .all(|chunk| chunk.bloom_filter_offset().is_some()),
            }
        })
        .collect()
}

/// Min and max of a top-level scalar column. Per-Row-Group statistics are converted to Arrow types before
/// comparison, so Date, Timestamp and Decimal compare by value and display like cells. Nested columns and
/// columns without statistics are `None`.
fn min_max(
    metadata: &ParquetMetaData,
    schema: &Schema,
    path: &[String],
    row_groups: &[usize],
) -> (Option<StatValue>, Option<StatValue>) {
    let [name] = path else {
        return (None, None);
    };
    if row_groups.is_empty() {
        return (None, None);
    }
    let parquet_schema = metadata.file_metadata().schema_descr();
    let Ok(converter) = StatisticsConverter::try_new(name, schema, parquet_schema) else {
        return (None, None);
    };
    let groups = || row_groups.iter().map(|&group| metadata.row_group(group));
    let pick = |values: parquet::errors::Result<ArrayRef>,
                exact: parquet::errors::Result<BooleanArray>,
                descending: bool| {
        let values = values.ok()?;
        let exact = exact.ok()?;
        let compare = make_comparator(&values, &values, SortOptions::default()).ok()?;
        let best = (0..values.len())
            .filter(|&ix| values.is_valid(ix))
            .reduce(|best, ix| {
                let ordering = compare(ix, best);
                let better = if descending {
                    ordering.is_gt()
                } else {
                    ordering.is_lt()
                };
                if better { ix } else { best }
            })?;
        let options = FormatOptions::new().with_display_error(false);
        let text = ArrayFormatter::try_new(values.as_ref(), &options)
            .ok()?
            .value(best)
            .try_to_string()
            .ok()?;
        Some(StatValue {
            text,
            exact: exact.is_valid(best) && exact.value(best),
        })
    };
    (
        pick(
            converter.row_group_mins(groups()),
            converter.row_group_is_min_value_exact(groups()),
            false,
        ),
        pick(
            converter.row_group_maxes(groups()),
            converter.row_group_is_max_value_exact(groups()),
            true,
        ),
    )
}

/// Short display names for common logical types.
fn logical_type_label(logical: &LogicalType) -> String {
    match logical {
        LogicalType::String => "String".into(),
        LogicalType::Integer(int) => format!(
            "Int({}, {})",
            int.bit_width,
            if int.is_signed { "signed" } else { "unsigned" }
        ),
        LogicalType::Decimal(decimal) => {
            format!("Decimal({}, {})", decimal.precision, decimal.scale)
        }
        LogicalType::Date => "Date".into(),
        LogicalType::Time(time) => {
            format!("Time({:?}, utc={})", time.unit, time.is_adjusted_to_u_t_c)
        }
        LogicalType::Timestamp(timestamp) => format!(
            "Timestamp({:?}, utc={})",
            timestamp.unit, timestamp.is_adjusted_to_u_t_c
        ),
        other => format!("{other:?}"),
    }
}
