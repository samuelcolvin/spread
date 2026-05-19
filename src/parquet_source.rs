use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    fs::File,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context as _, Result, anyhow};
use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    RecordBatch, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow_cast::display::array_value_to_string;
use arrow_schema::{DataType, SchemaRef};
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder, RowSelection,
    RowSelector,
};

use crate::workbook::{
    CellData, CellRawValue, CellStyle, DEFAULT_COLUMN_WIDTH, DEFAULT_ROW_HEIGHT, SheetData,
    SheetFreeze, SheetRowLayout, SheetSource,
};

const PARQUET_ROW_CACHE_CAPACITY: usize = 2_048;
const PARQUET_ROW_WINDOW_SIZE: usize = 256;

#[derive(Debug)]
pub(crate) struct ParquetSheetSource {
    path: PathBuf,
    reader_metadata: ArrowReaderMetadata,
    schema: SchemaRef,
    data_row_count: usize,
    row_groups: Vec<ParquetRowGroup>,
    cache: Mutex<ParquetRowCache>,
}

#[derive(Debug, Clone, Copy)]
struct ParquetRowGroup {
    start_row: usize,
    row_count: usize,
}

#[derive(Debug)]
struct ParquetRowCache {
    rows: HashMap<usize, Vec<CellData>>,
    order: VecDeque<usize>,
    capacity: usize,
}

pub(crate) fn load_parquet_sheet(path: &Path) -> Result<SheetData> {
    let source = ParquetSheetSource::new(path)?;
    let sheet_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    Ok(SheetData::from_source_with_freeze(
        sheet_name,
        source,
        SheetFreeze {
            rows: 1,
            columns: 0,
        },
    ))
}

impl ParquetSheetSource {
    fn new(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("failed to open Parquet file {}", path.display()))?;
        let reader_metadata = ArrowReaderMetadata::load(&file, ArrowReaderOptions::default())
            .with_context(|| format!("failed to read Parquet metadata from {}", path.display()))?;
        let metadata = reader_metadata.metadata();
        let schema = reader_metadata.schema().clone();
        let data_row_count = usize::try_from(metadata.file_metadata().num_rows())
            .with_context(|| format!("Parquet file {} has too many rows", path.display()))?;
        let mut row_groups = Vec::with_capacity(metadata.num_row_groups());
        let mut start_row = 0;

        for row_group_ix in 0..metadata.num_row_groups() {
            let row_count = usize::try_from(metadata.row_group(row_group_ix).num_rows())
                .with_context(|| format!("Parquet row group {row_group_ix} has too many rows"))?;
            row_groups.push(ParquetRowGroup {
                start_row,
                row_count,
            });
            start_row += row_count;
        }

