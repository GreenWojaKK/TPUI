//! 선택한 셀의 원본 값을 값 트리, 본문, 복사 텍스트로 바꾼다. worker thread에서 실행한다.
//!
//! 노드는 값의 루트부터의 자식 위치 목록으로 가리킨다. Struct는 필드 순서, List는 원소 위치,
//! Map은 entry 위치이고 entry 아래 0은 key, 1은 value다. Dictionary는 참조된 값으로 바꿔 다룬다.

use std::fmt::Write;
use std::ops::Range;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::{Array, ArrayRef, downcast_dictionary_array};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::DataType;
use gpui::SharedString;
use viewer_core::ValueSlice;

use crate::cell_preview::{CellPreview, is_number, preview_column};

/// 패널 상단에 표시하는 셀 정보.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueHeader {
    /// 파일 기준 0부터 시작하는 행.
    pub row: u64,
    pub column: SharedString,
    pub data_type: SharedString,
    pub size: Option<SharedString>,
}

/// 값 트리의 노드 하나. 자식은 따로 요청한다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueNode {
    pub label: SharedString,
    pub preview: CellPreview,
    pub type_label: SharedString,
    pub child_count: usize,
}

/// 선택한 노드의 원본 값 전체.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueBody {
    Null,
    Text(SharedString),
    Bytes(Arc<[u8]>),
    /// Struct·List·Map의 개수 요약.
    Summary(SharedString),
    Unsupported(SharedString),
}

/// 셀을 처음 선택했을 때 보내는 루트 정보.
#[derive(Clone, Debug)]
pub struct ValueDetail {
    pub header: ValueHeader,
    pub root: ValueNode,
    pub body: ValueBody,
}

pub fn detail(slice: &ValueSlice) -> ValueDetail {
    let (array, row) = resolve(&slice.value, 0);
    ValueDetail {
        header: ValueHeader {
            row: slice.row,
            column: slice.field.name().clone().into(),
            data_type: slice.field.data_type().to_string().into(),
            size: value_size(&array, row),
        },
        root: describe(&array, row, slice.field.name().clone().into()),
        body: body(&array, row),
    }
}

/// 경로가 가리키는 노드의 배열과 행. 경로가 값의 구조를 벗어나면 `None`이다.
pub fn node_at(root: &ArrayRef, path: &[usize]) -> Option<(ArrayRef, usize)> {
    let (mut array, mut row) = resolve(root, 0);
    for &step in path {
        let (child, child_row, _) = child(&array, row, step)?;
        (array, row) = resolve(&child, child_row);
    }
    Some((array, row))
}

/// 노드의 `range` 위치 자식들을 만든다. 범위는 자식 수 안으로 자른다.
pub fn children(array: &ArrayRef, row: usize, range: Range<usize>) -> Vec<ValueNode> {
    let end = range.end.min(child_count(array, row));
    (range.start..end)
        .filter_map(|ix| {
            let (child, child_row, label) = child(array, row, ix)?;
            let (child, child_row) = resolve(&child, child_row);
            Some(describe(&child, child_row, label))
        })
        .collect()
}

pub fn body(array: &ArrayRef, row: usize) -> ValueBody {
    if array.is_null(row) || *array.data_type() == DataType::Null {
        return ValueBody::Null;
    }
    match array.data_type() {
        DataType::Utf8 => ValueBody::Text(array.as_string::<i32>().value(row).to_string().into()),
        DataType::LargeUtf8 => {
            ValueBody::Text(array.as_string::<i64>().value(row).to_string().into())
        }
        DataType::Utf8View => ValueBody::Text(array.as_string_view().value(row).to_string().into()),
        _ if is_container(array.data_type()) => ValueBody::Summary(preview(array, row).text),
        _ => match bytes(array, row) {
            Some(bytes) => ValueBody::Bytes(bytes.into()),
            None => match format_value(array, row) {
                Some(text) => ValueBody::Text(text.into()),
                None => {
                    ValueBody::Unsupported(format!("{} · 표시 미지원", array.data_type()).into())
                }
            },
        },
    }
}

