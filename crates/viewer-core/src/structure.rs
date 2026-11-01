//! 파일 구조. Parquet 메타데이터에 적힌 저장 정보만으로 컬럼·Row Group의 크기, 압축, 인코딩,
//! 통계를 만든다. 데이터 페이지는 읽지 않는다.

use std::collections::BTreeSet;

use arrow_array::{Array, ArrayRef, BooleanArray};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_ord::ord::make_comparator;
use arrow_schema::{Schema, SortOptions};
use parquet::arrow::arrow_reader::statistics::StatisticsConverter;
use parquet::basic::LogicalType;
use parquet::file::metadata::ParquetMetaData;

/// 파일 전체의 구조.
#[derive(Clone, Debug)]
pub struct FileStructure {
    /// Parquet 형식 버전.
    pub format_version: i32,
    /// 파일을 쓴 프로그램.
    pub created_by: Option<String>,
    /// 파일에 적힌 key-value 메타데이터의 key와 값의 byte 길이.
    pub key_values: Vec<(String, usize)>,
    pub row_groups: Vec<RowGroupInfo>,
    /// 모든 Row Group을 합친 leaf 컬럼별 정보.
    pub columns: Vec<ColumnChunkInfo>,
}

/// Row Group 하나의 행 수와 크기.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGroupInfo {
    pub rows: u64,
    /// 압축 후 크기(byte).
    pub compressed: u64,
    /// 압축 전 크기(byte).
    pub uncompressed: u64,
}

/// leaf 컬럼 하나의 저장 정보. 여러 Row Group을 합쳤으면 크기와 null 수는 합계, 최솟값·최댓값은
/// 전체의 최솟값·최댓값이다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnChunkInfo {
    /// 점으로 이은 컬럼 경로. 예: `address.city`.
    pub path: String,
    pub physical_type: String,
    pub logical_type: Option<String>,
    pub codecs: Vec<String>,
    pub encodings: Vec<String>,
    pub compressed: u64,
    pub uncompressed: u64,
    /// 모든 Row Group에 null 수가 적혀 있을 때의 합계.
    pub nulls: Option<u64>,
    pub min: Option<StatValue>,
    pub max: Option<StatValue>,
    /// 모든 Row Group에 페이지 인덱스가 있다.
    pub page_index: bool,
    /// 모든 Row Group에 Bloom filter가 있다.
    pub bloom_filter: bool,
}

/// 통계 값의 표시 텍스트. `exact`가 거짓이면 파일에 잘라 적힌 근삿값이다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatValue {
    pub text: String,
    pub exact: bool,
}

/// 파일 전체의 구조를 만든다.
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

/// 지정한 Row Group들을 합친 leaf 컬럼별 정보.
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

/// 최상위 스칼라 컬럼의 최솟값·최댓값. Row Group별 통계를 Arrow 타입으로 바꿔 비교하므로
/// Date·Timestamp·Decimal도 값으로 비교하고 셀과 같은 형식으로 표시한다. 중첩 컬럼과 통계가
/// 없는 컬럼은 `None`이다.
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

/// 자주 쓰는 논리 타입을 짧게 표시한다.
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
