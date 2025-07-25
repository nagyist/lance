// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors

//! Extend Arrow Functionality
//!
//! To improve Arrow-RS ergonomic

use std::sync::Arc;
use std::{collections::HashMap, ptr::NonNull};

use arrow_array::{
    cast::AsArray, Array, ArrayRef, ArrowNumericType, FixedSizeBinaryArray, FixedSizeListArray,
    GenericListArray, OffsetSizeTrait, PrimitiveArray, RecordBatch, StructArray, UInt32Array,
    UInt8Array,
};
use arrow_array::{
    new_null_array, Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array,
};
use arrow_buffer::MutableBuffer;
use arrow_data::ArrayDataBuilder;
use arrow_schema::{ArrowError, DataType, Field, FieldRef, Fields, IntervalUnit, Schema};
use arrow_select::{interleave::interleave, take::take};
use rand::prelude::*;

pub mod deepcopy;
pub mod schema;
pub use schema::*;
pub mod bfloat16;
pub mod floats;
pub use floats::*;
pub mod cast;
pub mod list;
pub mod memory;

type Result<T> = std::result::Result<T, ArrowError>;

pub trait DataTypeExt {
    /// Returns true if the data type is binary-like, such as (Large)Utf8 and (Large)Binary.
    ///
    /// ```
    /// use lance_arrow::*;
    /// use arrow_schema::DataType;
    ///
    /// assert!(DataType::Utf8.is_binary_like());
    /// assert!(DataType::Binary.is_binary_like());
    /// assert!(DataType::LargeUtf8.is_binary_like());
    /// assert!(DataType::LargeBinary.is_binary_like());
    /// assert!(!DataType::Int32.is_binary_like());
    /// ```
    fn is_binary_like(&self) -> bool;

    /// Returns true if the data type is a struct.
    fn is_struct(&self) -> bool;

    /// Check whether the given Arrow DataType is fixed stride.
    ///
    /// A fixed stride type has the same byte width for all array elements
    /// This includes all PrimitiveType's Boolean, FixedSizeList, FixedSizeBinary, and Decimals
    fn is_fixed_stride(&self) -> bool;

    /// Returns true if the [DataType] is a dictionary type.
    fn is_dictionary(&self) -> bool;

    /// Returns the byte width of the data type
    /// Panics if the data type is not fixed stride.
    fn byte_width(&self) -> usize;

    /// Returns the byte width of the data type, if it is fixed stride.
    /// Returns None if the data type is not fixed stride.
    fn byte_width_opt(&self) -> Option<usize>;
}

impl DataTypeExt for DataType {
    fn is_binary_like(&self) -> bool {
        use DataType::*;
        matches!(self, Utf8 | Binary | LargeUtf8 | LargeBinary)
    }

    fn is_struct(&self) -> bool {
        matches!(self, Self::Struct(_))
    }

    fn is_fixed_stride(&self) -> bool {
        use DataType::*;
        matches!(
            self,
            Boolean
                | UInt8
                | UInt16
                | UInt32
                | UInt64
                | Int8
                | Int16
                | Int32
                | Int64
                | Float16
                | Float32
                | Float64
                | Decimal128(_, _)
                | Decimal256(_, _)
                | FixedSizeList(_, _)
                | FixedSizeBinary(_)
                | Duration(_)
                | Timestamp(_, _)
                | Date32
                | Date64
                | Time32(_)
                | Time64(_)
        )
    }

    fn is_dictionary(&self) -> bool {
        matches!(self, Self::Dictionary(_, _))
    }

    fn byte_width_opt(&self) -> Option<usize> {
        match self {
            Self::Int8 => Some(1),
            Self::Int16 => Some(2),
            Self::Int32 => Some(4),
            Self::Int64 => Some(8),
            Self::UInt8 => Some(1),
            Self::UInt16 => Some(2),
            Self::UInt32 => Some(4),
            Self::UInt64 => Some(8),
            Self::Float16 => Some(2),
            Self::Float32 => Some(4),
            Self::Float64 => Some(8),
            Self::Date32 => Some(4),
            Self::Date64 => Some(8),
            Self::Time32(_) => Some(4),
            Self::Time64(_) => Some(8),
            Self::Timestamp(_, _) => Some(8),
            Self::Duration(_) => Some(8),
            Self::Decimal128(_, _) => Some(16),
            Self::Decimal256(_, _) => Some(32),
            Self::Interval(unit) => match unit {
                IntervalUnit::YearMonth => Some(4),
                IntervalUnit::DayTime => Some(8),
                IntervalUnit::MonthDayNano => Some(16),
            },
            Self::FixedSizeBinary(s) => Some(*s as usize),
            Self::FixedSizeList(dt, s) => dt
                .data_type()
                .byte_width_opt()
                .map(|width| width * *s as usize),
            _ => None,
        }
    }

    fn byte_width(&self) -> usize {
        self.byte_width_opt()
            .unwrap_or_else(|| panic!("Expecting fixed stride data type, found {:?}", self))
    }
}

/// Create an [`GenericListArray`] from values and offsets.
///
/// ```
/// use arrow_array::{Int32Array, Int64Array, ListArray};
/// use arrow_array::types::Int64Type;
/// use lance_arrow::try_new_generic_list_array;
///
/// let offsets = Int32Array::from_iter([0, 2, 7, 10]);
/// let int_values = Int64Array::from_iter(0..10);
/// let list_arr = try_new_generic_list_array(int_values, &offsets).unwrap();
/// assert_eq!(list_arr,
///     ListArray::from_iter_primitive::<Int64Type, _, _>(vec![
///         Some(vec![Some(0), Some(1)]),
///         Some(vec![Some(2), Some(3), Some(4), Some(5), Some(6)]),
///         Some(vec![Some(7), Some(8), Some(9)]),
/// ]))
/// ```
pub fn try_new_generic_list_array<T: Array, Offset: ArrowNumericType>(
    values: T,
    offsets: &PrimitiveArray<Offset>,
) -> Result<GenericListArray<Offset::Native>>
where
    Offset::Native: OffsetSizeTrait,
{
    let data_type = if Offset::Native::IS_LARGE {
        DataType::LargeList(Arc::new(Field::new(
            "item",
            values.data_type().clone(),
            true,
        )))
    } else {
        DataType::List(Arc::new(Field::new(
            "item",
            values.data_type().clone(),
            true,
        )))
    };
    let data = ArrayDataBuilder::new(data_type)
        .len(offsets.len() - 1)
        .add_buffer(offsets.into_data().buffers()[0].clone())
        .add_child_data(values.into_data())
        .build()?;

    Ok(GenericListArray::from(data))
}

pub fn fixed_size_list_type(list_width: i32, inner_type: DataType) -> DataType {
    DataType::FixedSizeList(Arc::new(Field::new("item", inner_type, true)), list_width)
}