/// 복사할 텍스트. 문자열은 원문, Binary는 공백 없는 소문자 16진수, 중첩 값은 JSON이다.
pub fn copy_text(array: &ArrayRef, row: usize) -> String {
    if array.is_null(row) {
        return "null".into();
    }
    match array.data_type() {
        DataType::Utf8 => array.as_string::<i32>().value(row).into(),
        DataType::LargeUtf8 => array.as_string::<i64>().value(row).into(),
        DataType::Utf8View => array.as_string_view().value(row).into(),
        data_type if is_container(data_type) => {
            let mut json = String::new();
            write_json(&mut json, array, row);
            json
        }
        _ => match bytes(array, row) {
            Some(bytes) => hex(bytes),
            None => format_value(array, row).unwrap_or_default(),
        },
    }
}

/// Dictionary는 참조된 값으로 바꾼다. Null key는 Dictionary 자체의 Null로 둔다.
fn resolve(array: &ArrayRef, row: usize) -> (ArrayRef, usize) {
    if !matches!(array.data_type(), DataType::Dictionary(_, _)) || array.is_null(row) {
        return (array.clone(), row);
    }
    let key: Option<usize> = downcast_dictionary_array!(
        array => array.key(row),
        _ => None
    );
    match key {
        Some(key) => resolve(array.as_any_dictionary().values(), key),
        None => (array.clone(), row),
    }
}

fn is_container(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Struct(_)
            | DataType::List(_)
            | DataType::LargeList(_)
            | DataType::ListView(_)
            | DataType::LargeListView(_)
            | DataType::FixedSizeList(_, _)
            | DataType::Map(_, _)
    )
}

fn child_count(array: &ArrayRef, row: usize) -> usize {
    if array.is_null(row) {
        return 0;
    }
    match array.data_type() {
        DataType::Struct(fields) => fields.len(),
        DataType::List(_) => array.as_list::<i32>().value_length(row) as usize,
        DataType::LargeList(_) => array.as_list::<i64>().value_length(row) as usize,
        DataType::ListView(_) => array.as_list_view::<i32>().value_size(row) as usize,
        DataType::LargeListView(_) => array.as_list_view::<i64>().value_size(row) as usize,
        DataType::FixedSizeList(_, len) => *len as usize,
        DataType::Map(_, _) => array.as_map().value_length(row) as usize,
        _ => 0,
    }
}

/// `ix`번째 자식의 배열·행·이름.
fn child(array: &ArrayRef, row: usize, ix: usize) -> Option<(ArrayRef, usize, SharedString)> {
    if ix >= child_count(array, row) {
        return None;
    }
    let item = |values: &ArrayRef, offset: usize| {
        Some((values.clone(), offset + ix, format!("[{ix}]").into()))
    };
    match array.data_type() {
        DataType::Struct(fields) => Some((
            array.as_struct().column(ix).clone(),
            row,
            fields[ix].name().clone().into(),
        )),
        DataType::List(_) => {
            let list = array.as_list::<i32>();
            item(list.values(), list.value_offsets()[row] as usize)
        }
        DataType::LargeList(_) => {
            let list = array.as_list::<i64>();
            item(list.values(), list.value_offsets()[row] as usize)
        }
        DataType::ListView(_) => {
            let list = array.as_list_view::<i32>();
            item(list.values(), list.value_offsets()[row] as usize)
        }
        DataType::LargeListView(_) => {
            let list = array.as_list_view::<i64>();
            item(list.values(), list.value_offsets()[row] as usize)
        }
        DataType::FixedSizeList(_, _) => {
            let list = array.as_fixed_size_list();
            item(list.values(), list.value_offset_at(row))
        }
        DataType::Map(_, _) => {
            let map = array.as_map();
            let entries: ArrayRef = Arc::new(map.entries().clone());
            item(&entries, map.value_offsets()[row] as usize)
        }
        _ => None,
    }
}

fn describe(array: &ArrayRef, row: usize, label: SharedString) -> ValueNode {
    ValueNode {
        label,
        preview: preview(array, row),
        type_label: type_label(array.data_type()),
        child_count: child_count(array, row),
    }
}

fn preview(array: &ArrayRef, row: usize) -> CellPreview {
    preview_column(array.as_ref(), row..row + 1).remove(0)
}

fn type_label(data_type: &DataType) -> SharedString {
    match data_type {
        DataType::Struct(_) => "Struct".into(),
        DataType::List(_) => "List".into(),
        DataType::LargeList(_) => "LargeList".into(),
        DataType::ListView(_) => "ListView".into(),
        DataType::LargeListView(_) => "LargeListView".into(),
        DataType::FixedSizeList(_, len) => format!("FixedSizeList[{len}]").into(),
        DataType::Map(_, _) => "Map".into(),
        other => other.to_string().into(),
    }
}

