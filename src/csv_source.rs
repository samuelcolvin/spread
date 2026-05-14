use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    fs::File,
    io::{Seek as _, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context as _, Result};

use crate::workbook::{
    CellData, DEFAULT_COLUMN_WIDTH, DEFAULT_ROW_HEIGHT, SheetData, SheetRowLayout, SheetSource,
};

const CSV_ROW_CACHE_CAPACITY: usize = 512;

#[derive(Debug)]
pub(crate) struct CsvSheetSource {
    path: PathBuf,
    row_offsets: Vec<u64>,
    col_count: usize,
    cache: Mutex<CsvRowCache>,
}

#[derive(Debug)]
struct CsvRowCache {
    rows: HashMap<usize, Vec<CellData>>,
    order: VecDeque<usize>,
    capacity: usize,
}

pub(crate) fn load_csv_sheet(path: &Path) -> Result<SheetData> {
    let source = CsvSheetSource::new(path)?;
    Ok(SheetData::from_source(None, source))
}

impl CsvSheetSource {
    fn new(path: &Path) -> Result<Self> {
        let mut reader = csv_reader(File::open(path)?);
        let mut record = csv::StringRecord::new();
        let mut row_offsets = Vec::new();
        let mut col_count = 0;

        while reader
            .read_record(&mut record)
            .with_context(|| format!("failed to read CSV record from {}", path.display()))?
        {
            let offset = record
                .position()
                .map_or_else(|| reader.position().byte(), csv::Position::byte);
            row_offsets.push(offset);
            col_count = col_count.max(record.len());
        }

        Ok(Self {
            path: path.to_owned(),
            row_offsets,
            col_count,
            cache: Mutex::new(CsvRowCache::new(CSV_ROW_CACHE_CAPACITY)),
        })
    }

    fn load_row(&self, row: usize) -> Result<Vec<CellData>> {
        if let Some(cached) = self.cache.lock().expect("CSV cache poisoned").get(row) {
            return Ok(cached);
        }

        let Some(offset) = self.row_offsets.get(row).copied() else {
            return Ok(Vec::new());
        };

        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open CSV file {}", self.path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("failed to seek CSV file {}", self.path.display()))?;

        let mut reader = csv_reader(file);
        let mut record = csv::StringRecord::new();
        reader
            .read_record(&mut record)
            .with_context(|| format!("failed to read CSV record from {}", self.path.display()))?;
        let row_data = record
            .iter()
            .map(|value| CellData {
                value: value.to_owned(),
                ..Default::default()
            })
            .collect::<Vec<_>>();

        self.cache
            .lock()
            .expect("CSV cache poisoned")
            .insert(row, row_data.clone());
        Ok(row_data)
    }
}

impl SheetSource for CsvSheetSource {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn row_count(&self) -> usize {
        self.row_offsets.len()
    }

    fn col_count(&self) -> usize {
        self.col_count
    }

    fn cell_data(&self, row: usize, col: usize) -> CellData {
        self.load_row(row)
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

impl CsvRowCache {
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
}

fn csv_reader<R: std::io::Read>(reader: R) -> csv::Reader<R> {
    csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_reader(reader)
}