pub trait FixedSizeListArrayExt {
    /// Create an [`FixedSizeListArray`] from values and list size.
    ///
    /// ```
    /// use arrow_array::{Int64Array, FixedSizeListArray};
    /// use arrow_array::types::Int64Type;
    /// use lance_arrow::FixedSizeListArrayExt;
    ///
    /// let int_values = Int64Array::from_iter(0..10);
    /// let fixed_size_list_arr = FixedSizeListArray::try_new_from_values(int_values, 2).unwrap();
    /// assert_eq!(fixed_size_list_arr,
    ///     FixedSizeListArray::from_iter_primitive::<Int64Type, _, _>(vec![
    ///         Some(vec![Some(0), Some(1)]),
    ///         Some(vec![Some(2), Some(3)]),
    ///         Some(vec![Some(4), Some(5)]),
    ///         Some(vec![Some(6), Some(7)]),
    ///         Some(vec![Some(8), Some(9)])
    /// ], 2))
    /// ```
    fn try_new_from_values<T: Array + 'static>(
        values: T,
        list_size: i32,
    ) -> Result<FixedSizeListArray>;

    /// Sample `n` rows from the [FixedSizeListArray]
    ///
    /// ```
    /// use arrow_array::{Int64Array, FixedSizeListArray, Array};
    /// use lance_arrow::FixedSizeListArrayExt;
    ///
    /// let int_values = Int64Array::from_iter(0..256);
    /// let fixed_size_list_arr = FixedSizeListArray::try_new_from_values(int_values, 16).unwrap();
    /// let sampled = fixed_size_list_arr.sample(10).unwrap();
    /// assert_eq!(sampled.len(), 10);
    /// assert_eq!(sampled.value_length(), 16);
    /// assert_eq!(sampled.values().len(), 160);
    /// ```
    fn sample(&self, n: usize) -> Result<FixedSizeListArray>;

    /// Ensure the [FixedSizeListArray] of Float16, Float32, Float64,
    /// Int8, Int16, Int32, Int64, UInt8, UInt32 type to its closest floating point type.
    fn convert_to_floating_point(&self) -> Result<FixedSizeListArray>;
}

impl FixedSizeListArrayExt for FixedSizeListArray {
    fn try_new_from_values<T: Array + 'static>(values: T, list_size: i32) -> Result<Self> {
        let field = Arc::new(Field::new("item", values.data_type().clone(), true));
        let values = Arc::new(values);

        Self::try_new(field, list_size, values, None)
    }

    fn sample(&self, n: usize) -> Result<FixedSizeListArray> {
        if n >= self.len() {
            return Ok(self.clone());
        }
        let mut rng = SmallRng::from_entropy();
        let chosen = (0..self.len() as u32).choose_multiple(&mut rng, n);
        take(self, &UInt32Array::from(chosen), None).map(|arr| arr.as_fixed_size_list().clone())
    }

    fn convert_to_floating_point(&self) -> Result<FixedSizeListArray> {
        match self.data_type() {
            DataType::FixedSizeList(field, size) => match field.data_type() {
                DataType::Float16 | DataType::Float32 | DataType::Float64 => Ok(self.clone()),
                DataType::Int8 => Ok(Self::new(
                    Arc::new(arrow_schema::Field::new(
                        field.name(),
                        DataType::Float32,
                        field.is_nullable(),
                    )),
                    *size,
                    Arc::new(Float32Array::from_iter_values(
                        self.values()
                            .as_any()
                            .downcast_ref::<Int8Array>()
                            .ok_or(ArrowError::ParseError(
                                "Fail to cast primitive array to Int8Type".to_string(),
                            ))?
                            .into_iter()
                            .filter_map(|x| x.map(|y| y as f32)),
                    )),
                    self.nulls().cloned(),
                )),
                DataType::Int16 => Ok(Self::new(
                    Arc::new(arrow_schema::Field::new(
                        field.name(),
                        DataType::Float32,
                        field.is_nullable(),
                    )),
                    *size,
                    Arc::new(Float32Array::from_iter_values(
                        self.values()
                            .as_any()
                            .downcast_ref::<Int16Array>()
                            .ok_or(ArrowError::ParseError(
                                "Fail to cast primitive array to Int8Type".to_string(),
                            ))?
                            .into_iter()
                            .filter_map(|x| x.map(|y| y as f32)),
                    )),
                    self.nulls().cloned(),
                )),
                DataType::Int32 => Ok(Self::new(
                    Arc::new(arrow_schema::Field::new(
                        field.name(),
                        DataType::Float32,
                        field.is_nullable(),
                    )),
                    *size,
                    Arc::new(Float32Array::from_iter_values(
                        self.values()
                            .as_any()
                            .downcast_ref::<Int32Array>()
                            .ok_or(ArrowError::ParseError(
                                "Fail to cast primitive array to Int8Type".to_string(),
                            ))?
                            .into_iter()
                            .filter_map(|x| x.map(|y| y as f32)),
                    )),
                    self.nulls().cloned(),
                )),
                DataType::Int64 => Ok(Self::new(
                    Arc::new(arrow_schema::Field::new(
                        field.name(),
                        DataType::Float64,
                        field.is_nullable(),
                    )),
                    *size,
                    Arc::new(Float64Array::from_iter_values(
                        self.values()
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .ok_or(ArrowError::ParseError(
                                "Fail to cast primitive array to Int8Type".to_string(),
                            ))?
                            .into_iter()
                            .filter_map(|x| x.map(|y| y as f64)),
                    )),
                    self.nulls().cloned(),
                )),
                DataType::UInt8 => Ok(Self::new(
                    Arc::new(arrow_schema::Field::new(
                        field.name(),
                        DataType::Float64,
                        field.is_nullable(),
                    )),
                    *size,
                    Arc::new(Float64Array::from_iter_values(
                        self.values()
                            .as_any()
                            .downcast_ref::<UInt8Array>()
                            .ok_or(ArrowError::ParseError(
                                "Fail to cast primitive array to Int8Type".to_string(),
                            ))?
                            .into_iter()
                            .filter_map(|x| x.map(|y| y as f64)),
                    )),
                    self.nulls().cloned(),
                )),
                DataType::UInt32 => Ok(Self::new(
                    Arc::new(arrow_schema::Field::new(
                        field.name(),
                        DataType::Float64,
                        field.is_nullable(),
                    )),
                    *size,
                    Arc::new(Float64Array::from_iter_values(
                        self.values()
                            .as_any()
                            .downcast_ref::<UInt32Array>()
                            .ok_or(ArrowError::ParseError(
                                "Fail to cast primitive array to Int8Type".to_string(),
                            ))?
                            .into_iter()
                            .filter_map(|x| x.map(|y| y as f64)),
                    )),
                    self.nulls().cloned(),
                )),
                data_type => Err(ArrowError::ParseError(format!(
                    "Expect either floating type or integer got {:?}",
                    data_type
                ))),
            },
            data_type => Err(ArrowError::ParseError(format!(
                "Expect either FixedSizeList got {:?}",
                data_type
            ))),
        }
    }
}