fn value_size(array: &ArrayRef, row: usize) -> Option<SharedString> {
    if array.is_null(row) {
        return None;
    }
    let text = |value: &str| format!("{} byte · {}자", value.len(), value.chars().count());
    let size = match array.data_type() {
        DataType::Utf8 => text(array.as_string::<i32>().value(row)),
        DataType::LargeUtf8 => text(array.as_string::<i64>().value(row)),
        DataType::Utf8View => text(array.as_string_view().value(row)),
        DataType::Struct(fields) => format!("필드 {}개", fields.len()),
        DataType::Map(_, _) => format!("entry {}개", child_count(array, row)),
        data_type if is_container(data_type) => format!("원소 {}개", child_count(array, row)),
        _ => format!("{} byte", bytes(array, row)?.len()),
    };
    Some(size.into())
}

fn bytes(array: &ArrayRef, row: usize) -> Option<&[u8]> {
    match array.data_type() {
        DataType::Binary => Some(array.as_binary::<i32>().value(row)),
        DataType::LargeBinary => Some(array.as_binary::<i64>().value(row)),
        DataType::BinaryView => Some(array.as_binary_view().value(row)),
        DataType::FixedSizeBinary(_) => Some(array.as_fixed_size_binary().value(row)),
        _ => None,
    }
}

