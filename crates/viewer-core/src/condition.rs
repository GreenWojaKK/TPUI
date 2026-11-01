//! Parsing and evaluation of filter conditions. Converts user input into values of the column type
//! and computes, per batch, a Boolean of the rows that satisfy the conditions. Find parses values with the same rules.

use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::{
    Array, ArrayRef, BinaryArray, BinaryViewArray, BooleanArray, FixedSizeBinaryArray,
    LargeBinaryArray, LargeStringArray, RecordBatch, Scalar, StringArray, StringViewArray,
};
use arrow_buffer::BooleanBuffer;
use arrow_cast::cast::{CastOptions, cast_with_options};
use arrow_cast::display::FormatOptions;
use arrow_ord::cmp;
use arrow_schema::{DataType, Schema};

use crate::{ColumnId, FilterCondition, FilterOp, ViewerError};

/// Column types grouped by how conditions and find handle their values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueClass {
    /// Strings. The input is used as the value as is.
    Text,
    /// Boolean, numeric, Decimal, date and time. The input is parsed as the column type and compared by value.
    Scalar,
    /// Binary. The input is parsed as hexadecimal.
    Binary,
    /// Types without value comparison: nested types and everything else.
    Other,
}

/// For Dictionary, the type of the referenced values; otherwise the type itself.
pub fn value_type(data_type: &DataType) -> &DataType {
    match data_type {
        DataType::Dictionary(_, values) => value_type(values),
        other => other,
    }
}

pub fn value_class(data_type: &DataType) -> ValueClass {
    use DataType::*;
    match value_type(data_type) {
        Utf8 | LargeUtf8 | Utf8View => ValueClass::Text,
        Binary | LargeBinary | BinaryView | FixedSizeBinary(_) => ValueClass::Binary,
        Boolean
        | Int8
        | Int16
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
        | Date32
        | Date64
        | Time32(_)
        | Time64(_)
        | Timestamp(_, _) => ValueClass::Scalar,
        _ => ValueClass::Other,
    }
}

/// Filter operators available for a column type.
pub fn filter_ops(data_type: &DataType) -> &'static [FilterOp] {
    use FilterOp::*;
    match value_class(data_type) {
        ValueClass::Text => &[Eq, NotEq, Contains, Lt, LtEq, Gt, GtEq, IsNull, IsNotNull],
        ValueClass::Scalar => &[Eq, NotEq, Lt, LtEq, Gt, GtEq, IsNull, IsNotNull],
        ValueClass::Binary => &[Eq, NotEq, IsNull, IsNotNull],
        ValueClass::Other => &[IsNull, IsNotNull],
    }
}

/// Whether the column type can be sorted. Nested types and types without display support are not sorted.
pub fn is_sortable(data_type: &DataType) -> bool {
    match value_class(data_type) {
        ValueClass::Text | ValueClass::Scalar | ValueClass::Binary => true,
        ValueClass::Other => matches!(
            value_type(data_type),
            DataType::Duration(_) | DataType::Interval(_)
        ),
    }
}

/// Parses the input into an array holding one value of `data_type`. Dictionary is parsed as the type
/// of its values. On failure, returns a reason to show the user.
pub fn parse_value(text: &str, data_type: &DataType) -> Result<ArrayRef, String> {
    let target = value_type(data_type);
    let array: ArrayRef = match target {
        DataType::Utf8 => Arc::new(StringArray::from(vec![text])),
        DataType::LargeUtf8 => Arc::new(LargeStringArray::from(vec![text])),
        DataType::Utf8View => Arc::new(StringViewArray::from(vec![text])),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => {
            let bytes = parse_hex(text)
                .ok_or_else(|| format!("'{text}' is not hexadecimal. Example: 0aff"))?;
            match target {
                DataType::Binary => Arc::new(BinaryArray::from(vec![bytes.as_slice()])),
                DataType::LargeBinary => Arc::new(LargeBinaryArray::from(vec![bytes.as_slice()])),
                _ => Arc::new(BinaryViewArray::from(vec![bytes.as_slice()])),
            }
        }
        DataType::FixedSizeBinary(len) => {
            let bytes = parse_hex(text)
                .ok_or_else(|| format!("'{text}' is not hexadecimal. Example: 0aff"))?;
            if bytes.len() != *len as usize {
                return Err(format!("Enter a {len}-byte value."));
            }
            Arc::new(
                FixedSizeBinaryArray::try_from_iter(std::iter::once(bytes))
                    .map_err(|err| err.to_string())?,
            )
        }
        other if value_class(other) == ValueClass::Scalar => {
            let options = CastOptions {
                safe: false,
                format_options: FormatOptions::default(),
            };
            let parsed = cast_with_options(&StringArray::from(vec![text.trim()]), other, &options)
                .map_err(|_| format!("Cannot read '{text}' as a {other} value."))?;
            if parsed.is_null(0) {
                return Err(format!("Cannot read '{text}' as a {other} value."));
            }
            parsed
        }
        other => return Err(format!("{other} columns cannot be compared by value.")),
    };
    Ok(array)
}