/// Force downcast of an [`Array`], such as an [`ArrayRef`], to
/// [`FixedSizeListArray`], panic'ing on failure.
pub fn as_fixed_size_list_array(arr: &dyn Array) -> &FixedSizeListArray {
    arr.as_any().downcast_ref::<FixedSizeListArray>().unwrap()
}

pub trait FixedSizeBinaryArrayExt {
    /// Create an [`FixedSizeBinaryArray`] from values and stride.
    ///
    /// ```
    /// use arrow_array::{UInt8Array, FixedSizeBinaryArray};
    /// use arrow_array::types::UInt8Type;
    /// use lance_arrow::FixedSizeBinaryArrayExt;
    ///
    /// let int_values = UInt8Array::from_iter(0..10);
    /// let fixed_size_list_arr = FixedSizeBinaryArray::try_new_from_values(&int_values, 2).unwrap();
    /// assert_eq!(fixed_size_list_arr,
    ///     FixedSizeBinaryArray::from(vec![
    ///         Some(vec![0, 1].as_slice()),
    ///         Some(vec![2, 3].as_slice()),
    ///         Some(vec![4, 5].as_slice()),
    ///         Some(vec![6, 7].as_slice()),
    ///         Some(vec![8, 9].as_slice())
    /// ]))
    /// ```
    fn try_new_from_values(values: &UInt8Array, stride: i32) -> Result<FixedSizeBinaryArray>;
}

impl FixedSizeBinaryArrayExt for FixedSizeBinaryArray {
    fn try_new_from_values(values: &UInt8Array, stride: i32) -> Result<Self> {
        let data_type = DataType::FixedSizeBinary(stride);
        let data = ArrayDataBuilder::new(data_type)
            .len(values.len() / stride as usize)
            .add_buffer(values.into_data().buffers()[0].clone())
            .build()?;
        Ok(Self::from(data))
    }
}

pub fn as_fixed_size_binary_array(arr: &dyn Array) -> &FixedSizeBinaryArray {
    arr.as_any().downcast_ref::<FixedSizeBinaryArray>().unwrap()
}

pub fn iter_str_array(arr: &dyn Array) -> Box<dyn Iterator<Item = Option<&str>> + Send + '_> {
    match arr.data_type() {
        DataType::Utf8 => Box::new(arr.as_string::<i32>().iter()),
        DataType::LargeUtf8 => Box::new(arr.as_string::<i64>().iter()),
        _ => panic!("Expecting Utf8 or LargeUtf8, found {:?}", arr.data_type()),
    }
}

/// Extends Arrow's [RecordBatch].
pub trait RecordBatchExt {
    /// Append a new column to this [`RecordBatch`] and returns a new RecordBatch.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use arrow_array::{RecordBatch, Int32Array, StringArray};
    /// use arrow_schema::{Schema, Field, DataType};
    /// use lance_arrow::*;
    ///
    /// let schema = Arc::new(Schema::new(vec![Field::new("a", DataType::Int32, true)]));
    /// let int_arr = Arc::new(Int32Array::from(vec![1, 2, 3, 4]));
    /// let record_batch = RecordBatch::try_new(schema, vec![int_arr.clone()]).unwrap();
    ///
    /// let new_field = Field::new("s", DataType::Utf8, true);
    /// let str_arr = Arc::new(StringArray::from(vec!["a", "b", "c", "d"]));
    /// let new_record_batch = record_batch.try_with_column(new_field, str_arr.clone()).unwrap();
    ///
    /// assert_eq!(
    ///     new_record_batch,
    ///     RecordBatch::try_new(
    ///         Arc::new(Schema::new(
    ///             vec![
    ///                 Field::new("a", DataType::Int32, true),
    ///                 Field::new("s", DataType::Utf8, true)
    ///             ])
    ///         ),
    ///         vec![int_arr, str_arr],
    ///     ).unwrap()
    /// )
    /// ```
    fn try_with_column(&self, field: Field, arr: ArrayRef) -> Result<RecordBatch>;

    /// Created a new RecordBatch with column at index.
    fn try_with_column_at(&self, index: usize, field: Field, arr: ArrayRef) -> Result<RecordBatch>;

    /// Creates a new [`RecordBatch`] from the provided  [`StructArray`].
    ///
    /// The fields on the [`StructArray`] need to match this [`RecordBatch`] schema
    fn try_new_from_struct_array(&self, arr: StructArray) -> Result<RecordBatch>;

    /// Merge with another [`RecordBatch`] and returns a new one.
    ///
    /// Fields are merged based on name.  First we iterate the left columns.  If a matching
    /// name is found in the right then we merge the two columns.  If there is no match then
    /// we add the left column to the output.
    ///
    /// To merge two columns we consider the type.  If both arrays are struct arrays we recurse.
    /// Otherwise we use the left array.
    ///
    /// Afterwards we add all non-matching right columns to the output.
    ///
    /// Note: This method likely does not handle nested fields correctly and you may want to consider
    /// using [`merge_with_schema`] instead.
    /// ```
    /// use std::sync::Arc;
    /// use arrow_array::*;
    /// use arrow_schema::{Schema, Field, DataType};
    /// use lance_arrow::*;
    ///
    /// let left_schema = Arc::new(Schema::new(vec![Field::new("a", DataType::Int32, true)]));
    /// let int_arr = Arc::new(Int32Array::from(vec![1, 2, 3, 4]));
    /// let left = RecordBatch::try_new(left_schema, vec![int_arr.clone()]).unwrap();
    ///
    /// let right_schema = Arc::new(Schema::new(vec![Field::new("s", DataType::Utf8, true)]));
    /// let str_arr = Arc::new(StringArray::from(vec!["a", "b", "c", "d"]));
    /// let right = RecordBatch::try_new(right_schema, vec![str_arr.clone()]).unwrap();
    ///
    /// let new_record_batch = left.merge(&right).unwrap();
    ///
    /// assert_eq!(
    ///     new_record_batch,
    ///     RecordBatch::try_new(
    ///         Arc::new(Schema::new(
    ///             vec![
    ///                 Field::new("a", DataType::Int32, true),
    ///                 Field::new("s", DataType::Utf8, true)
    ///             ])
    ///         ),
    ///         vec![int_arr, str_arr],
    ///     ).unwrap()
    /// )
    /// ```
    ///
    /// TODO: add merge nested fields support.
    fn merge(&self, other: &RecordBatch) -> Result<RecordBatch>;

    /// Create a batch by merging columns between two batches with a given schema.
    ///
    /// A reference schema is used to determine the proper ordering of nested fields.
    ///
    /// For each field in the reference schema we look for corresponding fields in
    /// the left and right batches.  If a field is found in both batches we recursively merge
    /// it.
    ///
    /// If a field is only in the left or right batch we take it as it is.
    fn merge_with_schema(&self, other: &RecordBatch, schema: &Schema) -> Result<RecordBatch>;

