//! Turns Arrow values into one-line previews drawn in grid cells. Runs on the worker thread.
//!
//! A preview is a summary for drawing. Long strings and Binary copy only their beginning, and
//! nested values are summarized by count. The value detail view shows the full raw value.

use std::fmt::Write;
use std::ops::Range;

use arrow_array::cast::AsArray;
use arrow_array::{Array, downcast_dictionary_array};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::DataType;
use gpui::SharedString;

/// Maximum Unicode scalar values kept in a string preview.
pub const STRING_PREVIEW_CHARS: usize = 256;
/// Maximum bytes kept as hexadecimal in a Binary preview.
pub const BINARY_PREVIEW_BYTES: usize = 32;

/// How a cell is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Text,
    /// Right-aligned number.
    Number,
    /// Null, distinguished from the empty string.
    Null,
    /// Count summary of Struct, List or Map.
    Summary,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellPreview {
    pub text: SharedString,
    pub kind: CellKind,
    /// Cell whose whole value equals the find input.
    pub matched: bool,
}

impl CellPreview {
    pub(crate) fn new(text: impl Into<SharedString>, kind: CellKind) -> Self {
        Self {
            text: text.into(),
            kind,
            matched: false,
        }
    }

    fn null() -> Self {
        Self::new("null", CellKind::Null)
    }

    fn unsupported(data_type: &DataType) -> Self {
        Self::new(
            format!("{data_type} · display unsupported"),
            CellKind::Unsupported,
        )
    }
}

/// Turns the values at positions `rows` of an array into previews.
pub fn preview_column(array: &dyn Array, rows: Range<usize>) -> Vec<CellPreview> {
    preview_rows(array, &rows.collect::<Vec<_>>())
}

fn preview_rows(array: &dyn Array, rows: &[usize]) -> Vec<CellPreview> {
    let data_type = array.data_type();
    match data_type {
        DataType::Null => rows.iter().map(|_| CellPreview::null()).collect(),
        DataType::Dictionary(_, _) => preview_dictionary(array, rows),
        _ if is_formatted_by_arrow(data_type) => preview_formatted(array, rows),
        _ => rows
            .iter()
            .map(|&row| {
                if array.is_null(row) {
                    CellPreview::null()
                } else {
                    preview_value(array, row)
                }
            })
            .collect(),
    }
}

/// Dictionary is displayed by the type of its referenced values.
fn preview_dictionary(array: &dyn Array, rows: &[usize]) -> Vec<CellPreview> {
    let keys: Vec<Option<usize>> = downcast_dictionary_array!(
        array => rows.iter().map(|&row| array.key(row)).collect(),
        other => unreachable!("not a Dictionary type: {other}")
    );
    let referenced: Vec<usize> = keys.iter().flatten().copied().collect();
    let values = array.as_any_dictionary().values();
    let mut previews = preview_rows(values.as_ref(), &referenced).into_iter();
    keys.into_iter()
        .map(|key| match key {
            Some(_) => previews
                .next()
                .expect("every referenced value has a preview"),
            None => CellPreview::null(),
        })
        .collect()
}

/// Numeric and time types follow arrow-cast's display rules. Decimal scale and time precision are kept, and
/// only Timestamps with a timezone show that timezone's offset.
fn preview_formatted(array: &dyn Array, rows: &[usize]) -> Vec<CellPreview> {
    let data_type = array.data_type();
    let kind = if is_number(data_type) {
        CellKind::Number
    } else {
        CellKind::Text
    };
    let options = FormatOptions::new().with_display_error(false);
    let formatter = ArrayFormatter::try_new(array, &options).ok();
    rows.iter()
        .map(|&row| {
            if array.is_null(row) {
                return CellPreview::null();
            }
            match formatter
                .as_ref()
                .and_then(|formatter| formatter.value(row).try_to_string().ok())
            {
                Some(text) => CellPreview::new(text, kind),
                None => CellPreview::unsupported(data_type),
            }
        })
        .collect()
}

fn preview_value(array: &dyn Array, row: usize) -> CellPreview {
    match array.data_type() {
        DataType::Utf8 => preview_string(array.as_string::<i32>().value(row)),
        DataType::LargeUtf8 => preview_string(array.as_string::<i64>().value(row)),
        DataType::Utf8View => preview_string(array.as_string_view().value(row)),
        DataType::Binary => preview_binary(array.as_binary::<i32>().value(row)),
        DataType::LargeBinary => preview_binary(array.as_binary::<i64>().value(row)),
        DataType::BinaryView => preview_binary(array.as_binary_view().value(row)),
        DataType::FixedSizeBinary(_) => preview_binary(array.as_fixed_size_binary().value(row)),
        DataType::Struct(fields) => {
            CellPreview::new(format!("Struct {{{}}}", fields.len()), CellKind::Summary)
        }
        DataType::List(_) => summary_list(array.as_list::<i32>().value_length(row) as usize),
        DataType::LargeList(_) => summary_list(array.as_list::<i64>().value_length(row) as usize),
        DataType::ListView(_) => summary_list(array.as_list_view::<i32>().value_size(row) as usize),
        DataType::LargeListView(_) => {
            summary_list(array.as_list_view::<i64>().value_size(row) as usize)
        }
        DataType::FixedSizeList(_, len) => summary_list(*len as usize),
        DataType::Map(_, _) => CellPreview::new(
            format!("Map [{}]", array.as_map().value_length(row)),
            CellKind::Summary,
        ),
        other => CellPreview::unsupported(other),
    }
}