/// Converts hexadecimal without spaces into bytes. Case-insensitive.
pub(crate) fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some((high * 16 + low) as u8)
        })
        .collect()
}

/// Converts a Dictionary array into an array of the referenced values. Other arrays are returned as is.
pub(crate) fn unpack_dictionary(array: &ArrayRef) -> Result<ArrayRef, ViewerError> {
    match array.data_type() {
        DataType::Dictionary(_, values) => {
            cast_with_options(array, values, &CastOptions::default())
                .map_err(|err| ViewerError::Decode(err.into()))
        }
        _ => Ok(array.clone()),
    }
}

/// Turns Null into false in a Boolean result.
pub(crate) fn true_bits(result: &BooleanArray) -> BooleanBuffer {
    match result.nulls() {
        Some(nulls) => result.values() & nulls.inner(),
        None => result.values().clone(),
    }
}

/// Values of a string array that contain `needle`. Null is false.
fn contains(array: &dyn Array, needle: &str) -> Result<BooleanBuffer, ViewerError> {
    let len = array.len();
    let check = |is_null: &dyn Fn(usize) -> bool, value: &dyn Fn(usize) -> bool| {
        BooleanBuffer::collect_bool(len, |ix| !is_null(ix) && value(ix))
    };
    Ok(match array.data_type() {
        DataType::Utf8 => {
            let strings = array.as_string::<i32>();
            check(&|ix| strings.is_null(ix), &|ix| {
                strings.value(ix).contains(needle)
            })
        }
        DataType::LargeUtf8 => {
            let strings = array.as_string::<i64>();
            check(&|ix| strings.is_null(ix), &|ix| {
                strings.value(ix).contains(needle)
            })
        }
        DataType::Utf8View => {
            let strings = array.as_string_view();
            check(&|ix| strings.is_null(ix), &|ix| {
                strings.value(ix).contains(needle)
            })
        }
        _ => {
            return Err(ViewerError::InvalidCondition(
                "Contains can only be used on string columns.".into(),
            ));
        }
    })
}

/// Cells equal to one value. Null is false. `value` is an array made by [`parse_value`].
pub(crate) fn equals(array: &ArrayRef, value: &ArrayRef) -> Result<BooleanBuffer, ViewerError> {
    compare(array, FilterOp::Eq, value)
}

fn compare(array: &ArrayRef, op: FilterOp, value: &ArrayRef) -> Result<BooleanBuffer, ViewerError> {
    let array = unpack_dictionary(array)?;
    let scalar = Scalar::new(value.clone());
    let result = match op {
        FilterOp::Eq => cmp::eq(&array, &scalar),
        FilterOp::NotEq => cmp::neq(&array, &scalar),
        FilterOp::Lt => cmp::lt(&array, &scalar),
        FilterOp::LtEq => cmp::lt_eq(&array, &scalar),
        FilterOp::Gt => cmp::gt(&array, &scalar),
        FilterOp::GtEq => cmp::gt_eq(&array, &scalar),
        _ => unreachable!("only comparison operators reach here"),
    }
    .map_err(|err| ViewerError::Decode(err.into()))?;
    Ok(true_bits(&result))
}

/// One condition parsed for the column type.
struct CompiledCondition {
    /// Column position within the batch.
    position: usize,
    op: FilterOp,
    text: String,
    value: Option<ArrayRef>,
}