    /// Drop one column specified with the name and return the new [`RecordBatch`].
    ///
    /// If the named column does not exist, it returns a copy of this [`RecordBatch`].
    fn drop_column(&self, name: &str) -> Result<RecordBatch>;

    /// Replace a column (specified by name) and return the new [`RecordBatch`].
    fn replace_column_by_name(&self, name: &str, column: Arc<dyn Array>) -> Result<RecordBatch>;

    /// Replace a column schema (specified by name) and return the new [`RecordBatch`].
    fn replace_column_schema_by_name(
        &self,
        name: &str,
        new_data_type: DataType,
        column: Arc<dyn Array>,
    ) -> Result<RecordBatch>;

    /// Rename a column at a given index.
    fn rename_column(&self, index: usize, new_name: &str) -> Result<RecordBatch>;

    /// Get (potentially nested) column by qualified name.
    fn column_by_qualified_name(&self, name: &str) -> Option<&ArrayRef>;

    /// Project the schema over the [RecordBatch].
    fn project_by_schema(&self, schema: &Schema) -> Result<RecordBatch>;

    /// metadata of the schema.
    fn metadata(&self) -> &HashMap<String, String>;

    /// Add metadata to the schema.
    fn add_metadata(&self, key: String, value: String) -> Result<RecordBatch> {
        let mut metadata = self.metadata().clone();
        metadata.insert(key, value);
        self.with_metadata(metadata)
    }

    /// Replace the schema metadata with the provided one.
    fn with_metadata(&self, metadata: HashMap<String, String>) -> Result<RecordBatch>;

    /// Take selected rows from the [RecordBatch].
    fn take(&self, indices: &UInt32Array) -> Result<RecordBatch>;
}

impl RecordBatchExt for RecordBatch {
    fn try_with_column(&self, field: Field, arr: ArrayRef) -> Result<Self> {
        let new_schema = Arc::new(self.schema().as_ref().try_with_column(field)?);
        let mut new_columns = self.columns().to_vec();
        new_columns.push(arr);
        Self::try_new(new_schema, new_columns)
    }

    fn try_with_column_at(&self, index: usize, field: Field, arr: ArrayRef) -> Result<Self> {
        let new_schema = Arc::new(self.schema().as_ref().try_with_column_at(index, field)?);
        let mut new_columns = self.columns().to_vec();
        new_columns.insert(index, arr);
        Self::try_new(new_schema, new_columns)
    }

    fn try_new_from_struct_array(&self, arr: StructArray) -> Result<Self> {
        let schema = Arc::new(Schema::new_with_metadata(
            arr.fields().to_vec(),
            self.schema().metadata.clone(),
        ));
        let batch = Self::from(arr);
        batch.with_schema(schema)
    }

    fn merge(&self, other: &Self) -> Result<Self> {
        if self.num_rows() != other.num_rows() {
            return Err(ArrowError::InvalidArgumentError(format!(
                "Attempt to merge two RecordBatch with different sizes: {} != {}",
                self.num_rows(),
                other.num_rows()
            )));
        }
        let left_struct_array: StructArray = self.clone().into();
        let right_struct_array: StructArray = other.clone().into();
        self.try_new_from_struct_array(merge(&left_struct_array, &right_struct_array))
    }

    fn merge_with_schema(&self, other: &RecordBatch, schema: &Schema) -> Result<RecordBatch> {
        if self.num_rows() != other.num_rows() {
            return Err(ArrowError::InvalidArgumentError(format!(
                "Attempt to merge two RecordBatch with different sizes: {} != {}",
                self.num_rows(),
                other.num_rows()
            )));
        }
        let left_struct_array: StructArray = self.clone().into();
        let right_struct_array: StructArray = other.clone().into();
        self.try_new_from_struct_array(merge_with_schema(
            &left_struct_array,
            &right_struct_array,
            schema.fields(),
        ))
    }

    fn drop_column(&self, name: &str) -> Result<Self> {
        let mut fields = vec![];
        let mut columns = vec![];
        for i in 0..self.schema().fields.len() {
            if self.schema().field(i).name() != name {
                fields.push(self.schema().field(i).clone());
                columns.push(self.column(i).clone());
            }
        }
        Self::try_new(
            Arc::new(Schema::new_with_metadata(
                fields,
                self.schema().metadata().clone(),
            )),
            columns,
        )
    }

    fn rename_column(&self, index: usize, new_name: &str) -> Result<RecordBatch> {
        let mut fields = self.schema().fields().to_vec();
        if index >= fields.len() {
            return Err(ArrowError::InvalidArgumentError(format!(
                "Index out of bounds: {}",
                index
            )));
        }
        fields[index] = Arc::new(Field::new(
            new_name,
            fields[index].data_type().clone(),
            fields[index].is_nullable(),
        ));
        Self::try_new(
            Arc::new(Schema::new_with_metadata(
                fields,
                self.schema().metadata().clone(),
            )),
            self.columns().to_vec(),
        )
    }

    fn replace_column_by_name(&self, name: &str, column: Arc<dyn Array>) -> Result<RecordBatch> {
        let mut columns = self.columns().to_vec();
        let field_i = self
            .schema()
            .fields()
            .iter()
            .position(|f| f.name() == name)
            .ok_or_else(|| ArrowError::SchemaError(format!("Field {} does not exist", name)))?;
        columns[field_i] = column;
        Self::try_new(self.schema(), columns)
    }

    fn replace_column_schema_by_name(
        &self,
        name: &str,
        new_data_type: DataType,
        column: Arc<dyn Array>,
    ) -> Result<RecordBatch> {
        let fields = self
            .schema()
            .fields()
            .iter()
            .map(|x| {
                if x.name() != name {
                    x.clone()
                } else {
                    let new_field = Field::new(name, new_data_type.clone(), x.is_nullable());
                    Arc::new(new_field)
                }
            })
            .collect::<Vec<_>>();
        let schema = Schema::new_with_metadata(fields, self.schema().metadata.clone());
        let mut columns = self.columns().to_vec();
        let field_i = self
            .schema()
            .fields()
            .iter()
            .position(|f| f.name() == name)
            .ok_or_else(|| ArrowError::SchemaError(format!("Field {} does not exist", name)))?;
        columns[field_i] = column;
        Self::try_new(Arc::new(schema), columns)
    }

    fn column_by_qualified_name(&self, name: &str) -> Option<&ArrayRef> {
        let split = name.split('.').collect::<Vec<_>>();
        if split.is_empty() {
            return None;
        }

        self.column_by_name(split[0])
            .and_then(|arr| get_sub_array(arr, &split[1..]))
    }

    fn project_by_schema(&self, schema: &Schema) -> Result<Self> {
        let struct_array: StructArray = self.clone().into();
        self.try_new_from_struct_array(project(&struct_array, schema.fields())?)
    }

    fn metadata(&self) -> &HashMap<String, String> {
        self.schema_ref().metadata()
    }

