use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use calamine::{Data, ExcelDateTime, Range, Reader as _, Xlsx, open_workbook};
use quick_xml::{Reader as XmlReader, events::Event};
use serde::Serialize;
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub(crate) struct WorkbookData {
    path: PathBuf,
    sheets: Vec<SheetData>,
}

#[derive(Debug, Clone)]
pub(crate) struct SheetData {
    name: String,
    rows: Vec<Vec<CellData>>,
    column_widths: Vec<f32>,
    row_heights: Vec<f32>,
    row_offsets: Vec<f32>,
    default_column_width: f32,
    default_row_height: f32,
    row_count: usize,
    col_count: usize,
}

impl WorkbookData {
    fn new(path: PathBuf, sheets: Vec<SheetData>) -> Self {
        Self { path, sheets }
    }

    pub(crate) fn sheet_count(&self) -> usize {
        self.sheets.len()
    }

    pub(crate) fn sheet(&self, sheet_ix: usize) -> &SheetData {
        self.sheets
            .get(sheet_ix)
            .or_else(|| self.sheets.first())
            .expect("workbook should contain at least one sheet")
    }

    pub(crate) fn sheet_name(&self, sheet_ix: usize) -> &str {
        self.sheet(sheet_ix).name()
    }

    pub(crate) fn row_count(&self) -> usize {
        self.sheet(0).row_count()
    }

    #[cfg(test)]
    pub(crate) fn col_count(&self) -> usize {
        self.sheet(0).col_count()
    }

    #[cfg(test)]
    pub(crate) fn cell(&self, row: usize, col: usize) -> &str {
        self.sheet(0).cell(row, col)
    }

    #[cfg(test)]
    pub(crate) fn cell_data(&self, row: usize, col: usize) -> CellData {
        self.sheet(0).cell_data(row, col)
    }

    #[cfg(test)]
    pub(crate) fn column_width(&self, col: usize) -> f32 {
        self.sheet(0).column_width(col)
    }

    #[cfg(test)]
    pub(crate) fn row_height(&self, row: usize) -> f32 {
        self.sheet(0).row_height(row)
    }

    #[cfg(test)]
    pub(crate) fn sheet_height(&self) -> f32 {
        self.sheet(0).sheet_height()
    }

    pub(crate) fn display_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("spreadsheet")
            .to_owned()
    }

    pub(crate) fn inspect(&self) -> InspectedSheet {
        self.sheet(0).inspect()
    }
}