impl CompiledCondition {
    fn evaluate(&self, array: &ArrayRef) -> Result<BooleanBuffer, ViewerError> {
        let len = array.len();
        match self.op {
            FilterOp::IsNull => Ok(match array.logical_nulls() {
                Some(nulls) => !nulls.inner(),
                None => BooleanBuffer::new_unset(len),
            }),
            FilterOp::IsNotNull => Ok(match array.logical_nulls() {
                Some(nulls) => nulls.inner().clone(),
                None => BooleanBuffer::new_set(len),
            }),
            FilterOp::Contains => contains(unpack_dictionary(array)?.as_ref(), &self.text),
            op => compare(
                array,
                op,
                self.value
                    .as_ref()
                    .expect("comparison operators have a parsed value"),
            ),
        }
    }
}

/// The full set of filter conditions. Reads only the columns with conditions and computes the rows that satisfy all of them.
pub struct CompiledFilter {
    columns: Vec<ColumnId>,
    conditions: Vec<CompiledCondition>,
}

impl CompiledFilter {
    pub fn new(schema: &Schema, conditions: &[FilterCondition]) -> Result<Self, ViewerError> {
        let mut columns: Vec<ColumnId> = conditions.iter().map(|c| c.column).collect();
        columns.sort_unstable();
        columns.dedup();
        let compiled = conditions
            .iter()
            .map(|condition| {
                check_condition(schema, condition).map_err(ViewerError::InvalidCondition)?;
                let field = &schema.fields()[condition.column.0];
                let value = condition
                    .op
                    .takes_value()
                    .then(|| parse_value(&condition.value, field.data_type()))
                    .transpose()
                    .map_err(ViewerError::InvalidCondition)?;
                Ok(CompiledCondition {
                    position: columns
                        .binary_search(&condition.column)
                        .expect("condition columns are in the list"),
                    op: condition.op,
                    text: condition.value.clone(),
                    value,
                })
            })
            .collect::<Result<Vec<_>, ViewerError>>()?;
        Ok(Self {
            columns,
            conditions: compiled,
        })
    }

    /// Columns to read, in source schema order.
    pub fn columns(&self) -> &[ColumnId] {
        &self.columns
    }

    /// Rows of the batch that satisfy all conditions. Batch columns follow [`Self::columns`].
    pub fn evaluate(&self, batch: &RecordBatch) -> Result<BooleanBuffer, ViewerError> {
        let mut result = BooleanBuffer::new_set(batch.num_rows());
        for condition in &self.conditions {
            let matched = condition.evaluate(batch.column(condition.position))?;
            result = &result & &matched;
        }
        Ok(result)
    }
}