/// arrow-cast의 표시 규칙으로 값 하나를 텍스트로 바꾼다. Decimal scale과 시간 정밀도를 유지한다.
fn format_value(array: &ArrayRef, row: usize) -> Option<String> {
    let options = FormatOptions::new().with_display_error(false);
    let formatter = ArrayFormatter::try_new(array.as_ref(), &options).ok()?;
    formatter.value(row).try_to_string().ok()
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Struct는 객체, List는 배열, Map은 key·value 객체의 배열로 쓴다. Binary는 16진수 문자열,
/// NaN·Infinity와 시간 값처럼 JSON 숫자로 쓸 수 없는 값은 문자열로 쓴다.
fn write_json(out: &mut String, array: &ArrayRef, row: usize) {
    let (array, row) = resolve(array, row);
    if array.is_null(row) {
        out.push_str("null");
        return;
    }
    let data_type = array.data_type();
    match data_type {
        DataType::Boolean => out.push_str(if array.as_boolean().value(row) {
            "true"
        } else {
            "false"
        }),
        DataType::Struct(fields) => {
            out.push('{');
            for (ix, field) in fields.iter().enumerate() {
                if ix > 0 {
                    out.push(',');
                }
                write_json_string(out, field.name());
                out.push(':');
                write_json(out, array.as_struct().column(ix), row);
            }
            out.push('}');
        }
        DataType::Map(_, _) => {
            out.push('[');
            for ix in 0..child_count(&array, row) {
                if ix > 0 {
                    out.push(',');
                }
                let (entries, entry_row, _) = child(&array, row, ix).expect("entry 위치");
                let entry = entries.as_struct();
                out.push_str("{\"key\":");
                write_json(out, entry.column(0), entry_row);
                out.push_str(",\"value\":");
                write_json(out, entry.column(1), entry_row);
                out.push('}');
            }
            out.push(']');
        }
        _ if is_container(data_type) => {
            out.push('[');
            for ix in 0..child_count(&array, row) {
                if ix > 0 {
                    out.push(',');
                }
                let (values, value_row, _) = child(&array, row, ix).expect("원소 위치");
                write_json(out, &values, value_row);
            }
            out.push(']');
        }
        DataType::Utf8 => write_json_string(out, array.as_string::<i32>().value(row)),
        DataType::LargeUtf8 => write_json_string(out, array.as_string::<i64>().value(row)),
        DataType::Utf8View => write_json_string(out, array.as_string_view().value(row)),
        _ => {
            if let Some(bytes) = bytes(&array, row) {
                write_json_string(out, &hex(bytes));
                return;
            }
            let text = format_value(&array, row).unwrap_or_default();
            let is_json_number =
                is_number(data_type) && text.parse::<f64>().is_ok_and(f64::is_finite);
            if is_json_number {
                out.push_str(&text);
            } else {
                write_json_string(out, &text);
            }
        }
    }
}

fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use arrow_array::builder::{Int32Builder, ListBuilder, MapBuilder, StringBuilder};
    use arrow_array::types::Int8Type;
    use arrow_array::{
        BinaryArray, DictionaryArray, Float64Array, Int32Array, StringArray, StructArray,
    };
    use arrow_schema::Field;

    use super::*;

    fn tags() -> ArrayRef {
        let mut list = ListBuilder::new(StringBuilder::new());
        list.values().append_value("a");
        list.append(true);
        for ix in 0..2500 {
            list.values().append_value(format!("t{ix}"));
        }
        list.append(true);
        Arc::new(list.finish())
    }

    fn point() -> ArrayRef {
        Arc::new(StructArray::from(vec![
            (
                Arc::new(Field::new("x", DataType::Int32, false)),
                Arc::new(Int32Array::from(vec![7])) as ArrayRef,
            ),
            (
                Arc::new(Field::new("label", DataType::Utf8, true)),
                Arc::new(StringArray::from(vec![Some("a\"b\nc")])) as ArrayRef,
            ),
        ]))
    }

    fn attrs() -> ArrayRef {
        let mut map = MapBuilder::new(None, StringBuilder::new(), Int32Builder::new());
        map.keys().append_value("k1");
        map.values().append_value(1);
        map.keys().append_value("k2");
        map.values().append_null();
        map.append(true).unwrap();
        Arc::new(map.finish())
    }

    #[test]
    fn walks_list_struct_and_map_children() {
        let list = tags();
        let (array, row) = node_at(&list, &[]).unwrap();
        assert_eq!(child_count(&array, row), 1);
        let long = list.slice(1, 1);
        let (array, row) = node_at(&long, &[]).unwrap();
        assert_eq!(child_count(&array, row), 2500);
        let nodes = children(&array, row, 1000..1002);
        let labels: Vec<_> = nodes.iter().map(|node| node.label.as_ref()).collect();
        assert_eq!(labels, vec!["[1000]", "[1001]"]);
        assert_eq!(nodes[0].preview.text.as_ref(), "t1000");

        let point = point();
        let (array, row) = node_at(&point, &[1]).unwrap();
        assert_eq!(body(&array, row), ValueBody::Text("a\"b\nc".into()));

        let map = attrs();
        let (array, row) = node_at(&map, &[]).unwrap();
        let entries = children(&array, row, 0..10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].child_count, 2);
        let (key, key_row) = node_at(&map, &[1, 0]).unwrap();
        assert_eq!(body(&key, key_row), ValueBody::Text("k2".into()));
        let (value, value_row) = node_at(&map, &[1, 1]).unwrap();
        assert_eq!(body(&value, value_row), ValueBody::Null);
        assert!(node_at(&map, &[5]).is_none());
    }

    #[test]
    fn keeps_full_strings_and_bytes_in_the_body() {
        let long = "가".repeat(10_000) + "\r\n끝";
        let strings: ArrayRef = Arc::new(StringArray::from(vec![long.as_str()]));
        assert_eq!(body(&strings, 0), ValueBody::Text(long.clone().into()));
        assert_eq!(copy_text(&strings, 0), long);

        let raw: Vec<u8> = (0..=255).collect();
        let binary: ArrayRef = Arc::new(BinaryArray::from(vec![raw.as_slice()]));
        assert_eq!(body(&binary, 0), ValueBody::Bytes(raw.clone().into()));
        assert_eq!(copy_text(&binary, 0).len(), 512);
        assert!(copy_text(&binary, 0).starts_with("000102"));
    }

    #[test]
    fn copies_nested_values_as_json() {
        assert_eq!(copy_text(&point(), 0), r#"{"x":7,"label":"a\"b\nc"}"#);
        assert_eq!(
            copy_text(&attrs(), 0),
            r#"[{"key":"k1","value":1},{"key":"k2","value":null}]"#
        );
        let floats: ArrayRef = Arc::new(Float64Array::from(vec![f64::NAN]));
        let mut json = String::new();
        write_json(&mut json, &floats, 0);
        assert_eq!(json, "\"NaN\"");
    }

    #[test]
    fn describes_headers_and_dictionary_values() {
        let dictionary: DictionaryArray<Int8Type> =
            vec![Some("b"), Some("a")].into_iter().collect();
        let dictionary: ArrayRef = Arc::new(dictionary);
        let (array, row) = node_at(&dictionary.slice(1, 1), &[]).unwrap();
        assert_eq!(body(&array, row), ValueBody::Text("a".into()));

        let strings: ArrayRef = Arc::new(StringArray::from(vec!["가나a"]));
        assert_eq!(value_size(&strings, 0), Some("7 byte · 3자".into()));
        assert_eq!(value_size(&attrs(), 0), Some("entry 2개".into()));
    }
}