    fn with_metadata(&self, metadata: HashMap<String, String>) -> Result<RecordBatch> {
        let mut schema = self.schema_ref().as_ref().clone();
        schema.metadata = metadata;
        Self::try_new(schema.into(), self.columns().into())
    }

    fn take(&self, indices: &UInt32Array) -> Result<Self> {
        let struct_array: StructArray = self.clone().into();
        let taken = take(&struct_array, indices, None)?;
        self.try_new_from_struct_array(taken.as_struct().clone())
    }
}

fn project(struct_array: &StructArray, fields: &Fields) -> Result<StructArray> {
    if fields.is_empty() {
        return Ok(StructArray::new_empty_fields(
            struct_array.len(),
            struct_array.nulls().cloned(),
        ));
    }
    let mut columns: Vec<ArrayRef> = vec![];
    for field in fields.iter() {
        if let Some(col) = struct_array.column_by_name(field.name()) {
            match field.data_type() {
                // TODO handle list-of-struct
                DataType::Struct(subfields) => {
                    let projected = project(col.as_struct(), subfields)?;
                    columns.push(Arc::new(projected));
                }
                _ => {
                    columns.push(col.clone());
                }
            }
        } else {
            return Err(ArrowError::SchemaError(format!(
                "field {} does not exist in the RecordBatch",
                field.name()
            )));
        }
    }
    StructArray::try_new(fields.clone(), columns, None)
}

fn lists_have_same_offsets_helper<T: OffsetSizeTrait>(left: &dyn Array, right: &dyn Array) -> bool {
    let left_list: &GenericListArray<T> = left.as_list();
    let right_list: &GenericListArray<T> = right.as_list();
    left_list.offsets().inner() == right_list.offsets().inner()
}

fn merge_list_structs_helper<T: OffsetSizeTrait>(
    left: &dyn Array,
    right: &dyn Array,
    items_field_name: impl Into<String>,
    items_nullable: bool,
) -> Arc<dyn Array> {
    let left_list: &GenericListArray<T> = left.as_list();
    let right_list: &GenericListArray<T> = right.as_list();
    let left_struct = left_list.values();
    let right_struct = right_list.values();
    let left_struct_arr = left_struct.as_struct();
    let right_struct_arr = right_struct.as_struct();
    let merged_items = Arc::new(merge(left_struct_arr, right_struct_arr));
    let items_field = Arc::new(Field::new(
        items_field_name,
        merged_items.data_type().clone(),
        items_nullable,
    ));
    Arc::new(GenericListArray::<T>::new(
        items_field,
        left_list.offsets().clone(),
        merged_items,
        left_list.nulls().cloned(),
    ))
}

fn merge_list_struct_null_helper<T: OffsetSizeTrait>(
    left: &dyn Array,
    right: &dyn Array,
    not_null: &dyn Array,
    items_field_name: impl Into<String>,
) -> Arc<dyn Array> {
    let left_list: &GenericListArray<T> = left.as_list::<T>();
    let not_null_list = not_null.as_list::<T>();
    let right_list = right.as_list::<T>();

    let left_struct = left_list.values().as_struct();
    let not_null_struct: &StructArray = not_null_list.values().as_struct();
    let right_struct = right_list.values().as_struct();

    let values_len = not_null_list.values().len();
    let mut merged_fields =
        Vec::with_capacity(not_null_struct.num_columns() + right_struct.num_columns());
    let mut merged_columns =
        Vec::with_capacity(not_null_struct.num_columns() + right_struct.num_columns());

    for (_, field) in left_struct.columns().iter().zip(left_struct.fields()) {
        merged_fields.push(field.clone());
        if let Some(val) = not_null_struct.column_by_name(field.name()) {
            merged_columns.push(val.clone());
        } else {
            merged_columns.push(new_null_array(field.data_type(), values_len))
        }
    }
    for (_, field) in right_struct
        .columns()
        .iter()
        .zip(right_struct.fields())
        .filter(|(_, field)| left_struct.column_by_name(field.name()).is_none())
    {
        merged_fields.push(field.clone());
        if let Some(val) = not_null_struct.column_by_name(field.name()) {
            merged_columns.push(val.clone());
        } else {
            merged_columns.push(new_null_array(field.data_type(), values_len));
        }
    }

    let merged_struct = Arc::new(StructArray::new(
        Fields::from(merged_fields),
        merged_columns,
        not_null_struct.nulls().cloned(),
    ));
    let items_field = Arc::new(Field::new(
        items_field_name,
        merged_struct.data_type().clone(),
        true,
    ));
    Arc::new(GenericListArray::<T>::new(
        items_field,
        not_null_list.offsets().clone(),
        merged_struct,
        not_null_list.nulls().cloned(),
    ))
}

fn merge_list_struct_null(
    left: &dyn Array,
    right: &dyn Array,
    not_null: &dyn Array,
) -> Arc<dyn Array> {
    match left.data_type() {
        DataType::List(left_field) => {
            merge_list_struct_null_helper::<i32>(left, right, not_null, left_field.name())
        }
        DataType::LargeList(left_field) => {
            merge_list_struct_null_helper::<i64>(left, right, not_null, left_field.name())
        }
        _ => unreachable!(),
    }
}

fn merge_list_struct(left: &dyn Array, right: &dyn Array) -> Arc<dyn Array> {
    // Merging fields into a list<struct<...>> is tricky and can only succeed
    // in two ways.  First, if both lists have the same offsets.  Second, if
    // one of the lists is all-null
    if left.null_count() == left.len() {
        return merge_list_struct_null(left, right, right);
    } else if right.null_count() == right.len() {
        return merge_list_struct_null(left, right, left);
    }
    match (left.data_type(), right.data_type()) {
        (DataType::List(left_field), DataType::List(_)) => {
            if !lists_have_same_offsets_helper::<i32>(left, right) {
                panic!("Attempt to merge list struct arrays which do not have same offsets");
            }
            merge_list_structs_helper::<i32>(
                left,
                right,
                left_field.name(),
                left_field.is_nullable(),
            )
        }
        (DataType::LargeList(left_field), DataType::LargeList(_)) => {
            if !lists_have_same_offsets_helper::<i64>(left, right) {
                panic!("Attempt to merge list struct arrays which do not have same offsets");
            }
            merge_list_structs_helper::<i64>(
                left,
                right,
                left_field.name(),
                left_field.is_nullable(),
            )
        }
        _ => unreachable!(),
    }
}