/// Checks that the condition fits the column type and that its value can be parsed. On failure,
/// returns a reason to show the user.
pub fn check_condition(schema: &Schema, condition: &FilterCondition) -> Result<(), String> {
    let Some(field) = schema.fields().get(condition.column.0) else {
        return Err("Column is outside the schema.".into());
    };
    if !filter_ops(field.data_type()).contains(&condition.op) {
        return Err(format!(
            "This operator cannot be used on {} columns.",
            field.data_type()
        ));
    }
    if condition.op.takes_value() {
        parse_value(&condition.value, field.data_type())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use arrow_array::types::Int8Type;
    use arrow_array::{
        Decimal128Array, DictionaryArray, Float64Array, Int64Array, TimestampMillisecondArray,
    };
    use arrow_schema::Field;

    use super::*;

    fn bits(buffer: &BooleanBuffer) -> Vec<bool> {
        buffer.iter().collect()
    }

    fn filter(fields: Vec<Field>, conditions: Vec<FilterCondition>) -> CompiledFilter {
        CompiledFilter::new(&Schema::new(fields), &conditions).unwrap()
    }

    fn condition(column: usize, op: FilterOp, value: &str) -> FilterCondition {
        FilterCondition {
            column: ColumnId(column),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn compares_values_in_the_column_type_and_drops_nulls() {
        let decimals: ArrayRef = Arc::new(
            Decimal128Array::from(vec![Some(150), Some(-5), None])
                .with_precision_and_scale(10, 2)
                .unwrap(),
        );
        let field = Field::new("d", decimals.data_type().clone(), true);
        let batch = RecordBatch::try_from_iter([("d", decimals)]).unwrap();
        let eq = filter(vec![field.clone()], vec![condition(0, FilterOp::Eq, "1.5")]);
        assert_eq!(bits(&eq.evaluate(&batch).unwrap()), [true, false, false]);
        let ne = filter(
            vec![field.clone()],
            vec![condition(0, FilterOp::NotEq, "1.5")],
        );
        assert_eq!(bits(&ne.evaluate(&batch).unwrap()), [false, true, false]);
        let null = filter(vec![field], vec![condition(0, FilterOp::IsNull, "")]);
        assert_eq!(bits(&null.evaluate(&batch).unwrap()), [false, false, true]);
    }

    #[test]
    fn combines_conditions_on_several_columns() {
        let names: ArrayRef = Arc::new(StringArray::from(vec![
            Some("울산역"),
            Some("태화강역"),
            None,
            Some("시청"),
        ]));
        let lat: ArrayRef = Arc::new(Float64Array::from(vec![35.5, 35.6, 35.7, 35.8]));
        let fields = vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("lat", DataType::Float64, false),
        ];
        let batch = RecordBatch::try_from_iter([("name", names), ("lat", lat)]).unwrap();
        let both = filter(
            fields,
            vec![
                condition(0, FilterOp::Contains, "역"),
                condition(1, FilterOp::Gt, "35.55"),
            ],
        );
        assert_eq!(both.columns(), [ColumnId(0), ColumnId(1)]);
        assert_eq!(
            bits(&both.evaluate(&batch).unwrap()),
            [false, true, false, false]
        );
    }

    #[test]
    fn compares_dictionary_columns_by_their_values() {
        let dict: DictionaryArray<Int8Type> = vec![Some("b"), None, Some("a"), Some("b")]
            .into_iter()
            .collect();
        let array: ArrayRef = Arc::new(dict);
        let field = Field::new("k", array.data_type().clone(), true);
        let batch = RecordBatch::try_from_iter([("k", array)]).unwrap();
        let eq = filter(vec![field], vec![condition(0, FilterOp::Eq, "b")]);
        assert_eq!(
            bits(&eq.evaluate(&batch).unwrap()),
            [true, false, false, true]
        );
    }

    #[test]
    fn parses_timestamps_with_their_timezone() {
        let ts: ArrayRef = Arc::new(
            TimestampMillisecondArray::from(vec![1_700_000_000_123, 1_700_000_000_000])
                .with_timezone("+09:00"),
        );
        let field = Field::new("t", ts.data_type().clone(), false);
        let batch = RecordBatch::try_from_iter([("t", ts)]).unwrap();
        let ge = filter(
            vec![field],
            vec![condition(
                0,
                FilterOp::GtEq,
                "2023-11-15T07:13:20.100+09:00",
            )],
        );
        assert_eq!(bits(&ge.evaluate(&batch).unwrap()), [true, false]);
    }

    #[test]
    fn reports_values_that_do_not_fit_the_column() {
        let schema = Schema::new(vec![
            Field::new("n", DataType::Int64, false),
            Field::new("b", DataType::Binary, false),
        ]);
        assert!(check_condition(&schema, &condition(0, FilterOp::Eq, "12")).is_ok());
        assert!(check_condition(&schema, &condition(0, FilterOp::Eq, "1x")).is_err());
        assert!(check_condition(&schema, &condition(0, FilterOp::Contains, "1")).is_err());
        assert!(check_condition(&schema, &condition(1, FilterOp::Eq, "0AfF")).is_ok());
        assert!(check_condition(&schema, &condition(1, FilterOp::Eq, "0g")).is_err());
        assert!(check_condition(&schema, &condition(1, FilterOp::Lt, "00")).is_err());
    }

    #[test]
    fn evaluates_nulls_on_nested_columns() {
        let ints: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), None]));
        let batch = RecordBatch::try_from_iter([("n", ints.clone())]).unwrap();
        let field = Field::new("n", DataType::Int64, true);
        let not_null = filter(vec![field], vec![condition(0, FilterOp::IsNotNull, "")]);
        assert_eq!(bits(&not_null.evaluate(&batch).unwrap()), [true, false]);
    }
}