fn summary_list(len: usize) -> CellPreview {
    CellPreview::new(format!("List [{len}]"), CellKind::Summary)
}

/// Copies only the first [`STRING_PREVIEW_CHARS`] characters and replaces newlines and control characters with display symbols.
fn preview_string(value: &str) -> CellPreview {
    let mut text = String::new();
    let mut chars = value.chars().peekable();
    let mut taken = 0;
    while taken < STRING_PREVIEW_CHARS {
        let Some(ch) = chars.next() else {
            break;
        };
        taken += 1;
        match ch {
            '\r' => {
                // `\r\n` shows as a single newline.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    taken += 1;
                }
                text.push('↵');
            }
            '\n' => text.push('↵'),
            '\t' => text.push('⇥'),
            ch if ch.is_control() => text.push(control_picture(ch)),
            ch => text.push(ch),
        }
    }
    if chars.next().is_some() {
        text.push('…');
    }
    CellPreview::new(text, CellKind::Text)
}

/// Replaces control characters with Unicode Control Pictures symbols.
pub(crate) fn control_picture(ch: char) -> char {
    match ch as u32 {
        code @ 0x00..=0x1f => char::from_u32(0x2400 + code).unwrap_or('\u{2426}'),
        0x7f => '\u{2421}',
        _ => '\u{2426}',
    }
}

/// Shows the first [`BINARY_PREVIEW_BYTES`] bytes in hexadecimal and the total byte length.
fn preview_binary(bytes: &[u8]) -> CellPreview {
    let mut text = String::with_capacity(BINARY_PREVIEW_BYTES * 2 + 16);
    text.push_str("0x");
    for byte in bytes.iter().take(BINARY_PREVIEW_BYTES) {
        let _ = write!(text, "{byte:02x}");
    }
    if bytes.len() > BINARY_PREVIEW_BYTES {
        text.push('…');
    }
    let _ = write!(text, " ({} B)", bytes.len());
    CellPreview::new(text, CellKind::Text)
}

pub(crate) fn is_formatted_by_arrow(data_type: &DataType) -> bool {
    use DataType::*;
    is_number(data_type)
        || matches!(
            data_type,
            Boolean
                | Date32
                | Date64
                | Time32(_)
                | Time64(_)
                | Timestamp(_, _)
                | Duration(_)
                | Interval(_)
        )
}