fn merge(left_struct_array: &StructArray, right_struct_array: &StructArray) -> StructArray {
    let mut fields: Vec<Field> = vec![];
    let mut columns: Vec<ArrayRef> = vec![];
    let right_fields = right_struct_array.fields();
    let right_columns = right_struct_array.columns();

    // iterate through the fields on the left hand side
    for (left_field, left_column) in left_struct_array
        .fields()
        .iter()
        .zip(left_struct_array.columns().iter())
    {
        match right_fields
            .iter()
            .position(|f| f.name() == left_field.name())
        {
            // if the field exists on the right hand side, merge them recursively if appropriate
            Some(right_index) => {
                let right_field = right_fields.get(right_index).unwrap();
                let right_column = right_columns.get(right_index).unwrap();
                // if both fields are struct, merge them recursively
                match (left_field.data_type(), right_field.data_type()) {
                    (DataType::Struct(_), DataType::Struct(_)) => {
                        let left_sub_array = left_column.as_struct();
                        let right_sub_array = right_column.as_struct();
                        let merged_sub_array = merge(left_sub_array, right_sub_array);
                        fields.push(Field::new(
                            left_field.name(),
                            merged_sub_array.data_type().clone(),
                            left_field.is_nullable(),
                        ));
                        columns.push(Arc::new(merged_sub_array) as ArrayRef);
                    }
                    (DataType::List(left_list), DataType::List(right_list))
                        if left_list.data_type().is_struct()
                            && right_list.data_type().is_struct() =>
                    {
                        // If there is nothing to merge just use the left field
                        if left_list.data_type() == right_list.data_type() {
                            fields.push(left_field.as_ref().clone());
                            columns.push(left_column.clone());
                        }
                        // If we have two List<Struct> and they have different sets of fields then
                        // we can merge them if the offsets arrays are the same.  Otherwise, we
                        // have to consider it an error.
                        let merged_sub_array = merge_list_struct(&left_column, &right_column);

                        fields.push(Field::new(
                            left_field.name(),
                            merged_sub_array.data_type().clone(),
                            left_field.is_nullable(),
                        ));
                        columns.push(merged_sub_array);
                    }
                    // otherwise, just use the field on the left hand side
                    _ => {
                        // TODO handle list-of-struct and other types
                        fields.push(left_field.as_ref().clone());
                        columns.push(left_column.clone());
                    }
                }
            }
            None => {
                fields.push(left_field.as_ref().clone());
                columns.push(left_column.clone());
            }
        }
    }

    // now iterate through the fields on the right hand side
    right_fields
        .iter()
        .zip(right_columns.iter())
        .for_each(|(field, column)| {
            // add new columns on the right
            if !left_struct_array
                .fields()
                .iter()
                .any(|f| f.name() == field.name())
            {
                fields.push(field.as_ref().clone());
                columns.push(column.clone() as ArrayRef);
            }
        });

    let zipped: Vec<(FieldRef, ArrayRef)> = fields
        .iter()
        .cloned()
        .map(Arc::new)
        .zip(columns.iter().cloned())
        .collect::<Vec<_>>();
    StructArray::from(zipped)
}

fn merge_with_schema(
    left_struct_array: &StructArray,
    right_struct_array: &StructArray,
    fields: &Fields,
) -> StructArray {
    // Helper function that returns true if both types are struct or both are non-struct
    fn same_type_kind(left: &DataType, right: &DataType) -> bool {
        match (left, right) {
            (DataType::Struct(_), DataType::Struct(_)) => true,
            (DataType::Struct(_), _) => false,
            (_, DataType::Struct(_)) => false,
            _ => true,
        }
    }

    let mut output_fields: Vec<Field> = Vec::with_capacity(fields.len());
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(fields.len());

    let left_fields = left_struct_array.fields();
    let left_columns = left_struct_array.columns();
    let right_fields = right_struct_array.fields();
    let right_columns = right_struct_array.columns();

    for field in fields {
        let left_match_idx = left_fields.iter().position(|f| {
            f.name() == field.name() && same_type_kind(f.data_type(), field.data_type())
        });
        let right_match_idx = right_fields.iter().position(|f| {
            f.name() == field.name() && same_type_kind(f.data_type(), field.data_type())
        });

        match (left_match_idx, right_match_idx) {
            (None, Some(right_idx)) => {
                output_fields.push(right_fields[right_idx].as_ref().clone());
                columns.push(right_columns[right_idx].clone());
            }
            (Some(left_idx), None) => {
                output_fields.push(left_fields[left_idx].as_ref().clone());
                columns.push(left_columns[left_idx].clone());
            }
            (Some(left_idx), Some(right_idx)) => {
                if let DataType::Struct(child_fields) = field.data_type() {
                    let left_sub_array = left_columns[left_idx].as_struct();
                    let right_sub_array = right_columns[right_idx].as_struct();
                    let merged_sub_array =
                        merge_with_schema(left_sub_array, right_sub_array, child_fields);
                    output_fields.push(Field::new(
                        field.name(),
                        merged_sub_array.data_type().clone(),
                        field.is_nullable(),
                    ));
                    columns.push(Arc::new(merged_sub_array) as ArrayRef);
                } else {
                    output_fields.push(left_fields[left_idx].as_ref().clone());
                    columns.push(left_columns[left_idx].clone());
                }
            }
            (None, None) => {
                // The field will not be included in the output
            }
        }
    }

    let zipped: Vec<(FieldRef, ArrayRef)> = output_fields
        .into_iter()
        .map(Arc::new)
        .zip(columns)
        .collect::<Vec<_>>();
    StructArray::from(zipped)
}

fn get_sub_array<'a>(array: &'a ArrayRef, components: &[&str]) -> Option<&'a ArrayRef> {
    if components.is_empty() {
        return Some(array);
    }
    if !matches!(array.data_type(), DataType::Struct(_)) {
        return None;
    }
    let struct_arr = array.as_struct();
    struct_arr
        .column_by_name(components[0])
        .and_then(|arr| get_sub_array(arr, &components[1..]))
}

/// Interleave multiple RecordBatches into a single RecordBatch.
///
/// Behaves like [`arrow::compute::interleave`], but for RecordBatches.
pub fn interleave_batches(
    batches: &[RecordBatch],
    indices: &[(usize, usize)],
) -> Result<RecordBatch> {
    let first_batch = batches.first().ok_or_else(|| {
        ArrowError::InvalidArgumentError("Cannot interleave zero RecordBatches".to_string())
    })?;
    let schema = first_batch.schema();
    let num_columns = first_batch.num_columns();
    let mut columns = Vec::with_capacity(num_columns);
    let mut chunks = Vec::with_capacity(batches.len());

    for i in 0..num_columns {
        for batch in batches {
            chunks.push(batch.column(i).as_ref());
        }
        let new_column = interleave(&chunks, indices)?;
        columns.push(new_column);
        chunks.clear();
    }

    RecordBatch::try_new(schema, columns)
}

pub trait BufferExt {
    /// Create an `arrow_buffer::Buffer`` from a `bytes::Bytes` object
    ///
    /// The alignment must be specified (as `bytes_per_value`) since we want to make
    /// sure we can safely reinterpret the buffer.
    ///
    /// If the buffer is properly aligned this will be zero-copy.  If not, a copy
    /// will be made and an owned buffer returned.
    ///
    /// If `bytes_per_value` is not a power of two, then we assume the buffer is
    /// never going to be reinterpreted into another type and we can safely
    /// ignore the alignment.
    ///
    /// Yes, the method name is odd.  It's because there is already a `from_bytes`
    /// which converts from `arrow_buffer::bytes::Bytes` (not `bytes::Bytes`)
    fn from_bytes_bytes(bytes: bytes::Bytes, bytes_per_value: u64) -> Self;