        Ok(Self {
            path: path.to_owned(),
            reader_metadata,
            schema,
            data_row_count,
            row_groups,
            cache: Mutex::new(ParquetRowCache::new(PARQUET_ROW_CACHE_CAPACITY)),
        })
    }

    fn load_data_row(&self, data_row: usize) -> Result<Vec<CellData>> {
        if let Some(cached) = self
            .cache
            .lock()
            .expect("Parquet cache poisoned")
            .get(data_row)
        {
            return Ok(cached);
        }

        self.load_data_window(data_row)?;
        self.cache
            .lock()
            .expect("Parquet cache poisoned")
            .get(data_row)
            .ok_or_else(|| anyhow!("Parquet row {data_row} was not loaded"))
    }

    fn load_data_window(&self, data_row: usize) -> Result<()> {
        let (row_group_ix, row_group) = self
            .row_group_for_data_row(data_row)
            .ok_or_else(|| anyhow!("Parquet row {data_row} is out of bounds"))?;
        let row_in_group = data_row - row_group.start_row;
        let window_start = (row_in_group / PARQUET_ROW_WINDOW_SIZE) * PARQUET_ROW_WINDOW_SIZE;
        let window_len = PARQUET_ROW_WINDOW_SIZE.min(row_group.row_count - window_start);
        let selection = row_selection_for_range(window_start, window_len, row_group.row_count);
        let file = File::open(&self.path)
            .with_context(|| format!("failed to open Parquet file {}", self.path.display()))?;
        let mut reader =
            ParquetRecordBatchReaderBuilder::new_with_metadata(file, self.reader_metadata.clone())
                .with_row_groups(vec![row_group_ix])
                .with_row_selection(selection)
                .with_batch_size(window_len)
                .build()
                .with_context(|| format!("failed to read Parquet row group {row_group_ix}"))?;
        let mut loaded_rows = Vec::with_capacity(window_len);
        let mut absolute_row = row_group.start_row + window_start;

        for batch in &mut reader {
            let batch = batch?;
            for batch_row in 0..batch.num_rows() {
                loaded_rows.push((absolute_row, record_batch_row_to_cells(&batch, batch_row)));
                absolute_row += 1;
            }
        }

        self.cache
            .lock()
            .expect("Parquet cache poisoned")
            .insert_many(loaded_rows);
        Ok(())
    }

    fn row_group_for_data_row(&self, data_row: usize) -> Option<(usize, ParquetRowGroup)> {
        let row_group_ix = self
            .row_groups
            .partition_point(|row_group| row_group.start_row <= data_row)
            .saturating_sub(1);
        let row_group = *self.row_groups.get(row_group_ix)?;
        (data_row < row_group.start_row + row_group.row_count).then_some((row_group_ix, row_group))
    }

    fn header_cell(&self, col: usize) -> CellData {
        self.schema
            .fields()
            .get(col)
            .map(|field| CellData {
                value: field.name().to_owned(),
                raw_value: CellRawValue::Text,
                style: CellStyle {
                    bold: true,
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap_or_default()
    }
}

impl SheetSource for ParquetSheetSource {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn row_count(&self) -> usize {
        if self.col_count() == 0 {
            self.data_row_count
        } else {
            self.data_row_count.saturating_add(1)
        }
    }

    fn col_count(&self) -> usize {
        self.schema.fields().len()
    }

    fn cell_data(&self, row: usize, col: usize) -> CellData {
        if row == 0 && self.col_count() > 0 {
            return self.header_cell(col);
        }

        let data_row = if self.col_count() > 0 { row - 1 } else { row };
        self.load_data_row(data_row)
            .ok()
            .and_then(|row_data| row_data.get(col).cloned())
            .unwrap_or_default()
    }

    fn column_width(&self, _: usize) -> f32 {
        DEFAULT_COLUMN_WIDTH
    }

    fn row_height(&self, _: usize) -> f32 {
        DEFAULT_ROW_HEIGHT
    }

    fn row_layout(&self) -> SheetRowLayout {
        SheetRowLayout::Uniform {
            row_count: self.row_count(),
            height: DEFAULT_ROW_HEIGHT,
        }
    }

    fn is_fully_loaded(&self) -> bool {
        false
    }

    fn supports_full_range_operations(&self) -> bool {
        true
    }
}

impl ParquetRowCache {
    fn new(capacity: usize) -> Self {
        Self {
            rows: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    fn get(&mut self, row: usize) -> Option<Vec<CellData>> {
        self.rows.get(&row).cloned()
    }

    fn insert(&mut self, row: usize, row_data: Vec<CellData>) {
        if let std::collections::hash_map::Entry::Occupied(mut entry) = self.rows.entry(row) {
            entry.insert(row_data);
            return;
        }

        self.rows.insert(row, row_data);
        self.order.push_back(row);

        while self.rows.len() > self.capacity {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.rows.remove(&evicted);
        }
    }

    fn insert_many(&mut self, rows: Vec<(usize, Vec<CellData>)>) {
        for (row, row_data) in rows {
            self.insert(row, row_data);
        }
    }
}

fn row_selection_for_range(
    start_row: usize,
    row_count: usize,
    row_group_len: usize,
) -> RowSelection {
    let mut selectors = Vec::new();
    if start_row > 0 {
        selectors.push(RowSelector::skip(start_row));
    }
    if row_count > 0 {
        selectors.push(RowSelector::select(row_count));
    }
    let remaining = row_group_len.saturating_sub(start_row + row_count);
    if remaining > 0 {
        selectors.push(RowSelector::skip(remaining));
    }
    selectors.into()
}

fn record_batch_row_to_cells(batch: &RecordBatch, row: usize) -> Vec<CellData> {
    batch
        .columns()
        .iter()
        .map(|column| array_value_to_cell(column.as_ref(), row))
        .collect()
}

fn array_value_to_cell(array: &dyn Array, row: usize) -> CellData {
    if array.is_null(row) {
        return CellData::default();
    }

    let value =
        array_value_to_string(array, row).unwrap_or_else(|error| format!("<error: {error}>"));
    let raw_value = arrow_raw_value(array, row).unwrap_or(CellRawValue::Text);

    CellData {
        value,
        raw_value,
        ..Default::default()
    }
}

fn arrow_raw_value(array: &dyn Array, row: usize) -> Option<CellRawValue> {
    macro_rules! numeric_value {
        ($array_ty:ty) => {
            array
                .as_any()
                .downcast_ref::<$array_ty>()
                .map(|array| CellRawValue::Number(array.value(row) as f64))
        };
    }
    macro_rules! numeric_value_from {
        ($array_ty:ty) => {
            array
                .as_any()
                .downcast_ref::<$array_ty>()
                .map(|array| CellRawValue::Number(f64::from(array.value(row))))
        };
    }

    match array.data_type() {
        DataType::Int8 => numeric_value_from!(Int8Array),
        DataType::Int16 => numeric_value_from!(Int16Array),
        DataType::Int32 => numeric_value_from!(Int32Array),
        DataType::Int64 => numeric_value!(Int64Array),
        DataType::UInt8 => numeric_value_from!(UInt8Array),
        DataType::UInt16 => numeric_value_from!(UInt16Array),
        DataType::UInt32 => numeric_value_from!(UInt32Array),
        DataType::UInt64 => numeric_value!(UInt64Array),
        DataType::Float32 => numeric_value_from!(Float32Array),
        DataType::Float64 => array
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(|array| CellRawValue::Number(array.value(row))),
        DataType::Boolean => array
            .as_any()
            .downcast_ref::<BooleanArray>()
            .map(|array| CellRawValue::Bool(array.value(row))),
        DataType::Date32 | DataType::Date64 | DataType::Timestamp(_, _) => {
            Some(CellRawValue::DateTime)
        }
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => Some(CellRawValue::Text),
        _ => None,
    }
}