pub(crate) fn is_number(data_type: &DataType) -> bool {
    use DataType::*;
    matches!(
        data_type,
        Int8 | Int16
            | Int32
            | Int64
            | UInt8
            | UInt16
            | UInt32
            | UInt64
            | Float16
            | Float32
            | Float64
            | Decimal32(_, _)
            | Decimal64(_, _)
            | Decimal128(_, _)
            | Decimal256(_, _)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::builder::{Int32Builder, ListBuilder, MapBuilder, StringBuilder};
    use arrow_array::types::Int8Type;
    use arrow_array::{
        ArrayRef, BinaryArray, BooleanArray, Decimal128Array, DictionaryArray, Float64Array,
        Int64Array, StringArray, StructArray, TimestampMillisecondArray,
    };
    use arrow_schema::Field;

    use super::*;

    fn texts(previews: &[CellPreview]) -> Vec<&str> {
        previews
            .iter()
            .map(|preview| preview.text.as_ref())
            .collect()
    }

    #[test]
    fn distinguishes_null_from_empty_string() {
        let array = StringArray::from(vec![Some(""), None]);
        let previews = preview_column(&array, 0..2);
        assert_eq!(previews[0], CellPreview::new("", CellKind::Text));
        assert_eq!(previews[1], CellPreview::null());
    }

    #[test]
    fn truncates_long_strings_and_marks_line_breaks() {
        let long = "가".repeat(STRING_PREVIEW_CHARS + 10);
        let array = StringArray::from(vec![
            long.as_str(),
            "a\nb\r\nc\td\u{0}",
            &"x".repeat(STRING_PREVIEW_CHARS),
            r"C:\Users\data",
        ]);
        let previews = preview_column(&array, 0..4);

        let truncated = previews[0].text.as_ref();
        assert_eq!(truncated.chars().count(), STRING_PREVIEW_CHARS + 1);
        assert!(truncated.ends_with('…'));
        assert_eq!(previews[1].text.as_ref(), "a↵b↵c⇥d␀");
        assert!(!previews[2].text.ends_with('…'));
        assert_eq!(previews[3].text.as_ref(), r"C:\Users\data");
    }

    #[test]
    fn shows_binary_prefix_and_length() {
        let short = [0xde, 0xad];
        let long: Vec<u8> = (0..40).collect();
        let array = BinaryArray::from(vec![&short[..], &long[..]]);
        let previews = preview_column(&array, 0..2);
        assert_eq!(previews[0].text.as_ref(), "0xdead (2 B)");
        assert!(previews[1].text.starts_with("0x000102"));
        assert!(previews[1].text.ends_with("1e1f… (40 B)"));
    }

    #[test]
    fn formats_numbers_and_keeps_decimal_scale() {
        let ints = Int64Array::from(vec![Some(-42), None]);
        let floats = Float64Array::from(vec![0.1]);
        let decimals = Decimal128Array::from(vec![150, -5])
            .with_precision_and_scale(10, 2)
            .unwrap();
        let bools = BooleanArray::from(vec![true]);

        let int_previews = preview_column(&ints, 0..2);
        assert_eq!(int_previews[0], CellPreview::new("-42", CellKind::Number));
        assert_eq!(int_previews[1], CellPreview::null());
        assert_eq!(texts(&preview_column(&floats, 0..1)), vec!["0.1"]);
        assert_eq!(
            texts(&preview_column(&decimals, 0..2)),
            vec!["1.50", "-0.05"]
        );
        assert_eq!(
            preview_column(&bools, 0..1)[0],
            CellPreview::new("true", CellKind::Text)
        );
    }

    #[test]
    fn keeps_timestamp_timezone_without_local_conversion() {
        let naive = TimestampMillisecondArray::from(vec![1_700_000_000_123]);
        let offset = naive.clone().with_timezone("+09:00");
        let named = naive.clone().with_timezone("Asia/Seoul");
        assert_eq!(
            texts(&preview_column(&naive, 0..1)),
            vec!["2023-11-14T22:13:20.123"]
        );
        assert_eq!(
            texts(&preview_column(&offset, 0..1)),
            vec!["2023-11-15T07:13:20.123+09:00"]
        );
        assert_eq!(
            texts(&preview_column(&named, 0..1)),
            vec!["2023-11-15T07:13:20.123+09:00"]
        );
    }

    #[test]
    fn summarizes_nested_values() {
        let point = StructArray::from(vec![
            (
                Arc::new(Field::new("x", DataType::Int32, false)),
                Arc::new(arrow_array::Int32Array::from(vec![1])) as ArrayRef,
            ),
            (
                Arc::new(Field::new("y", DataType::Int32, false)),
                Arc::new(arrow_array::Int32Array::from(vec![2])) as ArrayRef,
            ),
        ]);
        let mut list = ListBuilder::new(StringBuilder::new());
        list.values().append_value("a");
        list.values().append_value("b");
        list.append(true);
        list.append_null();
        let mut map = MapBuilder::new(None, StringBuilder::new(), Int32Builder::new());
        map.keys().append_value("k");
        map.values().append_value(1);
        map.append(true).unwrap();

        assert_eq!(
            preview_column(&point, 0..1)[0],
            CellPreview::new("Struct {2}", CellKind::Summary)
        );
        let lists = preview_column(&list.finish(), 0..2);
        assert_eq!(texts(&lists), vec!["List [2]", "null"]);
        assert_eq!(texts(&preview_column(&map.finish(), 0..1)), vec!["Map [1]"]);
    }

    #[test]
    fn resolves_dictionary_values() {
        let array: DictionaryArray<Int8Type> = vec![Some("b"), None, Some("a"), Some("b")]
            .into_iter()
            .collect();
        assert_eq!(
            texts(&preview_column(&array, 0..4)),
            vec!["b", "null", "a", "b"]
        );
    }

    #[test]
    fn marks_unsupported_types() {
        let array = arrow_array::UnionArray::try_new(
            [(0, Arc::new(Field::new("a", DataType::Int32, false)))]
                .into_iter()
                .collect(),
            vec![0].into(),
            None,
            vec![Arc::new(arrow_array::Int32Array::from(vec![1])) as ArrayRef],
        )
        .unwrap();
        let preview = &preview_column(&array, 0..1)[0];
        assert_eq!(preview.kind, CellKind::Unsupported);
        assert!(preview.text.ends_with("display unsupported"));
    }
}