    /// Allocates a new properly aligned arrow buffer and copies `bytes` into it
    ///
    /// `size_bytes` can be larger than `bytes` and, if so, the trailing bytes will
    /// be zeroed out.
    ///
    /// # Panics
    ///
    /// Panics if `size_bytes` is less than `bytes.len()`
    fn copy_bytes_bytes(bytes: bytes::Bytes, size_bytes: usize) -> Self;
}

fn is_pwr_two(n: u64) -> bool {
    n & (n - 1) == 0
}

impl BufferExt for arrow_buffer::Buffer {
    fn from_bytes_bytes(bytes: bytes::Bytes, bytes_per_value: u64) -> Self {
        if is_pwr_two(bytes_per_value) && bytes.as_ptr().align_offset(bytes_per_value as usize) != 0
        {
            // The original buffer is not aligned, cannot zero-copy
            let size_bytes = bytes.len();
            Self::copy_bytes_bytes(bytes, size_bytes)
        } else {
            // The original buffer is aligned, can zero-copy
            // SAFETY: the alignment is correct we can make this conversion
            unsafe {
                Self::from_custom_allocation(
                    NonNull::new(bytes.as_ptr() as _).expect("should be a valid pointer"),
                    bytes.len(),
                    Arc::new(bytes),
                )
            }
        }
    }

