use std::{
    any::Any,
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io::{Seek as _, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context as _, Result};

use crate::workbook::{
    CellData, DEFAULT_COLUMN_WIDTH, DEFAULT_ROW_HEIGHT, SheetData, SheetFreeze, SheetRowLayout,
    SheetSource,
};

const CSV_ROW_CACHE_CAPACITY: usize = 512;
const CSV_HEADER_SAMPLE_DATA_ROWS: usize = 20;

#[derive(Debug)]
pub(crate) struct CsvSheetSource {
    path: PathBuf,
    row_offsets: Vec<u64>,
    col_count: usize,
    header_detection: CsvHeaderDetection,
    cache: Mutex<CsvRowCache>,
}

#[derive(Debug)]
struct CsvRowCache {
    rows: HashMap<usize, Vec<CellData>>,
    order: VecDeque<usize>,
    capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsvHeaderDetection {
    Header,
    NoHeader,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsvCellKind {
    Empty,
    Bool,
    Int,
    Float,
    Date,
    Text,
}

pub(crate) fn load_csv_sheet(path: &Path) -> Result<SheetData> {
    let source = CsvSheetSource::new(path)?;
    let freeze = match source.header_detection {
        CsvHeaderDetection::Header => SheetFreeze {
            rows: 1,
            columns: 0,
        },
        CsvHeaderDetection::NoHeader | CsvHeaderDetection::Unknown => SheetFreeze::default(),
    };
    Ok(SheetData::from_source_with_freeze(None, source, freeze))
}

impl CsvSheetSource {
    fn new(path: &Path) -> Result<Self> {
        let mut reader = csv_reader(File::open(path)?);
        let mut record = csv::StringRecord::new();
        let mut row_offsets = Vec::new();
        let mut header_sample = Vec::with_capacity(CSV_HEADER_SAMPLE_DATA_ROWS + 1);
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
            if header_sample.len() < CSV_HEADER_SAMPLE_DATA_ROWS + 1 {
                header_sample.push(record.iter().map(str::to_owned).collect());
            }
        }
        let header_detection = detect_csv_header(&header_sample);

        Ok(Self {
            path: path.to_owned(),
            row_offsets,
            col_count,
            header_detection,
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

fn detect_csv_header(rows: &[Vec<String>]) -> CsvHeaderDetection {
    let Some(candidate_header) = rows.first() else {
        return CsvHeaderDetection::Unknown;
    };
    let data_rows = rows.get(1..).unwrap_or_default();
    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);

    if col_count < 2 || non_empty_row_count(data_rows) < 2 {
        return CsvHeaderDetection::Unknown;
    }

    if has_duplicate_non_empty_value(candidate_header) {
        return CsvHeaderDetection::NoHeader;
    }

    let column_types = infer_csv_column_types(data_rows, col_count);
    let has_strong_column = column_types.iter().any(Option::is_some);
    if !has_strong_column {
        return CsvHeaderDetection::Unknown;
    }

    let header_kinds = (0..col_count)
        .map(|col_ix| csv_cell_kind(cell_value(candidate_header, col_ix)))
        .collect::<Vec<_>>();

    if !header_kinds.contains(&CsvCellKind::Text) {
        return CsvHeaderDetection::NoHeader;
    }

    if header_kinds
        .iter()
        .any(|kind| matches!(kind, CsvCellKind::Float | CsvCellKind::Date))
    {
        return CsvHeaderDetection::NoHeader;
    }

    let mut has_positive_column = false;
    for (col_ix, column_type) in column_types.iter().enumerate() {
        let Some(column_type) = column_type else {
            continue;
        };
        if header_kinds[col_ix] == *column_type {
            return CsvHeaderDetection::NoHeader;
        }
        has_positive_column = true;
    }

    if has_positive_column {
        CsvHeaderDetection::Header
    } else {
        CsvHeaderDetection::Unknown
    }
}

fn non_empty_row_count(rows: &[Vec<String>]) -> usize {
    rows.iter()
        .filter(|row| row.iter().any(|value| !value.trim().is_empty()))
        .count()
}

fn has_duplicate_non_empty_value(row: &[String]) -> bool {
    let mut seen = HashSet::new();
    row.iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .any(|value| !seen.insert(value))
}

fn infer_csv_column_types(rows: &[Vec<String>], col_count: usize) -> Vec<Option<CsvCellKind>> {
    (0..col_count)
        .map(|col_ix| infer_csv_column_type(rows, col_ix))
        .collect()
}

fn infer_csv_column_type(rows: &[Vec<String>], col_ix: usize) -> Option<CsvCellKind> {
    let mut inferred_type = None;
    let mut non_empty_count = 0;

    for row in rows {
        let kind = csv_cell_kind(cell_value(row, col_ix));
        if kind == CsvCellKind::Empty {
            continue;
        }
        if !kind.is_strong() {
            return None;
        }
        non_empty_count += 1;
        match inferred_type {
            Some(existing_type) if existing_type != kind => return None,
            Some(_) => {}
            None => inferred_type = Some(kind),
        }
    }

    if non_empty_count >= 2 {
        inferred_type
    } else {
        None
    }
}

fn csv_cell_kind(value: &str) -> CsvCellKind {
    let value = value.trim();
    if value.is_empty() {
        return CsvCellKind::Empty;
    }
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
        return CsvCellKind::Bool;
    }
    if is_csv_int(value) {
        return CsvCellKind::Int;
    }
    if value.parse::<f64>().is_ok() {
        return CsvCellKind::Float;
    }
    if is_iso_date_prefix(value) {
        return CsvCellKind::Date;
    }
    CsvCellKind::Text
}

fn is_csv_int(value: &str) -> bool {
    let digits = value
        .strip_prefix(['+', '-'])
        .filter(|digits| !digits.is_empty())
        .unwrap_or(value);
    digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_iso_date_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn cell_value(row: &[String], col_ix: usize) -> &str {
    row.get(col_ix).map_or("", String::as_str)
}

impl CsvCellKind {
    fn is_strong(self) -> bool {
        matches!(
            self,
            CsvCellKind::Bool | CsvCellKind::Int | CsvCellKind::Float | CsvCellKind::Date
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn detects_header_csv() {
        let rows = csv_rows(&[
            &["month", "customers", "revenue"],
            &["2025-01-01", "27", "1200"],
            &["2025-02-01", "38", "1500"],
        ]);

        assert_eq!(detect_csv_header(&rows), CsvHeaderDetection::Header);
    }

    #[test]
    fn does_not_detect_numeric_data_as_header() {
        let rows = csv_rows(&[
            &["2025-01-01", "27", "1200"],
            &["2025-02-01", "38", "1500"],
            &["2025-03-01", "44", "1800"],
        ]);

        assert_eq!(detect_csv_header(&rows), CsvHeaderDetection::NoHeader);
    }

    #[test]
    fn leaves_all_text_csv_unknown() {
        let rows = csv_rows(&[&["name", "city"], &["Alice", "London"], &["Bob", "Paris"]]);

        assert_eq!(detect_csv_header(&rows), CsvHeaderDetection::Unknown);
    }

    #[test]
    fn rejects_duplicate_header_row_data() {
        let rows = csv_rows(&[&["id", "id"], &["1", "10"], &["2", "20"]]);

        assert_eq!(detect_csv_header(&rows), CsvHeaderDetection::NoHeader);
    }

    #[test]
    fn leaves_weak_leading_null_sample_unknown() {
        let rows = csv_rows(&[&["name", "count"], &["", ""], &["Alice", "1"]]);

        assert_eq!(detect_csv_header(&rows), CsvHeaderDetection::Unknown);
    }

    #[test]
    fn freezes_first_row_for_detected_csv_header() {
        let path = temp_csv_path("header");
        fs::write(
            &path,
            "month,customers,revenue\n2025-01-01,27,1200\n2025-02-01,38,1500\n",
        )
        .expect("CSV fixture should be written");

        let sheet = load_csv_sheet(&path).expect("CSV sheet should load");
        let _ = fs::remove_file(&path);

        assert_eq!(
            sheet.freeze(),
            SheetFreeze {
                rows: 1,
                columns: 0,
            }
        );
    }

    fn csv_rows(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|value| (*value).to_owned()).collect())
            .collect()
    }

    fn temp_csv_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after UNIX epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("spread-{name}-{}-{nanos}.csv", std::process::id()))
    }
}