impl SheetData {
    fn new(
        sheet_name: Option<String>,
        rows: Vec<Vec<CellData>>,
        column_widths: Vec<f32>,
        row_heights: Vec<f32>,
        default_column_width: f32,
        default_row_height: f32,
    ) -> Self {
        let row_count = rows.len();
        let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
        let row_offsets = build_row_offsets(row_count, &row_heights, default_row_height);

        Self {
            name: sheet_name.unwrap_or_else(|| "Sheet1".to_owned()),
            rows,
            column_widths,
            row_heights,
            row_offsets,
            default_column_width,
            default_row_height,
            row_count,
            col_count,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn row_count(&self) -> usize {
        self.row_count
    }

    pub(crate) fn col_count(&self) -> usize {
        self.col_count
    }

    #[cfg(test)]
    pub(crate) fn cell(&self, row: usize, col: usize) -> &str {
        self.rows
            .get(row)
            .and_then(|columns| columns.get(col))
            .map_or("", |cell| cell.value.as_str())
    }

    pub(crate) fn cell_data(&self, row: usize, col: usize) -> CellData {
        self.rows
            .get(row)
            .and_then(|columns| columns.get(col))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn column_width(&self, col: usize) -> f32 {
        self.column_widths
            .get(col)
            .copied()
            .filter(|width| *width > 0.0)
            .unwrap_or(self.default_column_width)
    }

    pub(crate) fn row_height(&self, row: usize) -> f32 {
        self.row_heights
            .get(row)
            .copied()
            .filter(|height| *height > 0.0)
            .unwrap_or(self.default_row_height)
    }

    pub(crate) fn sheet_width(&self) -> f32 {
        (0..self.col_count).map(|col| self.column_width(col)).sum()
    }

    pub(crate) fn sheet_height(&self) -> f32 {
        self.row_offsets.last().copied().unwrap_or(0.0)
    }

    pub(crate) fn inspect(&self) -> InspectedSheet {
        let mut cells = Vec::with_capacity(self.row_count * self.col_count);

        for row_ix in 0..self.row_count {
            for col_ix in 0..self.col_count {
                let cell = self.cell_data(row_ix, col_ix);
                cells.push(InspectedCell {
                    x: column_name(col_ix),
                    y: row_ix + 1,
                    display_value: cell.value,
                    fg: color_hex(cell.style.text_color.unwrap_or(0x20_21_24)),
                    bg: color_hex(cell.style.background_color.unwrap_or(0xff_ff_ff)),
                    bold: cell.style.bold,
                    width: round2(self.column_width(col_ix)),
                    height: round2(self.row_height(row_ix)),
                });
            }
        }

        InspectedSheet {
            sheet: self.name.clone(),
            rows: self.row_count,
            cols: self.col_count,
            cells,
        }
    }

    pub(crate) fn summary_for_range(&self, range: CellRange) -> SelectionSummary {
        let mut summary = SelectionSummary {
            selected_cells: range.cell_count(),
            ..Default::default()
        };

        let range = range.normalized();
        for row_ix in range.start.row..=range.end.row {
            for col_ix in range.start.col..=range.end.col {
                if let Some(value) = self.numeric_value(row_ix, col_ix) {
                    summary.numeric_cells += 1;
                    summary.sum += value;
                    summary.min = Some(summary.min.map_or(value, |min| min.min(value)));
                    summary.max = Some(summary.max.map_or(value, |max| max.max(value)));
                }
            }
        }

        summary
    }

    fn numeric_value(&self, row: usize, col: usize) -> Option<f64> {
        self.rows
            .get(row)
            .and_then(|columns| columns.get(col))
            .and_then(|cell| match cell.raw_value {
                CellRawValue::Number(value) => Some(value),
                _ => None,
            })
    }

    pub(crate) fn row_offset_for_scroll_position(&self, scroll_position: f32) -> (usize, f32) {
        let scroll_position = scroll_position.clamp(0.0, self.sheet_height());
        let row_ix = self
            .row_offsets
            .partition_point(|offset| *offset <= scroll_position)
            .saturating_sub(1)
            .min(self.row_count);

        if row_ix >= self.row_count {
            return (self.row_count, 0.0);
        }

        (row_ix, scroll_position - self.row_offsets[row_ix])
    }

    pub(crate) fn scroll_position_for_row_offset(&self, row: usize, offset: f32) -> f32 {
        if row >= self.row_count {
            return self.sheet_height();
        }

        let row_offset = offset.clamp(0.0, self.row_height(row));
        self.row_offsets[row] + row_offset
    }
}

pub(crate) const DEFAULT_COLUMN_WIDTH: f32 = 120.0;
pub(crate) const DEFAULT_ROW_HEIGHT: f32 = 24.0;

fn build_row_offsets(row_count: usize, row_heights: &[f32], default_row_height: f32) -> Vec<f32> {
    let mut offsets = Vec::with_capacity(row_count + 1);
    let mut current = 0.0;
    offsets.push(current);

    for row_ix in 0..row_count {
        let height = row_heights
            .get(row_ix)
            .copied()
            .filter(|height| *height > 0.0)
            .unwrap_or(default_row_height);
        current += height;
        offsets.push(current);
    }

    offsets
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CellData {
    pub(crate) value: String,
    pub(crate) formula: Option<String>,
    pub(crate) raw_value: CellRawValue,
    pub(crate) style: CellStyle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) enum CellRawValue {
    #[default]
    Empty,
    Number(f64),
    Bool(bool),
    DateTime,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellCoord {
    pub(crate) row: usize,
    pub(crate) col: usize,
}

impl CellCoord {
    pub(crate) const fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellRange {
    pub(crate) start: CellCoord,
    pub(crate) end: CellCoord,
}

impl CellRange {
    pub(crate) const fn single(coord: CellCoord) -> Self {
        Self {
            start: coord,
            end: coord,
        }
    }

    pub(crate) const fn new(start: CellCoord, end: CellCoord) -> Self {
        Self { start, end }
    }

    pub(crate) fn normalized(self) -> Self {
        Self {
            start: CellCoord {
                row: self.start.row.min(self.end.row),
                col: self.start.col.min(self.end.col),
            },
            end: CellCoord {
                row: self.start.row.max(self.end.row),
                col: self.start.col.max(self.end.col),
            },
        }
    }

    pub(crate) fn contains(self, row: usize, col: usize) -> bool {
        let normalized = self.normalized();
        row >= normalized.start.row
            && row <= normalized.end.row
            && col >= normalized.start.col
            && col <= normalized.end.col
    }

    pub(crate) fn intersects_row(self, row: usize) -> bool {
        let normalized = self.normalized();
        row >= normalized.start.row && row <= normalized.end.row
    }

    pub(crate) fn intersects_col(self, col: usize) -> bool {
        let normalized = self.normalized();
        col >= normalized.start.col && col <= normalized.end.col
    }

    pub(crate) fn edge_sides(self, row: usize, col: usize) -> SelectionEdgeSides {
        let normalized = self.normalized();
        if !self.contains(row, col) {
            return SelectionEdgeSides::default();
        }

        let mut edges = SelectionEdgeSides::default();
        if row == normalized.start.row {
            edges.insert_top();
        }
        if col == normalized.end.col {
            edges.insert_right();
        }
        if row == normalized.end.row {
            edges.insert_bottom();
        }
        if col == normalized.start.col {
            edges.insert_left();
        }
        edges
    }

    pub(crate) fn cell_count(self) -> usize {
        let normalized = self.normalized();
        (normalized.end.row - normalized.start.row + 1)
            * (normalized.end.col - normalized.start.col + 1)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SelectionEdgeSides(u8);

impl SelectionEdgeSides {
    const TOP: u8 = 1;
    const RIGHT: u8 = 1 << 1;
    const BOTTOM: u8 = 1 << 2;
    const LEFT: u8 = 1 << 3;

    fn insert_top(&mut self) {
        self.0 |= Self::TOP;
    }

    fn insert_right(&mut self) {
        self.0 |= Self::RIGHT;
    }

    fn insert_bottom(&mut self) {
        self.0 |= Self::BOTTOM;
    }

    fn insert_left(&mut self) {
        self.0 |= Self::LEFT;
    }

    pub(crate) fn top(self) -> bool {
        self.0 & Self::TOP != 0
    }

    pub(crate) fn right(self) -> bool {
        self.0 & Self::RIGHT != 0
    }

    pub(crate) fn bottom(self) -> bool {
        self.0 & Self::BOTTOM != 0
    }

    pub(crate) fn left(self) -> bool {
        self.0 & Self::LEFT != 0
    }

    pub(crate) fn any(self) -> bool {
        self.0 != 0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SelectionSummary {
    pub(crate) selected_cells: usize,
    pub(crate) numeric_cells: usize,
    pub(crate) sum: f64,
    pub(crate) min: Option<f64>,
    pub(crate) max: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CellStyle {
    pub(crate) bold: bool,
    pub(crate) background_color: Option<u32>,
    pub(crate) text_color: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectedSheet {
    sheet: String,
    rows: usize,
    cols: usize,
    cells: Vec<InspectedCell>,
}

#[derive(Debug, Serialize)]
struct InspectedCell {
    x: String,
    y: usize,
    display_value: String,
    fg: String,
    bg: String,
    bold: bool,
    width: f32,
    height: f32,
}

pub(crate) fn load_workbook(path: &Path) -> Result<WorkbookData> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("csv") => load_csv(path),
        Some("xlsx") => load_xlsx(path),
        Some(extension) => {
            bail!("unsupported file extension '.{extension}'; expected .csv or .xlsx")
        }
        None => bail!("unsupported file without extension; expected .csv or .xlsx"),
    }
}

fn load_csv(path: &Path) -> Result<WorkbookData> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_path(path)
        .with_context(|| format!("failed to open CSV file {}", path.display()))?;

    let mut rows = Vec::new();
    for record in reader.records() {
        let record =
            record.with_context(|| format!("failed to read CSV record from {}", path.display()))?;
        rows.push(
            record
                .iter()
                .map(|value| CellData {
                    value: value.to_owned(),
                    ..Default::default()
                })
                .collect(),
        );
    }

    Ok(WorkbookData::new(
        path.to_owned(),
        vec![SheetData::new(
            None,
            rows,
            Vec::new(),
            Vec::new(),
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        )],
    ))
}

fn load_xlsx(path: &Path) -> Result<WorkbookData> {
    let xlsx_metadata = XlsxMetadata::read(path)
        .with_context(|| format!("failed to read XLSX metadata from {}", path.display()))?;
    let mut workbook: Xlsx<_> = open_workbook(path)
        .with_context(|| format!("failed to open XLSX file {}", path.display()))?;
    let sheet_names = workbook.sheet_names().clone();
    if sheet_names.is_empty() {
        bail!("XLSX file {} does not contain any sheets", path.display());
    }

    let mut sheets = Vec::with_capacity(sheet_names.len());
    for sheet_name in sheet_names {
        let range = workbook.worksheet_range(&sheet_name).with_context(|| {
            format!(
                "failed to read sheet '{sheet_name}' from {}",
                path.display()
            )
        })?;
        let formulas = workbook
            .worksheet_formula(&sheet_name)
            .with_context(|| {
                format!(
                    "failed to read formulas from sheet '{sheet_name}' in {}",
                    path.display()
                )
            })
            .unwrap_or_else(|_| Range::default());
        let sheet_metadata = xlsx_metadata.sheet_metadata(&sheet_name);

        let rows = range
            .rows()
            .enumerate()
            .map(|(row_ix, row)| {
                row.iter()
                    .enumerate()
                    .map(|(col_ix, cell)| {
                        let style = sheet_metadata.cell_style(row_ix, col_ix);
                        CellData {
                            value: display_cell(cell, style.display_format.as_ref()),
                            formula: formula_at(&formulas, row_ix, col_ix),
                            raw_value: raw_value(cell),
                            style: style.visual_style.clone(),
                        }
                    })
                    .collect()
            })
            .collect();

        sheets.push(SheetData::new(
            Some(sheet_name.clone()),
            rows,
            sheet_metadata.column_widths,
            sheet_metadata.row_heights,
            sheet_metadata.default_column_width,
            sheet_metadata.default_row_height,
        ));
    }

    Ok(WorkbookData::new(path.to_owned(), sheets))
}

fn formula_at(formulas: &Range<String>, row_ix: usize, col_ix: usize) -> Option<String> {
    let row_ix = u32::try_from(row_ix).ok()?;
    let col_ix = u32::try_from(col_ix).ok()?;

    formulas
        .get_value((row_ix, col_ix))
        .filter(|formula| !formula.is_empty())
        .cloned()
}

fn raw_value(cell: &Data) -> CellRawValue {
    match cell {
        Data::Empty => CellRawValue::Empty,
        Data::Float(value) => CellRawValue::Number(*value),
        Data::Int(value) => CellRawValue::Number(*value as f64),
        Data::Bool(value) => CellRawValue::Bool(*value),
        Data::DateTime(_) | Data::DateTimeIso(_) | Data::DurationIso(_) => CellRawValue::DateTime,
        _ => CellRawValue::Text,
    }
}

fn display_cell(cell: &Data, format: Option<&CellDisplayFormat>) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::DateTime(value) => display_excel_datetime(*value),
        Data::Float(value) => format.map_or_else(
            || display_float(*value),
            |format| format.format_number(*value),
        ),
        Data::Int(value) => format.map_or_else(
            || value.to_string(),
            |format| format.format_number(*value as f64),
        ),
        value => value.to_string(),
    }
}

fn display_excel_datetime(value: ExcelDateTime) -> String {
    let (year, month, day, hour, min, sec, milli) = value.to_ymd_hms_milli();
    if hour == 0 && min == 0 && sec == 0 && milli == 0 {
        format!("{year:04}-{month:02}-{day:02}")
    } else if milli == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{milli:03}")
    }
}

fn display_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[derive(Debug, Clone)]
enum CellDisplayFormat {
    Currency { decimals: usize },
    Percentage { decimals: usize },
}

#[derive(Debug, Clone, Default)]
struct XlsxCellStyle {
    display_format: Option<CellDisplayFormat>,
    visual_style: CellStyle,
}

impl CellDisplayFormat {
    fn from_format_code(format_code: &str) -> Option<Self> {
        if is_percentage_format(format_code) {
            return Some(Self::Percentage {
                decimals: format_decimals(format_code),
            });
        }

        if is_dollar_format(format_code) {
            return Some(Self::Currency {
                decimals: format_decimals(format_code),
            });
        }

        None
    }

    fn format_number(&self, value: f64) -> String {
        match self {
            Self::Currency { decimals } => format_currency(value, *decimals),
            Self::Percentage { decimals } => format_percentage(value, *decimals),
        }
    }
}

fn is_percentage_format(format_code: &str) -> bool {
    format_code.contains('%')
}

fn is_dollar_format(format_code: &str) -> bool {
    let lowercase = format_code.to_ascii_lowercase();
    lowercase.contains('$') || lowercase.contains("[$$")
}

fn format_decimals(format_code: &str) -> usize {
    let first_section = format_code.split(';').next().unwrap_or(format_code);
    let Some(dot_ix) = first_section.find('.') else {
        return 0;
    };

    first_section[dot_ix + 1..]
        .chars()
        .take_while(|ch| *ch == '0' || *ch == '#')
        .count()
}

fn format_currency(value: f64, decimals: usize) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let value = value.abs();
    let formatted = format!("{value:.decimals$}");
    let (whole, fraction) = formatted.split_once('.').unwrap_or((&formatted, ""));
    let whole = add_grouping(whole);

    if decimals == 0 {
        format!("{sign}${whole}")
    } else {
        format!("{sign}${whole}.{fraction}")
    }
}

fn format_percentage(value: f64, decimals: usize) -> String {
    format!("{:.decimals$}%", value * 100.0)
}

fn add_grouping(value: &str) -> String {
    let mut grouped = String::new();
    for (ix, ch) in value.chars().rev().enumerate() {
        if ix > 0 && ix.is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped.chars().rev().collect()
}

#[derive(Debug, Clone, Default)]
struct XlsxStyle {
    display_format: Option<CellDisplayFormat>,
    visual_style: CellStyle,
}

#[derive(Debug, Default)]
struct XlsxMetadata {
    sheets: HashMap<String, SheetMetadata>,
}

impl XlsxMetadata {
    fn read(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("failed to open XLSX archive {}", path.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("failed to read XLSX archive {}", path.display()))?;

        let styles = read_styles(&mut archive)?;
        let sheet_paths = workbook_sheet_paths(&mut archive)?;
        let mut sheets = HashMap::new();

        for (sheet_name, sheet_path) in sheet_paths {
            let sheet_metadata = read_sheet_metadata(&mut archive, &sheet_path, &styles)?;
            sheets.insert(sheet_name, sheet_metadata);
        }

        Ok(Self { sheets })
    }

    fn sheet_metadata(&self, sheet_name: &str) -> SheetMetadata {
        self.sheets.get(sheet_name).cloned().unwrap_or_default()
    }

    #[cfg(test)]
    fn cell_style(&self, row: usize, col: usize) -> XlsxCellStyle {
        self.sheets
            .values()
            .next()
            .map(|sheet| sheet.cell_style(row, col))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
struct SheetMetadata {
    cell_styles: HashMap<(usize, usize), XlsxCellStyle>,
    column_widths: Vec<f32>,
    row_heights: Vec<f32>,
    default_column_width: f32,
    default_row_height: f32,
}

impl Default for SheetMetadata {
    fn default() -> Self {
        Self {
            cell_styles: HashMap::new(),
            column_widths: Vec::new(),
            row_heights: Vec::new(),
            default_column_width: DEFAULT_COLUMN_WIDTH,
            default_row_height: DEFAULT_ROW_HEIGHT,
        }
    }
}

impl SheetMetadata {
    fn cell_style(&self, row: usize, col: usize) -> XlsxCellStyle {
        self.cell_styles
            .get(&(row, col))
            .cloned()
            .unwrap_or_default()
    }
}

fn read_styles(archive: &mut ZipArchive<File>) -> Result<Vec<XlsxStyle>> {
    let Some(styles_xml) = read_zip_text(archive, "xl/styles.xml")? else {
        return Ok(Vec::new());
    };

    let mut reader = XmlReader::from_str(&styles_xml);
    reader.config_mut().trim_text(true);

    let mut number_formats = builtin_number_formats();
    let mut fonts = Vec::new();
    let mut fills = Vec::new();
    let mut styles = Vec::new();
    let mut in_fonts = false;
    let mut in_font = false;
    let mut font = CellStyle::default();
    let mut in_fills = false;
    let mut in_fill = false;
    let mut in_solid_pattern_fill = false;
    let mut fill_color = None;
    let mut in_cell_xfs = false;

    loop {
        match reader.read_event()? {
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"numFmt" =>
            {
                if let (Some(id), Some(code)) = (
                    attr_usize(&reader, &event, b"numFmtId")?,
                    attr_string(&reader, &event, b"formatCode")?,
                ) {
                    number_formats.insert(id, code);
                }
            }
            Event::Start(event) if event.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = true;
            }
            Event::End(event) if event.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = false;
            }
            Event::Start(event) if event.local_name().as_ref() == b"fonts" => {
                in_fonts = true;
            }
            Event::End(event) if event.local_name().as_ref() == b"fonts" => {
                in_fonts = false;
            }
            Event::Start(event) if in_fonts && event.local_name().as_ref() == b"font" => {
                in_font = true;
                font = CellStyle::default();
            }
            Event::End(event) if in_font && event.local_name().as_ref() == b"font" => {
                in_font = false;
                fonts.push(font.clone());
            }
            Event::Start(event) | Event::Empty(event)
                if in_font && event.local_name().as_ref() == b"b" =>
            {
                font.bold = true;
            }
            Event::Start(event) | Event::Empty(event)
                if in_font && event.local_name().as_ref() == b"color" =>
            {
                font.text_color = attr_rgb(&reader, &event)?;
            }
            Event::Start(event) if event.local_name().as_ref() == b"fills" => {
                in_fills = true;
            }
            Event::End(event) if event.local_name().as_ref() == b"fills" => {
                in_fills = false;
            }
            Event::Start(event) if in_fills && event.local_name().as_ref() == b"fill" => {
                in_fill = true;
                in_solid_pattern_fill = false;
                fill_color = None;
            }
            Event::End(event) if in_fill && event.local_name().as_ref() == b"fill" => {
                in_fill = false;
                in_solid_pattern_fill = false;
                fills.push(fill_color);
            }
            Event::Start(event) if in_fill && event.local_name().as_ref() == b"patternFill" => {
                in_solid_pattern_fill =
                    attr_string(&reader, &event, b"patternType")?.as_deref() == Some("solid");
            }
            Event::End(event) if in_fill && event.local_name().as_ref() == b"patternFill" => {
                in_solid_pattern_fill = false;
            }
            Event::Start(event) | Event::Empty(event)
                if in_solid_pattern_fill
                    && event.local_name().as_ref() == b"fgColor"
                    && fill_color.is_none() =>
            {
                fill_color = attr_rgb(&reader, &event)?;
            }
            Event::Start(event) | Event::Empty(event)
                if in_cell_xfs && event.local_name().as_ref() == b"xf" =>
            {
                let display_format = attr_usize(&reader, &event, b"numFmtId")?
                    .and_then(|id| number_formats.get(&id))
                    .and_then(|format_code| CellDisplayFormat::from_format_code(format_code));
                let font_style = attr_usize(&reader, &event, b"fontId")?
                    .and_then(|id| fonts.get(id))
                    .cloned()
                    .unwrap_or_default();
                let background_color = attr_usize(&reader, &event, b"fillId")?
                    .and_then(|id| fills.get(id))
                    .copied()
                    .flatten();

                styles.push(XlsxStyle {
                    display_format,
                    visual_style: CellStyle {
                        background_color,
                        ..font_style
                    },
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(styles)
}

fn builtin_number_formats() -> HashMap<usize, String> {
    [
        (5, "$#,##0_);($#,##0)"),
        (6, "$#,##0_);[Red]($#,##0)"),
        (7, "$#,##0.00_);($#,##0.00)"),
        (8, "$#,##0.00_);[Red]($#,##0.00)"),
        (9, "0%"),
        (10, "0.00%"),
        (44, "_($* #,##0.00_);_($* (#,##0.00);_($* \"-\"??_);_(@_)"),
    ]
    .into_iter()
    .map(|(id, code)| (id, code.to_owned()))
    .collect()
}

fn workbook_sheet_paths(archive: &mut ZipArchive<File>) -> Result<Vec<(String, String)>> {
    let workbook_xml = read_zip_text(archive, "xl/workbook.xml")?
        .ok_or_else(|| anyhow!("XLSX archive is missing xl/workbook.xml"))?;
    let workbook_rels_xml = read_zip_text(archive, "xl/_rels/workbook.xml.rels")?
        .ok_or_else(|| anyhow!("XLSX archive is missing xl/_rels/workbook.xml.rels"))?;

    let relationships = relationships_by_id(&workbook_rels_xml)?;
    let mut sheets = Vec::new();
    for (sheet_name, relationship_id) in workbook_sheet_relationships(&workbook_xml)? {
        let target = relationships.get(&relationship_id).ok_or_else(|| {
            anyhow!("XLSX workbook is missing sheet relationship {relationship_id}")
        })?;
        sheets.push((sheet_name, normalize_workbook_target(target)));
    }

    if sheets.is_empty() {
        bail!("XLSX workbook does not contain any sheets");
    }

    Ok(sheets)
}

fn workbook_sheet_relationships(workbook_xml: &str) -> Result<Vec<(String, String)>> {
    let mut reader = XmlReader::from_str(workbook_xml);
    reader.config_mut().trim_text(true);
    let mut sheets = Vec::new();

    loop {
        match reader.read_event()? {
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"sheet" =>
            {
                if let (Some(name), Some(relationship_id)) = (
                    attr_string(&reader, &event, b"name")?,
                    attr_string(&reader, &event, b"id")?,
                ) {
                    sheets.push((name, relationship_id));
                }
            }
            Event::Eof => return Ok(sheets),
            _ => {}
        }
    }
}

fn relationships_by_id(rels_xml: &str) -> Result<HashMap<String, String>> {
    let mut reader = XmlReader::from_str(rels_xml);
    reader.config_mut().trim_text(true);
    let mut relationships = HashMap::new();

    loop {
        match reader.read_event()? {
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"Relationship" =>
            {
                if let (Some(id), Some(target)) = (
                    attr_string(&reader, &event, b"Id")?,
                    attr_string(&reader, &event, b"Target")?,
                ) {
                    relationships.insert(id, target);
                }
            }
            Event::Eof => return Ok(relationships),
            _ => {}
        }
    }
}

fn normalize_workbook_target(target: &str) -> String {
    if target.starts_with("xl/") {
        target.to_owned()
    } else if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_owned()
    } else {
        format!("xl/{target}")
    }
}

fn read_sheet_metadata(
    archive: &mut ZipArchive<File>,
    sheet_path: &str,
    styles: &[XlsxStyle],
) -> Result<SheetMetadata> {
    let Some(sheet_xml) = read_zip_text(archive, sheet_path)? else {
        return Ok(SheetMetadata::default());
    };
    let mut reader = XmlReader::from_str(&sheet_xml);
    reader.config_mut().trim_text(true);
    let mut metadata = SheetMetadata::default();

    loop {
        match reader.read_event()? {
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"sheetFormatPr" =>
            {
                if let Some(width) = attr_f32(&reader, &event, b"defaultColWidth")? {
                    metadata.default_column_width = excel_column_width_to_px(width);
                }
                if let Some(height) = attr_f32(&reader, &event, b"defaultRowHeight")? {
                    metadata.default_row_height = points_to_px(height);
                }
            }
            Event::Start(event) | Event::Empty(event) if event.local_name().as_ref() == b"col" => {
                if let (Some(min), Some(max), Some(width)) = (
                    attr_usize(&reader, &event, b"min")?,
                    attr_usize(&reader, &event, b"max")?,
                    attr_f32(&reader, &event, b"width")?,
                ) {
                    for col_ix in min.saturating_sub(1)..max {
                        set_vec_value(
                            &mut metadata.column_widths,
                            col_ix,
                            excel_column_width_to_px(width),
                        );
                    }
                }
            }
            Event::Start(event) | Event::Empty(event) if event.local_name().as_ref() == b"row" => {
                if let (Some(row_ix), Some(height)) = (
                    attr_usize(&reader, &event, b"r")?,
                    attr_f32(&reader, &event, b"ht")?,
                ) {
                    set_vec_value(
                        &mut metadata.row_heights,
                        row_ix.saturating_sub(1),
                        points_to_px(height),
                    );
                }
            }
            Event::Start(event) | Event::Empty(event) if event.local_name().as_ref() == b"c" => {
                if let (Some(cell_ref), Some(style_ix)) = (
                    attr_string(&reader, &event, b"r")?,
                    attr_usize(&reader, &event, b"s")?,
                ) && let Some(style) = styles.get(style_ix)
                    && let Some((row_ix, col_ix)) = cell_ref_to_indices(&cell_ref)
                {
                    metadata.cell_styles.insert(
                        (row_ix, col_ix),
                        XlsxCellStyle {
                            display_format: style.display_format.clone(),
                            visual_style: style.visual_style.clone(),
                        },
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(metadata)
}

fn set_vec_value(values: &mut Vec<f32>, ix: usize, value: f32) {
    if values.len() <= ix {
        values.resize(ix + 1, 0.0);
    }
    values[ix] = value;
}

fn excel_column_width_to_px(width: f32) -> f32 {
    (width * 7.0 + 5.0).max(24.0)
}

fn points_to_px(points: f32) -> f32 {
    (points * 4.0 / 3.0).max(12.0)
}

fn cell_ref_to_indices(cell_ref: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    let mut row = String::new();

    for ch in cell_ref.chars() {
        if ch.is_ascii_alphabetic() {
            col = (col * 26) + usize::from(ch.to_ascii_uppercase() as u8 - b'A' + 1);
        } else if ch.is_ascii_digit() {
            row.push(ch);
        }
    }

    let row = row.parse::<usize>().ok()?;
    Some((row.checked_sub(1)?, col.checked_sub(1)?))
}

fn read_zip_text(archive: &mut ZipArchive<File>, path: &str) -> Result<Option<String>> {
    let Ok(mut file) = archive.by_name(path) else {
        return Ok(None);
    };
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .with_context(|| format!("failed to read {path} from XLSX archive"))?;
    Ok(Some(contents))
}

fn attr_string(
    reader: &XmlReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>> {
    for attr in event.attributes() {
        let attr = attr?;
        if attr.key.local_name().as_ref() == name {
            return Ok(Some(
                attr.decode_and_unescape_value(reader.decoder())?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

fn attr_usize(
    reader: &XmlReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<usize>> {
    attr_string(reader, event, name)?
        .map(|value| {
            value
                .parse::<usize>()
                .with_context(|| format!("invalid numeric XML attribute value '{value}'"))
        })
        .transpose()
}

fn attr_f32(
    reader: &XmlReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<f32>> {
    attr_string(reader, event, name)?
        .map(|value| {
            value
                .parse::<f32>()
                .with_context(|| format!("invalid numeric XML attribute value '{value}'"))
        })
        .transpose()
}

fn attr_rgb(
    reader: &XmlReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
) -> Result<Option<u32>> {
    let Some(rgb) = attr_string(reader, event, b"rgb")? else {
        return Ok(None);
    };
    Ok(parse_argb_color(&rgb))
}

fn parse_argb_color(value: &str) -> Option<u32> {
    let rgb = if value.len() == 8 { &value[2..] } else { value };
    u32::from_str_radix(rgb, 16).ok()
}

fn color_hex(color: u32) -> String {
    format!("{color:06x}")
}

fn round2(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

fn column_name(mut index: usize) -> String {
    let mut name = String::new();

    loop {
        let remainder = index % 26;
        name.insert(
            0,
            char::from(b'A' + u8::try_from(remainder).expect("column remainder")),
        );

        if index < 26 {
            break;
        }

        index = (index / 26) - 1;
    }

    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, io::Write as _, time::SystemTime};
    use zip::{ZipWriter, write::SimpleFileOptions};

    #[test]
    fn rejects_unsupported_extension() {
        let error = load_workbook(Path::new("book.tsv")).unwrap_err();
        assert!(error.to_string().contains("unsupported file extension"));
    }

    #[test]
    fn loads_csv_with_quoted_and_uneven_rows() {
        let path = temp_file("spread-test.csv");
        fs::write(&path, "name,note\nAda,\"hello, csv\"\nGrace\n").unwrap();

        let workbook = load_csv(&path).unwrap();

        assert_eq!(workbook.row_count(), 3);
        assert_eq!(workbook.col_count(), 2);
        assert_eq!(workbook.cell(1, 0), "Ada");
        assert_eq!(workbook.cell(1, 1), "hello, csv");
        assert_eq!(workbook.cell(2, 0), "Grace");
        assert_eq!(workbook.cell(2, 1), "");

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_empty_csv_as_empty_grid() {
        let path = temp_file("spread-empty.csv");
        fs::write(&path, "").unwrap();

        let workbook = load_csv(&path).unwrap();

        assert_eq!(workbook.row_count(), 0);
        assert_eq!(workbook.col_count(), 0);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_cloud_usage_xlsx_fixture() {
        let path = Path::new("cloud-usage.xlsx");
        if !path.exists() {
            return;
        }

        let workbook = load_xlsx(path).unwrap();

        assert!(workbook.row_count() > 0);
        assert!(workbook.col_count() > 0);
        assert_eq!(workbook.cell(0, 4), "2026-02-09");
        assert!((0..workbook.row_count()).any(|row_ix| {
            (0..workbook.col_count()).any(|col_ix| !workbook.cell(row_ix, col_ix).is_empty())
        }));
    }

    #[test]
    fn loads_pydantic_by_numbers_dollar_formats() {
        let path = Path::new("pydantic-by-numbers.xlsx");
        if !path.exists() {
            return;
        }

        let workbook = load_xlsx(path).unwrap();

        assert_eq!(workbook.cell(1, 0), "2025-01-01");
        assert_eq!(workbook.cell(1, 2), "$2,213");
        assert_eq!(workbook.cell(1, 5), "$26,556");
        assert_eq!(workbook.cell(2, 7), "113.74%");
        assert!(workbook.cell_data(0, 0).style.bold);
        assert!((workbook.column_width(0) - excel_column_width_to_px(18.38)).abs() < 0.01);
        assert!((workbook.column_width(6) - excel_column_width_to_px(12.63)).abs() < 0.01);
        assert!((workbook.row_height(0) - points_to_px(15.75)).abs() < 0.01);
        assert!(workbook.sheet_height() > 0.0);
    }

    #[test]
    fn reads_xlsx_visual_styles_and_dimensions_from_metadata() {
        let path = temp_file("spread-styled.xlsx");
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="2">
    <font></font>
    <font><b/><color rgb="FF112233"/></font>
  </fonts>
  <fills count="3">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
    <fill><patternFill patternType="solid"><fgColor rgb="FFAABBCC"/></patternFill></fill>
  </fills>
  <cellXfs count="2">
    <xf fontId="0" fillId="0" numFmtId="0"/>
    <xf fontId="1" fillId="2" numFmtId="0"/>
  </cellXfs>
</styleSheet>"#;
        let sheet_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetFormatPr defaultColWidth="9" defaultRowHeight="18"/>
  <cols><col min="2" max="3" width="20"/></cols>
  <sheetData>
    <row r="4" ht="30"><c r="A4" s="1"/></row>
  </sheetData>
</worksheet>"#;
        write_metadata_xlsx(&path, styles_xml, sheet_xml);

        let metadata = XlsxMetadata::read(&path).unwrap();
        let sheet_metadata = metadata.sheet_metadata("Sheet1");
        let style = metadata.cell_style(3, 0).visual_style;

        assert!(style.bold);
        assert_eq!(style.text_color, Some(0x11_22_33));
        assert_eq!(style.background_color, Some(0xaa_bb_cc));
        assert!((sheet_metadata.default_column_width - excel_column_width_to_px(9.0)).abs() < 0.01);
        assert!((sheet_metadata.column_widths[1] - excel_column_width_to_px(20.0)).abs() < 0.01);
        assert!((sheet_metadata.column_widths[2] - excel_column_width_to_px(20.0)).abs() < 0.01);
        assert!((sheet_metadata.default_row_height - points_to_px(18.0)).abs() < 0.01);
        assert!((sheet_metadata.row_heights[3] - points_to_px(30.0)).abs() < 0.01);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_cells_display_blank() {
        assert_eq!(display_cell(&Data::Empty, None), "");
        assert_eq!(display_cell(&Data::Int(42), None), "42");
        assert_eq!(display_cell(&Data::String("text".to_owned()), None), "text");
    }

    #[test]
    fn formats_dollar_values() {
        let currency = CellDisplayFormat::Currency { decimals: 2 };

        assert_eq!(currency.format_number(1234.5), "$1,234.50");
        assert_eq!(currency.format_number(-1234.5), "-$1,234.50");
    }

    #[test]
    fn formats_percentage_values() {
        let one_decimal = CellDisplayFormat::Percentage { decimals: 1 };
        let two_decimals = CellDisplayFormat::from_format_code("0.00%").unwrap();

        assert_eq!(one_decimal.format_number(0.152), "15.2%");
        assert_eq!(two_decimals.format_number(1.137_370_086), "113.74%");
    }

    #[test]
    fn selection_summary_counts_cells_and_numeric_values() {
        let sheet = SheetData::new(
            Some("Sheet1".to_owned()),
            vec![
                vec![
                    CellData {
                        value: "1".to_owned(),
                        raw_value: CellRawValue::Number(1.0),
                        ..Default::default()
                    },
                    CellData {
                        value: "text".to_owned(),
                        raw_value: CellRawValue::Text,
                        ..Default::default()
                    },
                ],
                vec![
                    CellData {
                        value: "3".to_owned(),
                        raw_value: CellRawValue::Number(3.0),
                        ..Default::default()
                    },
                    CellData {
                        value: "2026-01-01".to_owned(),
                        raw_value: CellRawValue::DateTime,
                        ..Default::default()
                    },
                ],
            ],
            Vec::new(),
            Vec::new(),
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        let summary =
            sheet.summary_for_range(CellRange::new(CellCoord::new(0, 0), CellCoord::new(1, 1)));

        assert_eq!(summary.selected_cells, 4);
        assert_eq!(summary.numeric_cells, 2);
        assert!((summary.sum - 4.0).abs() < f64::EPSILON);
        assert_eq!(summary.min, Some(1.0));
        assert_eq!(summary.max, Some(3.0));
    }

    #[test]
    fn cell_range_identifies_selection_edges() {
        let range = CellRange::new(CellCoord::new(1, 1), CellCoord::new(3, 3));

        assert!(range.edge_sides(1, 2).top());
        assert!(range.edge_sides(2, 1).left());
        assert!(range.edge_sides(3, 2).bottom());
        assert!(range.edge_sides(2, 3).right());
        assert!(!range.edge_sides(2, 2).any());
        assert!(!range.edge_sides(4, 2).any());
    }

    fn temp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{nanos}-{name}"))
    }

    fn write_metadata_xlsx(path: &Path, styles_xml: &str, sheet_xml: &str) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        write_zip_entry(
            &mut zip,
            "xl/workbook.xml",
            r#"<workbook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            options,
        );
        write_zip_entry(
            &mut zip,
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
            options,
        );
        write_zip_entry(&mut zip, "xl/styles.xml", styles_xml, options);
        write_zip_entry(&mut zip, "xl/worksheets/sheet1.xml", sheet_xml, options);
        zip.finish().unwrap();
    }

    fn write_zip_entry(
        zip: &mut ZipWriter<File>,
        name: &str,
        contents: &str,
        options: SimpleFileOptions,
    ) {
        zip.start_file(name, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
}