    fn copy_bytes_bytes(bytes: bytes::Bytes, size_bytes: usize) -> Self {
        assert!(size_bytes >= bytes.len());
        let mut buf = MutableBuffer::with_capacity(size_bytes);
        let to_fill = size_bytes - bytes.len();
        buf.extend(bytes);
        buf.extend(std::iter::repeat_n(0_u8, to_fill));
        Self::from(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{new_empty_array, new_null_array, Int32Array, ListArray, StringArray};
    use arrow_buffer::OffsetBuffer;

    #[test]
    fn test_merge_recursive() {
        let a_array = Int32Array::from(vec![Some(1), Some(2), Some(3)]);
        let e_array = Int32Array::from(vec![Some(4), Some(5), Some(6)]);
        let c_array = Int32Array::from(vec![Some(7), Some(8), Some(9)]);
        let d_array = StringArray::from(vec![Some("a"), Some("b"), Some("c")]);

        let left_schema = Schema::new(vec![
            Field::new("a", DataType::Int32, true),
            Field::new(
                "b",
                DataType::Struct(vec![Field::new("c", DataType::Int32, true)].into()),
                true,
            ),
        ]);
        let left_batch = RecordBatch::try_new(
            Arc::new(left_schema),
            vec![
                Arc::new(a_array.clone()),
                Arc::new(StructArray::from(vec![(
                    Arc::new(Field::new("c", DataType::Int32, true)),
                    Arc::new(c_array.clone()) as ArrayRef,
                )])),
            ],
        )
        .unwrap();

        let right_schema = Schema::new(vec![
            Field::new("e", DataType::Int32, true),
            Field::new(
                "b",
                DataType::Struct(vec![Field::new("d", DataType::Utf8, true)].into()),
                true,
            ),
        ]);
        let right_batch = RecordBatch::try_new(
            Arc::new(right_schema),
            vec![
                Arc::new(e_array.clone()),
                Arc::new(StructArray::from(vec![(
                    Arc::new(Field::new("d", DataType::Utf8, true)),
                    Arc::new(d_array.clone()) as ArrayRef,
                )])) as ArrayRef,
            ],
        )
        .unwrap();

        let merged_schema = Schema::new(vec![
            Field::new("a", DataType::Int32, true),
            Field::new(
                "b",
                DataType::Struct(
                    vec![
                        Field::new("c", DataType::Int32, true),
                        Field::new("d", DataType::Utf8, true),
                    ]
                    .into(),
                ),
                true,
            ),
            Field::new("e", DataType::Int32, true),
        ]);
        let merged_batch = RecordBatch::try_new(
            Arc::new(merged_schema),
            vec![
                Arc::new(a_array) as ArrayRef,
                Arc::new(StructArray::from(vec![
                    (
                        Arc::new(Field::new("c", DataType::Int32, true)),
                        Arc::new(c_array) as ArrayRef,
                    ),
                    (
                        Arc::new(Field::new("d", DataType::Utf8, true)),
                        Arc::new(d_array) as ArrayRef,
                    ),
                ])) as ArrayRef,
                Arc::new(e_array) as ArrayRef,
            ],
        )
        .unwrap();

        let result = left_batch.merge(&right_batch).unwrap();
        assert_eq!(result, merged_batch);
    }

    #[test]
    fn test_merge_with_schema() {
        fn test_batch(names: &[&str], types: &[DataType]) -> (Schema, RecordBatch) {
            let fields: Fields = names
                .iter()
                .zip(types)
                .map(|(name, ty)| Field::new(name.to_string(), ty.clone(), false))
                .collect();
            let schema = Schema::new(vec![Field::new(
                "struct",
                DataType::Struct(fields.clone()),
                false,
            )]);
            let children = types.iter().map(new_empty_array).collect::<Vec<_>>();
            let batch = RecordBatch::try_new(
                Arc::new(schema.clone()),
                vec![Arc::new(StructArray::new(fields, children, None)) as ArrayRef],
            );
            (schema, batch.unwrap())
        }

        let (_, left_batch) = test_batch(&["a", "b"], &[DataType::Int32, DataType::Int64]);
        let (_, right_batch) = test_batch(&["c", "b"], &[DataType::Int32, DataType::Int64]);
        let (output_schema, _) = test_batch(
            &["b", "a", "c"],
            &[DataType::Int64, DataType::Int32, DataType::Int32],
        );

        // If we use merge_with_schema the schema is respected
        let merged = left_batch
            .merge_with_schema(&right_batch, &output_schema)
            .unwrap();
        assert_eq!(merged.schema().as_ref(), &output_schema);

        // If we use merge we get first-come first-serve based on the left batch
        let (naive_schema, _) = test_batch(
            &["a", "b", "c"],
            &[DataType::Int32, DataType::Int64, DataType::Int32],
        );
        let merged = left_batch.merge(&right_batch).unwrap();
        assert_eq!(merged.schema().as_ref(), &naive_schema);
    }

    #[test]
    fn test_merge_list_struct() {
        let x_field = Arc::new(Field::new("x", DataType::Int32, true));
        let y_field = Arc::new(Field::new("y", DataType::Int32, true));
        let x_struct_field = Arc::new(Field::new(
            "item",
            DataType::Struct(Fields::from(vec![x_field.clone()])),
            true,
        ));
        let y_struct_field = Arc::new(Field::new(
            "item",
            DataType::Struct(Fields::from(vec![y_field.clone()])),
            true,
        ));
        let both_struct_field = Arc::new(Field::new(
            "item",
            DataType::Struct(Fields::from(vec![x_field.clone(), y_field.clone()])),
            true,
        ));
        let left_schema = Schema::new(vec![Field::new(
            "list_struct",
            DataType::List(x_struct_field.clone()),
            true,
        )]);
        let right_schema = Schema::new(vec![Field::new(
            "list_struct",
            DataType::List(y_struct_field.clone()),
            true,
        )]);
        let both_schema = Schema::new(vec![Field::new(
            "list_struct",
            DataType::List(both_struct_field.clone()),
            true,
        )]);

        let x = Arc::new(Int32Array::from(vec![1]));
        let y = Arc::new(Int32Array::from(vec![2]));
        let x_struct = Arc::new(StructArray::new(
            Fields::from(vec![x_field.clone()]),
            vec![x.clone()],
            None,
        ));
        let y_struct = Arc::new(StructArray::new(
            Fields::from(vec![y_field.clone()]),
            vec![y.clone()],
            None,
        ));
        let both_struct = Arc::new(StructArray::new(
            Fields::from(vec![x_field.clone(), y_field.clone()]),
            vec![x.clone(), y],
            None,
        ));
        let both_null_struct = Arc::new(StructArray::new(
            Fields::from(vec![x_field, y_field]),
            vec![x, Arc::new(new_null_array(&DataType::Int32, 1))],
            None,
        ));
        let offsets = OffsetBuffer::from_lengths([1]);
        let x_s_list = ListArray::new(x_struct_field, offsets.clone(), x_struct, None);
        let y_s_list = ListArray::new(y_struct_field, offsets.clone(), y_struct, None);
        let both_list = ListArray::new(
            both_struct_field.clone(),
            offsets.clone(),
            both_struct,
            None,
        );
        let both_null_list = ListArray::new(both_struct_field, offsets, both_null_struct, None);
        let x_batch =
            RecordBatch::try_new(Arc::new(left_schema), vec![Arc::new(x_s_list)]).unwrap();
        let y_batch = RecordBatch::try_new(
            Arc::new(right_schema.clone()),
            vec![Arc::new(y_s_list.clone())],
        )
        .unwrap();
        let merged = x_batch.merge(&y_batch).unwrap();
        let expected =
            RecordBatch::try_new(Arc::new(both_schema.clone()), vec![Arc::new(both_list)]).unwrap();
        assert_eq!(merged, expected);

        let y_null_list = new_null_array(y_s_list.data_type(), 1);
        let y_null_batch =
            RecordBatch::try_new(Arc::new(right_schema), vec![Arc::new(y_null_list.clone())])
                .unwrap();
        let expected =
            RecordBatch::try_new(Arc::new(both_schema), vec![Arc::new(both_null_list)]).unwrap();
        let merged = x_batch.merge(&y_null_batch).unwrap();
        assert_eq!(merged, expected);
    }

    #[test]
    fn test_byte_width_opt() {
        assert_eq!(DataType::Int32.byte_width_opt(), Some(4));
        assert_eq!(DataType::Int64.byte_width_opt(), Some(8));
        assert_eq!(DataType::Float32.byte_width_opt(), Some(4));
        assert_eq!(DataType::Float64.byte_width_opt(), Some(8));
        assert_eq!(DataType::Utf8.byte_width_opt(), None);
        assert_eq!(DataType::Binary.byte_width_opt(), None);
        assert_eq!(
            DataType::List(Arc::new(Field::new("item", DataType::Int32, true))).byte_width_opt(),
            None
        );
        assert_eq!(
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Int32, true)), 3)
                .byte_width_opt(),
            Some(12)
        );
        assert_eq!(
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Int32, true)), 4)
                .byte_width_opt(),
            Some(16)
        );
        assert_eq!(
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Utf8, true)), 5)
                .byte_width_opt(),
            None
        );
    }

    #[test]
    fn test_take_record_batch() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Int32, true),
            Field::new("b", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int32Array::from_iter_values(0..20)),
                Arc::new(StringArray::from_iter_values(
                    (0..20).map(|i| format!("str-{}", i)),
                )),
            ],
        )
        .unwrap();
        let taken = batch.take(&(vec![1_u32, 5_u32, 10_u32].into())).unwrap();
        assert_eq!(
            taken,
            RecordBatch::try_new(
                schema,
                vec![
                    Arc::new(Int32Array::from(vec![1, 5, 10])),
                    Arc::new(StringArray::from(vec!["str-1", "str-5", "str-10"])),
                ],
            )
            .unwrap()
        )
    }

    #[test]
    fn test_schema_project_by_schema() {
        let metadata = [("key".to_string(), "value".to_string())];
        let schema = Arc::new(
            Schema::new(vec![
                Field::new("a", DataType::Int32, true),
                Field::new("b", DataType::Utf8, true),
            ])
            .with_metadata(metadata.clone().into()),
        );
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from_iter_values(0..20)),
                Arc::new(StringArray::from_iter_values(
                    (0..20).map(|i| format!("str-{}", i)),
                )),
            ],
        )
        .unwrap();

        // Empty schema
        let empty_schema = Schema::empty();
        let empty_projected = batch.project_by_schema(&empty_schema).unwrap();
        let expected_schema = empty_schema.with_metadata(metadata.clone().into());
        assert_eq!(
            empty_projected,
            RecordBatch::from(StructArray::new_empty_fields(batch.num_rows(), None))
                .with_schema(Arc::new(expected_schema))
                .unwrap()
        );

        // Re-ordered schema
        let reordered_schema = Schema::new(vec![
            Field::new("b", DataType::Utf8, true),
            Field::new("a", DataType::Int32, true),
        ]);
        let reordered_projected = batch.project_by_schema(&reordered_schema).unwrap();
        let expected_schema = Arc::new(reordered_schema.with_metadata(metadata.clone().into()));
        assert_eq!(
            reordered_projected,
            RecordBatch::try_new(
                expected_schema,
                vec![
                    Arc::new(StringArray::from_iter_values(
                        (0..20).map(|i| format!("str-{}", i)),
                    )),
                    Arc::new(Int32Array::from_iter_values(0..20)),
                ],
            )
            .unwrap()
        );

        // Sub schema
        let sub_schema = Schema::new(vec![Field::new("a", DataType::Int32, true)]);
        let sub_projected = batch.project_by_schema(&sub_schema).unwrap();
        let expected_schema = Arc::new(sub_schema.with_metadata(metadata.into()));
        assert_eq!(
            sub_projected,
            RecordBatch::try_new(
                expected_schema,
                vec![Arc::new(Int32Array::from_iter_values(0..20))],
            )
            .unwrap()
        );
    }
}
